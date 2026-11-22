//! 화면(JS)이 부르는 명령 처리. 입력과 출력은 모두 JSON 이다.
//! 파일 고르기(pick.*)만 창이 필요해 bin 쪽에서 처리한다.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime};

use chrono::{Local, NaiveDate};
use serde_json::{Value, json};

use crate::model::{self, Host, Model};
use crate::paths::{self, DataDir};
use crate::settings::{self, Behavior};
use crate::{jobs, logs, proc, sys};

pub struct AppState {
    pub dd: DataDir,
    /// hosts.toml 을 읽지 못하면 None. 이때는 덮어쓰지 않는다.
    pub model: Option<Model>,
    pub load_error: Option<String>,
    pub behavior: Behavior,
    pub notes: Vec<String>,
    /// 세션 첫 저장에서만 .bak 을 만든다
    pub backed_up: bool,
    pub hosts_mtime: Option<SystemTime>,
    /// 열린 사이트 계정 보관함과 마지막 사용 시각. 10분 동안 쓰지 않으면 잠근다
    /// 역방향 터널(설정 앱이 떠 있는 동안 유지)
    pub tunnel: crate::tunnel::Tunnel,
    pub tunnel_error: String,
}

pub type Shared = Arc<Mutex<AppState>>;

fn mtime(p: &PathBuf) -> Option<SystemTime> {
    fs::metadata(p).and_then(|m| m.modified()).ok()
}

impl AppState {
    /// 데이터 폴더를 정리하고(이전·기본 파일 생성) 파일을 읽어 온다.
    pub fn open(home: &std::path::Path) -> AppState {
        let (dd, mut notes) = paths::prepare(home);
        let mut st = AppState {
            dd,
            model: None,
            load_error: None,
            behavior: Behavior::default(),
            notes: Vec::new(),
            backed_up: false,
            hosts_mtime: None,
            tunnel: crate::tunnel::Tunnel::default(),
            tunnel_error: String::new(),
        };
        st.load_hosts();
        match settings::load(&st.manager_file()) {
            Ok(b) => st.behavior = b,
            Err(e) => {
                crate::tunnel::log(&format!("manager.toml 을 읽지 못했다: {e}"));
                notes.push(format!("manager.toml 을 읽지 못했습니다: {e}"))
            }
        }
        if let Some(e) = &st.load_error {
            crate::tunnel::log(&format!("hosts.toml 을 읽지 못했다: {e}"));
        }
        st.notes = notes;
        st.apply_tunnel();
        st
    }

    /// 동작 설정과 호스트 목록으로 터널을 맞춘다
    pub fn apply_tunnel(&mut self) {
        let b = &self.behavior;
        if !b.tunnel_enabled {
            self.tunnel_error.clear();
            self.tunnel.apply(None);
            return;
        }
        let via = b.tunnel_via.trim();
        let spec = self
            .model
            .as_ref()
            .and_then(|m| m.hosts.iter().find(|h| h.name.trim() == via))
            .ok_or_else(|| if via.is_empty() { "터널 서버를 고르세요".to_string() } else { format!("`{via}` 호스트가 목록에 없습니다") })
            .and_then(|h| crate::tunnel::Spec::from_host(h, b.tunnel_remote_port, b.tunnel_local_port));
        match spec {
            Ok(s) => {
                self.tunnel_error.clear();
                self.tunnel.apply(Some(s));
            }
            Err(e) => {
                crate::tunnel::log(&format!("터널 설정 오류: {e}"));
                self.tunnel_error = e;
                self.tunnel.apply(None);
            }
        }
    }

    pub fn manager_file(&self) -> PathBuf {
        self.dd.dir.join("manager.toml")
    }

    fn load_hosts(&mut self) {
        let path = self.dd.hosts_file();
        self.hosts_mtime = mtime(&path);
        match model::load(&path) {
            Ok(Some(m)) => {
                self.model = Some(m);
                self.load_error = None;
            }
            Ok(None) => {
                self.model = Some(Model::initial());
                self.load_error = None;
            }
            Err(e) => {
                self.model = None;
                self.load_error = Some(e);
            }
        }
    }

    fn save_model(&mut self, m: Model) -> Result<(), String> {
        let path = self.dd.hosts_file();
        model::save_with(&path, &m, !self.backed_up)?;
        self.backed_up = true;
        self.hosts_mtime = mtime(&path);
        self.model = Some(m);
        if self.behavior.tunnel_enabled {
            self.apply_tunnel();
        }
        Ok(())
    }
}

pub fn host_json(h: &Host) -> Value {
    json!({
        "name": h.name,
        "kind": h.kind,
        "shell": h.shell,
        "claude": h.claude,
        "workdir": h.workdir,
        "host": h.host,
        "port": h.port,
        "user": h.user,
        "identity_file": h.identity_file,
        "accept_new_host_key": h.accept_new_host_key,
        "claude_permissions": h.claude_permissions,
        "startup": h.startup,
        "run_as_root": h.run_as_root,
        "jump": h.jump,
        "desktop": h.desktop,
    })
}

/// JSON 에 들어 있는 칸만 host 에 덮어쓴다. 없는 칸과 알 수 없는 항목은 그대로 둔다.
pub fn apply_fields(h: &mut Host, v: &Value) {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(|x| x.to_string());
    if let Some(x) = s("name") {
        h.name = x;
    }
    if let Some(x) = s("kind") {
        h.kind = x;
    }
    if let Some(x) = s("shell") {
        h.shell = x;
    }
    if let Some(x) = s("claude") {
        h.claude = x;
    }
    if let Some(x) = s("workdir") {
        h.workdir = x;
    }
    if let Some(x) = s("host") {
        h.host = x;
    }
    if let Some(x) = s("port") {
        h.port = x;
    }
    if let Some(x) = s("user") {
        h.user = x;
    }
    if let Some(x) = s("identity_file") {
        h.identity_file = x;
    }
    if let Some(b) = v.get("run_as_root").and_then(Value::as_bool) {
        h.run_as_root = b;
    }
    if let Some(x) = s("jump") {
        h.jump = x;
    }
    if let Some(b) = v.get("desktop").and_then(Value::as_bool) {
        h.desktop = b;
    }
    if let Some(x) = s("startup") {
        h.startup = x;
    }
    if let Some(x) = s("claude_permissions") {
        h.claude_permissions = x;
    }
    if let Some(b) = v.get("accept_new_host_key").and_then(Value::as_bool) {
        h.accept_new_host_key = b;
    }
}

fn hosts_value(st: &AppState) -> Value {
    match &st.model {
        Some(m) => Value::Array(m.hosts.iter().map(host_json).collect()),
        None => json!([]),
    }
}

fn need_model(st: &mut AppState) -> Result<&mut Model, String> {
    // 다른 곳(서버, 손 편집)에서 hosts.toml 을 고쳤으면 다시 읽고 그 위에 바꾼다. 남의 수정을 덮어쓰지 않기 위함
    if mtime(&st.dd.hosts_file()) != st.hosts_mtime {
        st.load_hosts();
    }
    let msg = st.load_error.clone().unwrap_or_default();
    st.model.as_mut().ok_or_else(|| format!("hosts.toml 을 읽지 못해 저장하지 않습니다. {msg}"))
}

fn arg_str(args: &Value, k: &str) -> String {
    args.get(k).and_then(Value::as_str).unwrap_or("").to_string()
}

/// 칸별 오류를 {칸: 메시지} 로 만든다. 이름 중복은 name 에 붙인다.
fn host_errors(h: &Host, others: &[&Host]) -> serde_json::Map<String, Value> {
    let mut out = serde_json::Map::new();
    for (f, m) in h.field_errors() {
        out.entry(f.to_string()).or_insert(Value::String(m));
    }
    let n = h.name.trim();
    if !n.is_empty() && others.iter().any(|o| o.name.trim() == n) {
        out.insert("name".into(), Value::String(format!("같은 이름의 호스트가 있습니다: {n}")));
    }
    out
}

fn cmd_hosts_save(st: &mut AppState, args: &Value) -> Result<Value, String> {
    let original = arg_str(args, "original");
    let fields = args.get("host").cloned().unwrap_or(Value::Null);
    let m = need_model(st)?;
    let mut next = m.clone();
    let idx = if original.is_empty() { None } else { next.hosts.iter().position(|h| h.name == original) };
    let mut h = match idx {
        Some(i) => next.hosts[i].clone(),
        None if original.is_empty() => Host { kind: "ssh".into(), port: "22".into(), ..Default::default() },
        None => return Err(format!("호스트를 찾을 수 없습니다: {original}")),
    };
    apply_fields(&mut h, &fields);
    let errors = {
        let others: Vec<&Host> = next.hosts.iter().enumerate().filter(|(i, _)| Some(*i) != idx).map(|(_, h)| h).collect();
        host_errors(&h, &others)
    };
    if !errors.is_empty() {
        return Ok(json!({"saved": false, "errors": errors}));
    }
    h.name = h.name.trim().to_string();
    match idx {
        Some(i) => next.hosts[i] = h.clone(),
        None => next.hosts.push(h.clone()),
    }
    st.save_model(next)?;
    Ok(json!({"saved": true, "name": h.name, "hosts": hosts_value(st)}))
}

fn cmd_hosts_delete(st: &mut AppState, args: &Value) -> Result<Value, String> {
    let name = arg_str(args, "name");
    let m = need_model(st)?;
    let mut next = m.clone();
    if next.hosts.len() <= 1 && next.hosts.iter().any(|h| h.name == name) {
        return Ok(json!({"saved": false, "message": "호스트는 최소 한 개가 있어야 합니다."}));
    }
    let before = next.hosts.len();
    next.hosts.retain(|h| h.name != name);
    if next.hosts.len() == before {
        return Err(format!("호스트를 찾을 수 없습니다: {name}"));
    }
    st.save_model(next)?;
    Ok(json!({"saved": true, "hosts": hosts_value(st)}))
}

/// 시험·등록에 쓸 호스트. name 이 있으면 저장된 호스트, host 가 있으면 입력 중인 값.
fn pick_host(st: &AppState, args: &Value) -> Result<Host, String> {
    if let Some(v) = args.get("host").filter(|v| v.is_object()) {
        let mut h = Host { kind: "ssh".into(), ..Default::default() };
        apply_fields(&mut h, v);
        if let Some(m) = st.model.as_ref() {
            h = h.with_jump(&m.hosts);
        }
        return Ok(h);
    }
    let name = arg_str(args, "name");
    let m = st.model.as_ref();
    m.and_then(|m| m.hosts.iter().find(|h| h.name == name).cloned().map(|h| h.with_jump(&m.hosts)))
        .ok_or_else(|| format!("호스트를 찾을 수 없습니다: {name}"))
}

fn fmt_ms(ms: u128) -> String {
    if ms < 1000 { format!("{ms} ms") } else { format!("{:.1}초", ms as f64 / 1000.0) }
}

fn last_line(s: &str) -> String {
    s.lines().map(str::trim).filter(|l| !l.is_empty()).last().unwrap_or("").to_string()
}

fn cmd_host_test(h: Host) -> Result<Value, String> {
    if let Err(e) = proc::can_connect(&h) {
        return Ok(json!({"ok": false, "text": "실패", "detail": e}));
    }
    let c = proc::test_cmd(&h);
    let t = Instant::now();
    let r = proc::run(&c);
    let ms = t.elapsed().as_millis();
    if r.spawn_error.is_some() {
        let detail = proc::test_summary(&r);
        return Ok(json!({"ok": false, "text": "실행하지 못했습니다", "detail": detail.trim(), "summary": detail}));
    }
    let ok = r.code == Some(0);
    let summary = proc::test_summary(&r);
    if ok {
        let ver = last_line(&r.out);
        Ok(json!({"ok": true, "text": format!("연결됨 · {}", fmt_ms(ms)), "detail": ver, "ms": ms as u64, "summary": summary}))
    } else {
        let code = r.code.map(|c| c.to_string()).unwrap_or_else(|| "없음".into());
        let why = last_line(&r.err);
        Ok(json!({"ok": false, "text": format!("실패 · 종료 코드 {code}"), "detail": why, "ms": ms as u64, "summary": summary}))
    }
}

fn cmd_host_trust(h: Host) -> Result<Value, String> {
    if let Err(e) = proc::can_connect(&h) {
        return Ok(json!({"ok": false, "detail": e}));
    }
    if h.kind.trim() != "ssh" {
        return Ok(json!({"ok": false, "detail": "SSH 호스트에서만 등록할 수 있습니다."}));
    }
    let r = proc::run(&proc::hostkey_cmd(&h));
    let summary = proc::hostkey_summary(&r);
    Ok(json!({"ok": r.spawn_error.is_none() && r.code == Some(0), "detail": summary, "summary": summary}))
}

fn cmd_claude_get(st: &AppState, args: &Value) -> Result<Value, String> {
    let h = pick_host(st, &json!({"name": arg_str(args, "name")}))?;
    let perm = h.claude_permissions.trim();
    Ok(json!({
        "claude": h.claude,
        "workdir": h.workdir,
        "permissions": if perm.is_empty() { "skip" } else { perm },
    }))
}

fn cmd_claude_set(st: &mut AppState, args: &Value) -> Result<Value, String> {
    let name = arg_str(args, "name");
    let mut fields = serde_json::Map::new();
    for k in ["claude", "workdir"] {
        if let Some(v) = args.get(k).and_then(Value::as_str) {
            fields.insert(k.into(), Value::String(v.to_string()));
        }
    }
    if let Some(p) = args.get("permissions").and_then(Value::as_str) {
        // 값이 없으면 전체 허용으로 보이므로, 이미 값이 있을 때만 skip 을 쓴다.
        let cur_empty = st
            .model
            .as_ref()
            .and_then(|m| m.hosts.iter().find(|h| h.name == name))
            .map(|h| h.claude_permissions.trim().is_empty())
            .unwrap_or(true);
        if !(p == "skip" && cur_empty) {
            fields.insert("claude_permissions".into(), Value::String(p.to_string()));
        }
    }
    cmd_hosts_save(st, &json!({"original": name, "host": Value::Object(fields)}))
}

fn security_value(m: &Model) -> Value {
    json!({"list": model::deny_to_edit(&m.deny).lines().map(str::to_string).collect::<Vec<_>>(), "custom": m.deny.is_some()})
}

fn cmd_security_set(st: &mut AppState, args: &Value) -> Result<Value, String> {
    let list: Vec<String> = args
        .get("list")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
        .unwrap_or_default();
    for (i, p) in list.iter().enumerate() {
        if let Err(e) = regex::Regex::new(p) {
            let first = e.to_string().lines().last().unwrap_or("").trim().to_string();
            return Ok(json!({"saved": false, "index": i, "message": format!("정규식이 올바르지 않습니다: {first}")}));
        }
    }
    let m = need_model(st)?;
    let mut next = m.clone();
    next.deny = model::deny_from_edit(&list.join("\n"));
    st.save_model(next)?;
    let m = st.model.as_ref().unwrap();
    Ok(json!({"saved": true, "security": security_value(m)}))
}

fn parse_date(args: &Value) -> NaiveDate {
    args.get("date")
        .and_then(Value::as_str)
        .and_then(|s| NaiveDate::parse_from_str(s, "%Y-%m-%d").ok())
        .unwrap_or_else(|| Local::now().date_naive())
}

const LOG_LIMIT: usize = 500;

fn cmd_logs_day(st: &AppState, args: &Value) -> Result<Value, String> {
    let date = parse_date(args);
    let query = arg_str(args, "query");
    let path = logs::file_for(&st.dd.logs_dir(), date);
    let Some(raw) = logs::read_raw(&path) else {
        return Ok(json!({"exists": false, "rows": [], "total": 0, "truncated": false}));
    };
    // 최근 기록이 위로 오도록 거꾸로 훑는다
    let all: Vec<logs::LogLine> =
        raw.iter().rev().filter(|r| logs::matches(r, &query)).filter_map(|r| logs::parse_line(r)).collect();
    let total = all.len();
    let rows: Vec<Value> = all
        .into_iter()
        .take(LOG_LIMIT)
        .map(|l| {
            json!({"t": l.time, "tool": l.tool, "host": l.host, "code": l.exit_code, "dur": l.duration_ms, "cmd": l.text, "denied": l.denied})
        })
        .collect();
    Ok(json!({"exists": true, "rows": rows, "total": total, "truncated": total > LOG_LIMIT}))
}

fn cmd_jobs_list(st: &AppState) -> Result<Value, String> {
    let rows: Vec<Value> = jobs::read_rows(&st.dd.jobs_dir())
        .into_iter()
        .map(|r| {
            let state = match r.state {
                jobs::State::Running => "run",
                jobs::State::Finished if r.exit_code.unwrap_or(0) == 0 => "ok",
                jobs::State::Finished => "fail",
                jobs::State::Interrupted => "stop",
            };
            json!({"id": r.id, "host": r.host, "kind": r.kind, "started": r.started, "state": state, "label": r.state.label(), "code": r.exit_code})
        })
        .collect();
    Ok(json!({"rows": rows}))
}

/// 파일 이름으로 쓸 수 있는 id 인지 본다(경로 이탈 방지)
fn safe_name(s: &str) -> bool {
    !s.is_empty() && !s.contains(['/', '\\', ':']) && s != "." && s != ".."
}

fn cmd_jobs_log(st: &AppState, args: &Value) -> Result<Value, String> {
    let id = arg_str(args, "id");
    if !safe_name(&id) {
        return Err("작업 번호가 올바르지 않습니다.".into());
    }
    Ok(json!({"text": jobs::tail(&st.dd.jobs_dir().join(format!("{id}.log")), 200)}))
}

fn behavior_value(b: &Behavior) -> Value {
    json!({
        "stealth": b.stealth_mode,
        "autostart": b.autostart,
        "tunnel": b.tunnel_enabled,
        "tunnelVia": b.tunnel_via,
        "tunnelPort": b.tunnel_remote_port,
        "hwpGuard": b.hwp_guard,
    })
}

/// 파일 끝 n 줄
fn tail_lines(path: &std::path::Path, n: usize) -> Vec<String> {
    let text = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).to_string()).unwrap_or_default();
    let lines: Vec<String> = text.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect();
    lines[lines.len().saturating_sub(n)..].to_vec()
}

/// Windows 자동화 상태: 한글 창 자동 승인 기록, 화면 세션 실행 대행 상태
fn automation_value(st: &AppState) -> Value {
    #[cfg(windows)]
    let (ready, runs) = crate::deskrun::status();
    #[cfg(not(windows))]
    let (ready, runs) = (false, 0u64);
    json!({
        "windows": cfg!(windows),
        "hwpGuard": st.behavior.hwp_guard,
        "hwpLog": tail_lines(&st.dd.dir.join("hwpguard.log"), 20),
        "deskReady": ready,
        "deskRuns": runs,
        "tunnelLog": tail_lines(&st.dd.dir.join("tunnel.log"), 8),
        "iso": iso_value(st),
        "isoLog": tail_lines(&st.dd.dir.join("isolate.log"), 12),
    })
}

#[cfg(windows)]
fn iso_value(st: &AppState) -> Value {
    crate::isolation::status(&st.dd.dir)
}
#[cfg(not(windows))]
fn iso_value(_st: &AppState) -> Value {
    json!({"guard": false, "procs": []})
}

fn tunnel_value(st: &AppState) -> Value {
    let mut v = st.tunnel.status();
    if !st.tunnel_error.is_empty() {
        v["state"] = json!("설정 확인 필요");
        v["error"] = json!(st.tunnel_error);
    }
    v
}

fn cmd_behavior_set(st: &mut AppState, args: &Value) -> Result<Value, String> {
    let mut b = st.behavior.clone();
    if let Some(v) = args.get("stealth").and_then(Value::as_bool) {
        b.stealth_mode = v;
    }
    if let Some(v) = args.get("autostart").and_then(Value::as_bool) {
        b.autostart = v;
    }
    if let Some(v) = args.get("tunnel").and_then(Value::as_bool) {
        b.tunnel_enabled = v;
    }
    if let Some(v) = args.get("hwpGuard").and_then(Value::as_bool) {
        b.hwp_guard = v;
    }
    if let Some(v) = args.get("tunnelVia").and_then(Value::as_str) {
        b.tunnel_via = v.trim().to_string();
    }
    if let Some(v) = args.get("tunnelPort") {
        let p = v.as_u64().or_else(|| v.as_str().and_then(|s| s.trim().parse().ok())).unwrap_or(0);
        if !(1024..=65535).contains(&p) {
            return Err("터널 포트는 1024~65535 사이의 숫자여야 합니다".into());
        }
        b.tunnel_remote_port = p as u16;
    }
    settings::save(&st.manager_file(), &b)?;
    #[cfg(windows)]
    crate::hwpguard::set_enabled(b.hwp_guard);
    st.behavior = b;
    st.apply_tunnel();
    Ok(behavior_value(&st.behavior))
}

fn lock(st: &Shared) -> std::sync::MutexGuard<'_, AppState> {
    st.lock().unwrap_or_else(|p| p.into_inner())
}

/// 명령 하나를 처리한다. Err 의 문자열은 화면에 오류로 보인다.
/// 시간이 걸리는 명령(host.test, host.trust_key)은 잠금을 풀고 실행한다.
pub fn dispatch(st: &Shared, cmd: &str, args: &Value) -> Result<Value, String> {
    match cmd {
        "hosts.list" => {
            let mut s = lock(st);
            let path = s.dd.hosts_file();
            if args.get("reload").and_then(Value::as_bool) == Some(true) && mtime(&path) != s.hosts_mtime {
                s.load_hosts();
            }
            Ok(json!({"hosts": hosts_value(&s), "error": s.load_error, "notes": s.notes, "security": s.model.as_ref().map(security_value)}))
        }
        "hosts.save" => cmd_hosts_save(&mut lock(st), args),
        "hosts.delete" => cmd_hosts_delete(&mut lock(st), args),
        "host.test" => {
            let h = pick_host(&lock(st), args)?;
            cmd_host_test(h)
        }
        "host.install_key" => {
            let h = pick_host(&lock(st), args)?;
            let pw = arg_str(args, "password");
            let home = crate::paths::home_dir();
            let key = crate::keyinstall::install(&h, &home, &pw)?;
            let mut s = lock(st);
            let fields = json!({"identity_file": key.display().to_string()});
            let r = cmd_hosts_save(&mut s, &json!({"original": h.name, "host": fields}))?;
            Ok(json!({"ok": true, "key": key.display().to_string(), "hosts": r.get("hosts").cloned().unwrap_or(Value::Null)}))
        }
        "host.run_startup" => {
            let h = pick_host(&lock(st), args)?;
            if h.kind.trim() != "ssh" || h.startup.lines().all(|l| l.trim().is_empty() || l.trim().starts_with('#')) {
                return Ok(json!({"ok": false, "detail": "접속 후 명령을 먼저 적어 주세요."}));
            }
            if let Err(e) = proc::can_connect(&h) {
                return Ok(json!({"ok": false, "detail": e}));
            }
            let r = proc::run(&proc::startup_cmd(&h));
            Ok(json!({"ok": r.spawn_error.is_none() && r.code == Some(0), "detail": proc::test_summary(&r)}))
        }
        "host.trust_key" => {
            let h = pick_host(&lock(st), args)?;
            cmd_host_trust(h)
        }
        "claude.get" => cmd_claude_get(&lock(st), args),
        "claude.set" => cmd_claude_set(&mut lock(st), args),
        "security.get" => {
            let s = lock(st);
            let m = s.model.as_ref().ok_or_else(|| s.load_error.clone().unwrap_or_default())?;
            Ok(security_value(m))
        }
        "security.set" => cmd_security_set(&mut lock(st), args),
        "security.reset" => cmd_security_set(&mut lock(st), &json!({"list": []})),
        "logs.day" => cmd_logs_day(&lock(st), args),
        "jobs.list" => cmd_jobs_list(&lock(st)),
        "jobs.log" => cmd_jobs_log(&lock(st), args),
        "app.info" => {
            let s = lock(st);
            Ok(json!({
                "version": crate::VERSION,
                "dataDir": s.dd.dir.display().to_string(),
                "hostsFile": s.dd.hosts_file().display().to_string(),
                "managerFile": s.manager_file().display().to_string(),
            }))
        }
        "app.status" => {
            let hosts = lock(st).model.as_ref().map(|m| m.hosts.len()).unwrap_or(0);
            Ok(json!({"running": sys::server_running(), "hosts": hosts}))
        }
        "behavior.get" => Ok(behavior_value(&lock(st).behavior)),
        "tunnel.status" => Ok(tunnel_value(&lock(st))),
        "automation.status" => Ok(automation_value(&lock(st))),
        "isolation.close_all" => {
            #[cfg(windows)]
            let n = crate::isolation::close_all(&lock(st).dd.dir);
            #[cfg(not(windows))]
            let n = 0usize;
            Ok(json!({"closed": n}))
        }
        "behavior.set" => cmd_behavior_set(&mut lock(st), args),
        other => Err(format!("알 수 없는 명령입니다: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(name: &str) -> PathBuf {
        let b = std::env::temp_dir().join(format!("claudy-api-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&b);
        fs::create_dir_all(&b).unwrap();
        b
    }

    fn shared(name: &str) -> (Shared, PathBuf) {
        let h = home(name);
        (Arc::new(Mutex::new(AppState::open(&h))), h)
    }

    fn ok(st: &Shared, cmd: &str, args: Value) -> Value {
        dispatch(st, cmd, &args).unwrap_or_else(|e| panic!("{cmd}: {e}"))
    }

    #[test]
    fn first_start_creates_default_host() {
        let (st, h) = shared("first");
        let v = ok(&st, "hosts.list", json!({}));
        assert_eq!(v["hosts"][0]["name"], "this-pc");
        assert!(h.join(".claudy").join("hosts.toml").exists());
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn save_validates_and_backs_up_once() {
        let (st, h) = shared("save");
        // 새 ssh 호스트: 주소와 사용자가 비어 있으면 저장하지 않는다
        let v = ok(&st, "hosts.save", json!({"original": "", "host": {"name": "pi", "kind": "ssh"}}));
        assert_eq!(v["saved"], false);
        assert!(v["errors"]["host"].is_string() && v["errors"]["user"].is_string());
        let v = ok(&st, "hosts.save", json!({"original": "", "host": {"name": "pi", "kind": "ssh", "host": "x.invalid", "user": "u", "port": "22"}}));
        assert_eq!(v["saved"], true);
        let dir = h.join(".claudy");
        assert!(dir.join("hosts.toml.bak").exists(), "첫 저장에서 백업");
        let bak1 = fs::read_to_string(dir.join("hosts.toml.bak")).unwrap();
        // 두 번째 저장은 .bak 을 다시 만들지 않는다
        ok(&st, "hosts.save", json!({"original": "pi", "host": {"user": "v"}}));
        assert_eq!(fs::read_to_string(dir.join("hosts.toml.bak")).unwrap(), bak1);
        let text = fs::read_to_string(dir.join("hosts.toml")).unwrap();
        assert!(text.contains("user = \"v\""));
        // 이름 중복, 포트 오류
        let v = ok(&st, "hosts.save", json!({"original": "pi", "host": {"name": "this-pc"}}));
        assert!(v["errors"]["name"].as_str().unwrap().contains("같은 이름"));
        let v = ok(&st, "hosts.save", json!({"original": "pi", "host": {"port": "70000"}}));
        assert!(v["errors"]["port"].is_string());
        // 저장하지 않았으므로 파일은 그대로
        assert_eq!(fs::read_to_string(dir.join("hosts.toml")).unwrap(), text);
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn unknown_fields_survive_save() {
        let h = home("extra");
        fs::create_dir_all(h.join(".claudy")).unwrap();
        fs::write(
            h.join(".claudy").join("hosts.toml"),
            "top = 1\n[[host]]\nname = \"this-pc\"\nkind = \"local\"\nfuture = true\n",
        )
        .unwrap();
        let st: Shared = Arc::new(Mutex::new(AppState::open(&h)));
        ok(&st, "claude.set", json!({"name": "this-pc", "workdir": "C:\\w"}));
        let t = fs::read_to_string(h.join(".claudy").join("hosts.toml")).unwrap();
        assert!(t.contains("future = true") && t.contains("top = 1") && t.contains("workdir"));
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn rename_and_delete() {
        let (st, h) = shared("del");
        let v = ok(&st, "hosts.delete", json!({"name": "this-pc"}));
        assert_eq!(v["saved"], false, "마지막 호스트는 지울 수 없다");
        ok(&st, "hosts.save", json!({"original": "", "host": {"name": "a", "kind": "local"}}));
        let v = ok(&st, "hosts.save", json!({"original": "a", "host": {"name": "b"}}));
        assert_eq!(v["name"], "b");
        let v = ok(&st, "hosts.delete", json!({"name": "this-pc"}));
        assert_eq!(v["hosts"].as_array().unwrap().len(), 1);
        assert!(dispatch(&st, "hosts.delete", &json!({"name": "zzz"})).is_err());
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn claude_permissions_skip_stays_empty() {
        let (st, h) = shared("perm");
        assert_eq!(ok(&st, "claude.get", json!({"name": "this-pc"}))["permissions"], "skip");
        ok(&st, "claude.set", json!({"name": "this-pc", "permissions": "skip"}));
        assert!(!fs::read_to_string(h.join(".claudy").join("hosts.toml")).unwrap().contains("\nclaude_permissions ="));
        ok(&st, "claude.set", json!({"name": "this-pc", "permissions": "default"}));
        assert_eq!(ok(&st, "claude.get", json!({"name": "this-pc"}))["permissions"], "default");
        ok(&st, "claude.set", json!({"name": "this-pc", "permissions": "skip"}));
        assert_eq!(ok(&st, "claude.get", json!({"name": "this-pc"}))["permissions"], "skip");
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn security_list_and_reset() {
        let (st, h) = shared("sec");
        let g = ok(&st, "security.get", json!({}));
        assert_eq!(g["custom"], false);
        assert_eq!(g["list"].as_array().unwrap().len(), model::DEFAULT_DENY.len());
        let bad = ok(&st, "security.set", json!({"list": ["ok", "("]}));
        assert_eq!(bad["saved"], false);
        assert_eq!(bad["index"], 1);
        let v = ok(&st, "security.set", json!({"list": ["abc", "d+"]}));
        assert_eq!(v["security"]["custom"], true);
        assert_eq!(v["security"]["list"], json!(["abc", "d+"]));
        let v = ok(&st, "security.reset", json!({}));
        assert_eq!(v["security"]["custom"], false);
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn broken_hosts_file_is_never_overwritten() {
        let h = home("broken");
        fs::create_dir_all(h.join(".claudy")).unwrap();
        let p = h.join(".claudy").join("hosts.toml");
        fs::write(&p, "this is = = not toml").unwrap();
        let st: Shared = Arc::new(Mutex::new(AppState::open(&h)));
        let v = ok(&st, "hosts.list", json!({}));
        assert!(v["error"].is_string());
        assert!(dispatch(&st, "hosts.save", &json!({"original": "", "host": {"name": "x", "kind": "local"}})).is_err());
        assert_eq!(fs::read_to_string(&p).unwrap(), "this is = = not toml");
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn logs_and_jobs_from_files() {
        let (st, h) = shared("data");
        let d = h.join(".claudy");
        fs::create_dir_all(d.join("logs")).unwrap();
        fs::create_dir_all(d.join("jobs")).unwrap();
        fs::write(
            d.join("logs").join("2026-11-06.jsonl"),
            "{\"ts\":\"2026-11-06T10:00:00+09:00\",\"tool\":\"run_command\",\"host\":\"this-pc\",\"text\":\"dir\",\"exit_code\":0,\"duration_ms\":12}\n\n{\"ts\":\"2026-11-06T11:00:00+09:00\",\"tool\":\"claude_ask\",\"host\":\"pi\",\"text\":\"hello\",\"exit_code\":1,\"duration_ms\":900,\"denied\":true}\n",
        )
        .unwrap();
        let v = ok(&st, "logs.day", json!({"date": "2026-11-06", "query": ""}));
        assert_eq!(v["rows"].as_array().unwrap().len(), 2);
        assert_eq!(v["rows"][0]["tool"], "claude_ask", "최근 기록이 위");
        assert_eq!(v["rows"][0]["denied"], true);
        let q = ok(&st, "logs.day", json!({"date": "2026-11-06", "query": "HELLO"}));
        assert_eq!(q["rows"].as_array().unwrap().len(), 1);
        assert_eq!(ok(&st, "logs.day", json!({"date": "2026-11-05"}))["exists"], false);

        fs::write(d.join("jobs").join("abc.json"), "{\"host\":\"pi\",\"kind\":\"claude\",\"started_at\":\"2026-11-06T01:02:03+09:00\",\"finished\":true,\"exit_code\":2}").unwrap();
        fs::write(d.join("jobs").join("abc.log"), "l1\nl2\n").unwrap();
        let j = ok(&st, "jobs.list", json!({}));
        assert_eq!(j["rows"][0]["state"], "fail");
        assert_eq!(ok(&st, "jobs.log", json!({"id": "abc"}))["text"], "l1\nl2");
        assert!(dispatch(&st, "jobs.log", &json!({"id": "../x"})).is_err());

        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn behavior_is_stored_in_manager_toml() {
        let (st, h) = shared("beh");
        assert_eq!(ok(&st, "behavior.get", json!({}))["stealth"], false);
        ok(&st, "behavior.set", json!({"stealth": true}));
        let v = ok(&st, "behavior.set", json!({"autostart": true}));
        assert_eq!(v["stealth"], true);
        let t = fs::read_to_string(h.join(".claudy").join("manager.toml")).unwrap();
        assert!(t.contains("stealth_mode = true") && t.contains("autostart = true"));
        let _ = fs::remove_dir_all(&h);
    }

    #[test]
    fn unknown_command_is_error() {
        let (st, h) = shared("unk");
        assert!(dispatch(&st, "nope", &json!({})).is_err());
        let _ = fs::remove_dir_all(&h);
    }
}
