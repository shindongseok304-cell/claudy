use serde_json::{Value, json};

use crate::config::HostCfg;
use crate::exec::{self, Spawnable};
use crate::util;

/// 세션 ID 와 모델 이름을 검사한다 (원격 명령에 그대로 들어가므로 엄격하게)
pub fn check_opts(session_id: Option<&str>, model: Option<&str>) -> Result<(), String> {
    if let Some(s) = session_id {
        let ok = (1..=128).contains(&s.len()) && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !ok {
            return Err("session_id 형식이 올바르지 않다".into());
        }
    }
    if let Some(m) = model {
        let ok = (1..=100).contains(&m.len())
            && m.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '[' | ']'));
        if !ok {
            return Err("model 값이 올바르지 않다".into());
        }
    }
    Ok(())
}

/// 호스트의 claude_permissions 값을 Claude Code 인자로 바꾼다. skip 은 승인 생략, accept_edits 는 편집만 자동 허용, default 는 인자 없음.
fn permission_args(mode: &str) -> Result<Vec<String>, String> {
    match mode {
        "skip" => Ok(vec!["--dangerously-skip-permissions".into()]),
        "accept_edits" => Ok(vec!["--permission-mode".into(), "acceptEdits".into()]),
        "default" => Ok(Vec::new()),
        other => Err(format!("claude_permissions 값이 잘못되었다: {other}")),
    }
}

fn flags(session_id: Option<&str>, model: Option<&str>, perm: &str) -> Result<Vec<String>, String> {
    let mut v: Vec<String> = ["-p", "--output-format", "json"].iter().map(|s| s.to_string()).collect();
    v.extend(permission_args(perm)?);
    if let Some(s) = session_id {
        v.push("--resume".into());
        v.push(s.into());
    }
    if let Some(m) = model {
        v.push("--model".into());
        v.push(m.into());
    }
    Ok(v)
}

/// 이 PC 의 claude 실행 파일 위치를 찾는다. 이름만 주어졌을 때 PATH 와 Windows 의 흔한 설치 위치를 차례로 본다.
/// 데스크톱 앱이 띄운 프로세스는 PATH 에 사용자 폴더가 빠져 있을 수 있기 때문이다. 못 찾으면 원래 이름을 돌려준다.
pub fn resolve_local_bin(bin: &str) -> String {
    if bin.contains('/') || bin.contains('\\') {
        return bin.to_string();
    }
    let exts: Vec<String> = if cfg!(windows) {
        vec![".exe".into(), ".cmd".into(), ".bat".into(), String::new()]
    } else {
        vec![String::new()]
    };
    let mut dirs: Vec<std::path::PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    if cfg!(target_os = "linux") {
        let h = crate::util::home_dir();
        dirs.push(h.join(".local").join("bin"));
        dirs.push(h.join(".claude").join("local"));
        dirs.push(h.join(".npm-global").join("bin"));
        dirs.push(std::path::PathBuf::from("/usr/local/bin"));
    }
    if cfg!(target_os = "macos") {
        let h = crate::util::home_dir();
        dirs.push(h.join(".local").join("bin"));
        dirs.push(h.join(".claude").join("local"));
        dirs.push(std::path::PathBuf::from("/opt/homebrew/bin"));
        dirs.push(std::path::PathBuf::from("/usr/local/bin"));
    }
    if cfg!(windows) {
        if let Some(h) = std::env::var_os("USERPROFILE") {
            let h = std::path::PathBuf::from(h);
            dirs.push(h.join(".local").join("bin"));
            dirs.push(h.join(".claude").join("local"));
        }
        if let Some(a) = std::env::var_os("APPDATA") {
            dirs.push(std::path::PathBuf::from(a).join("npm"));
        }
        if let Some(l) = std::env::var_os("LOCALAPPDATA") {
            dirs.push(std::path::PathBuf::from(l).join("Programs").join("claude"));
        }
    }
    for d in dirs {
        for e in &exts {
            let p = d.join(format!("{bin}{e}"));
            if p.is_file() {
                return p.to_string_lossy().into_owned();
            }
        }
    }
    bin.to_string()
}

/// Claude Code 실행 명세. 프롬프트는 stdin 으로 넘긴다.
/// local 은 셸을 거치지 않고 직접 실행하고, ssh 는 원격에서 base64 로 받은 프롬프트를 파이프로 넘긴다.
pub fn claude_plan(
    host: &HostCfg,
    prompt: &str,
    session_id: Option<&str>,
    model: Option<&str>,
    cwd: Option<String>,
) -> Result<Spawnable, String> {
    check_opts(session_id, model)?;
    let bin = host.claude_bin();
    let args = flags(session_id, model, host.permission_mode())?;
    if host.as_root() && host.permission_mode() == "skip" {
        return Err("root 로 실행하는 호스트는 Claude Code 가 전체 허용(skip)을 거부한다. 이 호스트의 claude_permissions 를 accept_edits 또는 default 로 바꾸거나, root 권한 실행을 끄고 접속 계정으로 Claude 에 로그인한다".into());
    }
    if host.kind == "ssh" {
        if !bin.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | '/' | '~')) {
            return Err("claude 실행 이름이 올바르지 않다".into());
        }
        let body = if host.remote_windows() {
            exec::ps_pipe_b64(prompt, &format!("{bin} {}", args.join(" ")))
        } else {
            format!("echo {} | base64 -d | {bin} {}", util::b64(prompt), args.join(" "))
        };
        exec::ssh_plan(host, &host.with_startup(exec::remote_script_for(host, cwd.as_deref(), &body)))
    } else {
        Ok(Spawnable {
            program: resolve_local_bin(bin),
            args,
            stdin: Some(prompt.to_string()),
            cwd: Some(exec::expand_tilde(&cwd.unwrap_or_else(exec::home_string))),
            cmd_fallback: true,
        })
    }
}

/// claude --output-format json 결과에서 JSON 객체를 찾는다. 전체 또는 마지막 줄부터 시도한다.
pub fn parse_result(text: &str) -> Option<Value> {
    let t = text.trim();
    let mut tries: Vec<&str> = vec![t];
    tries.extend(t.lines().rev().map(str::trim).filter(|l| !l.is_empty()));
    for c in tries {
        if let Ok(v) = serde_json::from_str::<Value>(c) {
            let is_result = v.get("type").and_then(Value::as_str) == Some("result") || v.get("result").is_some();
            if v.is_object() && is_result {
                return Some(v);
            }
        }
    }
    None
}

/// 도구 응답에 넣을 핵심 필드만 뽑는다
pub fn summarize(v: &Value) -> Value {
    let get = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    let result = match v.get("result").and_then(Value::as_str) {
        Some(s) => Value::String(util::truncate(s)),
        None => get("result"),
    };
    json!({
        "result": result,
        "session_id": get("session_id"),
        "is_error": v.get("is_error").and_then(Value::as_bool).unwrap_or(false),
        "total_cost_usd": get("total_cost_usd"),
        "num_turns": get("num_turns"),
        "subtype": get("subtype"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_result_object() {
        let raw = r#"{"type":"result","subtype":"success","is_error":false,"num_turns":1,"result":"안녕","session_id":"abc","total_cost_usd":0.01}"#;
        let v = parse_result(raw).unwrap();
        let s = summarize(&v);
        assert_eq!(s["result"], "안녕");
        assert_eq!(s["session_id"], "abc");
        assert_eq!(s["num_turns"], 1);
        assert_eq!(s["is_error"], false);
    }

    #[test]
    fn parses_last_line_after_noise() {
        let raw = "warning: something\n{\"type\":\"result\",\"result\":\"ok\",\"session_id\":\"s1\"}\n";
        assert_eq!(parse_result(raw).unwrap()["session_id"], "s1");
    }

    #[test]
    fn rejects_non_json() {
        assert!(parse_result("plain text").is_none());
        assert!(parse_result("{\"other\":1}").is_none());
    }

    #[test]
    fn permission_modes_map_to_args() {
        let f = |m: &str| flags(None, None, m).unwrap();
        assert!(f("skip").contains(&"--dangerously-skip-permissions".to_string()));
        let acc = f("accept_edits");
        assert!(acc.windows(2).any(|w| w == ["--permission-mode", "acceptEdits"]));
        assert!(!acc.contains(&"--dangerously-skip-permissions".to_string()));
        let def = f("default");
        assert!(!def.iter().any(|a| a.contains("permission") || a.contains("dangerously")));
        assert!(flags(None, None, "yolo").is_err());
    }

    #[test]
    fn options_are_validated() {
        assert!(check_opts(Some("abc-123_x"), Some("claude-opus-4")).is_ok());
        assert!(check_opts(Some("a;rm"), None).is_err());
        assert!(check_opts(None, Some("m $(x)")).is_err());
    }
}
