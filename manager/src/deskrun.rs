//! 화면 세션 실행 대행 (Windows)
//!
//! 원격(ssh) 명령은 화면이 없는 세션 0 에서 돈다. 예전에는 Claudy 서버가 매번 예약 작업을 만들어
//! 사용자 화면 세션에서 run.ps1 을 실행했다(호출마다 약 4~5초).
//! 설정 앱은 이미 사용자 화면 세션에 떠 있으므로, 명명된 파이프로 「이 폴더의 run.ps1 을 실행하라」는
//! 요청을 받아 바로 실행한다. 파이프는 같은 사용자 계정만 열 수 있고, 실행 대상은
//! %TEMP%\claudy-<16진수 10자>\run.ps1 로 한정한다. 결과 파일은 예전과 같이 그 폴더에 쓰인다.

use std::io::Write;
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use windows_sys::Win32::Foundation::{CloseHandle, ERROR_PIPE_CONNECTED, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree};
use windows_sys::Win32::Security::Authorization::{ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW};
use windows_sys::Win32::Security::{GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_QUERY, TOKEN_USER, TokenUser};
use windows_sys::Win32::Storage::FileSystem::{FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE,
    PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

static READY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static RUNS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// (대기 중인지, 이번 실행 뒤 처리한 요청 수)
pub fn status() -> (bool, u64) {
    (READY.load(std::sync::atomic::Ordering::Relaxed), RUNS.load(std::sync::atomic::Ordering::Relaxed))
}

/// 파이프 이름. Claudy 서버의 실행기와 같은 규칙을 쓴다
pub fn pipe_name() -> String {
    format!(r"\\.\pipe\claudy-desk-{}", std::env::var("USERNAME").unwrap_or_default())
}

pub fn spawn() {
    std::thread::spawn(|| {
        let sddl = match user_sid() {
            Some(sid) => format!("D:P(A;;GA;;;{sid})(A;;GA;;;SY)"),
            None => {
                log("사용자 SID 를 읽지 못해 화면 세션 실행 대행을 켜지 않았다");
                return;
            }
        };
        loop {
            if let Err(e) = serve_once(&sddl) {
                log(&format!("파이프 오류: {e}"));
                std::thread::sleep(Duration::from_secs(2));
            }
        }
    });
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn serve_once(sddl: &str) -> Result<(), String> {
    unsafe {
        let mut sd: PSECURITY_DESCRIPTOR = std::ptr::null_mut();
        let w = wide(sddl);
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), 1, &mut sd, std::ptr::null_mut()) == 0 {
            return Err(format!("보안 설명자 생성 실패 {}", GetLastError()));
        }
        let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: sd, bInheritHandle: 0 };
        let name = wide(&pipe_name());
        let h: HANDLE = CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            PIPE_UNLIMITED_INSTANCES,
            4096,
            4096,
            0,
            &sa,
        );
        LocalFree(sd as _);
        if h == INVALID_HANDLE_VALUE {
            READY.store(false, std::sync::atomic::Ordering::Relaxed);
            return Err(format!("파이프 생성 실패 {}", GetLastError()));
        }
        READY.store(true, std::sync::atomic::Ordering::Relaxed);
        if ConnectNamedPipe(h, std::ptr::null_mut()) == 0 && GetLastError() != ERROR_PIPE_CONNECTED {
            CloseHandle(h);
            return Err(format!("연결 대기 실패 {}", GetLastError()));
        }
        let line = read_line(h);
        let answer = handle(&line);
        let out = format!("{answer}\n");
        let mut n = 0u32;
        WriteFile(h, out.as_ptr(), out.len() as u32, &mut n, std::ptr::null_mut());
        FlushFileBuffers(h);
        DisconnectNamedPipe(h);
        CloseHandle(h);
    }
    Ok(())
}

unsafe fn read_line(h: HANDLE) -> String {
    let mut buf: Vec<u8> = Vec::new();
    let mut chunk = [0u8; 1024];
    while buf.len() < 8192 && !buf.contains(&b'\n') {
        let mut n = 0u32;
        if unsafe { ReadFile(h, chunk.as_mut_ptr(), chunk.len() as u32, &mut n, std::ptr::null_mut()) } == 0 || n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n as usize]);
    }
    String::from_utf8_lossy(&buf).lines().next().unwrap_or("").trim().to_string()
}

/// 요청 한 줄을 처리한다. 「PING」 은 살아 있는지 확인, 「RUN <폴더>」 는 그 폴더의 run.ps1 실행
fn handle(line: &str) -> String {
    if line == "PING" {
        return format!("PONG {}", env!("CARGO_PKG_VERSION"));
    }
    let Some(dir) = line.strip_prefix("RUN ") else {
        return "ERR 알 수 없는 요청".into();
    };
    let dir = PathBuf::from(dir.trim());
    if let Err(e) = check_dir(&dir) {
        log(&format!("거부: {} ({e})", dir.display()));
        return format!("ERR {e}");
    }
    let run = dir.join("run.ps1");
    match Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-WindowStyle", "Hidden", "-File"])
        .arg(&run)
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
    {
        Ok(_) => {
            RUNS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            "OK".into()
        }
        Err(e) => format!("ERR 실행 실패: {e}"),
    }
}

/// 실행 대상 폴더 검사. %TEMP% 바로 아래 claudy-<16진수 10자> 폴더이고 run.ps1 이 있어야 한다
fn check_dir(dir: &Path) -> Result<(), String> {
    let temp = std::env::temp_dir();
    let parent = dir.parent().ok_or("폴더 경로가 아니다")?;
    let same_parent = parent.to_string_lossy().trim_end_matches('\\').eq_ignore_ascii_case(temp.to_string_lossy().trim_end_matches('\\'));
    if !same_parent {
        return Err("임시 폴더 밖이다".into());
    }
    let name = dir.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
    let ok = name.strip_prefix("claudy-").map(|id| id.len() == 10 && id.chars().all(|c| c.is_ascii_hexdigit())).unwrap_or(false);
    if !ok {
        return Err("폴더 이름이 규칙과 다르다".into());
    }
    if !dir.join("run.ps1").is_file() {
        return Err("run.ps1 이 없다".into());
    }
    Ok(())
}

fn user_sid() -> Option<String> {
    unsafe {
        let mut tok: HANDLE = std::ptr::null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return None;
        }
        let mut buf = vec![0u8; 256];
        let mut len = 0u32;
        let ok = GetTokenInformation(tok, TokenUser, buf.as_mut_ptr() as _, buf.len() as u32, &mut len) != 0;
        CloseHandle(tok);
        if !ok {
            return None;
        }
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s: windows_sys::core::PWSTR = std::ptr::null_mut();
        if ConvertSidToStringSidW(tu.User.Sid, &mut s) == 0 {
            return None;
        }
        let mut n = 0;
        while *s.add(n) != 0 {
            n += 1;
        }
        let out = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
        LocalFree(s as _);
        Some(out)
    }
}

fn log(msg: &str) {
    let path = crate::paths::home_dir().join(".claudy").join("deskrun.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_rules() {
        let t = std::env::temp_dir();
        assert!(check_dir(&t.join("claudy-zz")).is_err());
        assert!(check_dir(Path::new("C:\\Windows\\claudy-0123456789")).is_err());
        let d = t.join("claudy-0123456789");
        assert!(check_dir(&d).is_err()); // run.ps1 없음
    }
}
