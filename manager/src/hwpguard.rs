//! 한글(Hwp.exe) 「접근 허용」 보안 창 자동 승인 (Windows)
//!
//! 자동화 스크립트가 보안 승인 모듈(RegisterModule)을 빠뜨리거나 경로가 깨진 채로
//! 파일을 열면 한글이 「접근 허용」 창을 띄우고 작업이 멈춘다.
//! 설정 앱이 떠 있는 동안 0.5초마다 Hwp.exe 의 WPF 창을 살펴, 내용이 그 보안 창일 때만
//! 「모두 허용」(Alt+N)을 보낸다. 다른 프로그램의 창이나 다른 내용의 한글 창은 건드리지 않는다.

use std::collections::HashMap;
use std::io::Write;
use std::time::{Duration, Instant};

use uiautomation::UIAutomation;
use windows_sys::Win32::Foundation::{CloseHandle, HWND, LPARAM};
use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GetClassNameW, GetWindowThreadProcessId, IsWindow, IsWindowVisible, PostMessageW, WM_SYSKEYDOWN, WM_SYSKEYUP,
};

/// 보안 창 본문에 들어 있는 문구
const KEY_TEXT: &str = "접근하려는 시도";
/// 한글 보안 창의 WPF 클래스 이름
const BOX_CLASS: &str = "MessageBoxImpl";
const VK_N: usize = 0x4E;

static ENABLED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

/// 설정 앱의 「한글 접근 허용 창 자동 승인」 스위치
pub fn set_enabled(on: bool) {
    ENABLED.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn spawn() {
    std::thread::spawn(|| {
        let ua = match UIAutomation::new() {
            Ok(u) => u,
            Err(e) => {
                log(&format!("UI Automation 초기화 실패: {e}"));
                return;
            }
        };
        let mut sent: HashMap<isize, Instant> = HashMap::new();
        loop {
            if !ENABLED.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1000));
                continue;
            }
            sent.retain(|h, t| unsafe { IsWindow(*h as HWND) } != 0 && t.elapsed() < Duration::from_secs(30));
            for h in candidates() {
                let key = h as isize;
                if sent.get(&key).map(|t| t.elapsed() < Duration::from_secs(2)).unwrap_or(false) {
                    continue;
                }
                let Ok(el) = ua.element_from_handle(key.into()) else { continue };
                if el.get_classname().unwrap_or_default() != BOX_CLASS {
                    continue;
                }
                let name = el.get_name().unwrap_or_default();
                if !name.contains(KEY_TEXT) {
                    continue;
                }
                press_alt_n(h);
                let first = !sent.contains_key(&key);
                sent.insert(key, Instant::now());
                if first {
                    let file = name.lines().find(|l| l.contains(":\\") || l.starts_with("\\\\")).unwrap_or("").trim().to_string();
                    log(&format!("접근 허용 창 자동 승인(모두 허용) {file}"));
                }
            }
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

/// Hwp.exe 가 띄운 보이는 WPF 최상위 창
fn candidates() -> Vec<HWND> {
    let mut v: Vec<HWND> = Vec::new();
    unsafe {
        EnumWindows(Some(top_cb), &mut v as *mut _ as LPARAM);
    }
    v
}

unsafe extern "system" fn top_cb(h: HWND, lp: LPARAM) -> i32 {
    unsafe {
        if IsWindowVisible(h) != 0 && class_of(h).starts_with("HwndWrapper[hwp.exe") && is_hwp(h) {
            (*(lp as *mut Vec<HWND>)).push(h);
        }
    }
    1
}

/// 「모두 허용(N)」 단축키. 창이 앞에 없어도 WPF 가 접근 키로 처리한다
fn press_alt_n(h: HWND) {
    unsafe {
        PostMessageW(h, WM_SYSKEYDOWN, VK_N, 0x2031_0001);
        PostMessageW(h, WM_SYSKEYUP, VK_N, 0xE031_0001u32 as i32 as LPARAM);
    }
}

fn class_of(h: HWND) -> String {
    let mut buf = [0u16; 128];
    let n = unsafe { GetClassNameW(h, buf.as_mut_ptr(), buf.len() as i32) };
    String::from_utf16_lossy(&buf[..n.max(0) as usize])
}

fn is_hwp(h: HWND) -> bool {
    let mut pid: u32 = 0;
    unsafe {
        GetWindowThreadProcessId(h, &mut pid);
        let ph = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if ph.is_null() {
            return false;
        }
        let mut buf = [0u16; 520];
        let mut len = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(ph, 0, buf.as_mut_ptr(), &mut len) != 0;
        CloseHandle(ph);
        ok && String::from_utf16_lossy(&buf[..len as usize]).to_ascii_lowercase().ends_with("\\hwp.exe")
    }
}

fn log(msg: &str) {
    let path = crate::paths::home_dir().join(".claudy").join("hwpguard.log");
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "{} {}", chrono::Local::now().format("%Y-%m-%d %H:%M:%S"), msg);
    }
}
