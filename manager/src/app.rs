//! Windows 창과 WebView2. JS 의 window.ipc.postMessage 를 받아 api::dispatch 로 처리하고
//! 결과를 window.__claudy_reply(id, result) 로 돌려준다.
//! 알림 영역 아이콘: 누르면 창을 열고, 오른쪽 단추로 메뉴(설정 열기, 잠수함 모드, 종료)를 띄운다.
//! 잠수함 모드: 작업 표시줄 단추를 숨기고 알림 영역 아이콘만 남긴다.
//! 창의 닫기 단추는 창만 숨긴다. 종료는 알림 영역 메뉴의 「Claudy 종료」로 한다.
//! 시작 시 실행: HKCU\\...\\Run 에 「Claudy」 값을 쓰고 --background 로 창 없이 시작한다.

use std::borrow::Cow;
use std::env;
use std::num::NonZeroIsize;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use raw_window_handle::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawWindowHandle, Win32WindowHandle, WindowHandle,
};
use serde_json::{Value, json};
use tao::dpi::{LogicalSize, PhysicalPosition};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::windows::{IconExtWindows, WindowBuilderExtWindows, WindowExtWindows};
use tao::window::{Icon, Theme, Window, WindowBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Foundation::{ERROR_ALREADY_EXISTS, GetLastError};
use windows_sys::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_SZ, RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW,
};
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowW, MB_ICONERROR, MB_OK, MessageBoxW, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
};
use wry::http::{Request, Response};
use wry::{WebContext, WebViewBuilder, WebViewBuilderExtWindows};

use claudy_manager::api::{self, AppState, Shared};
use claudy_manager::paths;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const FONT_REGULAR: &[u8] = include_bytes!("../ui/fonts/Pretendard-Regular.woff2");
const FONT_SEMIBOLD: &[u8] = include_bytes!("../ui/fonts/Pretendard-SemiBold.woff2");

/// Microsoft WebView2 SDK 의 WebView2Loader.dll(x64). 실행할 때 %LOCALAPPDATA%\\Claudy 에 풀어 읽는다.
const LOADER_DLL: &[u8] = include_bytes!("../res/WebView2Loader.dll");

const CLIENT_W: f64 = 720.0;
const CLIENT_H: f64 = 520.0;
const HOME_URL: &str = "http://claudy.localhost/index.html";

const WINDOW_TITLE: &str = "Claudy 설정";
const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const RUN_VALUE: &str = "Claudy";

enum UserEvent {
    Reply { id: u64, text: String },
    BehaviorChanged,
    Menu(MenuId),
    TrayOpen,
}

/// 알림 영역 메뉴의 항목들. 선택 이벤트는 id 로 구분한다.
struct TrayMenu {
    open: MenuItem,
    stealth: CheckMenuItem,
    quit: MenuItem,
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

fn message_box(text: &str) {
    let (t, c) = (wide(text), wide("Claudy 설정"));
    unsafe {
        MessageBoxW(std::ptr::null_mut(), t.as_ptr(), c.as_ptr(), MB_OK | MB_ICONERROR);
    }
}

/// 파일 대화상자의 부모 창. 스레드로 넘기려고 HWND 숫자만 들고 있다.
struct Owner(isize);

impl HasWindowHandle for Owner {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        let h = NonZeroIsize::new(self.0).ok_or(HandleError::Unavailable)?;
        let raw = RawWindowHandle::Win32(Win32WindowHandle::new(h));
        Ok(unsafe { WindowHandle::borrow_raw(raw) })
    }
}

impl HasDisplayHandle for Owner {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(DisplayHandle::windows())
    }
}

fn pick(owner: isize, cmd: &str, args: &Value) -> Result<Value, String> {
    let title = args.get("title").and_then(Value::as_str).unwrap_or("");
    let filter = args.get("filter").and_then(Value::as_str).unwrap_or("");
    let parent = Owner(owner);
    let mut d = rfd::FileDialog::new().set_parent(&parent);
    if !title.is_empty() {
        d = d.set_title(title);
    }
    let picked: Option<PathBuf> = if cmd == "pick.folder" {
        d.pick_folder()
    } else {
        if filter == "exe" {
            d = d.add_filter("실행 파일", &["exe", "cmd", "bat"]).add_filter("모든 파일", &["*"]);
        }
        d.pick_file()
    };
    Ok(picked.map(|p| Value::String(p.display().to_string())).unwrap_or(Value::Null))
}

/// 시간이 걸릴 수 있는 명령은 스레드에서 처리한다.
fn is_slow(cmd: &str) -> bool {
    matches!(cmd, "host.test" | "host.install_key" | "host.run_startup" | "host.trust_key" | "pick.file" | "pick.folder" | "logs.day" | "jobs.list" | "jobs.log")
}

fn run_command(st: &Shared, owner: isize, cmd: &str, args: &Value) -> String {
    let r = if cmd.starts_with("pick.") { pick(owner, cmd, args) } else { api::dispatch(st, cmd, args) };
    match r {
        Ok(data) => json!({"ok": true, "data": data}).to_string(),
        Err(e) => json!({"ok": false, "error": e}).to_string(),
    }
}

fn handle_ipc(st: &Shared, proxy: &EventLoopProxy<UserEvent>, owner: isize, body: &str) {
    let Ok(v) = serde_json::from_str::<Value>(body) else { return };
    let Some(id) = v.get("id").and_then(Value::as_u64) else { return };
    let cmd = v.get("cmd").and_then(Value::as_str).unwrap_or("").to_string();
    let args = v.get("args").cloned().unwrap_or(Value::Null);
    if is_slow(&cmd) {
        let (st, proxy) = (st.clone(), proxy.clone());
        thread::spawn(move || {
            let text = run_command(&st, owner, &cmd, &args);
            let _ = proxy.send_event(UserEvent::Reply { id, text });
        });
    } else {
        let text = run_command(st, owner, &cmd, &args);
        let _ = proxy.send_event(UserEvent::Reply { id, text });
        if cmd == "behavior.set" {
            let _ = proxy.send_event(UserEvent::BehaviorChanged);
        }
    }
}

fn serve(req: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let (mime, body): (&str, Cow<'static, [u8]>) = match req.uri().path() {
        "/" | "/index.html" => ("text/html; charset=utf-8", Cow::Borrowed(INDEX_HTML.as_bytes())),
        "/fonts/Pretendard-Regular.woff2" => ("font/woff2", Cow::Borrowed(FONT_REGULAR)),
        "/fonts/Pretendard-SemiBold.woff2" => ("font/woff2", Cow::Borrowed(FONT_SEMIBOLD)),
        _ => {
            return Response::builder().status(404).body(Cow::Borrowed(&b""[..])).unwrap();
        }
    };
    Response::builder().header("Content-Type", mime).header("Cache-Control", "no-store").body(body).unwrap()
}

/// 제목줄을 시스템 테마에 맞춘다. DWM 속성 중 지원하지 않는 것(Windows 10 의 캡션 색 등)은 무시된다.
fn apply_titlebar(hwnd: isize, dark: bool) {
    let hwnd = hwnd as *mut core::ffi::c_void;
    let on: i32 = dark as i32;
    // 20: DWMWA_USE_IMMERSIVE_DARK_MODE (19 는 Windows 10 1809~1903)
    // 35: DWMWA_CAPTION_COLOR, 36: DWMWA_TEXT_COLOR (Windows 11). 사이드바 색과 글자 색에 맞춘다.
    let caption: u32 = if dark { 0x0025_2523 } else { 0x00F1_ECEC };
    let text: u32 = if dark { 0x00F7_F2F2 } else { 0x001E_1C1C };
    unsafe {
        for attr in [20u32, 19] {
            DwmSetWindowAttribute(hwnd, attr, &on as *const i32 as *const _, 4);
        }
        DwmSetWindowAttribute(hwnd, 35, &caption as *const u32 as *const _, 4);
        DwmSetWindowAttribute(hwnd, 36, &text as *const u32 as *const _, 4);
    }
}

fn background(dark: bool) -> (u8, u8, u8, u8) {
    if dark { (0x1C, 0x1C, 0x1E, 255) } else { (0xF2, 0xF2, 0xF7, 255) }
}

fn webview_dir() -> PathBuf {
    local_dir().join("webview")
}

fn local_dir() -> PathBuf {
    let base = env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| paths::home_dir().join("AppData").join("Local"));
    base.join("Claudy")
}

/// 내장한 WebView2Loader.dll 을 풀어 두고 그 경로를 로더에 알린다.
fn prepare_loader() -> Result<(), String> {
    let dir = local_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join("WebView2Loader.dll");
    let same = std::fs::metadata(&path).map(|m| m.len() == LOADER_DLL.len() as u64).unwrap_or(false);
    if !same {
        let tmp = dir.join("WebView2Loader.dll.tmp");
        std::fs::write(&tmp, LOADER_DLL).map_err(|e| format!("{}: {e}", tmp.display()))?;
        std::fs::rename(&tmp, &path).map_err(|e| format!("{}: {e}", path.display()))?;
    }
    webview2_com_sys::loader::set_path(&path);
    Ok(())
}

/// 이미 떠 있는 설정 프로그램이 있으면 그 창을 앞으로 올리고 true 를 돌려준다.
/// 뮤텍스는 프로세스가 끝날 때까지 쥐고 있는다.
fn already_running() -> bool {
    let name = wide("Local\\ClaudyManager.single");
    unsafe {
        let h = CreateMutexW(std::ptr::null(), 0, name.as_ptr());
        if h.is_null() || GetLastError() != ERROR_ALREADY_EXISTS {
            return false;
        }
        let title = wide(WINDOW_TITLE);
        let w = FindWindowW(std::ptr::null(), title.as_ptr());
        if !w.is_null() {
            ShowWindow(w, SW_SHOW);
            ShowWindow(w, SW_RESTORE);
            SetForegroundWindow(w);
        }
        true
    }
}

/// 「시작 시 실행」: HKCU Run 키에 실행 항목을 쓰거나 지운다. 다음 로그인부터 적용된다.
fn apply_autostart(on: bool) -> Result<(), String> {
    let key = wide(RUN_KEY);
    let value = wide(RUN_VALUE);
    unsafe {
        let mut h: HKEY = std::ptr::null_mut();
        if RegOpenKeyExW(HKEY_CURRENT_USER, key.as_ptr(), 0, KEY_SET_VALUE, &mut h) != 0 {
            return Err("시작 프로그램 레지스트리 키를 열지 못했다".into());
        }
        let r = if on {
            let exe = env::current_exe().map_err(|e| e.to_string())?;
            let data = wide(&format!("\"{}\" --background", exe.display()));
            RegSetValueExW(h, value.as_ptr(), 0, REG_SZ, data.as_ptr() as *const u8, (data.len() * 2) as u32)
        } else {
            let r = RegDeleteValueW(h, value.as_ptr());
            if r == 2 { 0 } else { r } // 2: ERROR_FILE_NOT_FOUND, 원래 없던 값
        };
        RegCloseKey(h);
        if r != 0 { Err(format!("시작 프로그램 등록 실패(코드 {r})")) } else { Ok(()) }
    }
}

fn behavior(st: &Shared) -> (bool, bool) {
    let v = api::dispatch(st, "behavior.get", &Value::Null).unwrap_or(Value::Null);
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    (b("stealth"), b("autostart"))
}

fn tray(stealth: bool) -> Result<(TrayIcon, TrayMenu), String> {
    let items = TrayMenu {
        open: MenuItem::new("Claudy 설정 열기", true, None),
        stealth: CheckMenuItem::new("잠수함 모드", true, stealth, None),
        quit: MenuItem::new("Claudy 종료", true, None),
    };
    let ver = MenuItem::new(format!("Claudy 설정 {}", claudy_manager::VERSION), false, None);
    let menu = Menu::new();
    let _ = menu.append_items(&[
        &ver,
        &PredefinedMenuItem::separator(),
        &items.open,
        &items.stealth,
        &PredefinedMenuItem::separator(),
        &items.quit,
    ]);
    let icon = tray_icon::Icon::from_resource(1, Some((32, 32))).map_err(|e| e.to_string())?;
    let t = TrayIconBuilder::new()
        .with_icon(icon)
        .with_tooltip("Claudy")
        .with_menu(Box::new(menu))
        .with_menu_on_left_click(false)
        .build()
        .map_err(|e| e.to_string())?;
    Ok((t, items))
}

fn show(window: &Window) {
    window.set_visible(true);
    window.set_minimized(false);
    window.set_focus();
}

/// 동작 설정을 실제로 적용한다(작업 표시줄 단추, 알림 영역 체크, 시작 프로그램)
fn apply_behavior(st: &Shared, items: Option<&TrayMenu>, window: &Window) {
    let (stealth, autostart) = behavior(st);
    let _ = window.set_skip_taskbar(stealth);
    if let Some(m) = items {
        m.stealth.set_checked(stealth);
    }
    if let Err(e) = apply_autostart(autostart) {
        message_box(&format!("시작 시 실행 설정을 적용하지 못했습니다.\n{e}"));
    }
}

pub fn run() {
    if already_running() {
        return;
    }
    let start_hidden = env::args().any(|a| a == "--background");
    // 한글 「접근 허용」 창 자동 승인
    claudy_manager::hwpguard::spawn();
    // 원격 명령을 사용자 화면 세션에서 실행하는 대행(예약 작업 대신)
    claudy_manager::deskrun::spawn();
    if let Err(e) = prepare_loader() {
        message_box(&format!("WebView2 로더를 준비하지 못했습니다.\n{e}"));
        return;
    }
    if wry::webview_version().is_err() {
        message_box(
            "WebView2 런타임을 찾지 못했습니다.\n\nMicrosoft Edge WebView2 Runtime 을 설치한 뒤 다시 실행해 주세요.\nhttps://developer.microsoft.com/microsoft-edge/webview2/",
        );
        return;
    }

    let st: Shared = Arc::new(Mutex::new(AppState::open(&paths::home_dir())));

    let (stealth, autostart) = behavior(&st);
    claudy_manager::hwpguard::set_enabled(st.lock().map(|g| g.behavior.hwp_guard).unwrap_or(true));
    // 실행 파일 위치가 바뀌었을 수 있어 시작할 때 다시 쓴다
    if autostart {
        let _ = apply_autostart(true);
    }

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    {
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = p.send_event(UserEvent::Menu(e.id().clone()));
        }));
        let p = proxy.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = e {
                let _ = p.send_event(UserEvent::TrayOpen);
            }
        }));
    }

    let icon = Icon::from_resource(1, None).ok();
    let window = match WindowBuilder::new()
        .with_title(WINDOW_TITLE)
        .with_inner_size(LogicalSize::new(CLIENT_W, CLIENT_H))
        .with_resizable(false)
        .with_maximizable(false)
        .with_visible(false)
        .with_skip_taskbar(stealth)
        .with_window_icon(icon.clone())
        .with_taskbar_icon(icon)
        .build(&event_loop)
    {
        Ok(w) => w,
        Err(e) => {
            message_box(&format!("창을 만들지 못했습니다.\n{e}"));
            return;
        }
    };
    if let Some(m) = window.primary_monitor() {
        let (ms, ws) = (m.size(), window.outer_size());
        let mp = m.position();
        let x = mp.x + (ms.width as i32 - ws.width as i32) / 2;
        let y = mp.y + (ms.height as i32 - ws.height as i32) / 2;
        window.set_outer_position(PhysicalPosition::new(x.max(mp.x), y.max(mp.y)));
    }
    let hwnd = window.hwnd();
    let dark = window.theme() == Theme::Dark;
    apply_titlebar(hwnd, dark);

    let dir = webview_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut ctx = WebContext::new(Some(dir));
    let ipc_state = st.clone();
    let ipc_proxy = proxy.clone();
    let built = WebViewBuilder::new_with_web_context(&mut ctx)
        .with_background_color(background(dark))
        .with_browser_accelerator_keys(false)
        .with_devtools(false)
        .with_custom_protocol("claudy".into(), |_id, req| serve(req))
        .with_navigation_handler(|u| u.starts_with("http://claudy.localhost/"))
        .with_ipc_handler(move |req| handle_ipc(&ipc_state, &ipc_proxy, hwnd, req.body()))
        .with_url(HOME_URL)
        .build(&window);
    let webview = match built {
        Ok(w) => w,
        Err(e) => {
            message_box(&format!("WebView2 를 시작하지 못했습니다.\n{e}"));
            return;
        }
    };

    let mut tray_icon: Option<TrayIcon> = None;
    let mut items: Option<TrayMenu> = None;

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // 알림 영역 아이콘은 이벤트 루프가 시작된 뒤에 만든다
            Event::NewEvents(StartCause::Init) => {
                match tray(stealth) {
                    Ok((icon, it)) => {
                        tray_icon = Some(icon);
                        items = Some(it);
                    }
                    Err(e) => message_box(&format!("알림 영역 아이콘을 만들지 못했습니다.\n{e}")),
                }
                // 아이콘이 없으면 숨긴 창을 다시 열 방법이 없으므로 창을 보인다
                if !start_hidden || tray_icon.is_none() {
                    show(&window);
                }
            }
            Event::UserEvent(UserEvent::BehaviorChanged) => apply_behavior(&st, items.as_ref(), &window),
            Event::UserEvent(UserEvent::TrayOpen) => show(&window),
            Event::UserEvent(UserEvent::Menu(id)) => {
                let Some(m) = items.as_ref() else { return };
                if id == *m.open.id() {
                    show(&window);
                } else if id == *m.stealth.id() {
                    let on = m.stealth.is_checked();
                    let _ = api::dispatch(&st, "behavior.set", &json!({"stealth": on}));
                    apply_behavior(&st, items.as_ref(), &window);
                    let _ = webview.evaluate_script(
                        "(async()=>{try{S.beh=await call('behavior.get');render(true)}catch(e){}})()",
                    );
                } else if id == *m.quit.id() {
                    st.lock().unwrap_or_else(|p| p.into_inner()).tunnel.stop();
                    tray_icon.take();
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::UserEvent(UserEvent::Reply { id, text }) => {
                let _ = webview.evaluate_script(&format!("window.__claudy_reply({id},{text})"));
            }
            Event::WindowEvent { event: WindowEvent::ThemeChanged(t), .. } => {
                let dark = t == Theme::Dark;
                apply_titlebar(hwnd, dark);
                let _ = webview.set_background_color(background(dark));
            }
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                if tray_icon.is_some() {
                    window.set_visible(false);
                } else {
                    *control_flow = ControlFlow::Exit;
                }
            }
            _ => {}
        }
    });
}
