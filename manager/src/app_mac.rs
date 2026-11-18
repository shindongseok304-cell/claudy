//! macOS 창(WKWebView)과 메뉴 막대 아이콘.
//! JS 의 window.ipc.postMessage 를 받아 api::dispatch 로 처리하고 결과를 window.__claudy_reply(id, result) 로 돌려준다.
//! 잠수함 모드: Dock 과 앱 전환기에서 숨기고(ActivationPolicy::Accessory) 메뉴 막대 아이콘만 남긴다.
//! 창의 닫기 단추는 창만 숨긴다. 종료는 메뉴 막대 아이콘의 「Claudy 종료」로 한다.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::macos::{ActivationPolicy, EventLoopExtMacOS, EventLoopWindowTargetExtMacOS};
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{
    AboutMetadata, CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem, Submenu,
};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use wry::http::{Request, Response};
use wry::{WebView, WebViewBuilder};

use claudy_manager::api::{self, AppState, Shared};
use claudy_manager::paths;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const FONT_REGULAR: &[u8] = include_bytes!("../ui/fonts/Pretendard-Regular.woff2");
const FONT_SEMIBOLD: &[u8] = include_bytes!("../ui/fonts/Pretendard-SemiBold.woff2");

const CLIENT_W: f64 = 720.0;
const CLIENT_H: f64 = 520.0;
const HOME_URL: &str = "claudy://localhost/index.html";
const AGENT_LABEL: &str = "com.claudy.manager";

enum UserEvent {
    Reply { id: u64, text: String },
    Pick { id: u64, cmd: String, args: Value },
    BehaviorChanged,
    Menu(MenuId),
}

/// 메뉴 막대 메뉴의 항목들. 선택 이벤트는 id 로 구분한다.
struct TrayMenu {
    open: MenuItem,
    stealth: CheckMenuItem,
    quit: MenuItem,
}

fn is_slow(cmd: &str) -> bool {
    matches!(cmd, "host.test" | "host.install_key" | "host.run_startup" | "host.trust_key" | "logs.day" | "jobs.list" | "jobs.log")
}

fn reply_text(r: Result<Value, String>) -> String {
    match r {
        Ok(data) => json!({"ok": true, "data": data}).to_string(),
        Err(e) => json!({"ok": false, "error": e}).to_string(),
    }
}

fn handle_ipc(st: &Shared, proxy: &EventLoopProxy<UserEvent>, body: &str) {
    let Ok(v) = serde_json::from_str::<Value>(body) else { return };
    let Some(id) = v.get("id").and_then(Value::as_u64) else { return };
    let cmd = v.get("cmd").and_then(Value::as_str).unwrap_or("").to_string();
    let args = v.get("args").cloned().unwrap_or(Value::Null);
    // 파일 대화상자는 macOS 에서 주 스레드에서만 띄울 수 있다
    if cmd.starts_with("pick.") {
        let _ = proxy.send_event(UserEvent::Pick { id, cmd, args });
        return;
    }
    let changed = cmd == "behavior.set";
    if is_slow(&cmd) {
        let (st, proxy) = (st.clone(), proxy.clone());
        thread::spawn(move || {
            let text = reply_text(api::dispatch(&st, &cmd, &args));
            let _ = proxy.send_event(UserEvent::Reply { id, text });
        });
    } else {
        let text = reply_text(api::dispatch(st, &cmd, &args));
        let _ = proxy.send_event(UserEvent::Reply { id, text });
        if changed {
            let _ = proxy.send_event(UserEvent::BehaviorChanged);
        }
    }
}

fn pick(cmd: &str, args: &Value) -> Result<Value, String> {
    let title = args.get("title").and_then(Value::as_str).unwrap_or("");
    let mut d = rfd::FileDialog::new();
    if !title.is_empty() {
        d = d.set_title(title);
    }
    let picked: Option<PathBuf> = if cmd == "pick.folder" { d.pick_folder() } else { d.pick_file() };
    Ok(picked.map(|p| Value::String(p.display().to_string())).unwrap_or(Value::Null))
}

fn serve(req: Request<Vec<u8>>) -> Response<Cow<'static, [u8]>> {
    let (mime, body): (&str, Cow<'static, [u8]>) = match req.uri().path() {
        "/" | "/index.html" => ("text/html; charset=utf-8", Cow::Borrowed(INDEX_HTML.as_bytes())),
        "/fonts/Pretendard-Regular.woff2" => ("font/woff2", Cow::Borrowed(FONT_REGULAR)),
        "/fonts/Pretendard-SemiBold.woff2" => ("font/woff2", Cow::Borrowed(FONT_SEMIBOLD)),
        _ => return Response::builder().status(404).body(Cow::Borrowed(&b""[..])).unwrap(),
    };
    Response::builder().header("Content-Type", mime).header("Cache-Control", "no-store").body(body).unwrap()
}

/// 메뉴 막대 아이콘(템플릿 이미지). 둥근 사각 테두리 안에 「>_」 를 그린다. 검정과 투명만 쓰고 색은 macOS 가 입힌다.
fn tray_image() -> Icon {
    const N: usize = 44; // 22pt @2x
    let mut px = vec![0u8; N * N * 4];
    let cover = |x: f32, y: f32| -> f32 {
        // 둥근 사각 테두리
        let (cx, cy, hw, hh, r, w) = (22.0, 22.0, 17.0, 14.0, 6.0, 3.0);
        let qx = (x - cx).abs() - (hw - r);
        let qy = (y - cy).abs() - (hh - r);
        let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r;
        let ring = (w / 2.0 - (outside + w / 2.0).abs()).clamp(-0.5, 0.5) + 0.5;
        // 「>」 두 획
        let seg = |ax: f32, ay: f32, bx: f32, by: f32, t: f32| -> f32 {
            let (dx, dy) = (bx - ax, by - ay);
            let h = (((x - ax) * dx + (y - ay) * dy) / (dx * dx + dy * dy)).clamp(0.0, 1.0);
            let d = ((x - ax - dx * h).powi(2) + (y - ay - dy * h).powi(2)).sqrt();
            (t / 2.0 - d).clamp(-0.5, 0.5) + 0.5
        };
        let chev = seg(13.0, 15.0, 19.0, 21.0, 3.2).max(seg(19.0, 21.0, 13.0, 27.0, 3.2));
        let under = seg(23.0, 27.5, 31.0, 27.5, 3.2);
        ring.max(chev).max(under)
    };
    for y in 0..N {
        for x in 0..N {
            // 4x4 표본으로 경계를 부드럽게 한다
            let mut a = 0.0;
            for sy in 0..4 {
                for sx in 0..4 {
                    a += cover(x as f32 + (sx as f32 + 0.5) / 4.0, y as f32 + (sy as f32 + 0.5) / 4.0);
                }
            }
            let i = (y * N + x) * 4;
            px[i + 3] = ((a / 16.0) * 255.0).round() as u8;
        }
    }
    Icon::from_rgba(px, N as u32, N as u32).expect("메뉴 막대 아이콘")
}

/// 앱 메뉴(편집 메뉴가 있어야 입력 칸에서 ⌘C·⌘V 가 동작한다)
fn app_menu() -> Menu {
    let menu = Menu::new();
    let about = AboutMetadata {
        name: Some("Claudy".into()),
        version: Some(claudy_manager::VERSION.into()),
        ..Default::default()
    };
    let app = Submenu::with_items(
        "Claudy",
        true,
        &[
            &PredefinedMenuItem::about(Some("Claudy에 관하여"), Some(about)),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::hide(Some("Claudy 가리기")),
            &PredefinedMenuItem::hide_others(Some("기타 가리기")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::quit(Some("Claudy 종료")),
        ],
    )
    .expect("앱 메뉴");
    let edit = Submenu::with_items(
        "편집",
        true,
        &[
            &PredefinedMenuItem::undo(Some("실행 취소")),
            &PredefinedMenuItem::redo(Some("실행 복귀")),
            &PredefinedMenuItem::separator(),
            &PredefinedMenuItem::cut(Some("오려두기")),
            &PredefinedMenuItem::copy(Some("복사하기")),
            &PredefinedMenuItem::paste(Some("붙여넣기")),
            &PredefinedMenuItem::select_all(Some("전체 선택")),
        ],
    )
    .expect("편집 메뉴");
    let win = Submenu::with_items(
        "윈도우",
        true,
        &[&PredefinedMenuItem::minimize(Some("최소화")), &PredefinedMenuItem::close_window(Some("닫기"))],
    )
    .expect("윈도우 메뉴");
    let _ = menu.append_items(&[&app, &edit, &win]);
    menu
}

fn tray(stealth: bool) -> (TrayIcon, TrayMenu) {
    let items = TrayMenu {
        open: MenuItem::new("Claudy 설정 열기…", true, None),
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
    let icon = TrayIconBuilder::new()
        .with_icon_templated(tray_image())
        .with_tooltip("Claudy")
        .with_menu(Box::new(menu))
        .build()
        .expect("메뉴 막대 아이콘");
    (icon, items)
}

fn policy(stealth: bool) -> ActivationPolicy {
    if stealth { ActivationPolicy::Accessory } else { ActivationPolicy::Regular }
}

/// 창을 주 화면 가운데에 놓는다
fn center(window: &Window) {
    let Some(m) = window.current_monitor().or_else(|| window.primary_monitor()) else { return };
    let (ms, ws, mp) = (m.size(), window.outer_size(), m.position());
    let x = mp.x + (ms.width as i32 - ws.width as i32) / 2;
    let y = mp.y + (ms.height as i32 - ws.height as i32) / 2;
    window.set_outer_position(tao::dpi::PhysicalPosition::new(x.max(mp.x), y.max(mp.y)));
}

fn show(window: &Window) {
    window.set_visible(true);
    window.set_minimized(false);
    window.set_focus();
}

fn agent_file() -> PathBuf {
    paths::home_dir().join("Library").join("LaunchAgents").join(format!("{AGENT_LABEL}.plist"))
}

/// 「로그인 시 실행」: ~/Library/LaunchAgents 에 실행 항목을 쓰거나 지운다. 다음 로그인부터 적용된다.
fn apply_autostart(on: bool) -> Result<(), String> {
    let path = agent_file();
    if !on {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let esc = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let text = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{AGENT_LABEL}</string>
  <key>ProgramArguments</key>
  <array><string>{}</string><string>--background</string></array>
  <key>RunAtLoad</key><true/>
  <key>ProcessType</key><string>Interactive</string>
</dict>
</plist>
"#,
        esc(&exe.display().to_string())
    );
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    std::fs::write(&path, text).map_err(|e| e.to_string())
}

fn behavior(st: &Shared) -> (bool, bool) {
    let v = api::dispatch(st, "behavior.get", &Value::Null).unwrap_or(Value::Null);
    let b = |k: &str| v.get(k).and_then(Value::as_bool).unwrap_or(false);
    (b("stealth"), b("autostart"))
}

/// 동작 설정을 실제로 적용한다(Dock 표시, 메뉴 막대 체크, 로그인 항목)
fn apply_behavior(st: &Shared, target: &EventLoopWindowTarget<UserEvent>, items: Option<&TrayMenu>, window: &Window) {
    let (stealth, autostart) = behavior(st);
    target.set_activation_policy_at_runtime(policy(stealth));
    if let Some(m) = items {
        m.stealth.set_checked(stealth);
    }
    if let Err(e) = apply_autostart(autostart) {
        eprintln!("claudy-manager: 로그인 항목을 고치지 못했다: {e}");
    }
    // 정책을 바꾸면 창이 뒤로 갈 수 있어 보이는 창은 앞으로 다시 올린다
    if window.is_visible() {
        window.set_focus();
    }
}

fn refresh_ui(webview: &WebView) {
    let _ = webview.evaluate_script(
        "(async()=>{try{S.beh=await call('behavior.get');render(true)}catch(e){}})()",
    );
}

pub fn run() {
    let background = std::env::args().any(|a| a == "--background");
    let st: Shared = Arc::new(Mutex::new(AppState::open(&paths::home_dir())));
    let (stealth, autostart) = behavior(&st);
    // 로그인 항목의 실행 파일 경로가 바뀌었을 수 있어 시작할 때 다시 쓴다
    if autostart {
        let _ = apply_autostart(true);
    }

    let mut event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    event_loop.set_activation_policy(policy(stealth));
    let proxy = event_loop.create_proxy();
    {
        let p = proxy.clone();
        MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
            let _ = p.send_event(UserEvent::Menu(e.id().clone()));
        }));
    }

    let window = WindowBuilder::new()
        .with_title("Claudy 설정")
        .with_inner_size(LogicalSize::new(CLIENT_W, CLIENT_H))
        .with_resizable(false)
        .with_maximizable(false)
        .with_visible(false)
        .build(&event_loop)
        .expect("창");

    let ipc_state = st.clone();
    let ipc_proxy = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_devtools(false)
        .with_custom_protocol("claudy".into(), |_id, req| serve(req))
        .with_navigation_handler(|u| u.starts_with("claudy://localhost/"))
        .with_ipc_handler(move |req| handle_ipc(&ipc_state, &ipc_proxy, req.body()))
        .with_url(HOME_URL)
        .build(&window)
        .expect("WKWebView");

    let mut tray_icon: Option<TrayIcon> = None;
    let mut items: Option<TrayMenu> = None;
    let mut menu_bar: Option<Menu> = None;

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // 메뉴 막대 아이콘과 앱 메뉴는 이벤트 루프가 시작된 뒤에 만들어야 한다
            Event::NewEvents(StartCause::Init) => {
                let m = app_menu();
                m.init_for_nsapp();
                menu_bar = Some(m);
                let (icon, it) = tray(stealth);
                tray_icon = Some(icon);
                items = Some(it);
                if !background {
                    center(&window);
                    show(&window);
                }
            }
            Event::UserEvent(UserEvent::Reply { id, text }) => {
                let _ = webview.evaluate_script(&format!("window.__claudy_reply({id},{text})"));
            }
            Event::UserEvent(UserEvent::Pick { id, cmd, args }) => {
                let text = reply_text(pick(&cmd, &args));
                let _ = webview.evaluate_script(&format!("window.__claudy_reply({id},{text})"));
            }
            Event::UserEvent(UserEvent::BehaviorChanged) => {
                apply_behavior(&st, target, items.as_ref(), &window);
            }
            Event::UserEvent(UserEvent::Menu(id)) => {
                let Some(m) = items.as_ref() else { return };
                if id == *m.open.id() {
                    center(&window);
                    show(&window);
                } else if id == *m.stealth.id() {
                    let on = m.stealth.is_checked();
                    let _ = api::dispatch(&st, "behavior.set", &json!({"stealth": on}));
                    apply_behavior(&st, target, items.as_ref(), &window);
                    refresh_ui(&webview);
                } else if id == *m.quit.id() {
                    st.lock().unwrap_or_else(|p| p.into_inner()).tunnel.stop();
                    tray_icon.take();
                    *control_flow = ControlFlow::Exit;
                }
            }
            // Dock 아이콘을 누르면 창을 다시 보인다
            Event::Reopen { .. } => show(&window),
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                window.set_visible(false);
            }
            _ => {}
        }
        let _ = &menu_bar;
    });
}
