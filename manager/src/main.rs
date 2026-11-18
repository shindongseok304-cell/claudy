#![cfg_attr(windows, windows_subsystem = "windows")]
//! Claudy 설정: Claudy(로컬 MCP 서버)의 hosts.toml 과 데이터 폴더를 보고 고치는 창 프로그램.
//! 화면은 ui/index.html 을 Windows 는 WebView2, macOS 는 WKWebView, Linux 는 WebKitGTK 로 띄우고, 데이터 처리는 lib(claudy_manager)가 맡는다.

#[cfg(windows)]
mod app;
#[cfg(target_os = "macos")]
mod app_mac;
#[cfg(target_os = "linux")]
mod app_linux;

#[cfg(windows)]
fn main() {
    if claudy_manager::keyinstall::serve_askpass() {
        return;
    }
    app::run();
}

#[cfg(target_os = "macos")]
fn main() {
    if claudy_manager::keyinstall::serve_askpass() {
        return;
    }
    app_mac::run();
}

#[cfg(target_os = "linux")]
fn main() {
    if claudy_manager::keyinstall::serve_askpass() {
        return;
    }
    app_linux::run();
}

#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
fn main() {
    eprintln!("ClaudyManager 는 Windows, macOS, Linux 전용이다.");
}
