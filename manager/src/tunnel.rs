//! 역방향 터널. 이 PC 의 SSH 서버(127.0.0.1:로컬 포트)를 경유 서버의 127.0.0.1:원격 포트에 열어 둔다(ssh -N -R).
//! 다른 PC 는 그 서버를 경유 호스트로 두고 127.0.0.1:원격 포트로 이 PC 에 접속한다.
//! 설정 앱이 떠 있는 동안 유지하고, 끊기면 15초 뒤 다시 잇는다.

use std::io::Read;
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::model::Host;

#[derive(Debug, Clone, PartialEq)]
pub struct Spec {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub identity: String,
    pub accept_new: bool,
    pub remote_port: u16,
    pub local_port: u16,
}

impl Spec {
    pub fn from_host(h: &Host, remote_port: u16, local_port: u16) -> Result<Spec, String> {
        if h.kind.trim() != "ssh" {
            return Err(format!("`{}` 는 ssh 호스트가 아닙니다", h.name));
        }
        if !h.jump.trim().is_empty() {
            return Err(format!("`{}` 는 다른 호스트를 경유합니다. 바로 접속하는 서버를 고르세요", h.name));
        }
        let ok = |s: &str| !s.is_empty() && !s.starts_with('-') && s.chars().all(|c| c.is_ascii_alphanumeric() || "._-@".contains(c));
        if !ok(h.host.trim()) || !ok(h.user.trim()) {
            return Err(format!("`{}` 의 주소나 사용자 이름이 올바르지 않습니다", h.name));
        }
        Ok(Spec {
            host: h.host.trim().to_string(),
            port: h.port.trim().parse().unwrap_or(22),
            user: h.user.trim().to_string(),
            identity: h.identity_file.trim().to_string(),
            accept_new: h.accept_new_host_key,
            remote_port,
            local_port,
        })
    }

    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = ["-N", "-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=30", "-o", "ServerAliveCountMax=3", "-o", "ExitOnForwardFailure=yes"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        if self.accept_new {
            a.push("-o".into());
            a.push("StrictHostKeyChecking=accept-new".into());
        }
        if !self.identity.is_empty() {
            a.push("-i".into());
            a.push(self.identity.clone());
        }
        a.push("-p".into());
        a.push(self.port.to_string());
        a.push("-R".into());
        a.push(format!("127.0.0.1:{}:127.0.0.1:{}", self.remote_port, self.local_port));
        a.push(format!("{}@{}", self.user, self.host));
        a
    }
}

/// ~/.claudy/tunnel.log 에 한 줄 남긴다(원격에서도 원인을 볼 수 있게). 1MB 를 넘으면 비운다
pub fn log(msg: &str) {
    let path = crate::paths::home_dir().join(".claudy").join("tunnel.log");
    if std::fs::metadata(&path).map(|m| m.len() > 1_000_000).unwrap_or(false) {
        let _ = std::fs::remove_file(&path);
    }
    let line = format!("{} {msg}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}

/// Windows: 설정 앱이 어떤 식으로 끝나든 ssh 가 함께 끝나도록 작업 개체(Job)에 묶는다
#[cfg(windows)]
fn job() -> Option<isize> {
    use std::sync::OnceLock;
    use windows_sys::Win32::System::JobObjects::{
        CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JobObjectExtendedLimitInformation, SetInformationJobObject,
    };
    static JOB: OnceLock<Option<isize>> = OnceLock::new();
    *JOB.get_or_init(|| unsafe {
        let h = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if h.is_null() {
            return None;
        }
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        SetInformationJobObject(
            h,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const core::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        Some(h as isize)
    })
}

#[cfg(windows)]
fn bind_to_job(child: &Child) {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;
    if let Some(j) = job() {
        unsafe {
            AssignProcessToJobObject(j as _, child.as_raw_handle() as _);
        }
    }
}

/// 이전 설정 앱이 강제로 끝나 남긴 같은 터널의 ssh 를 정리한다(포트를 쥐고 있으면 새 터널이 열리지 않는다)
fn kill_orphans(remote_port: u16, local_port: u16) {
    let pat = format!("-R 127.0.0.1:{remote_port}:127.0.0.1:{local_port}");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let me = std::process::id();
        let ps = format!(
            "Get-CimInstance Win32_Process -Filter \"Name='ssh.exe'\" | Where-Object {{ $_.CommandLine -like '*{pat}*' -and $_.ParentProcessId -ne {me} }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force; $_.ProcessId }}"
        );
        if let Ok(o) = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &ps])
            .creation_flags(0x0800_0000)
            .output()
        {
            let killed = String::from_utf8_lossy(&o.stdout).split_whitespace().collect::<Vec<_>>().join(",");
            if !killed.is_empty() {
                log(&format!("남아 있던 터널 ssh 정리: {killed}"));
            }
        }
    }
    #[cfg(not(windows))]
    {
        if let Ok(o) = Command::new("/usr/bin/pkill").args(["-f", &format!("ssh .*{pat}")]).output() {
            if o.status.success() {
                log("남아 있던 터널 ssh 정리");
            }
        }
    }
}

#[derive(Default)]
struct Inner {
    spec: Option<Spec>,
    generation: u64,
    child: Option<Child>,
    started: Option<Instant>,
    state: String,
    last_error: String,
}

#[derive(Clone, Default)]
pub struct Tunnel(Arc<Mutex<Inner>>);

const RETRY: Duration = Duration::from_secs(15);

fn ssh_program() -> String {
    #[cfg(windows)]
    {
        let sys = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
        let p = format!(r"{sys}\System32\OpenSSH\ssh.exe");
        if std::path::Path::new(&p).exists() {
            return p;
        }
    }
    "ssh".into()
}

impl Tunnel {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// 설정을 적용한다. 같으면 그대로 두고, 바뀌면 끊고 새로 연다. None 이면 끈다.
    pub fn apply(&self, spec: Option<Spec>) {
        let my_gen = {
            let mut g = self.lock();
            if g.spec == spec && (spec.is_none() || g.child.is_some() || !g.state.is_empty()) {
                return;
            }
            g.generation += 1;
            if let Some(mut c) = g.child.take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            g.spec = spec.clone();
            g.started = None;
            g.last_error.clear();
            g.state = if spec.is_some() { "연결 중".into() } else { String::new() };
            g.generation
        };
        match &spec {
            Some(s) => log(&format!("터널 시작: {}@{}:{} -R 127.0.0.1:{}:127.0.0.1:{}", s.user, s.host, s.port, s.remote_port, s.local_port)),
            None => log("터널 끔"),
        }
        if let Some(s) = spec {
            let t = self.clone();
            thread::spawn(move || t.run(my_gen, s));
        }
    }

    fn current(&self, my_gen: u64) -> bool {
        self.lock().generation == my_gen
    }

    fn run(&self, my_gen: u64, spec: Spec) {
        kill_orphans(spec.remote_port, spec.local_port);
        while self.current(my_gen) {
            let mut cmd = Command::new(ssh_program());
            cmd.args(spec.args()).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::piped());
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                cmd.creation_flags(0x0800_0000);
            }
            let prog = ssh_program();
            match cmd.spawn() {
                Ok(mut child) => {
                    #[cfg(windows)]
                    bind_to_job(&child);
                    log(&format!("ssh 실행({prog}) pid {}", child.id()));
                    let mut err = child.stderr.take();
                    {
                        let mut g = self.lock();
                        if g.generation != my_gen {
                            let _ = child.kill();
                            let _ = child.wait();
                            return;
                        }
                        g.child = Some(child);
                        g.started = Some(Instant::now());
                        g.state = "연결 중".into();
                    }
                    // 자식이 끝날 때까지 표준오류를 모은다(끝나면 파이프가 닫힌다)
                    let mut buf = String::new();
                    if let Some(e) = err.as_mut() {
                        let _ = e.read_to_string(&mut buf);
                    }
                    let mut g = self.lock();
                    if g.generation != my_gen {
                        return;
                    }
                    let code = g.child.take().and_then(|mut c| c.wait().ok()).and_then(|s| s.code());
                    let line = buf.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("").to_string();
                    g.last_error = if line.is_empty() { format!("끊김(코드 {})", code.unwrap_or(-1)) } else { line };
                    log(&format!("ssh 종료 코드 {:?}: {}", code, g.last_error));
                    g.started = None;
                    g.state = "다시 연결 대기".into();
                }
                Err(e) => {
                    let mut g = self.lock();
                    if g.generation != my_gen {
                        return;
                    }
                    g.last_error = format!("ssh 를 실행하지 못했습니다: {e}");
                    log(&g.last_error);
                    g.state = "다시 연결 대기".into();
                }
            }
            let until = Instant::now() + RETRY;
            while Instant::now() < until {
                if !self.current(my_gen) {
                    return;
                }
                thread::sleep(Duration::from_millis(300));
            }
        }
    }

    /// 화면에 보일 상태. 연결 뒤 5초가 지나도 살아 있으면 연결됨으로 본다
    pub fn status(&self) -> serde_json::Value {
        let g = self.lock();
        let state = match (&g.spec, g.started) {
            (None, _) => "꺼짐".to_string(),
            (Some(_), Some(t)) if t.elapsed() >= Duration::from_secs(5) => {
                let m = t.elapsed().as_secs() / 60;
                if m == 0 { "연결됨".into() } else { format!("연결됨 ({m}분째)") }
            }
            _ => g.state.clone(),
        };
        serde_json::json!({
            "state": state,
            "error": g.last_error,
            "target": g.spec.as_ref().map(|s| format!("{}@{}:{} → 127.0.0.1:{}", s.user, s.host, s.port, s.remote_port)),
        })
    }

    /// 앱을 끝낼 때 ssh 를 정리한다
    pub fn stop(&self) {
        self.apply(None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host() -> Host {
        Host { name: "gp".into(), kind: "ssh".into(), host: "example.com".into(), user: "tun".into(), identity_file: "/k/id".into(), ..Default::default() }
    }

    #[test]
    fn spec_args() {
        let s = Spec::from_host(&host(), 22022, 22).unwrap();
        let a = s.args();
        assert!(a.contains(&"127.0.0.1:22022:127.0.0.1:22".to_string()));
        assert_eq!(a.last().unwrap(), "tun@example.com");
        assert!(a.windows(2).any(|w| w[0] == "-i" && w[1] == "/k/id"));
        assert!(a.contains(&"ExitOnForwardFailure=yes".to_string()));
    }

    #[test]
    fn rejects_bad_hosts() {
        let mut h = host();
        h.kind = "local".into();
        assert!(Spec::from_host(&h, 1, 22).is_err());
        let mut h = host();
        h.jump = "x".into();
        assert!(Spec::from_host(&h, 1, 22).is_err());
        let mut h = host();
        h.host = "-oProxyCommand=x".into();
        assert!(Spec::from_host(&h, 1, 22).is_err());
    }

    #[test]
    fn off_status() {
        let t = Tunnel::default();
        assert_eq!(t.status()["state"], "꺼짐");
    }
}
