//! 「비밀번호로 키 등록」. 처음 접속하는 SSH 서버에 사용자가 비밀번호를 한 번 입력하면
//! Claudy 전용 키(~/.ssh/id_ed25519_claudy)를 만들어 서버의 ~/.ssh/authorized_keys 에 넣는다.
//! 비밀번호는 저장하지 않는다. ssh 에는 SSH_ASKPASS 로 이 실행 파일 자신을 넘겨 한 번만 전달한다.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::model::Host;
use crate::proc;

/// SSH_ASKPASS 로 불렸을 때 쓰는 환경 변수
pub const ENV_FLAG: &str = "CLAUDY_ASKPASS";
pub const ENV_SECRET: &str = "CLAUDY_ASKPASS_SECRET";

/// 이 실행 파일이 ssh 의 비밀번호 질의(askpass)로 불렸는지 보고, 그렇다면 비밀번호를 내보내고 true 를 돌려준다.
/// main 맨 앞에서 부른다.
pub fn serve_askpass() -> bool {
    if std::env::var_os(ENV_FLAG).is_none() {
        return false;
    }
    let prompt = std::env::args().nth(1).unwrap_or_default().to_lowercase();
    // 호스트 키 확인 질문(yes/no)에는 답하지 않는다. accept-new 로 처리하므로 오지 않아야 한다.
    if prompt.contains("yes/no") || prompt.contains("fingerprint") {
        println!("no");
        return true;
    }
    if let Ok(s) = std::env::var(ENV_SECRET) {
        println!("{s}");
    }
    true
}

pub fn key_path(home: &Path) -> PathBuf {
    home.join(".ssh").join("id_ed25519_claudy")
}

fn no_window(c: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    #[cfg(not(windows))]
    let _ = c;
}

/// 키가 없으면 만든다. 공개키 한 줄을 돌려준다.
pub fn ensure_key(home: &Path) -> Result<String, String> {
    let key = key_path(home);
    let pubp = key.with_extension("pub");
    if !key.exists() {
        std::fs::create_dir_all(key.parent().unwrap()).map_err(|e| format!(".ssh 폴더를 만들지 못했습니다: {e}"))?;
        let mut c = Command::new("ssh-keygen");
        c.args(["-q", "-t", "ed25519", "-N", "", "-C", "claudy"]).arg("-f").arg(&key).stdin(Stdio::null());
        no_window(&mut c);
        let o = c.output().map_err(|e| format!("ssh-keygen 을 실행하지 못했습니다: {e}"))?;
        if !o.status.success() {
            return Err(format!("키를 만들지 못했습니다: {}", proc::decode(&o.stderr).trim()));
        }
    }
    let text = std::fs::read_to_string(&pubp).map_err(|e| format!("공개키를 읽지 못했습니다: {e}"))?;
    let line = text.lines().next().unwrap_or("").trim().to_string();
    if !line.starts_with("ssh-") || line.contains('\'') {
        return Err("공개키 형식이 올바르지 않습니다".into());
    }
    Ok(line)
}

/// 서버에서 실행할 명령. 같은 키가 이미 있으면 다시 넣지 않는다.
pub fn remote_script(pubkey: &str) -> String {
    format!(
        "umask 077; mkdir -p ~/.ssh && touch ~/.ssh/authorized_keys && (grep -qxF '{k}' ~/.ssh/authorized_keys || echo '{k}' >> ~/.ssh/authorized_keys) && echo claudy-key-ok",
        k = pubkey
    )
}

/// Windows 서버에서 실행할 PowerShell. 관리자 계정이면 administrators_authorized_keys(권한도 맞춘다), 아니면 사용자 폴더에 넣는다
pub fn windows_script(pubkey: &str) -> String {
    format!(
        r#"$k='{k}'
$admin=([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator) -or ((net localgroup Administrators) -contains $env:USERNAME)
if ($admin) {{ $f=Join-Path $env:ProgramData 'ssh\administrators_authorized_keys' }} else {{ $d=Join-Path $env:USERPROFILE '.ssh'; New-Item -ItemType Directory -Force $d | Out-Null; $f=Join-Path $d 'authorized_keys' }}
if (-not (Select-String -Path $f -SimpleMatch $k -Quiet -ErrorAction SilentlyContinue)) {{ Add-Content -Path $f -Value $k -Encoding ascii }}
if ($admin) {{ icacls $f /inheritance:r /grant '*S-1-5-32-544:F' /grant '*S-1-5-18:F' | Out-Null }}
'claudy-key-ok'"#,
        k = pubkey
    )
}

pub fn ssh_args(h: &Host, pubkey: &str) -> Vec<String> {
    let mut a: Vec<String> = [
        "-o", "PubkeyAuthentication=no",
        "-o", "PreferredAuthentications=password,keyboard-interactive",
        "-o", "NumberOfPasswordPrompts=1",
        "-o", "StrictHostKeyChecking=accept-new",
        "-o", "ConnectTimeout=10",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    a.extend(proc::route_opts(h));
    let port = h.port.trim();
    if !port.is_empty() {
        a.push("-p".into());
        a.push(port.into());
    }
    a.push(format!("{}@{}", h.user.trim(), h.host.trim()));
    if h.remote_windows() {
        a.push(proc::ps_remote(&windows_script(pubkey)));
    } else {
        a.push(remote_script(pubkey));
    }
    a
}

/// 비밀번호로 접속해 키를 넣는다. 성공하면 개인키 경로를 돌려준다.
pub fn install(h: &Host, home: &Path, password: &str) -> Result<PathBuf, String> {
    if h.kind.trim() != "ssh" || h.host.trim().is_empty() || h.user.trim().is_empty() {
        return Err("SSH 호스트의 주소와 사용자를 먼저 적어 주세요.".into());
    }
    if password.is_empty() {
        return Err("비밀번호를 입력해 주세요.".into());
    }
    let pubkey = ensure_key(home)?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let mut c = Command::new("ssh");
    c.args(ssh_args(h, &pubkey))
        .env("SSH_ASKPASS", &exe)
        .env("SSH_ASKPASS_REQUIRE", "force")
        .env("DISPLAY", std::env::var("DISPLAY").unwrap_or_else(|_| ":0".into()))
        .env(ENV_FLAG, "1")
        .env(ENV_SECRET, password)
        .stdin(Stdio::null());
    no_window(&mut c);
    let o = c.output().map_err(|e| format!("ssh 를 실행하지 못했습니다: {e}"))?;
    let out = proc::decode(&o.stdout);
    if o.status.success() && out.contains("claudy-key-ok") {
        return Ok(key_path(home));
    }
    let err = proc::decode(&o.stderr);
    let last = err.lines().rev().find(|l| !l.trim().is_empty()).unwrap_or("").trim().to_string();
    if last.contains("Permission denied") {
        return Err("비밀번호가 맞지 않거나 서버가 비밀번호 접속을 막았습니다.".into());
    }
    Err(if last.is_empty() { "키를 등록하지 못했습니다.".into() } else { last })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn script_is_idempotent_and_quoted() {
        let s = remote_script("ssh-ed25519 AAAA claudy");
        assert!(s.contains("grep -qxF 'ssh-ed25519 AAAA claudy'"));
        assert!(s.ends_with("echo claudy-key-ok"));
    }

    #[test]
    fn windows_target_uses_powershell() {
        let h = Host { kind: "ssh".into(), host: "127.0.0.1".into(), user: "oh".into(), shell: "powershell".into(), port: "22022".into(), ..Default::default() };
        let a = ssh_args(&h, "ssh-ed25519 K c");
        assert!(a.last().unwrap().starts_with("powershell -NoProfile -NonInteractive -EncodedCommand "));
        assert!(windows_script("ssh-ed25519 K c").contains("administrators_authorized_keys"));
    }

    #[test]
    fn args_force_password_once() {
        let h = Host { kind: "ssh".into(), host: "10.0.0.1".into(), user: "u".into(), port: "2222".into(), ..Default::default() };
        let a = ssh_args(&h, "ssh-ed25519 K c");
        assert!(a.windows(2).any(|w| w == ["-o", "PubkeyAuthentication=no"]));
        assert!(a.windows(2).any(|w| w == ["-p", "2222"]));
        assert!(a.contains(&"u@10.0.0.1".to_string()));
    }
}
