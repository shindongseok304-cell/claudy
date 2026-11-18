//! Linux 창(WebKitGTK)과 알림 영역 아이콘(AppIndicator).
//! JS 의 window.ipc.postMessage 를 받아 api::dispatch 로 처리하고 결과를 window.__claudy_reply(id, result) 로 돌려준다.
//! 잠수함 모드: 작업 표시줄에서 숨기고(skip taskbar) 알림 영역 아이콘만 남긴다.
//! 창의 닫기 단추는 창만 숨긴다. 종료는 알림 영역 아이콘 메뉴의 「Claudy 종료」로 한다.

use std::borrow::Cow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use serde_json::{Value, json};
use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoopWindowTarget};
use tao::platform::unix::WindowExtUnix;
use tao::window::{Window, WindowBuilder};
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuId, MenuItem, PredefinedMenuItem};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};
use wry::http::{Request, Response};
use wry::{WebView, WebViewBuilder, WebViewBuilderExtUnix};

use claudy_manager::api::{self, AppState, Shared};
use claudy_manager::paths;

const INDEX_HTML: &str = include_str!("../ui/index.html");
const FONT_REGULAR: &[u8] = include_bytes!("../ui/fonts/Pretendard-Regular.woff2");
const FONT_SEMIBOLD: &[u8] = include_bytes!("../ui/fonts/Pretendard-SemiBold.woff2");

const CLIENT_W: f64 = 720.0;
const CLIENT_H: f64 = 520.0;
const HOME_URL: &str = "claudy://localhost/index.html";

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
    // 파일 대화상자는 주 스레드(GTK)에서 띄운다
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

/// 알림 영역 아이콘. 둥근 사각 테두리 안에 「>_」 를 파란색으로 그린다.
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
            px[i] = 0x4A;
            px[i + 1] = 0x7B;
            px[i + 2] = 0xD0;
            px[i + 3] = ((a / 16.0) * 255.0).round() as u8;
        }
    }
    Icon::from_rgba(px, N as u32, N as u32).expect("알림 영역 아이콘")
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
        .with_icon(tray_image())
        .with_tooltip("Claudy")
        .with_menu(Box::new(menu))
        .build()
        .expect("알림 영역 아이콘");
    (icon, items)
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

fn autostart_file() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| paths::home_dir().join(".config"));
    base.join("autostart").join("claudy-manager.desktop")
}

/// 「로그인 시 실행」: ~/.config/autostart 에 실행 항목을 쓰거나 지운다. 다음 로그인부터 적용된다.
fn apply_autostart(on: bool) -> Result<(), String> {
    let path = autostart_file();
    if !on {
        return match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.to_string()),
        };
    }
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let text = format!(
        "[Desktop Entry]\nType=Application\nName=Claudy\nComment=Claudy 설정\nExec=\"{}\" --background\nTerminal=false\nX-GNOME-Autostart-enabled=true\n",
        exe.display().to_string().replace('"', "")
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

/// 동작 설정을 실제로 적용한다(작업 표시줄 표시, 메뉴 체크, 로그인 항목)
fn apply_behavior(st: &Shared, _target: &EventLoopWindowTarget<UserEvent>, items: Option<&TrayMenu>, window: &Window) {
    let (stealth, autostart) = behavior(st);
    let _ = window.set_skip_taskbar(stealth);
    if let Some(m) = items {
        m.stealth.set_checked(stealth);
    }
    if let Err(e) = apply_autostart(autostart) {
        eprintln!("claudy-manager: 로그인 항목을 고치지 못했다: {e}");
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

    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
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
    let _ = window.set_skip_taskbar(stealth);

    let ipc_state = st.clone();
    let ipc_proxy = proxy.clone();
    let webview = WebViewBuilder::new()
        .with_devtools(false)
        .with_custom_protocol("claudy".into(), |_id, req| serve(req))
        .with_navigation_handler(|u| u.starts_with("claudy://localhost/"))
        .with_ipc_handler(move |req| handle_ipc(&ipc_state, &ipc_proxy, req.body()))
        .with_url(HOME_URL)
        .build_gtk(window.default_vbox().expect("창 상자"))
        .expect("WebKitGTK");

    let mut tray_icon: Option<TrayIcon> = None;
    let mut items: Option<TrayMenu> = None;

    event_loop.run(move |event, target, control_flow| {
        *control_flow = ControlFlow::Wait;
        match event {
            // 알림 영역 아이콘은 이벤트 루프가 시작된 뒤에 만든다
            Event::NewEvents(StartCause::Init) => {
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
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } => {
                window.set_visible(false);
            }
            _ => {}
        }
    });
}
