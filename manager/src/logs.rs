//! logs/YYYY-MM-DD.jsonl 한 줄을 해석한다.

use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Datelike, Local, NaiveDate};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq)]
pub struct LogLine {
    /// 시각(지역 시간 HH:MM:SS)
    pub time: String,
    pub tool: String,
    pub host: String,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<i64>,
    pub text: String,
    pub denied: bool,
}

pub fn file_for(dir: &Path, date: NaiveDate) -> PathBuf {
    dir.join(format!("{}.jsonl", date.format("%Y-%m-%d")))
}

pub fn parse_line(raw: &str) -> Option<LogLine> {
    let v: Value = serde_json::from_str(raw).ok()?;
    let s = |k: &str| v[k].as_str().unwrap_or("").to_string();
    let ts = s("ts");
    let time = match DateTime::parse_from_rfc3339(&ts) {
        Ok(t) => t.with_timezone(&Local).format("%H:%M:%S").to_string(),
        Err(_) => ts.clone(),
    };
    Some(LogLine {
        time,
        tool: s("tool"),
        host: s("host"),
        exit_code: v["exit_code"].as_i64(),
        duration_ms: v["duration_ms"].as_i64(),
        text: s("text"),
        denied: v["denied"].as_bool().unwrap_or(false),
    })
}

/// 파일의 빈 줄을 뺀 원문 줄들. 파일이 없으면 None.
pub fn read_raw(path: &Path) -> Option<Vec<String>> {
    let text = fs::read_to_string(path).ok()?;
    Some(text.lines().filter(|l| !l.trim().is_empty()).map(String::from).collect())
}

/// 포함 문자열 검색(대소문자 무시). 빈 검색어는 모두 통과한다.
pub fn matches(raw: &str, query: &str) -> bool {
    let q = query.trim();
    q.is_empty() || raw.to_lowercase().contains(&q.to_lowercase())
}

/// 앞 n 글자만 남기고 줄바꿈은 공백으로 바꾼다.
pub fn short(text: &str, n: usize) -> String {
    let flat: String = text.chars().map(|c| if c == '\n' || c == '\r' { ' ' } else { c }).collect();
    flat.chars().take(n).collect()
}

/// 「2026-11-06 (금)」 꼴의 날짜 표시
pub fn date_label(d: NaiveDate) -> String {
    let wd = ["월", "화", "수", "목", "금", "토", "일"][d.weekday().num_days_from_monday() as usize];
    format!("{} ({wd})", d.format("%Y-%m-%d"))
}

pub fn exit_text(code: Option<i64>) -> String {
    code.map(|c| c.to_string()).unwrap_or_else(|| "-".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_jsonl_line() {
        let raw = r#"{"ts":"2026-11-06T10:20:30+09:00","tool":"run_command","host":"this-pc","text":"dir\nC:","exit_code":0,"duration_ms":123,"denied":false}"#;
        let l = parse_line(raw).expect("parse");
        assert_eq!(l.tool, "run_command");
        assert_eq!(l.exit_code, Some(0));
        assert_eq!(l.duration_ms, Some(123));
        assert!(!l.denied);
        assert_eq!(l.text, "dir\nC:");
        assert_eq!(exit_text(None), "-");
    }

    #[test]
    fn denied_line_and_null_exit() {
        let raw = r#"{"ts":"x","tool":"claude_ask","host":"pi","text":"rm","exit_code":null,"duration_ms":5,"denied":true}"#;
        let l = parse_line(raw).unwrap();
        assert!(l.denied);
        assert_eq!(l.exit_code, None);
        assert_eq!(l.time, "x", "ts 가 시각으로 바뀌지 않으면 원문을 그대로 쓴다");
        assert!(parse_line("not json").is_none());
    }

    #[test]
    fn filter_and_short() {
        assert!(matches("{\"text\":\"ABC\"}", "abc"));
        assert!(matches("anything", "  "));
        assert!(!matches("x", "y"));
        assert_eq!(short("a\nb\rc", 10), "a b c");
        assert_eq!(short("가나다라마", 3), "가나다");
    }

    #[test]
    fn date_label_has_korean_weekday() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        assert_eq!(date_label(d), "2026-11-06 (금)");
    }

    #[test]
    fn file_name_uses_date() {
        let d = NaiveDate::from_ymd_opt(2026, 11, 6).unwrap();
        assert!(file_for(Path::new("/x"), d).ends_with("2026-11-06.jsonl"));
    }
}
