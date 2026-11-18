mod audit;
mod claude;
mod config;
mod exec;
mod jobs;
mod safety;
mod util;

use std::time::Duration;

use regex::Regex;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{CallToolResult, ContentBlock as Content, Implementation, InitializeResult, ServerCapabilities, ServerConfig};
use rmcp::transport::stdio;
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use config::{Config, HostCfg};
use exec::Captured;

const INSTRUCTIONS_BASE: &str = "Claudy: 등록된 호스트(이 PC 또는 SSH 서버)에서 셸 명령과 Claude Code(claude -p)를 실행한다. \
호스트 이름은 hosts_list 로 확인한다. 긴 작업은 job_start 로 백그라운드에 띄우고 job_status 로 확인한다. \
claude_ask 는 호출마다 Claude Code 를 새로 띄워 수십 초와 별도 토큰이 들므로 코드 작성처럼 판단이 필요한 일에만 쓴다. \
웹 페이지 열기·클릭·읽기 같은 브라우저 작업은 claude_ask 에 맡기지 말고 ego_run(ego 브라우저) 또는 browser_run(Chrome·Edge·Aside)으로 직접 실행한다(수 초, 추가 토큰 없음). \
로그인은 사용자 브라우저의 비밀번호 관리 확장 프로그램에 맡긴다. 아이디·비밀번호를 채팅에서 묻거나 받거나 스크립트에 넣지 않는다. \
호스트 사이 파일 복사는 file_copy 를 쓴다(scp 를 직접 짜지 않는다). Windows 호스트의 Claudy 갱신은 claudy_update 를 쓴다. \
창이 있는 프로그램은 app_open 으로 띄운다(원격 호스트에서 run_command 로 띄운 창은 화면에 보이지 않는다). \
한글(HWPFrame.HwpObject) 자동화 스크립트는 객체를 만든 직후 반드시 RegisterModule(\"FilePathCheckDLL\", \"FilePathCheckerModule\") 을 부른다. 빠뜨리면 파일을 열 때마다 「접근 허용」 창이 떠서 작업이 멈춘다.";
const INSTRUCTIONS: &str = INSTRUCTIONS_BASE;

/// 호출마다 설정을 다시 읽는다 (hosts.toml 을 고치면 재시작 없이 반영)
struct Ctx {
    cfg: Config,
    deny: Vec<Regex>,
}

impl Ctx {
    fn load() -> Result<(Ctx, Option<String>), String> {
        let loaded = config::load();
        let patterns = loaded
            .config
            .policy
            .deny
            .clone()
            .unwrap_or_else(|| safety::DEFAULT_DENY.iter().map(|s| s.to_string()).collect());
        let deny = safety::compile_deny(&patterns)?;
        Ok((Ctx { cfg: loaded.config, deny }, loaded.warning))
    }

    fn host(&self, name: &str) -> Result<&HostCfg, String> {
        self.cfg
            .hosts
            .iter()
            .find(|h| h.name == name)
            .ok_or_else(|| format!("알 수 없는 host: {name} (hosts_list 로 등록 목록을 확인하라)"))
    }
}

fn ok_text(s: String) -> CallToolResult {
    CallToolResult::success(vec![Content::text(s)])
}

fn err_text(s: String) -> CallToolResult {
    CallToolResult::error(vec![Content::text(s)])
}

fn pretty(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

/// ssh 호스트 키 확인 실패면 응답 끝에 안내 한 줄을 붙인다
fn with_hint(text: String, host: &config::HostCfg, stderr: &str) -> String {
    match exec::host_key_hint(host, stderr) {
        Some(h) => format!("{text}\n{h}"),
        None => text,
    }
}

fn non_empty(s: Option<String>) -> Option<String> {
    s.filter(|x| !x.trim().is_empty())
}

fn timeout(v: Option<u64>, default: u64, max: u64) -> Duration {
    Duration::from_secs(v.unwrap_or(default).clamp(1, max))
}

pub(crate) fn log_audit(tool: &str, host: &str, text: &str, exit_code: Option<i32>, duration_ms: u128, denied: bool) {
    audit::record(
        &util::base_dir(),
        audit::Entry { tool, host, text, exit_code, duration_ms, denied },
    );
}

fn format_cmd(host: &str, cap: &Captured) -> String {
    let code = cap.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "없음".into());
    format!(
        "[host: {host}]\nexit_code: {code}\nduration_ms: {}\n--- stdout ---\n{}\n--- stderr ---\n{}",
        cap.duration_ms,
        util::truncate(&safety::redact(&cap.stdout)),
        util::truncate(&safety::redact(&cap.stderr)),
    )
}

/// 반환: (본문, 오류 여부)
fn format_claude(host: &str, cap: &Captured) -> (String, bool) {
    let out = safety::redact(&cap.stdout);
    match claude::parse_result(&out) {
        Some(v) => {
            let mut s = claude::summarize(&v);
            s["host"] = json!(host);
            s["exit_code"] = json!(cap.exit_code);
            s["duration_ms"] = json!(cap.duration_ms);
            let is_err = s["is_error"].as_bool().unwrap_or(false) || cap.exit_code != Some(0);
            (pretty(&s), is_err)
        }
        None => {
            let code = cap.exit_code.map(|c| c.to_string()).unwrap_or_else(|| "없음".into());
            (
                format!(
                    "[JSON 해석 실패, 원문 반환]\nhost: {host}\nexit_code: {code}\n--- stdout ---\n{}\n--- stderr ---\n{}",
                    util::truncate(&out),
                    util::truncate(&safety::redact(&cap.stderr)),
                ),
                true,
            )
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct RunCommandParams {
    /// 실행할 호스트 이름 (hosts_list 로 확인)
    pub host: String,
    /// 실행할 셸 명령
    pub command: String,
    /// 작업 폴더. 비우면 호스트의 workdir, 그것도 없으면 사용자 홈
    pub cwd: Option<String>,
    /// 제한 시간(초). 기본 120, 최대 1800
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct HostOnlyParams {
    /// 호스트 이름 (hosts_list 로 확인)
    pub host: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClaudyUpdateParams {
    /// 갱신할 Windows 호스트 이름(원격 셸이 powershell 인 ssh 호스트)
    pub host: String,
    /// 이 PC 에 있는 새 서버 실행 파일(claudy.exe) 경로
    pub server_file: Option<String>,
    /// 이 PC 에 있는 새 manifest.json 경로
    pub manifest_file: Option<String>,
    /// 이 PC 에 있는 새 설정 앱(ClaudyManager.exe) 경로
    pub manager_file: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct FileCopyParams {
    /// 보낼 쪽 호스트 이름 (hosts_list 로 확인)
    pub src_host: String,
    /// 보낼 파일·폴더 경로. Windows 원격은 C:/Users/... 처럼 쓴다
    pub src_path: String,
    /// 받을 쪽 호스트 이름
    pub dst_host: String,
    /// 받을 경로. 폴더로 끝나면(/ 또는 \\) 그 안에 같은 이름으로 둔다
    pub dst_path: String,
    /// 폴더를 통째로 복사한다
    pub recursive: Option<bool>,
    /// 제한 시간(초). 기본 600, 최대 3600
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct AppOpenParams {
    /// 실행할 호스트 이름 (hosts_list 로 확인)
    pub host: String,
    /// 프로그램. 실행 파일 전체 경로, PATH 의 이름(notepad), Windows 앱 경로 이름(Hwp), macOS 앱 이름(Final Cut Pro)
    pub program: String,
    /// 프로그램에 넘길 인자(예: 열 파일 경로)
    pub args: Option<Vec<String>>,
    /// 떠오르기를 기다릴 시간(초). 기본 15, 최대 60
    pub wait_sec: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct BrowserRunParams {
    /// 실행할 호스트 이름 (hosts_list 로 확인)
    pub host: String,
    /// 브라우저. chrome, edge, aside 중 하나
    pub browser: String,
    /// async 함수 본문(JavaScript). page·context·browser(Playwright 객체)를 쓸 수 있고 return 한 값이 결과로 나온다. 예 await page.goto("https://example.com"); return await page.title();
    pub script: Option<String>,
    /// true 면 스크립트를 실행하지 않고 사용 안내를 돌려준다
    pub guide: Option<bool>,
    /// 제한 시간(초). 기본 90, 최대 900
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct EgoRunParams {
    /// 실행할 호스트 이름. ego-browser 가 설치된 호스트
    pub host: String,
    /// ego-browser 의 Node 스크립트(ESM). 예 const t = await taskSpace("이름"); await t.page("p1").goto("https://example.com"); console.log(await t.page("p1").title());
    pub script: Option<String>,
    /// true 면 스크립트를 실행하지 않고 ego-browser 사용 안내(API 목록)를 돌려준다. 처음 쓸 때 한 번 본다
    pub guide: Option<bool>,
    /// 제한 시간(초). 기본 60, 최대 600
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct ClaudeAskParams {
    /// 실행할 호스트 이름 (hosts_list 로 확인)
    pub host: String,
    /// Claude Code 에 보낼 프롬프트
    pub prompt: String,
    /// 이어서 대화할 때만 넣는다. 값은 직전 결과의 session_id 값이다
    #[serde(alias = "session_id")]
    pub resume_id: Option<String>,
    /// 모델 이름 (예: claude-opus-4). 비우면 Claude Code 기본값
    pub model: Option<String>,
    /// 작업 폴더. 비우면 호스트의 workdir, 그것도 없으면 사용자 홈
    pub cwd: Option<String>,
    /// 제한 시간(초). 기본 600, 최대 1800
    pub timeout_sec: Option<u64>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct JobStartParams {
    /// 실행할 호스트 이름
    pub host: String,
    /// 작업 종류: command(셸 명령) 또는 claude(Claude Code)
    pub kind: String,
    /// kind 가 command 일 때 실행할 셸 명령
    pub command: Option<String>,
    /// kind 가 claude 일 때 보낼 프롬프트
    pub prompt: Option<String>,
    /// kind 가 claude 일 때만 넣는다. 이어서 대화하려면 직전 결과의 session_id 값을 넣는다
    #[serde(alias = "session_id")]
    pub resume_id: Option<String>,
    /// kind 가 claude 일 때 모델 이름
    pub model: Option<String>,
    /// 작업 폴더
    pub cwd: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
pub struct JobStatusParams {
    /// job_start 가 돌려준 job_id
    pub job_id: String,
    /// 로그 끝에서 몇 줄을 보여줄지. 기본 60, 최대 2000
    pub tail_lines: Option<usize>,
}

#[derive(Debug, Clone)]
struct Claudy {
    tool_router: ToolRouter<Self>,
}

#[tool_router(router = base_router)]
impl Claudy {
    fn new() -> Self {
        Self { tool_router: Self::base_router() }
    }

    #[tool(description = "등록된 호스트 목록(name, kind, 대상 주소, claude, workdir)과 설정 파일 경로, 경고를 보여준다.")]
    async fn hosts_list(&self) -> CallToolResult {
        let loaded = config::load();
        let hosts: Vec<Value> = loaded
            .config
            .hosts
            .iter()
            .map(|h| {
                json!({
                    "name": h.name,
                    "kind": h.kind,
                    "target": h.target_label(),
                    "claude": h.claude_bin(),
                    "workdir": h.workdir().unwrap_or(""),
                    "claude_permissions": h.permission_mode(),
                })
            })
            .collect();
        ok_text(pretty(&json!({
            "config_path": loaded.path.to_string_lossy(),
            "warning": loaded.warning,
            "hosts": hosts,
        })))
    }

    #[tool(description = "등록된 호스트에서 셸 명령을 실행하고 표준출력, 표준오류, 종료 코드, 소요 시간을 돌려준다. this-pc 는 이 PC, ssh 호스트는 원격 서버에서 실행한다. 차단 목록에 걸리는 명령은 실행하지 않는다.")]
    async fn run_command(&self, Parameters(p): Parameters<RunCommandParams>) -> CallToolResult {
        let (ctx, _) = match Ctx::load() {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let host = match ctx.host(&p.host) {
            Ok(h) => h,
            Err(e) => return err_text(e),
        };
        if let Some(reason) = safety::check_deny(&ctx.deny, &p.command) {
            log_audit("run_command", &p.host, &p.command, None, 0, true);
            return err_text(reason);
        }
        let cwd = non_empty(p.cwd.clone()).or_else(|| host.workdir().map(str::to_string));
        let plan = match exec::command_plan(host, cwd, &p.command) {
            Ok(pl) => pl,
            Err(e) => return err_text(e),
        };
        let limit = timeout(p.timeout_sec, 120, 1800);
        match exec::run(&plan, limit).await {
            Ok(cap) => {
                log_audit("run_command", &p.host, &p.command, cap.exit_code, cap.duration_ms, false);
                if cap.timed_out {
                    err_text(format!("제한 시간({}초)을 넘어 중단했다", limit.as_secs()))
                } else {
                    ok_text(with_hint(format_cmd(&p.host, &cap), host, &cap.stderr))
                }
            }
            Err(e) => {
                log_audit("run_command", &p.host, &p.command, None, 0, false);
                err_text(e)
            }
        }
    }

    #[tool(description = "호스트의 Claudy 상태를 본다: 서버 판(파일·실행 중), 설정 앱 판과 실행 여부, 역방향 터널 설정과 최근 기록(tunnel.log), 최근 갱신 기록(update.log). 연결·터널·갱신 문제를 원격에서 진단할 때 먼저 부른다.")]
    async fn host_status(&self, Parameters(p): Parameters<HostOnlyParams>) -> CallToolResult {
        let (ctx, _) = match Ctx::load() {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let host = match ctx.host(&p.host) {
            Ok(h) => h.clone(),
            Err(e) => return err_text(e),
        };
        let windows = host.remote_windows() || (host.kind != "ssh" && cfg!(windows));
        let script = if windows { STATUS_PS } else { STATUS_SH };
        match run_simple(&host, script, 60).await {
            Ok(out) => {
                log_audit("host_status", &p.host, "상태 조회", Some(0), 0, false);
                ok_text(format!("[{}]\n{}", p.host, out.trim()))
            }
            Err(e) => err_text(format!("[{}] 상태를 읽지 못했다: {e}", p.host)),
        }
    }

    #[tool(description = "호스트의 사용자 화면을 찍어 그림으로 돌려준다(가로 최대 1600). 막힌 대화 상자(예: 한글 「접근 허용」 창)나 프로그램 상태를 원격에서 확인할 때 쓴다. Windows 원격은 화면 세션 실행(desktop = true)인 호스트만 된다. 사진 파일은 이 PC 의 ~/.claudy/captures 에 남는다.")]
    async fn screen_capture(&self, Parameters(p): Parameters<HostOnlyParams>) -> CallToolResult {
        let (ctx, _) = match Ctx::load() {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let host = match ctx.host(&p.host) {
            Ok(h) => h.clone(),
            Err(e) => return err_text(e),
        };
        let this_pc = match ctx.cfg.hosts.iter().find(|h| h.kind != "ssh") {
            Some(h) => h.clone(),
            None => return err_text("이 PC 호스트가 없다".into()),
        };
        let windows = host.remote_windows() || (host.kind != "ssh" && cfg!(windows));
        if host.remote_windows() && !host.desktop_session() {
            return err_text("이 Windows 호스트는 화면 세션 실행(desktop = true)이 꺼져 있어 화면을 찍을 수 없다".into());
        }
        let sh_capture = format!("{LINUX_GUI_ENV}{CAPTURE_SH}");
        let out = match run_simple(&host, if windows { CAPTURE_PS } else { &sh_capture }, 60).await {
            Ok(o) => o,
            Err(e) => return err_text(format!("화면을 찍지 못했다: {e}")),
        };
        let Some(remote) = out.lines().find_map(|l| l.trim().strip_prefix("saved ").map(str::to_string)) else {
            return err_text(format!("화면을 찍지 못했다: {}", out.trim()));
        };
        let dir = util::base_dir().join("captures");
        let _ = std::fs::create_dir_all(&dir);
        let stamp = util::local_now().format("%Y%m%d-%H%M%S").to_string();
        let local = dir.join(format!("{}-{stamp}.jpg", p.host.replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "_")));
        if host.kind == "ssh" {
            if let Err(e) = file_copy_run(&host, &remote, &this_pc, &local.to_string_lossy(), false, Duration::from_secs(120)).await {
                return err_text(format!("사진을 가져오지 못했다: {e}"));
            }
        } else if let Err(e) = std::fs::copy(&remote, &local) {
            return err_text(format!("사진을 옮기지 못했다: {e}"));
        }
        let bytes = match std::fs::read(&local) {
            Ok(b) => b,
            Err(e) => return err_text(format!("사진을 읽지 못했다: {e}")),
        };
        log_audit("screen_capture", &p.host, &local.to_string_lossy(), Some(0), 0, false);
        CallToolResult::success(vec![
            Content::text(format!("{} 화면 ({}) {}", p.host, human_size(bytes.len() as u64), local.display())),
            Content::image(base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes), "image/jpeg"),
        ])
    }

    #[tool(description = "Windows 호스트의 Claudy 를 새 판으로 갱신한다. 이 PC 의 새 파일(서버 claudy.exe, manifest.json, 설정 앱 ClaudyManager.exe 중 필요한 것)을 그 호스트의 사용자 폴더 Claudy\\_upd 로 보내고 갱신기를 따로 띄운다. 갱신기는 서버 교체(Claude 앱을 다시 켜면 적용), 설정 앱 교체 뒤 터널이 다시 붙는지 확인하고, 안 붙으면 이전 설정 앱으로 되돌리고 그래도 안 되면 비상 터널을 띄운다. 터널을 거쳐 접속 중이면 잠시 끊겼다가 다시 이어진다. 결과는 update.log 의 RESULT 줄로 확인한다.")]
    async fn claudy_update(&self, Parameters(p): Parameters<ClaudyUpdateParams>) -> CallToolResult {
        let (ctx, _) = match Ctx::load() {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let host = match ctx.host(&p.host) {
            Ok(h) => h.clone(),
            Err(e) => return err_text(e),
        };
        if !host.remote_windows() {
            return err_text("Windows 원격 호스트(shell = powershell 인 ssh 호스트)만 갱신한다. macOS 는 .mcpb 설치 창으로 갱신한다".into());
        }
        let this_pc = match ctx.cfg.hosts.iter().find(|h| h.kind != "ssh") {
            Some(h) => h.clone(),
            None => return err_text("이 PC 호스트가 없다".into()),
        };
        let files: Vec<(&str, String)> = [("claudy.exe", &p.server_file), ("manifest.json", &p.manifest_file), ("ClaudyManager.exe", &p.manager_file)]
            .into_iter()
            .filter_map(|(n, v)| v.as_deref().filter(|s| !s.trim().is_empty()).map(|s| (n, exec::expand_tilde(s.trim()))))
            .collect();
        if files.is_empty() {
            return err_text("server_file, manifest_file, manager_file 중 하나는 있어야 한다".into());
        }
        for (_, f) in &files {
            if !std::path::Path::new(f).is_file() {
                return err_text(format!("이 PC 에 파일이 없다: {f}"));
            }
        }
        let mut report = Vec::new();
        // 1) 받을 폴더 만들기
        let prep = "New-Item -ItemType Directory -Force (Join-Path $env:USERPROFILE 'Claudy\\_upd') | Out-Null; 'ready'";
        if let Err(e) = run_simple(&host, prep, 60).await {
            return err_text(format!("받을 폴더를 만들지 못했다: {e}"));
        }
        // 2) 파일 보내기
        for (name, f) in &files {
            match file_copy_run(&this_pc, f, &host, &format!("Claudy/_upd/{name}"), false, Duration::from_secs(900)).await {
                Ok(size) => report.push(format!("보냄: {name} ({size})")),
                Err(e) => return err_text(format!("{name} 를 보내지 못했다: {e}")),
            }
        }
        // 3) 갱신기를 따로 띄운다(설정 앱을 바꾸면 터널이 잠시 끊기므로 이 연결과 떨어져 돌아야 한다)
        let script = include_str!("../res/win_update.ps1");
        let start = format!(
            r#"$f=Join-Path $env:USERPROFILE 'Claudy\_upd_run.ps1'; [IO.File]::WriteAllText($f, [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('{}')), (New-Object Text.UTF8Encoding $true)); $t=Get-Date -Format 'yyyy-MM-dd HH:mm:ss'; Start-Process powershell.exe -ArgumentList ('-NoProfile -ExecutionPolicy Bypass -WindowStyle Hidden -File "' + $f + '"') -WindowStyle Hidden; "started $t""#,
            util::b64(script)
        );
        match run_simple(&host, &start, 60).await {
            Ok(out) => report.push(format!("갱신기 시작: {}", out.lines().find_map(|l| l.trim().strip_prefix("started ")).unwrap_or(""))),
            // 갱신기가 설정 앱을 끄면서 터널이 끊겨 응답을 못 받을 수 있다. 결과는 update.log 로 확인한다
            Err(e) if e.contains("255") || e.contains("closed") => report.push("갱신기 시작(응답 전에 연결이 끊김, 기록으로 확인)".into()),
            Err(e) => return err_text(format!("갱신기를 띄우지 못했다: {e}")),
        }
        // 4) 결과 기다리기. 터널이 다시 붙을 때까지 연결이 실패할 수 있어 여러 번 시도한다
        let read = "Get-Content (Join-Path $env:USERPROFILE 'Claudy\\update.log') -Encoding UTF8 -Tail 30";
        let deadline = std::time::Instant::now() + Duration::from_secs(240);
        tokio::time::sleep(Duration::from_secs(15)).await;
        let mut last = String::new();
        while std::time::Instant::now() < deadline {
            if let Ok(out) = run_simple(&host, read, 40).await {
                // 마지막 「갱신 시작」 이후만 본다
                let all: Vec<&str> = out.lines().collect();
                let from = all.iter().rposition(|l| l.contains("갱신 시작")).unwrap_or(all.len());
                let lines: Vec<&str> = all[from..].to_vec();
                last = lines.join("\n");
                if lines.iter().any(|l| l.contains("RESULT ")) {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_secs(10)).await;
        }
        log_audit("claudy_update", &p.host, &report.join(" / "), Some(0), 0, false);
        if last.contains("RESULT ") {
            ok_text(format!("{}\n--- update.log ---\n{last}", report.join("\n")))
        } else {
            err_text(format!("{}\n갱신 결과를 4분 안에 확인하지 못했다. 나중에 update.log 를 본다\n{last}", report.join("\n")))
        }
    }

    #[tool(description = "호스트 사이에서 파일이나 폴더를 복사한다. 이 PC ↔ ssh 호스트, ssh 호스트 ↔ ssh 호스트(이 PC 를 거쳐) 모두 된다. 경유 호스트·Windows 원격도 그대로 쓴다. 파일을 옮기려고 run_command 로 scp 를 직접 짜지 말고 이 도구를 쓴다.")]
    async fn file_copy(&self, Parameters(p): Parameters<FileCopyParams>) -> CallToolResult {
        let (ctx, _) = match Ctx::load() {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let (src, dst) = match (ctx.host(&p.src_host), ctx.host(&p.dst_host)) {
            (Ok(a), Ok(b)) => (a.clone(), b.clone()),
            (Err(e), _) | (_, Err(e)) => return err_text(e),
        };
        let rec = p.recursive.unwrap_or(false);
        let limit = timeout(p.timeout_sec, 600, 3600);
        let started = std::time::Instant::now();
        let note = format!("{}:{} → {}:{}", p.src_host, p.src_path, p.dst_host, p.dst_path);
        let r = file_copy_run(&src, &p.src_path, &dst, &p.dst_path, rec, limit).await;
        let ms = started.elapsed().as_millis();
        log_audit("file_copy", &p.src_host, &note, Some(if r.is_ok() { 0 } else { 1 }), ms, false);
        match r {
            Ok(size) => ok_text(format!("복사함: {note}\n크기: {size}\n걸린 시간: {:.1}초", ms as f64 / 1000.0)),
            Err(e) => err_text(e),
        }
    }

    #[tool(description = "호스트의 사용자 화면에 프로그램을 띄운다(한글, 메모장, Final Cut Pro 등 창이 있는 프로그램). run_command 로 띄운 창은 원격(ssh) 호스트에서 화면에 보이지 않으므로 이 도구를 쓴다. Windows 원격은 로그온한 사용자 화면에서 실행하는 예약 작업을 잠깐 만들어 실행하고 지운다. macOS 는 open -a 를 쓴다. 띄운 뒤에는 기다리지 않고 프로세스 번호만 돌려준다.")]
    async fn app_open(&self, Parameters(p): Parameters<AppOpenParams>) -> CallToolResult {
        let (ctx, _) = match Ctx::load() {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let host = match ctx.host(&p.host) {
            Ok(h) => h,
            Err(e) => return err_text(e),
        };
        if p.program.trim().is_empty() {
            return err_text("program 이 비어 있다".into());
        }
        let wait = p.wait_sec.unwrap_or(15).clamp(1, 60);
        let args = p.args.clone().unwrap_or_default();
        let windows = host.remote_windows() || (host.kind != "ssh" && cfg!(windows));
        let body = if windows { app_open_ps(&p.program, &args, wait, host.kind == "ssh") } else { format!("{LINUX_GUI_ENV}{}", app_open_sh(&p.program, &args)) };
        if let Some(reason) = safety::check_deny(&ctx.deny, &body) {
            log_audit("app_open", &p.host, &p.program, None, 0, true);
            return err_text(reason);
        }
        let plan = match exec::command_plan(host, None, &body) {
            Ok(pl) => pl,
            Err(e) => return err_text(e),
        };
        match exec::run(&plan, Duration::from_secs(wait + 30)).await {
            Ok(cap) => {
                log_audit("app_open", &p.host, &p.program, cap.exit_code, cap.duration_ms, false);
                ok_text(format_cmd(&p.host, &cap))
            }
            Err(e) => err_text(e),
        }
    }

    #[tool(description = "호스트의 Chrome·Edge·Aside 를 조작용 프로필로 띄우고(이미 떠 있으면 그대로) Playwright(CDP)로 조작한다. 평소 쓰는 프로필과 따로 쓰며, 로그인은 그 프로필에 설치한 비밀번호 관리 확장에 맡긴다. script 는 async 함수 본문으로 page·context·browser 를 쓰고 return 값이 결과다. 처음 쓰면 Node.js 와 playwright-core 를 사용자 폴더에 설치한다. 사용법은 guide=true.")]
    async fn browser_run(&self, Parameters(p): Parameters<BrowserRunParams>) -> CallToolResult {
        if p.guide.unwrap_or(false) {
            return ok_text(BROWSER_GUIDE.into());
        }
        let name = p.browser.trim().to_lowercase();
        if !["chrome", "edge", "aside"].contains(&name.as_str()) {
            return err_text("browser 는 chrome, edge, aside 중 하나다".into());
        }
        let script = p.script.clone().unwrap_or_default();
        if script.trim().is_empty() {
            return err_text("script 를 적거나 guide=true 로 사용 안내를 받는다".into());
        }
        let (ctx, _) = match Ctx::load() {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let host = match ctx.host(&p.host) {
            Ok(h) => h.clone(),
            Err(e) => return err_text(e),
        };
        let windows = host.remote_windows() || (host.kind != "ssh" && cfg!(windows));
        if host.remote_windows() && !host.desktop_session() {
            return err_text("Windows 원격 호스트는 화면 세션 실행(desktop = true)을 켜야 브라우저 창이 사용자 화면에 뜬다".into());
        }
        let job = format!("export default async function({{ page, context, browser }}) {{\n{script}\n}}\n");
        let id = uuid::Uuid::new_v4().simple().to_string()[..10].to_string();
        let body = if windows { browser_ps(&name, &job, &id) } else { format!("{LINUX_GUI_ENV}{}", browser_sh(&name, &job, &id)) };
        let plan = match exec::command_plan(&host, None, &body) {
            Ok(pl) => pl,
            Err(e) => return err_text(e),
        };
        let limit = timeout(p.timeout_sec, 90, 900);
        let label: String = format!("{name}: {}", script.chars().take(180).collect::<String>());
        match exec::run(&plan, limit).await {
            Ok(cap) => {
                log_audit("browser_run", &p.host, &label, cap.exit_code, cap.duration_ms, false);
                if cap.timed_out {
                    err_text(format!("제한 시간({}초)을 넘어 중단했다", limit.as_secs()))
                } else {
                    ok_text(format_cmd(&p.host, &cap))
                }
            }
            Err(e) => {
                log_audit("browser_run", &p.host, &label, None, 0, false);
                err_text(e)
            }
        }
    }

    #[tool(description = "호스트의 ego 브라우저를 Node 스크립트로 직접 조작한다(ego-browser nodejs). 페이지 열기, 클릭, 입력, 글 읽기, 화면 저장을 수 초 안에 처리하며 Claude Code 를 띄우지 않는다. 브라우저 작업은 claude_ask 대신 이 도구를 쓴다. API 는 guide=true 로 한 번 확인한다. 작업 공간은 taskSpace(이름)로 만들고 같은 이름으로 다시 쓰며, 끝나면 finish({ keep: [] }) 로 닫는다.")]
    async fn ego_run(&self, Parameters(p): Parameters<EgoRunParams>) -> CallToolResult {
        let (ctx, _) = match Ctx::load() {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let host = match ctx.host(&p.host) {
            Ok(h) => h,
            Err(e) => return err_text(e),
        };
        let guide = p.guide.unwrap_or(false);
        let script = p.script.clone().unwrap_or_default();
        if !guide && script.trim().is_empty() {
            return err_text("script 를 적거나 guide=true 로 사용 안내를 받는다".into());
        }
        let plan = if host.kind == "ssh" {
            let body = if guide {
                "ego-browser skill".to_string()
            } else if host.remote_windows() {
                exec::ps_pipe_b64(&script, "ego-browser nodejs")
            } else {
                format!("echo {} | base64 -d | ego-browser nodejs", util::b64(&script))
            };
            exec::command_plan(host, None, &body)
        } else {
            Ok(exec::Spawnable {
                program: claude::resolve_local_bin("ego-browser"),
                args: if guide { vec!["skill".into()] } else { vec!["nodejs".into()] },
                stdin: if guide { None } else { Some(script.clone()) },
                cwd: Some(exec::home_string()),
                cmd_fallback: true,
            })
        };
        let plan = match plan {
            Ok(pl) => pl,
            Err(e) => return err_text(e),
        };
        let label = if guide { "ego-browser skill".to_string() } else { script.chars().take(200).collect() };
        let limit = timeout(p.timeout_sec, 60, 600);
        match exec::run(&plan, limit).await {
            Ok(cap) => {
                log_audit("ego_run", &p.host, &label, cap.exit_code, cap.duration_ms, false);
                if cap.timed_out {
                    err_text(format!("제한 시간({}초)을 넘어 중단했다", limit.as_secs()))
                } else {
                    let text = format_cmd(&p.host, &cap);
                    ok_text(text)
                }
            }
            Err(e) => {
                log_audit("ego_run", &p.host, &label, None, 0, false);
                err_text(e)
            }
        }
    }

    #[tool(description = "호스트의 Claude Code(claude -p)에 프롬프트를 보내고 결과(result, session_id, total_cost_usd, is_error, num_turns)를 돌려준다. 호출마다 Claude Code 를 새로 띄워 수십 초와 별도 토큰이 든다. 셸 명령으로 되는 일은 run_command, 브라우저 작업은 ego_run 을 쓴다. 이어서 대화하려면 직전 결과의 session_id 값을 resume_id 인자에 넣는다.")]
    async fn claude_ask(&self, Parameters(p): Parameters<ClaudeAskParams>) -> CallToolResult {
        let (ctx, _) = match Ctx::load() {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let host = match ctx.host(&p.host) {
            Ok(h) => h,
            Err(e) => return err_text(e),
        };
        let cwd = non_empty(p.cwd.clone()).or_else(|| host.workdir().map(str::to_string));
        let plan = match claude::claude_plan(host, &p.prompt, p.resume_id.as_deref(), p.model.as_deref(), cwd) {
            Ok(pl) => pl,
            Err(e) => return err_text(e),
        };
        let limit = timeout(p.timeout_sec, 600, 1800);
        match exec::run(&plan, limit).await {
            Ok(cap) if cap.timed_out => {
                log_audit("claude_ask", &p.host, &p.prompt, None, cap.duration_ms, false);
                err_text(format!("제한 시간({}초)을 넘어 중단했다", limit.as_secs()))
            }
            Ok(cap) => {
                log_audit("claude_ask", &p.host, &p.prompt, cap.exit_code, cap.duration_ms, false);
                let (text, is_err) = format_claude(&p.host, &cap);
                let text = with_hint(text, host, &cap.stderr);
                if is_err { err_text(text) } else { ok_text(text) }
            }
            Err(e) => {
                log_audit("claude_ask", &p.host, &p.prompt, None, 0, false);
                err_text(e)
            }
        }
    }

    #[tool(description = "명령(command) 또는 Claude Code 작업(claude)을 백그라운드로 띄우고 job_id 를 돌려준다. 출력은 ~/.claudy/jobs/<job_id>.log 에 쌓이고 상태는 <job_id>.json 에 남는다. 상태는 job_status 로 확인한다.")]
    async fn job_start(&self, Parameters(p): Parameters<JobStartParams>) -> CallToolResult {
        let (ctx, _) = match Ctx::load() {
            Ok(v) => v,
            Err(e) => return err_text(e),
        };
        let host = match ctx.host(&p.host) {
            Ok(h) => h,
            Err(e) => return err_text(e),
        };
        let cwd = non_empty(p.cwd.clone()).or_else(|| host.workdir().map(str::to_string));
        let (plan, text) = match p.kind.as_str() {
            "command" => {
                let Some(cmd) = non_empty(p.command.clone()) else {
                    return err_text("kind 가 command 이면 command 가 필요하다".into());
                };
                if let Some(reason) = safety::check_deny(&ctx.deny, &cmd) {
                    log_audit("job_start", &p.host, &cmd, None, 0, true);
                    return err_text(reason);
                }
                match exec::command_plan(host, cwd, &cmd) {
                    Ok(pl) => (pl, cmd),
                    Err(e) => return err_text(e),
                }
            }
            "claude" => {
                let Some(prompt) = non_empty(p.prompt.clone()) else {
                    return err_text("kind 가 claude 이면 prompt 가 필요하다".into());
                };
                match claude::claude_plan(host, &prompt, p.resume_id.as_deref(), p.model.as_deref(), cwd) {
                    Ok(pl) => (pl, prompt),
                    Err(e) => return err_text(e),
                }
            }
            other => return err_text(format!("kind 는 command 또는 claude 여야 한다 (받은 값: {other})")),
        };
        let id = jobs::new_id();
        let base = util::base_dir();
        if let Err(e) = jobs::start(&base, &id, &p.host, &p.kind, plan) {
            return err_text(e);
        }
        log_audit("job_start", &p.host, &text, None, 0, false);
        ok_text(pretty(&json!({
            "job_id": id,
            "host": p.host,
            "kind": p.kind,
            "status_file": jobs::status_path(&base, &id).to_string_lossy(),
            "log_file": jobs::log_path(&base, &id).to_string_lossy(),
        })))
    }

    #[tool(description = "백그라운드 작업의 상태(끝났는지, 종료 코드)와 로그 끝부분을 보여준다. claude 작업이 끝났으면 결과 JSON 해석값을 함께 준다.")]
    async fn job_status(&self, Parameters(p): Parameters<JobStatusParams>) -> CallToolResult {
        let base = util::base_dir();
        let status = match jobs::read_status(&base, &p.job_id) {
            Ok(s) => s,
            Err(e) => return err_text(e),
        };
        let n = p.tail_lines.unwrap_or(60).clamp(1, 2000);
        let log_file = jobs::log_path(&base, &p.job_id);
        let tail = jobs::tail(&log_file, n);
        let mut out = json!({ "status": status, "log_tail": util::truncate(&safety::redact(&tail)) });
        if status["finished"].as_bool() == Some(true) && status["kind"] == "claude" {
            if let Ok(full) = std::fs::read_to_string(&log_file) {
                if let Some(v) = claude::parse_result(&full) {
                    out["claude"] = claude::summarize(&v);
                }
            }
        }
        ok_text(pretty(&out))
    }

    #[tool(description = "최근 백그라운드 작업 20개를 시작 시각 역순으로 보여준다.")]
    async fn job_list(&self) -> CallToolResult {
        ok_text(pretty(&Value::Array(jobs::list(&util::base_dir(), 20))))
    }

}

const STATUS_PS: &str = r#"$d=Join-Path $env:USERPROFILE 'Claudy'
$v={ param($f) try { ([IO.File]::ReadAllText($f) | ConvertFrom-Json).version } catch { '없음' } }
$ext=Get-ChildItem (Join-Path $env:APPDATA 'Claude\Claude Extensions') -Directory -ErrorAction SilentlyContinue | Where-Object Name -match 'claudy' | Select-Object -First 1
$sf = if (Test-Path (Join-Path $d 'manifest.json')) { & $v (Join-Path $d 'manifest.json') } elseif ($ext) { & $v (Join-Path $ext.FullName 'manifest.json') } else { '없음' }
"서버 파일 판: $sf"
"서버 실행: " + ((Get-Process claudy -ErrorAction SilentlyContinue | ForEach-Object { "pid $($_.Id) 시작 $($_.StartTime.ToString('MM-dd HH:mm'))" }) -join ', ')
$m=Join-Path $d 'ClaudyManager.exe'
"설정 앱 파일 판: " + $(if (Test-Path $m) { (Get-Item $m).VersionInfo.FileVersion } else { '없음' })
"설정 앱 실행: " + ((Get-Process ClaudyManager -ErrorAction SilentlyContinue | ForEach-Object { "pid $($_.Id) 세션 $($_.SessionId)" }) -join ', ')
$mt=Join-Path $env:USERPROFILE '.claudy\manager.toml'
if (Test-Path $mt) { "터널 설정: " + ((Get-Content $mt -Encoding UTF8 | Where-Object { $_ -match '^tunnel_(enabled|via|remote_port)' }) -join ' / ') }
"터널 ssh: " + ((Get-CimInstance Win32_Process | Where-Object { $_.Name -eq 'ssh.exe' -and $_.CommandLine -like '*-R 127.0.0.1:*' } | ForEach-Object { "pid $($_.ProcessId)" }) -join ', ')
"--- tunnel.log"; Get-Content (Join-Path $env:USERPROFILE '.claudy\tunnel.log') -Encoding UTF8 -Tail 5 -ErrorAction SilentlyContinue
"--- update.log"; Get-Content (Join-Path $d 'update.log') -Encoding UTF8 -Tail 4 -ErrorAction SilentlyContinue"#;

const STATUS_SH: &str = r#"if [ "$(uname)" = Darwin ]; then
  m=$(ls -d "$HOME/Library/Application Support/Claude/Claude Extensions/"*claudy*/manifest.json 2>/dev/null | head -1)
  echo "서버 파일 판: $( [ -n "$m" ] && grep -m1 '"version"' "$m" | cut -d'"' -f4 || echo 없음)"
  echo "서버 실행: $(pgrep -x claudy | tr '
' ' ')"
  a="$HOME/Applications/Claudy.app/Contents/Info.plist"
  echo "설정 앱 파일 판: $( [ -f "$a" ] && defaults read "$a" CFBundleShortVersionString 2>/dev/null || echo 없음)"
  echo "설정 앱 실행: $(pgrep -x Claudy | tr '
' ' ')"
else
  d="$HOME/.local/share/claudy"
  echo "운영체제: $(uname -sr)"
  echo "서버 파일 판: $( [ -x "$d/claudy" ] && "$d/claudy" --version 2>/dev/null | cut -d' ' -f2 || echo 없음)"
  echo "서버 실행: $(pgrep -x claudy | tr '
' ' ')"
  echo "설정 앱 파일 판: $(cat "$d/MANAGER_VERSION" 2>/dev/null || echo 없음)"
  echo "설정 앱 실행: $(pgrep -x ClaudyManager | tr '
' ' ')"
  echo "Claude Code: $(claude --version 2>/dev/null || echo 없음)"
fi
t="$HOME/.claudy/manager.toml"; [ -f "$t" ] && echo "터널 설정: $(grep -E '^tunnel_(enabled|via|remote_port)' "$t" | tr '
' ' ')"
echo "--- tunnel.log"; tail -n 5 "$HOME/.claudy/tunnel.log" 2>/dev/null; true"#;

const CAPTURE_PS: &str = r#"Add-Type -AssemblyName System.Windows.Forms,System.Drawing
Add-Type -Namespace W -Name U -MemberDefinition '[DllImport("user32.dll")] public static extern bool SetProcessDPIAware();'
[void][W.U]::SetProcessDPIAware()
$b=[System.Windows.Forms.SystemInformation]::VirtualScreen
$bmp=New-Object System.Drawing.Bitmap $b.Width,$b.Height
$g=[System.Drawing.Graphics]::FromImage($bmp); $g.CopyFromScreen($b.Left,$b.Top,0,0,$bmp.Size); $g.Dispose()
$w=[Math]::Min(1600,$b.Width); $h=[int]($b.Height*$w/$b.Width)
$s=New-Object System.Drawing.Bitmap $w,$h
$g2=[System.Drawing.Graphics]::FromImage($s); $g2.InterpolationMode='HighQualityBicubic'; $g2.DrawImage($bmp,0,0,$w,$h); $g2.Dispose()
$f=Join-Path $env:TEMP 'claudy-capture.jpg'; $s.Save($f,[System.Drawing.Imaging.ImageFormat]::Jpeg); $s.Dispose(); $bmp.Dispose()
if (-not (Test-Path $f) -or (Get-Item $f).Length -eq 0) { 'capture failed'; exit 3 }
"saved " + ($f -replace '\\','/')"#;

const BROWSER_RUNNER: &str = include_str!("../res/browser_runner.mjs");
const NODE_VER: &str = "v22.12.0";

const BROWSER_GUIDE: &str = "browser_run 사용 안내
- browser: chrome, edge, aside. 각 브라우저는 조작용 프로필(사용자 폴더 .claudy/browser/profiles/<이름>)과 원격 디버깅 포트(9231, 9232, 9233)로 뜬다. 평소 쓰는 프로필과 로그인·확장이 따로다
- 처음 한 번: 조작용 프로필 창에서 비밀번호 관리 확장을 설치(또는 개발자 모드로 압축 해제 로드)하고 사이트에 로그인해 두면 이후 그대로 쓴다
- script 는 async 함수 본문이다. page(첫 탭), context, browser 를 쓴다. return 한 값이 결과(문자열이 아니면 JSON)
- 자주 쓰는 것: await page.goto(url); await page.click('text=로그인'); await page.fill('#id', '값'); await page.locator('css').innerText(); await page.screenshot({ path }); const p2 = await context.newPage();
- 새 탭은 context.newPage(), 열린 탭 목록은 context.pages()
- 작업이 끝나도 브라우저는 닫지 않는다(연결만 끊는다). 닫으려면 await browser.close()
- 비밀번호를 스크립트에 적지 않는다. 로그인 화면은 확장이 채우도록 두고 기다린다(await page.waitForURL(...))";

/// Linux·macOS 에서 browser_run 을 실행하는 셸 본문
fn browser_sh(name: &str, job: &str, id: &str) -> String {
    format!(
        r#"B="$HOME/.claudy/browser"; mkdir -p "$B"
N=""; for c in "$(command -v node)" "$B/node/bin/node"; do if [ -n "$c" ] && [ -x "$c" ]; then N="$c"; break; fi; done
if [ -z "$N" ]; then
  os=$(uname -s | tr 'A-Z' 'a-z'); a=$(uname -m); case "$a" in x86_64) a=x64;; aarch64|arm64) a=arm64;; esac
  t="node-{v}-$os-$a"; echo "[claudy] Node.js 를 내려받는다($t)" >&2
  curl -fsSL "https://nodejs.org/dist/{v}/$t.tar.gz" -o "$B/node.tgz" && tar -xzf "$B/node.tgz" -C "$B" && rm -f "$B/node.tgz" && rm -rf "$B/node" && mv "$B/$t" "$B/node" || {{ echo "Node.js 를 설치하지 못했다" >&2; exit 2; }}
  N="$B/node/bin/node"
fi
if [ ! -d "$B/node_modules/playwright-core" ]; then
  echo "[claudy] playwright-core 를 설치한다" >&2
  M="$(dirname "$N")/npm"; [ -x "$M" ] || M=$(command -v npm)
  PATH="$(dirname "$N"):$PATH" "$M" install --prefix "$B" --no-audit --no-fund --silent playwright-core@1 >&2 || {{ echo "playwright-core 를 설치하지 못했다" >&2; exit 2; }}
fi
echo {r} | base64 -d > "$B/runner.mjs"
J="$B/job-{id}.mjs"; echo {j} | base64 -d > "$J"
"$N" "$B/runner.mjs" {name} "$J""#,
        v = NODE_VER,
        r = util::b64(BROWSER_RUNNER),
        j = util::b64(job),
    )
}

/// Windows 에서 browser_run 을 실행하는 PowerShell 본문
fn browser_ps(name: &str, job: &str, id: &str) -> String {
    format!(
        r#"$B=Join-Path $env:USERPROFILE '.claudy\browser'; New-Item -ItemType Directory -Force $B | Out-Null
$N=(Get-Command node -ErrorAction SilentlyContinue).Source
if (-not $N -and (Test-Path "$B\node\node.exe")) {{ $N="$B\node\node.exe" }}
if (-not $N) {{
  $t='node-{v}-win-x64'; [Console]::Error.WriteLine("[claudy] Node.js 를 내려받는다($t)")
  Invoke-WebRequest "https://nodejs.org/dist/{v}/$t.zip" -OutFile "$B\node.zip" -UseBasicParsing
  Expand-Archive "$B\node.zip" $B -Force; Remove-Item "$B\node.zip"; if (Test-Path "$B\node") {{ Remove-Item -Recurse -Force "$B\node" }}; Rename-Item "$B\$t" 'node'
  $N="$B\node\node.exe"
}}
if (-not (Test-Path "$B\node_modules\playwright-core")) {{
  [Console]::Error.WriteLine('[claudy] playwright-core 를 설치한다')
  $M=Join-Path (Split-Path $N) 'npm.cmd'; if (-not (Test-Path $M)) {{ $M='npm.cmd' }}
  & $M install --prefix $B --no-audit --no-fund --silent playwright-core@1 2>&1 | Out-Null
  if (-not (Test-Path "$B\node_modules\playwright-core")) {{ [Console]::Error.WriteLine('playwright-core 를 설치하지 못했다'); exit 2 }}
}}
[IO.File]::WriteAllBytes("$B\runner.mjs", [Convert]::FromBase64String('{r}'))
$J="$B\job-{id}.mjs"; [IO.File]::WriteAllBytes($J, [Convert]::FromBase64String('{j}'))
& $N "$B\runner.mjs" {name} $J; exit $LASTEXITCODE"#,
        v = NODE_VER,
        r = util::b64(BROWSER_RUNNER),
        j = util::b64(job),
    )
}

/// Linux: ssh 로 들어온 명령에서도 사용자 화면(X11·Wayland)에 닿도록 환경 변수를 채운다
const LINUX_GUI_ENV: &str = r#"if [ "$(uname)" = Linux ] && [ -z "$DISPLAY" ] && [ -z "$WAYLAND_DISPLAY" ]; then
  export XDG_RUNTIME_DIR="${XDG_RUNTIME_DIR:-/run/user/$(id -u)}"
  w=$(ls "$XDG_RUNTIME_DIR" 2>/dev/null | grep -m1 -E '^wayland-[0-9]+$'); [ -n "$w" ] && export WAYLAND_DISPLAY="$w"
  x=$(ls /tmp/.X11-unix 2>/dev/null | sed -n 's/^X//p' | head -1); export DISPLAY=":${x:-0}"
  for a in "$XDG_RUNTIME_DIR/gdm/Xauthority" "$HOME/.Xauthority"; do [ -f "$a" ] && export XAUTHORITY="${XAUTHORITY:-$a}"; done
  [ -S "$XDG_RUNTIME_DIR/bus" ] && export DBUS_SESSION_BUS_ADDRESS="${DBUS_SESSION_BUS_ADDRESS:-unix:path=$XDG_RUNTIME_DIR/bus}"
fi
"#;

const CAPTURE_SH: &str = r#"f="${TMPDIR:-/tmp}/claudy-capture.jpg"
if [ "$(uname)" = Darwin ]; then screencapture -x -t jpg "$f" && sips -Z 1600 "$f" >/dev/null && echo "saved $f"; exit $?; fi
r="${TMPDIR:-/tmp}/claudy-capture-raw.png"; rm -f "$r" "$f"
if [ -n "$WAYLAND_DISPLAY" ] && command -v grim >/dev/null; then grim "$r"
elif command -v gnome-screenshot >/dev/null; then gnome-screenshot -f "$r"
elif command -v spectacle >/dev/null; then spectacle -b -n -o "$r"
elif command -v import >/dev/null; then import -window root "$r"
elif command -v scrot >/dev/null; then scrot -o "$r"
else echo "화면 찍기 프로그램이 없다. grim(Wayland) 또는 imagemagick·scrot(X11) 중 하나를 설치한다"; exit 2; fi
[ -s "$r" ] || { echo "화면을 찍지 못했다(화면 세션에 로그인해 있어야 한다)"; exit 2; }
if command -v convert >/dev/null; then convert "$r" -resize '1600x>' -quality 85 "$f"
elif command -v ffmpeg >/dev/null; then ffmpeg -loglevel error -y -i "$r" -vf "scale='min(1600,iw)':-2" "$f"
elif command -v python3 >/dev/null && python3 -c 'import PIL' 2>/dev/null; then python3 -c 'import sys;from PIL import Image;i=Image.open(sys.argv[1]).convert("RGB");i.thumbnail((1600,10000));i.save(sys.argv[2],quality=85)' "$r" "$f"
else echo "그림 변환 프로그램이 없다. imagemagick 또는 ffmpeg 를 설치한다"; exit 2; fi
rm -f "$r"; [ -s "$f" ] && echo "saved $f""#;

/// 호스트에서 명령 하나를 실행해 표준출력을 돌려준다(종료 코드 0 이 아니면 오류)
async fn run_simple(host: &config::HostCfg, cmd: &str, secs: u64) -> Result<String, String> {
    let plan = exec::command_plan(host, None, cmd)?;
    let cap = exec::run(&plan, Duration::from_secs(secs)).await?;
    if cap.timed_out {
        return Err("제한 시간을 넘었다".into());
    }
    if cap.exit_code != Some(0) {
        return Err(format!("코드 {:?}: {}", cap.exit_code, cap.stderr.trim()));
    }
    Ok(cap.stdout)
}

fn human_size(n: u64) -> String {
    match n {
        n if n >= 1 << 30 => format!("{:.2} GB", n as f64 / (1u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MB", n as f64 / (1u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{n} 바이트"),
    }
}

fn path_size(p: &std::path::Path) -> u64 {
    match std::fs::metadata(p) {
        Ok(m) if m.is_dir() => std::fs::read_dir(p).map(|rd| rd.flatten().map(|e| path_size(&e.path())).sum()).unwrap_or(0),
        Ok(m) => m.len(),
        Err(_) => 0,
    }
}

fn copy_local(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
    if src.is_dir() {
        std::fs::create_dir_all(dst)?;
        for e in std::fs::read_dir(src)? {
            let e = e?;
            copy_local(&e.path(), &dst.join(e.file_name()))?;
        }
        Ok(())
    } else {
        if let Some(d) = dst.parent() {
            std::fs::create_dir_all(d)?;
        }
        std::fs::copy(src, dst).map(|_| ())
    }
}

async fn scp_run(sp: exec::Spawnable, limit: Duration) -> Result<(), String> {
    let cap = exec::run(&sp, limit).await?;
    if cap.timed_out {
        return Err(format!("제한 시간({}초)을 넘어 중단했다", limit.as_secs()));
    }
    if cap.exit_code != Some(0) {
        let msg = cap.stderr.trim();
        return Err(format!("복사 실패(코드 {:?}): {}", cap.exit_code, if msg.is_empty() { cap.stdout.trim() } else { msg }));
    }
    Ok(())
}

/// 실제 복사. 돌려주는 값은 크기(알 수 있을 때)
async fn file_copy_run(src: &config::HostCfg, sp: &str, dst: &config::HostCfg, dp: &str, rec: bool, limit: Duration) -> Result<String, String> {
    let local = |h: &config::HostCfg| h.kind != "ssh";
    match (local(src), local(dst)) {
        (true, true) => {
            let s = std::path::PathBuf::from(exec::expand_tilde(sp));
            let mut d = std::path::PathBuf::from(exec::expand_tilde(dp));
            if dp.ends_with('/') || dp.ends_with('\\') || d.is_dir() {
                d = d.join(s.file_name().ok_or("보낼 경로에 이름이 없다")?);
            }
            if s.is_dir() && !rec {
                return Err("폴더는 recursive=true 로 복사한다".into());
            }
            copy_local(&s, &d).map_err(|e| format!("복사 실패: {e}"))?;
            Ok(human_size(path_size(&d)))
        }
        (true, false) => {
            let s = exec::expand_tilde(sp);
            let size = path_size(std::path::Path::new(&s));
            scp_run(exec::scp_plan(dst, &s, dp, true, rec)?, limit).await?;
            Ok(human_size(size))
        }
        (false, true) => {
            let d = exec::expand_tilde(dp);
            if let Some(parent) = std::path::Path::new(&d).parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            scp_run(exec::scp_plan(src, &d, sp, false, rec)?, limit).await?;
            let mut shown = std::path::PathBuf::from(&d);
            if shown.is_dir() {
                if let Some(n) = std::path::Path::new(sp.trim_end_matches(['/', '\\'])).file_name() {
                    if shown.join(n).exists() {
                        shown = shown.join(n);
                    }
                }
            }
            Ok(human_size(path_size(&shown)))
        }
        (false, false) => {
            // 두 원격 사이는 이 PC 의 임시 폴더를 거친다
            let tmp = std::env::temp_dir().join(format!("claudy-xfer-{}", uuid::Uuid::new_v4().simple()));
            std::fs::create_dir_all(&tmp).map_err(|e| format!("임시 폴더를 만들 수 없다: {e}"))?;
            let name = std::path::Path::new(sp.trim_end_matches(['/', '\\']))
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| "data".into());
            let mid = tmp.join(&name);
            let res = async {
                scp_run(exec::scp_plan(src, &mid.to_string_lossy(), sp, false, rec)?, limit).await?;
                let size = path_size(&mid);
                scp_run(exec::scp_plan(dst, &mid.to_string_lossy(), dp, true, rec)?, limit).await?;
                Ok::<String, String>(human_size(size))
            }
            .await;
            let _ = std::fs::remove_dir_all(&tmp);
            res
        }
    }
}

/// PowerShell 작은따옴표 문자열
fn ps_q(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Windows: 사용자 화면에 프로그램을 띄우는 PowerShell. 원격이면 대화형 예약 작업으로, 이 PC 면 Start-Process 로 띄운다
fn app_open_ps(program: &str, args: &[String], wait: u64, remote: bool) -> String {
    let arg_line = args
        .iter()
        .map(|a| if a.contains(' ') || a.is_empty() { format!("\"{}\"", a.replace('"', "\\\"")) } else { a.clone() })
        .collect::<Vec<_>>()
        .join(" ");
    let launch = if remote {
        r#"$name='ClaudyOpen-'+[guid]::NewGuid().ToString('N').Substring(0,8)
$act = if ($argLine) { New-ScheduledTaskAction -Execute $exe -Argument $argLine } else { New-ScheduledTaskAction -Execute $exe }
$pr = New-ScheduledTaskPrincipal -UserId (whoami) -LogonType Interactive
Register-ScheduledTask -TaskName $name -Action $act -Principal $pr -Force | Out-Null
Start-ScheduledTask -TaskName $name"#
    } else {
        r#"if ($argLine) { Start-Process -FilePath $exe -ArgumentList $argLine } else { Start-Process -FilePath $exe }"#
    };
    let cleanup = if remote { "$r=(Get-ScheduledTaskInfo -TaskName $name).LastTaskResult; Unregister-ScheduledTask -TaskName $name -Confirm:$false" } else { "$r=0" };
    format!(
        r#"$ErrorActionPreference='Stop'
$exe={prog}; $argLine={args}
if (-not (Test-Path -LiteralPath $exe)) {{
  $c = Get-Command $exe -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
  if ($c) {{ $exe = $c.Source }} else {{
    $n = if ($exe -like '*.exe') {{ $exe }} else {{ "$exe.exe" }}
    foreach ($root in 'HKCU:','HKLM:') {{ $ap = (Get-ItemProperty "$root\SOFTWARE\Microsoft\Windows\CurrentVersion\App Paths\$n" -ErrorAction SilentlyContinue).'(default)'; if ($ap) {{ $exe = $ap.Trim('"'); break }} }}
  }}
}}
if (-not (Test-Path -LiteralPath $exe)) {{ Write-Output "프로그램을 찾지 못했다: $exe"; exit 2 }}
$pn=[IO.Path]::GetFileNameWithoutExtension($exe)
$before=@(Get-Process $pn -ErrorAction SilentlyContinue | ForEach-Object Id)
{launch}
$p=$null
for ($i=0; $i -lt {ticks}; $i++) {{ Start-Sleep -Milliseconds 500; $p = Get-Process $pn -ErrorAction SilentlyContinue | Where-Object {{ $_.SessionId -ne 0 -and $before -notcontains $_.Id }} | Select-Object -First 1; if ($p) {{ break }} }}
{cleanup}
if ($p) {{ Write-Output "실행됨: $exe (pid $($p.Id), 화면 세션 $($p.SessionId))" }}
elseif ($before.Count) {{ Write-Output "이미 실행 중인 $pn 이 있다(pid $($before -join ',')). 새 창이 같은 프로세스에 열렸을 수 있다: $exe" }}
else {{ Write-Output "프로세스를 확인하지 못했다: $exe (예약 작업 결과 $r). 사용자가 Windows 에 로그온해 있어야 한다"; exit 3 }}"#,
        prog = ps_q(program),
        args = ps_q(&arg_line),
        ticks = wait * 2,
    )
}

/// macOS·Linux: macOS 는 open -a(앱 이름) 또는 open(파일 경로 실행), 그 밖은 nohup 으로 띄운다
fn app_open_sh(program: &str, args: &[String]) -> String {
    let q = util::sh_quote;
    let a = args.iter().map(|x| q(x)).collect::<Vec<_>>().join(" ");
    format!(
        r#"P={p}
if [ "$(uname)" = Darwin ]; then
  if [ -x "$P" ] && [ ! -d "$P" ]; then nohup "$P" {a} >/dev/null 2>&1 & echo "실행됨: $P (pid $!)"
  else open -a "$P" {args_flag} && echo "실행됨: $P"; fi
else
  if command -v "$P" >/dev/null 2>&1 || [ -x "$P" ]; then setsid nohup "$P" {a} >/dev/null 2>&1 < /dev/null & echo "실행됨: $P (pid $!)"
  elif command -v gtk-launch >/dev/null 2>&1; then gtk-launch "$P" {a} && echo "실행됨: $P"
  else echo "프로그램을 찾지 못했다: $P"; exit 2; fi
fi"#,
        p = q(program),
        args_flag = if a.is_empty() { String::new() } else { format!("--args {a}") },
    )
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Claudy {
    fn get_info(&self) -> ServerConfig {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("claudy", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    if std::env::args().nth(1).as_deref() == Some("--version") {
        println!("claudy {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if let Some(msg) = util::migrate_legacy_dir() {
        eprintln!("claudy: {msg}");
    }
    // 14일 지난 백그라운드 작업 파일을 지운다
    jobs::cleanup_old(&util::base_dir(), std::time::SystemTime::now(), Duration::from_secs(14 * 86400));
    let claudy = Claudy::new();
    let service = claudy.serve(stdio()).await?;
    service.waiting().await?;
    Ok(())
}

#[cfg(test)]
mod app_open_tests {
    use super::*;

    #[test]
    fn ps_script_quotes_and_remote_task() {
        let s = app_open_ps(r"C:\Program Files (x86)\HNC\Hwp.exe", &[r"D:\a b.hwp".into()], 5, true);
        assert!(s.contains(r"$exe='C:\Program Files (x86)\HNC\Hwp.exe'"));
        assert!(s.contains(r#"$argLine='"D:\a b.hwp"'"#));
        assert!(s.contains("Register-ScheduledTask") && s.contains("Unregister-ScheduledTask") && s.contains("-lt 10"));
        let l = app_open_ps("notepad", &[], 3, false);
        assert!(l.contains("Start-Process") && !l.contains("Register-ScheduledTask"));
        assert_eq!(ps_q("it's"), "'it''s'");
    }

    #[test]
    fn sh_script_mac_app() {
        let s = app_open_sh("Final Cut Pro", &[]);
        assert!(s.contains("P='Final Cut Pro'") && s.contains(r#"open -a "$P" "#));
    }

    #[test]
    fn clixml_is_decoded() {
        let raw = "#< CLIXML\n<Objs Version=\"1.1.0.1\"><S S=\"Error\">python : 오류 &lt;x&gt;_x000D__x000A_</S><S S=\"Error\">위치 줄:8 문자:1_x000D__x000A_</S><S S=\"Error\">+ python -c ..._x000D__x000A_</S><S S=\"Error\">ModuleNotFoundError: pyhwpx_x000D__x000A_</S></Objs>";
        assert_eq!(util::decode_clixml(raw), "python : 오류 <x>\nModuleNotFoundError: pyhwpx");
        assert_eq!(util::decode_clixml("plain"), "plain");
    }
}
