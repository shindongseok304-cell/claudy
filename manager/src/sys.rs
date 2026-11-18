//! 운영체제 조회. Claudy 서버(claudy.exe 또는 claudly.exe)가 떠 있는지 본다.

const NAMES: [&str; 2] = ["claudy.exe", "claudly.exe"];

/// 프로세스 이름 목록에서 서버 이름이 있는지 본다(대소문자 무시, .exe 유무 무시).
pub fn is_server_name(name: &str) -> bool {
    let n = name.trim().to_lowercase();
    let n = n.strip_suffix(".exe").unwrap_or(&n);
    NAMES.iter().any(|s| s.strip_suffix(".exe") == Some(n))
}

#[cfg(windows)]
pub fn server_running() -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
    };
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut found = false;
        let mut ok = Process32FirstW(snap, &mut e);
        while ok != 0 {
            let len = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
            if is_server_name(&String::from_utf16_lossy(&e.szExeFile[..len])) {
                found = true;
                break;
            }
            ok = Process32NextW(snap, &mut e);
        }
        CloseHandle(snap);
        found
    }
}

#[cfg(target_os = "macos")]
pub fn server_running() -> bool {
    std::process::Command::new("/usr/bin/pgrep")
        .args(["-x", "claudy"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(any(windows, target_os = "macos")))]
pub fn server_running() -> bool {
    let Ok(rd) = std::fs::read_dir("/proc") else {
        return false;
    };
    rd.flatten().any(|e| {
        std::fs::read_to_string(e.path().join("comm")).map(|c| is_server_name(&c)).unwrap_or(false)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_matching() {
        assert!(is_server_name("claudy.exe"));
        assert!(is_server_name("Claudly.EXE"));
        assert!(is_server_name("claudy"));
        assert!(!is_server_name("ClaudyManager.exe"));
        assert!(!is_server_name("claude.exe"));
    }
}
