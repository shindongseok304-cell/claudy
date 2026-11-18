//! Claudy 설정 프로그램의 데이터 처리와 명령 처리. 화면은 ui/index.html(WebView2)이 맡는다.

pub mod api;
#[cfg(windows)]
pub mod deskrun;
#[cfg(windows)]
pub mod hwpguard;
pub mod jobs;
pub mod keyinstall;
pub mod logs;
pub mod model;
pub mod paths;
pub mod proc;
pub mod settings;
pub mod sys;
pub mod tunnel;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
