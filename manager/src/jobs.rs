//! jobs/<id>.json 상태와 jobs/<id>.log 꼬리를 읽는다.

use std::fs;
use std::path::Path;

use chrono::{DateTime, Local};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Running,
    Finished,
    Interrupted,
}

impl State {
    pub fn label(self) -> &'static str {
        match self {
            State::Running => "진행",
            State::Finished => "끝남",
            State::Interrupted => "중단",
        }
    }
}

/// 상태 파일의 값으로 상태를 정한다. 서버가 중단으로 고친 작업은 interrupted 가 참이다.
pub fn state_of(v: &Value) -> State {
    if v["interrupted"].as_bool() == Some(true) {
        State::Interrupted
    } else if v["finished"].as_bool() == Some(true) {
        State::Finished
    } else {
        State::Running
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct JobRow {
    pub id: String,
    pub host: String,
    pub kind: String,
    pub started: String,
    pub started_raw: String,
    pub state: State,
    pub exit_code: Option<i64>,
}

pub fn row_from(id: &str, v: &Value) -> JobRow {
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let started_raw = s("started_at");
    let started = match DateTime::parse_from_rfc3339(&started_raw) {
        Ok(t) => t.with_timezone(&Local).format("%m-%d %H:%M:%S").to_string(),
        Err(_) => started_raw.clone(),
    };
    JobRow {
        id: id.to_string(),
        host: s("host"),
        kind: s("kind"),
        started,
        started_raw,
        state: state_of(v),
        exit_code: v["exit_code"].as_i64(),
    }
}

/// 작업 목록. 시작 시각이 늦은 것이 위로 온다.
pub fn read_rows(dir: &Path) -> Vec<JobRow> {
    let mut rows = Vec::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return rows;
    };
    for e in entries.flatten() {
        let path = e.path();
        if path.extension().and_then(|x| x.to_str()) != Some("json") {
            continue;
        }
        let Some(id) = path.file_stem().and_then(|x| x.to_str()).map(String::from) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(v) = serde_json::from_str::<Value>(&text) {
            rows.push(row_from(&id, &v));
        }
    }
    rows.sort_by(|a, b| b.started_raw.cmp(&a.started_raw));
    rows
}

/// 로그 파일의 끝 n 줄
pub fn tail(path: &Path, n: usize) -> String {
    let Ok(bytes) = fs::read(path) else {
        return "(로그 파일이 없다)".into();
    };
    let text = crate::proc::decode(&bytes);
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn state_mapping() {
        assert_eq!(state_of(&json!({"finished": false})), State::Running);
        assert_eq!(state_of(&json!({"finished": true, "exit_code": 0})), State::Finished);
        assert_eq!(state_of(&json!({"finished": true, "interrupted": true})), State::Interrupted);
        assert_eq!(state_of(&json!({})), State::Running);
        assert_eq!(State::Interrupted.label(), "중단");
    }

    #[test]
    fn row_reads_fields() {
        let v = json!({"job_id": "abc", "host": "pi", "kind": "claude", "started_at": "2026-11-06T01:02:03+09:00", "finished": true, "exit_code": 2});
        let r = row_from("abc", &v);
        assert_eq!(r.host, "pi");
        assert_eq!(r.kind, "claude");
        assert_eq!(r.exit_code, Some(2));
        assert_eq!(r.state, State::Finished);
    }

    #[test]
    fn tail_keeps_last_lines() {
        let dir = std::env::temp_dir().join(format!("claudy-jobs-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let p = dir.join("x.log");
        let body: String = (1..=5).map(|i| format!("line{i}\n")).collect();
        fs::write(&p, body).unwrap();
        assert_eq!(tail(&p, 2), "line4\nline5");
        assert_eq!(tail(&dir.join("none.log"), 2), "(로그 파일이 없다)");
        let _ = fs::remove_dir_all(&dir);
    }
}
