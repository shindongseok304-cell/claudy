use std::io;
use std::process::Stdio;
use std::time::{Duration, Instant};

use base64::Engine;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::{Child, Command};

use crate::config::HostCfg;
use crate::util;

/// 실행할 자식 프로세스 명세
#[derive(Debug, Clone)]
pub struct Spawnable {
    pub program: String,
    pub args: Vec<String>,
    pub stdin: Option<String>,
    pub cwd: Option<String>,
    /// Windows 에서 실행 파일을 못 찾으면 `cmd /C` 로 한 번 더 시도할지 여부 (claude.cmd 대응)
    pub cmd_fallback: bool,
}

#[derive(Debug)]
pub struct Captured {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub duration_ms: u128,
    pub timed_out: bool,
}

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 호스트 이름, 사용자, 파일 이름 등에 쓸 수 있는 안전한 토큰인지 검사
pub fn valid_token(s: &str) -> bool {
    !s.is_empty()
        && !s.starts_with('-')
        && s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

pub fn ps_encode(script: &str) -> String {
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

pub fn home_string() -> String {
    util::home_dir().to_string_lossy().into_owned()
}

pub fn expand_tilde(p: &str) -> String {
    if p == "~" {
        return home_string();
    }
    if let Some(rest) = p.strip_prefix("~/").or_else(|| p.strip_prefix("~\\")) {
        return util::home_dir().join(rest).to_string_lossy().into_owned();
    }
    p.to_string()
}

/// 이 PC 에서 셸 명령을 실행하는 명세. powershell 은 -EncodedCommand 로 넘겨 인용 문제를 피한다.
pub fn local_plan(host: &HostCfg, cwd: Option<String>, body: &str) -> Result<Spawnable, String> {
    let cwd = expand_tilde(&cwd.unwrap_or_else(home_string));
    let (program, args): (&str, Vec<String>) = match host.shell_name() {
        "powershell" => {
            let script = format!("$ProgressPreference = 'SilentlyContinue'\ntry {{ [Console]::OutputEncoding = [Text.Encoding]::UTF8 }} catch {{}}\n{body}");
            (
                "powershell",
                ["-NoProfile", "-OutputFormat", "Text", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand"]
                    .iter()
                    .map(|s| s.to_string())
                    .chain(std::iter::once(ps_encode(&script)))
                    .collect(),
            )
        }
        "cmd" => ("cmd", vec!["/C".into(), body.to_string()]),
        "sh" => ("sh", vec!["-c".into(), body.to_string()]),
        // 로그인 셸로 실행해 ~/.zprofile 등의 PATH(Homebrew, ~/.local/bin)를 읽는다
        "zsh" => ("/bin/zsh", vec!["-l".into(), "-c".into(), body.to_string()]),
        "bash" => ("/bin/bash", vec!["-l".into(), "-c".into(), body.to_string()]),
        other => return Err(format!("지원하지 않는 shell 값이다: {other}")),
    };
    Ok(Spawnable {
        program: program.to_string(),
        args,
        stdin: None,
        cwd: Some(cwd),
        cmd_fallback: false,
    })
}

fn cd_part(dir: &str) -> String {
    if dir == "~" {
        "cd ~".to_string()
    } else if let Some(rest) = dir.strip_prefix("~/") {
        format!("cd ~/{}", util::sh_quote(rest))
    } else {
        format!("cd {}", util::sh_quote(dir))
    }
}

/// 원격에서 실행할 스크립트. 작업 폴더가 있으면 먼저 cd 한다.
pub fn remote_script(cwd: Option<&str>, body: &str) -> String {
    match cwd {
        Some(d) if !d.is_empty() => format!("{} || exit 1\n{body}", cd_part(d)),
        _ => body.to_string(),
    }
}

/// Windows(PowerShell) 원격용 작업 폴더 이동
fn ps_cd_part(dir: &str) -> String {
    if dir == "~" {
        "Set-Location ~".to_string()
    } else {
        let d = dir
            .strip_prefix("~/")
            .or_else(|| dir.strip_prefix("~\\"))
            .map(|r| r.replace('`', "``").replace('"', "`\"").replace('$', "`$"));
        match d {
            Some(r) => format!("Set-Location -LiteralPath \"$HOME\\{r}\" -ErrorAction Stop"),
            None => format!("Set-Location -LiteralPath '{}' -ErrorAction Stop", dir.replace('\'', "''")),
        }
    }
}

/// 호스트 종류에 맞는 원격 스크립트
pub fn remote_script_for(host: &HostCfg, cwd: Option<&str>, body: &str) -> String {
    if !host.remote_windows() {
        return remote_script(cwd, body);
    }
    match cwd {
        Some(d) if !d.is_empty() => format!("{}\n{body}", ps_cd_part(d)),
        _ => body.to_string(),
    }
}

/// Windows 원격 실행 틀: 출력과 파이프를 UTF-8 로 맞추고, 마지막 실행 파일의 종료 코드를 돌려준다
pub fn ps_remote_wrap(script: &str) -> String {
    format!(
        "$ProgressPreference='SilentlyContinue'\ntry {{ [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false) }} catch {{}}\n$OutputEncoding=[Text.UTF8Encoding]::new($false)\n$global:LASTEXITCODE=0\n{script}\nif ($LASTEXITCODE) {{ exit $LASTEXITCODE }} elseif (-not $?) {{ exit 1 }}"
    )
}

/// 화면 세션 실행기. ssh(세션 0)에서 받은 스크립트를 로그온한 사용자의 화면 세션에서 예약 작업으로 실행하고,
/// 끝날 때까지 기다려 표준출력·표준오류·종료 코드를 그대로 돌려준다. 임시 폴더와 예약 작업은 지운다.
/// 실행할 본문은 표준입력으로 받은 $payload 에 들어 있다(명령줄 길이 제한을 피한다)
pub fn desktop_launcher() -> String {
    format!(
        r#"$id=[guid]::NewGuid().ToString('N').Substring(0,10)
$dir=Join-Path $env:TEMP "claudy-$id"; New-Item -ItemType Directory -Path $dir | Out-Null
$rlog=Join-Path $env:USERPROFILE '.claudy\remote.log'
function RL($m) {{ try {{ if ((Test-Path $rlog) -and (Get-Item $rlog).Length -gt 1MB) {{ Move-Item -Force $rlog "$rlog.1" }}; [IO.File]::AppendAllText($rlog, (Get-Date -Format 'yyyy-MM-dd HH:mm:ss.fff') + " $id $m`r`n") }} catch {{}} }}
RL "시작 길이 $($payload.Length)"
$utf8=New-Object Text.UTF8Encoding $true
[IO.File]::WriteAllText("$dir\body.ps1", $payload, $utf8)
$run = @"
Set-Content -Path '$dir\started' -Value (Get-Date -Format 'HH:mm:ss.fff')
`$env:PYTHONIOENCODING='utf-8'
cmd /c "chcp 65001 >nul & powershell.exe -NoProfile -NonInteractive -ExecutionPolicy Bypass -File `"$dir\body.ps1`" > `"$dir\out.txt`" 2> `"$dir\err.txt`""
Set-Content -Path '$dir\code' -Value `$LASTEXITCODE
Set-Content -Path '$dir\done' -Value (Get-Date -Format 'HH:mm:ss.fff')
"@
[IO.File]::WriteAllText("$dir\run.ps1", $run, $utf8)
# 설정 앱(0.6.7 이상)이 떠 있으면 파이프로 실행을 맡긴다(예약 작업보다 4~5초 빠르다)
$task=$null
try {{
  $pc=New-Object IO.Pipes.NamedPipeClientStream('.', "claudy-desk-$env:USERNAME", [IO.Pipes.PipeDirection]::InOut)
  $pc.Connect(1000)
  $pw=New-Object IO.StreamWriter($pc, (New-Object Text.UTF8Encoding $false)); $pw.AutoFlush=$true
  $pw.WriteLine("RUN $dir")
  $ans=(New-Object IO.StreamReader($pc)).ReadLine()
  $pc.Dispose()
}} catch {{ $ans=$null }}
RL "파이프 $ans"
if ($ans -ne 'OK') {{
$task="Claudy-$id"
$act=New-ScheduledTaskAction -Execute 'powershell.exe' -Argument "-NoProfile -NonInteractive -ExecutionPolicy Bypass -WindowStyle Hidden -File `"$dir\run.ps1`""
$pr=New-ScheduledTaskPrincipal -UserId (whoami) -LogonType Interactive
$set=New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Hours 3) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries
Register-ScheduledTask -TaskName $task -Action $act -Principal $pr -Settings $set -Force | Out-Null
Start-ScheduledTask -TaskName $task
}}
$t0=Get-Date
while (-not (Test-Path "$dir\done")) {{
  Start-Sleep -Milliseconds 100
  if (((Get-Date)-$t0).TotalMinutes -gt 31) {{ RL "기다림 중단(31분)"; exit 124 }}
  if (-not (Test-Path "$dir\started") -and ((Get-Date)-$t0).TotalSeconds -gt 15) {{
    if ($task) {{ Unregister-ScheduledTask -TaskName $task -Confirm:$false }}
    [Console]::Error.WriteLine('화면 세션에서 실행하지 못했다. 이 PC 에 사용자가 로그온해 있어야 한다(잠금 화면은 괜찮다)')
    exit 125
  }}
}}
RL "본문 시작 $((Get-Content "$dir\started" -EA 0) -join '') 끝 $((Get-Content "$dir\done" -EA 0) -join '') 코드 $((Get-Content "$dir\code" -EA 0) -join '')"
if ($task) {{ Unregister-ScheduledTask -TaskName $task -Confirm:$false }}
$strict=New-Object Text.UTF8Encoding $false, $true
$rd={{ param($f) if (-not (Test-Path $f)) {{ return '' }}; $b=[IO.File]::ReadAllBytes($f); try {{ $strict.GetString($b).TrimStart([char]0xFEFF) }} catch {{ [Text.Encoding]::Default.GetString($b) }} }}
[Console]::Out.Write((& $rd "$dir\out.txt"))
$e=(& $rd "$dir\err.txt"); if ($e) {{ [Console]::Error.Write($e) }}
$code=[int]((& $rd "$dir\code").Trim())
Remove-Item -Recurse -Force $dir -ErrorAction SilentlyContinue
RL "끝"
exit $code"#
    )
}

/// 표준입력의 base64 를 풀어 $payload 에 담는 머리말
const PS_STDIN_PAYLOAD: &str = "$payload=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String([Console]::In.ReadToEnd().Trim()))";

/// Windows 원격에서 base64 로 받은 글을 실행 파일의 표준입력으로 넘기는 명령
pub fn ps_pipe_b64(text: &str, cmd: &str) -> String {
    format!("[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}')) | {cmd}", util::b64(text))
}

/// 접속할 주소를 고른다. alt_host 가 있으면 host 에 1.2초 안에 TCP 연결이 되는지 보고, 안 되면 alt_host 를 쓴다.
/// 고른 주소와, 두 주소가 같은 기기임을 ssh 에 알리는 HostKeyAlias(설정에 없을 때 host) 를 돌려준다
pub fn pick_addr(v: &HostCfg) -> (String, Option<String>) {
    let h = v.host.clone().unwrap_or_default();
    let alt = v.alt_host.as_deref().map(str::trim).filter(|s| !s.is_empty());
    let Some(alt) = alt else { return (h, None) };
    let port = v.port.unwrap_or(22);
    let alias = v.host_key_alias.clone().filter(|s| !s.is_empty()).or_else(|| Some(h.clone()));
    use std::net::ToSocketAddrs;
    let reach = |host: &str, ms: u64| {
        format!("{host}:{port}")
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.next())
            .map(|a| std::net::TcpStream::connect_timeout(&a, std::time::Duration::from_millis(ms)).is_ok())
            .unwrap_or(false)
    };
    if reach(&h, 1500) {
        return (h, alias);
    }
    // 바깥 주소도 닿지 않으면(집 안에서 바깥 주소로 되돌아오지 못하는 공유기 등) 원래 주소를 쓴다
    if reach(alt, 1500) { (alt.to_string(), alias) } else { (h, alias) }
}

/// 경유 호스트를 거쳐 가는 ProxyCommand. ssh -W 로 목적지까지 통로만 연다
pub fn proxy_command(v: &HostCfg) -> Result<String, String> {
    let (picked, alias) = pick_addr(v);
    let h = picked.as_str();
    let u = v.user.as_deref().unwrap_or("");
    if !valid_token(h) || !valid_token(u) {
        return Err(format!("경유 호스트 `{}` 의 host 또는 user 값이 올바르지 않다", v.name));
    }
    let mut c = String::from("ssh -o BatchMode=yes -o ConnectTimeout=10");
    if let Some(a) = alias.filter(|a| valid_token(a)) {
        c.push_str(&format!(" -o HostKeyAlias={a}"));
    }
    if v.accept_new_host_key() {
        c.push_str(" -o StrictHostKeyChecking=accept-new");
    }
    if let Some(id) = v.identity_file.as_deref().filter(|s| !s.is_empty()) {
        if id.starts_with('-') || id.contains('"') {
            return Err("경유 호스트의 identity_file 값이 올바르지 않다".into());
        }
        // ProxyCommand 는 macOS·Linux 에서 sh 로, Windows 에서 직접 실행된다. 공백 경로는 큰따옴표로 감싼다
        if id.contains(' ') {
            c.push_str(&format!(" -i \"{id}\""));
        } else {
            c.push_str(&format!(" -i {id}"));
        }
    }
    c.push_str(&format!(" -p {} -W %h:%p {u}@{h}", v.port.unwrap_or(22)));
    Ok(c)
}

/// ssh·scp 공통 접속 옵션과 「사용자@주소」. port_flag 는 ssh 가 -p, scp 가 -P 다
pub fn ssh_base(host: &HostCfg, port_flag: &str) -> Result<(Vec<String>, String), String> {
    let (picked, alt_alias) = if host.jump_via.is_some() { (host.host.clone().unwrap_or_default(), None) } else { pick_addr(host) };
    let h = picked.as_str();
    let u = host.user.as_deref().unwrap_or("");
    if !valid_token(h) || !valid_token(u) {
        return Err(format!("host `{}` 의 host 또는 user 값이 올바르지 않다", host.name));
    }
    let mut args: Vec<String> = [
        "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=30",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if host.accept_new_host_key() {
        args.push("-o".into());
        args.push("StrictHostKeyChecking=accept-new".into());
    }
    if let Some(a) = host.host_key_alias.clone().filter(|s| !s.is_empty()).or(alt_alias).as_deref() {
        if !valid_token(a) {
            return Err("host_key_alias 값이 올바르지 않다".into());
        }
        args.push("-o".into());
        args.push(format!("HostKeyAlias={a}"));
    }
    if let Some(v) = host.jump_via.as_deref() {
        args.push("-o".into());
        args.push(format!("ProxyCommand={}", proxy_command(v)?));
    }
    args.push(port_flag.into());
    args.push(host.port.unwrap_or(22).to_string());
    if let Some(id) = host.identity_file.as_deref().filter(|s| !s.is_empty()) {
        if id.starts_with('-') {
            return Err("identity_file 값이 올바르지 않다".into());
        }
        args.push("-i".into());
        args.push(id.to_string());
    }
    Ok((args, format!("{u}@{h}")))
}

/// scp 명세. upload 면 local → host:remote, 아니면 host:remote → local
pub fn scp_plan(host: &HostCfg, local: &str, remote: &str, upload: bool, recursive: bool) -> Result<Spawnable, String> {
    if host.kind != "ssh" {
        return Err("scp 는 ssh 호스트에만 쓴다".into());
    }
    if local.starts_with('-') || remote.starts_with('-') || local.is_empty() || remote.is_empty() {
        return Err("경로가 올바르지 않다".into());
    }
    let (mut args, target) = ssh_base(host, "-P")?;
    args.insert(0, "-q".into());
    if recursive {
        args.insert(0, "-r".into());
    }
    let r = format!("{target}:{remote}");
    if upload {
        args.push(local.into());
        args.push(r);
    } else {
        args.push(r);
        args.push(local.into());
    }
    Ok(Spawnable { program: "scp".into(), args, stdin: None, cwd: None, cmd_fallback: false })
}

/// ssh 실행 명세. 원격 스크립트는 base64 로 넘긴다.
pub fn ssh_plan(host: &HostCfg, script: &str) -> Result<Spawnable, String> {
    let (mut args, target) = ssh_base(host, "-p")?;
    args.push(target);
    args.push("--".into());
    if host.remote_windows() {
        if host.as_root() {
            return Err("Windows 원격 호스트는 root 권한 실행을 지원하지 않는다. run_as_root 를 끈다".into());
        }
        // Windows sshd 의 기본 셸을 PowerShell 로 둔다고 가정한다(설정 안내 참고).
        // 바깥 PowerShell(-c)은 안쪽 종료 코드를 1 로 뭉개므로 $LASTEXITCODE 로 다시 넘긴다
        // 본문은 표준입력(base64)으로 보내고 명령줄에는 짧은 실행기만 둔다(Windows 명령줄 32K 제한)
        let stub = if host.desktop_session() {
            format!("$ProgressPreference='SilentlyContinue'\ntry {{ [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false) }} catch {{}}\n{PS_STDIN_PAYLOAD}\n{}", desktop_launcher())
        } else {
            format!("$ProgressPreference='SilentlyContinue'\n{PS_STDIN_PAYLOAD}\n. ([scriptblock]::Create($payload))")
        };
        args.push(format!(
            "powershell -NoProfile -NonInteractive -ExecutionPolicy Bypass -OutputFormat Text -EncodedCommand {}; exit $LASTEXITCODE",
            ps_encode(&stub)
        ));
        let payload = util::b64(&ps_remote_wrap(script));
        return Ok(Spawnable { program: "ssh".into(), args, stdin: Some(payload), cwd: None, cmd_fallback: false });
    }
    // root 권한으로 실행: 비밀번호를 묻지 않는 sudo(-n)로 root 로그인 셸을 띄운다. sudo 가 비밀번호를 요구하면 바로 실패한다
    let shell = if host.as_root() { "sudo -n -i bash -s" } else { "bash" };
    args.push(format!("echo {} | base64 -d | {shell}", util::b64(script)));
    Ok(Spawnable { program: "ssh".into(), args, stdin: None, cwd: None, cmd_fallback: false })
}

/// 셸 명령 실행 명세 (호스트 종류에 따라 local 또는 ssh)
pub fn command_plan(host: &HostCfg, cwd: Option<String>, body: &str) -> Result<Spawnable, String> {
    if host.kind == "ssh" {
        ssh_plan(host, &host.with_startup(remote_script_for(host, cwd.as_deref(), body)))
    } else {
        local_plan(host, cwd, body)
    }
}

fn try_spawn(program: &str, args: &[String], sp: &Spawnable) -> io::Result<Child> {
    let mut cmd = Command::new(program);
    cmd.args(args);
    if let Some(d) = &sp.cwd {
        cmd.current_dir(d);
    }
    cmd.stdin(if sp.stdin.is_some() { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd.spawn()
}

/// 자식 프로세스를 띄운다. Windows 에서 실행 파일을 못 찾으면 cmd /C 로 한 번 더 시도한다.
pub fn spawn_child(sp: &Spawnable) -> io::Result<Child> {
    match try_spawn(&sp.program, &sp.args, sp) {
        Err(e) if e.kind() == io::ErrorKind::NotFound && sp.cmd_fallback && cfg!(windows) => {
            let mut args = vec!["/C".to_string(), sp.program.clone()];
            args.extend(sp.args.iter().cloned());
            try_spawn("cmd", &args, sp)
        }
        other => other,
    }
}

/// 표준입력 내용을 백그라운드로 써 넣는다 (출력 파이프가 막히지 않도록)
pub fn feed_stdin(child: &mut Child, input: Option<&str>) {
    if let (Some(data), Some(mut si)) = (input, child.stdin.take()) {
        let data = data.to_string();
        tokio::spawn(async move {
            let _ = si.write_all(data.as_bytes()).await;
            let _ = si.shutdown().await;
        });
    }
}

/// Windows 에서 자식과 그 하위 프로세스를 함께 끝낸다. 자식이 살아 있을 때 불러야 트리를 찾을 수 있다.
#[cfg(windows)]
async fn kill_tree(pid: Option<u32>) {
    if let Some(pid) = pid {
        let mut cmd = Command::new("taskkill");
        cmd.args(["/T", "/F", "/PID", &pid.to_string()])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        cmd.creation_flags(CREATE_NO_WINDOW);
        let _ = cmd.status().await;
    }
}

#[cfg(not(windows))]
async fn kill_tree(_pid: Option<u32>) {}

async fn read_all<R: AsyncRead + Unpin>(pipe: Option<R>) -> Vec<u8> {
    let mut buf = Vec::new();
    if let Some(mut p) = pipe {
        let _ = p.read_to_end(&mut buf).await;
    }
    buf
}

/// 실행하고 끝날 때까지 기다린다. 제한 시간을 넘으면 프로세스 트리를 끝내고 timed_out 을 표시한다.
/// pid 는 wait 전에 미리 받아 둔다. 시간이 넘으면 먼저 taskkill 로 트리를 끝내야 하위 프로세스를 찾을 수 있다.
pub async fn run(sp: &Spawnable, limit: Duration) -> Result<Captured, String> {
    let started = Instant::now();
    let mut child = spawn_child(sp).map_err(|e| format!("실행 실패 ({}): {e}", sp.program))?;
    let pid = child.id();
    feed_stdin(&mut child, sp.stdin.as_deref());
    let out_pipe = child.stdout.take();
    let err_pipe = child.stderr.take();
    let work = async {
        tokio::join!(read_all(out_pipe), read_all(err_pipe), child.wait())
    };
    match tokio::time::timeout(limit, work).await {
        Ok((out, err, status)) => {
            let status = status.map_err(|e| format!("프로세스 대기 중 오류: {e}"))?;
            Ok(Captured {
                exit_code: status.code(),
                stdout: util::decode_clixml(&util::decode_bytes(&out)),
                stderr: util::decode_clixml(&util::decode_bytes(&err)),
                duration_ms: started.elapsed().as_millis(),
                timed_out: false,
            })
        }
        Err(_) => {
            kill_tree(pid).await;
            let _ = child.start_kill();
            Ok(Captured {
                exit_code: None,
                stdout: String::new(),
                stderr: String::new(),
                duration_ms: started.elapsed().as_millis(),
                timed_out: true,
            })
        }
    }
}

/// ssh 의 호스트 키 확인 실패 시 응답 끝에 붙일 안내. 키 자동 등록을 이미 켠 호스트와 키가 바뀐 경우에는 붙이지 않는다.
pub fn host_key_hint(host: &HostCfg, stderr: &str) -> Option<&'static str> {
    if host.kind != "ssh" || host.accept_new_host_key() {
        return None;
    }
    if stderr.contains("Host key verification failed") && !stderr.contains("REMOTE HOST IDENTIFICATION") {
        return Some("처음 접속하는 서버다. hosts.toml 의 이 호스트에 accept_new_host_key = true 를 넣거나 PC 에서 ssh 로 한 번 접속해 호스트 키를 등록하라");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alt_host_fallback() {
        let mut h = ssh_host();
        h.host = Some("127.0.0.1".into());
        h.port = Some(1);
        // 두 주소 모두 닿지 않으면 원래 주소를 쓴다
        h.alt_host = Some("127.0.0.2".into());
        let (addr, alias) = pick_addr(&h);
        assert_eq!(addr, "127.0.0.1");
        assert_eq!(alias.as_deref(), Some("127.0.0.1"));
        // 원래 주소가 막히고 바깥 주소가 열려 있으면 바깥 주소
        let l = std::net::TcpListener::bind("127.0.0.3:0").unwrap();
        let p = l.local_addr().unwrap().port();
        h.port = Some(p);
        h.host = Some("127.0.0.4".into());
        h.alt_host = Some("127.0.0.3".into());
        assert_eq!(pick_addr(&h).0, "127.0.0.3");
        h.alt_host = None;
        assert_eq!(pick_addr(&h), ("127.0.0.4".to_string(), None));
    }

    fn ssh_host() -> HostCfg {
        HostCfg {
            name: "pi".into(),
            kind: "ssh".into(),
            shell: None,
            claude: None,
            workdir: None,
            host: Some("server.example.com".into()),
            port: Some(2222),
            user: Some("user".into()),
            identity_file: Some("C:/keys/id".into()),
            accept_new_host_key: None,
            claude_permissions: None,
            startup: None,
            run_as_root: None,
            desktop: None,
            jump: None,
            host_key_alias: None,
            alt_host: None,
            jump_via: None,
        }
    }

    #[test]
    fn ssh_args_are_assembled() {
        let sp = ssh_plan(&ssh_host(), "echo hi").unwrap();
        assert_eq!(sp.program, "ssh");
        let a = &sp.args;
        assert_eq!(&a[0..2], &["-o", "BatchMode=yes"]);
        assert!(a.windows(2).any(|w| w == ["-p", "2222"]));
        assert!(a.windows(2).any(|w| w == ["-i", "C:/keys/id"]));
        assert!(a.contains(&"user@server.example.com".to_string()));
        let sep = a.iter().position(|s| s == "--").unwrap();
        assert_eq!(a[sep + 1], format!("echo {} | base64 -d | bash", util::b64("echo hi")));
    }

    #[test]
    fn ssh_rejects_option_like_host() {
        let mut h = ssh_host();
        h.host = Some("-oProxyCommand=x".into());
        assert!(ssh_plan(&h, "x").is_err());
    }

    #[test]
    fn ssh_accept_new_adds_option() {
        let mut h = ssh_host();
        assert!(!ssh_plan(&h, "x").unwrap().args.iter().any(|s| s.contains("StrictHostKeyChecking")));
        h.accept_new_host_key = Some(true);
        let a = ssh_plan(&h, "x").unwrap().args;
        assert!(a.windows(2).any(|w| w == ["-o", "StrictHostKeyChecking=accept-new"]));
    }

    #[test]
    fn host_key_hint_only_for_ssh_first_contact() {
        let mut h = ssh_host();
        let msg = "Host key verification failed.";
        assert!(host_key_hint(&h, msg).is_some());
        assert!(host_key_hint(&h, "ok").is_none());
        assert!(host_key_hint(&h, "REMOTE HOST IDENTIFICATION HAS CHANGED\nHost key verification failed.").is_none());
        h.accept_new_host_key = Some(true);
        assert!(host_key_hint(&h, msg).is_none());
        h.kind = "local".into();
        h.accept_new_host_key = None;
        assert!(host_key_hint(&h, msg).is_none());
    }

    #[test]
    fn windows_remote_plan() {
        let mut h = ssh_host();
        h.shell = Some("powershell".into());
        assert!(h.remote_windows());
        let sp = ssh_plan(&h, "Get-Date").unwrap();
        let last = sp.args.last().unwrap();
        assert!(last.starts_with("powershell -NoProfile") && last.contains("-EncodedCommand "));
        let raw = base64::engine::general_purpose::STANDARD.decode(sp.stdin.as_deref().unwrap()).unwrap();
        let text = String::from_utf8(raw).unwrap();
        assert!(text.contains("Get-Date") && text.contains("exit $LASTEXITCODE"));
        h.desktop = Some(true);
        let d = ssh_plan(&h, "Get-Date").unwrap();
        let b64 = d.args.last().unwrap().split(' ').find(|w| w.len() > 40).unwrap().trim_end_matches(';');
        let raw = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
        let u16s: Vec<u16> = raw.chunks(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        let stub = String::from_utf16(&u16s).unwrap();
        assert!(stub.contains("Register-ScheduledTask") && stub.contains("$payload"));
        h.desktop = None;
        assert_eq!(remote_script_for(&h, Some("~/proj a"), "ls"), "Set-Location -LiteralPath \"$HOME\\proj a\" -ErrorAction Stop\nls");
        assert_eq!(remote_script_for(&h, Some("D:\\x'y"), "ls"), "Set-Location -LiteralPath 'D:\\x''y' -ErrorAction Stop\nls");
        h.startup = Some("cd D:\\w\n# memo\n".into());
        assert_eq!(h.with_startup("ls".into()), "cd D:\\w\nif (-not $?) { exit 97 }\nls");
        h.run_as_root = Some(true);
        assert!(ssh_plan(&h, "x").is_err());
    }

    #[test]
    fn jump_host_proxy() {
        let text = "[[host]]\nname='relay'\nkind='ssh'\nhost='10.0.0.5'\nuser='me'\nidentity_file='/k/id a'\n\n[[host]]\nname='w'\nkind='ssh'\nhost='127.0.0.1'\nport=22022\nuser='user'\nshell='powershell'\njump='relay'\nhost_key_alias='pc'\n";
        let mut cfg: crate::config::Config = toml::from_str(text).unwrap();
        crate::config::resolve_jumps_for_test(&mut cfg).unwrap();
        let w = cfg.hosts.iter().find(|h| h.name == "w").unwrap();
        let a = ssh_plan(w, "x").unwrap().args;
        assert!(a.contains(&"HostKeyAlias=pc".to_string()));
        assert!(a.contains(&"ProxyCommand=ssh -o BatchMode=yes -o ConnectTimeout=10 -i \"/k/id a\" -p 22 -W %h:%p me@10.0.0.5".to_string()));
        let bad = "[[host]]\nname='w'\nkind='ssh'\nhost='h'\nuser='u'\njump='w'\n";
        let mut c2: crate::config::Config = toml::from_str(bad).unwrap();
        assert!(crate::config::resolve_jumps_for_test(&mut c2).is_err());
    }

    #[test]
    fn remote_script_cds_first() {
        assert_eq!(remote_script(None, "ls"), "ls");
        let mut h = crate::config::default_config().hosts[0].clone();
        assert_eq!(h.with_startup("ls".into()), "ls");
        h.startup = Some("cd ~/site".into());
        assert_eq!(h.with_startup("ls".into()), "cd ~/site || exit 97\nls");
        h.startup = Some("cd ~/a\n\n# 주석\nsource x".into());
        let mut r = h.clone();
        r.kind = "ssh".into();
        r.host = Some("example.com".into());
        r.user = Some("u".into());
        r.run_as_root = Some(true);
        let sp = ssh_plan(&r, "id").unwrap();
        assert!(sp.args.last().unwrap().ends_with("| sudo -n -i bash -s"));
        r.run_as_root = None;
        assert!(ssh_plan(&r, "id").unwrap().args.last().unwrap().ends_with("| bash"));
        assert_eq!(h.with_startup("ls".into()), "cd ~/a &&\nsource x || exit 97\nls");
        assert_eq!(remote_script(Some("/srv/a b"), "ls"), "cd '/srv/a b' || exit 1\nls");
        assert_eq!(remote_script(Some("~/proj"), "ls"), "cd ~/'proj' || exit 1\nls");
    }

    #[test]
    fn powershell_is_encoded() {
        let h = HostCfg {
            name: "t".into(),
            kind: "local".into(),
            shell: Some("powershell".into()),
            claude: None,
            workdir: None,
            host: None,
            port: None,
            user: None,
            identity_file: None,
            accept_new_host_key: None,
            claude_permissions: None,
            startup: None,
            run_as_root: None,
            desktop: None,
            jump: None,
            host_key_alias: None,
            alt_host: None,
            jump_via: None,
        };
        let sp = local_plan(&h, Some("C:\\".into()), "Write-Output hi").unwrap();
        assert_eq!(sp.program, "powershell");
        assert_eq!(&sp.args[0..3], &["-NoProfile", "-OutputFormat", "Text"]);
        assert!(sp.args.iter().any(|s| s == "-EncodedCommand"));
    }
}

#[cfg(test)]
mod dump_plan {
    use super::*;
    /// CLAUDY_DUMP=경로 로 실행하면 Windows 원격 화면 세션 계획(명령·표준입력)을 파일로 쓴다(수동 시험용)
    #[test]
    fn dump() {
        let Ok(out) = std::env::var("CLAUDY_DUMP") else { return };
        let script = std::fs::read_to_string(format!("{out}.in")).unwrap();
        let mut h: HostCfg = toml::from_str("name='w'\nkind='ssh'\nhost='h'\nuser='u'\nshell='powershell'\ndesktop=true").unwrap();
        h.desktop = Some(std::env::var("CLAUDY_DESKTOP").is_ok());
        let sp = ssh_plan(&h, &script).unwrap();
        std::fs::write(format!("{out}.cmd"), sp.args.last().unwrap()).unwrap();
        std::fs::write(format!("{out}.stdin"), sp.stdin.unwrap()).unwrap();
    }
}
