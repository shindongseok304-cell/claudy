//! ssh 와 Claude Code 실행. 콘솔 창이 뜨지 않도록 Windows 에서는 CREATE_NO_WINDOW 를 쓴다.

use std::process::{Command, Stdio};

use crate::model::Host;

#[derive(Debug, Clone, PartialEq)]
pub struct Cmd {
    pub program: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RunResult {
    pub code: Option<i32>,
    pub out: String,
    pub err: String,
    pub spawn_error: Option<String>,
}

/// POSIX 셸용 작은따옴표 인용(원격 명령의 claude 경로 보호)
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn target(h: &Host) -> String {
    format!("{}@{}", h.user.trim(), h.host.trim())
}

/// 경유 호스트를 거쳐 가는 ProxyCommand (Claudy 서버 exec.rs 와 같은 모양)
pub fn proxy_command(v: &Host) -> String {
    let mut c = String::from("ssh -o BatchMode=yes -o ConnectTimeout=10");
    if v.accept_new_host_key {
        c.push_str(" -o StrictHostKeyChecking=accept-new");
    }
    let id = v.identity_file.trim();
    if !id.is_empty() {
        if id.contains(' ') { c.push_str(&format!(" -i \"{id}\"")) } else { c.push_str(&format!(" -i {id}")) }
    }
    let port = v.port.trim();
    c.push_str(&format!(" -p {} -W %h:%p {}", if port.is_empty() { "22" } else { port }, target(v)));
    c
}

/// 경유·호스트 키 이름 옵션(비밀번호 접속인 키 등록에서도 쓴다)
pub fn route_opts(h: &Host) -> Vec<String> {
    let mut a = Vec::new();
    if let Some(alias) = h.host_key_alias() {
        a.push("-o".into());
        a.push(format!("HostKeyAlias={alias}"));
    }
    if let Some(v) = h.jump_via.as_deref() {
        a.push("-o".into());
        a.push(format!("ProxyCommand={}", proxy_command(v)));
    }
    a
}

/// Windows 원격에서 PowerShell 명령을 실행하는 원격 명령줄. 원격 기본 셸이 cmd 든 PowerShell 이든 같게 동작한다
pub fn ps_remote(script: &str) -> String {
    use base64::Engine;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    format!("powershell -NoProfile -NonInteractive -EncodedCommand {}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// ssh 옵션과 목적지. 원격 명령은 호출하는 쪽이 뒤에 붙인다.
fn ssh_base(h: &Host, accept_new: bool) -> Vec<String> {
    let mut a: Vec<String> = vec!["-o".into(), "BatchMode=yes".into()];
    a.extend(route_opts(h));
    if accept_new {
        a.push("-o".into());
        a.push("StrictHostKeyChecking=accept-new".into());
    }
    a.push("-o".into());
    a.push("ConnectTimeout=10".into());
    let port = h.port.trim();
    if !port.is_empty() {
        a.push("-p".into());
        a.push(port.into());
    }
    let id = h.identity_file.trim();
    if !id.is_empty() {
        a.push("-i".into());
        a.push(id.into());
    }
    a.push(target(h));
    a
}

/// 「호스트 키 등록」: 처음 보는 서버의 키를 known_hosts 에 등록하고 exit 한다.
pub fn hostkey_cmd(h: &Host) -> Cmd {
    let mut args = ssh_base(h, true);
    args.push("exit".into());
    Cmd { program: "ssh".into(), args }
}

/// 「연결 시험」. 이 PC 는 claude --version, ssh 는 원격에서 echo 후 claude --version 을 실행한다.
pub fn test_cmd(h: &Host) -> Cmd {
    let claude = if h.claude.trim().is_empty() { "claude" } else { h.claude.trim() };
    if h.kind.trim() == "ssh" {
        let mut args = ssh_base(h, h.accept_new_host_key);
        if h.remote_windows() {
            args.push(ps_remote(&format!("'claudy-ok'; & '{}' --version", claude.replace('\'', "''"))));
        } else {
            args.push(format!("echo claudy-ok && {} --version", sh_quote(claude)));
        }
        Cmd { program: "ssh".into(), args }
    } else {
        Cmd { program: resolve_local(claude), args: vec!["--version".into()] }
    }
}

/// Finder 나 로그인 항목으로 띄운 앱은 PATH 가 짧다. macOS 에서는 흔한 설치 위치를 더 찾아본다.
pub fn resolve_local(bin: &str) -> String {
    if !cfg!(target_os = "macos") || bin.contains('/') {
        return bin.to_string();
    }
    let home = crate::paths::home_dir();
    let mut dirs: Vec<std::path::PathBuf> =
        std::env::var_os("PATH").map(|p| std::env::split_paths(&p).collect()).unwrap_or_default();
    dirs.push(home.join(".local").join("bin"));
    dirs.push(home.join(".claude").join("local"));
    dirs.push("/opt/homebrew/bin".into());
    dirs.push("/usr/local/bin".into());
    dirs.into_iter().map(|d| d.join(bin)).find(|p| p.is_file()).map(|p| p.display().to_string()).unwrap_or_else(|| bin.to_string())
}

/// 「접속 후 명령」만 지금 한 번 실행해 본다
pub fn startup_cmd(h: &Host) -> Cmd {
    let mut args = ssh_base(h, h.accept_new_host_key);
    let lines: Vec<&str> = h.startup.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    if h.remote_windows() {
        let body: Vec<String> = lines.iter().map(|l| format!("{l}\nif (-not $?) {{ exit 97 }}")).collect();
        args.push(ps_remote(&format!("{}\n\"[접속 후 명령 완료] $((Get-Location).Path)\"", body.join("\n"))));
    } else {
        args.push(format!("{} && echo \"[접속 후 명령 완료] $(pwd)\"", lines.join(" &&\n")));
    }
    Cmd { program: "ssh".into(), args }
}

/// 입력이 연결 시험을 돌릴 만한지 본다
pub fn can_connect(h: &Host) -> Result<(), String> {
    if h.kind.trim() == "ssh" && (h.host.trim().is_empty() || h.user.trim().is_empty()) {
        return Err("SSH 호스트 이름과 사용자를 먼저 적는다.".into());
    }
    Ok(())
}

/// UTF-8 이면 그대로, 아니면 Windows 에서는 CP949(EUC-KR 계열)로 읽는다.
pub fn decode(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) => decode_legacy(bytes),
    }
}

#[cfg(windows)]
fn decode_legacy(bytes: &[u8]) -> String {
    use windows_sys::Win32::Globalization::MultiByteToWideChar;
    const CP949: u32 = 949;
    let len = bytes.len() as i32;
    unsafe {
        let n = MultiByteToWideChar(CP949, 0, bytes.as_ptr(), len, std::ptr::null_mut(), 0);
        if n <= 0 {
            return String::from_utf8_lossy(bytes).into_owned();
        }
        let mut buf = vec![0u16; n as usize];
        MultiByteToWideChar(CP949, 0, bytes.as_ptr(), len, buf.as_mut_ptr(), n);
        String::from_utf16_lossy(&buf)
    }
}

#[cfg(not(windows))]
fn decode_legacy(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

pub fn run(cmd: &Cmd) -> RunResult {
    let mut c = Command::new(&cmd.program);
    c.args(&cmd.args).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        c.creation_flags(CREATE_NO_WINDOW);
    }
    match c.output() {
        Ok(o) => RunResult {
            code: o.status.code(),
            out: decode(&o.stdout),
            err: decode(&o.stderr),
            spawn_error: None,
        },
        Err(e) => RunResult {
            code: None,
            out: String::new(),
            err: String::new(),
            spawn_error: Some(e.to_string()),
        },
    }
}

/// 끝 n 줄
pub fn last_lines(text: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

/// 「호스트 키 등록」 결과 표시: 종료 코드와 표준 오류의 끝 3줄
pub fn hostkey_summary(r: &RunResult) -> String {
    if let Some(e) = &r.spawn_error {
        return format!("실행하지 못했다: {e}");
    }
    let code = r.code.map(|c| c.to_string()).unwrap_or_else(|| "없음".into());
    let mut s = format!("종료 코드: {code}\n");
    let tail = last_lines(&r.err, 3);
    if tail.is_empty() {
        s.push_str("(표준 오류 출력 없음)");
    } else {
        s.push_str(&tail.join("\n"));
    }
    s
}

/// 「연결 시험」 결과 표시
pub fn test_summary(r: &RunResult) -> String {
    if let Some(e) = &r.spawn_error {
        return format!("실행하지 못했다: {e}");
    }
    let code = r.code.map(|c| c.to_string()).unwrap_or_else(|| "없음".into());
    let mut s = format!("종료 코드: {code}\n결과: {}\n", if r.code == Some(0) { "성공" } else { "실패" });
    for l in last_lines(&r.out, 20) {
        s.push_str(l);
        s.push('\n');
    }
    for l in last_lines(&r.err, 10) {
        s.push_str("[오류] ");
        s.push_str(l);
        s.push('\n');
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh_host() -> Host {
        Host {
            name: "pi".into(),
            kind: "ssh".into(),
            host: "example.invalid".into(),
            user: "tester".into(),
            port: "2222".into(),
            identity_file: "C:\\k\\id".into(),
            ..Default::default()
        }
    }

    #[test]
    fn hostkey_args_follow_spec() {
        let c = hostkey_cmd(&ssh_host());
        assert_eq!(c.program, "ssh");
        assert_eq!(
            c.args,
            vec![
                "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=accept-new", "-o", "ConnectTimeout=10",
                "-p", "2222", "-i", "C:\\k\\id", "tester@example.invalid", "exit",
            ]
        );
    }

    #[test]
    fn test_args_for_ssh_and_local() {
        let mut h = ssh_host();
        let c = test_cmd(&h);
        assert_eq!(c.args.last().unwrap(), "echo claudy-ok && 'claude' --version");
        assert!(!c.args.contains(&"StrictHostKeyChecking=accept-new".to_string()));
        h.accept_new_host_key = true;
        assert!(test_cmd(&h).args.contains(&"StrictHostKeyChecking=accept-new".to_string()));
        let local = Host { name: "this-pc".into(), kind: "local".into(), claude: String::new(), ..Default::default() };
        let lc = test_cmd(&local);
        assert!(lc.program == "claude" || lc.program.ends_with("/claude"), "{}", lc.program);
        assert_eq!(lc.args, vec!["--version".to_string()]);
    }

    #[test]
    fn optional_ssh_fields_are_omitted() {
        let h = Host { kind: "ssh".into(), host: "h".into(), user: "u".into(), ..Default::default() };
        let c = hostkey_cmd(&h);
        assert!(!c.args.contains(&"-p".to_string()));
        assert!(!c.args.contains(&"-i".to_string()));
    }

    #[test]
    fn can_connect_checks_ssh_fields() {
        let mut h = ssh_host();
        assert!(can_connect(&h).is_ok());
        h.user.clear();
        assert!(can_connect(&h).is_err());
    }

    #[test]
    fn summaries() {
        let r = RunResult { code: Some(255), out: String::new(), err: "a\nb\nc\nd\n".into(), spawn_error: None };
        let s = hostkey_summary(&r);
        assert!(s.starts_with("종료 코드: 255"));
        assert!(s.ends_with("b\nc\nd"));
        let ok = RunResult { code: Some(0), out: "claudy-ok\n1.0.0\n".into(), err: String::new(), spawn_error: None };
        assert!(test_summary(&ok).contains("결과: 성공"));
        let missing = RunResult { code: None, out: String::new(), err: String::new(), spawn_error: Some("no such file".into()) };
        assert!(test_summary(&missing).contains("실행하지 못했다"));
    }

    #[test]
    fn decode_utf8_and_fallback() {
        assert_eq!(decode("한글".as_bytes()), "한글");
    }
}
