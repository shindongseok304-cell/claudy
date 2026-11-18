use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use serde_json::json;

use crate::safety;
use crate::util;

pub struct Entry<'a> {
    pub tool: &'a str,
    pub host: &'a str,
    pub text: &'a str,
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub denied: bool,
}

/// <base>/logs/YYYY-MM-DD.jsonl 에 한 줄을 덧붙인다. 명령과 프롬프트는 가린 뒤 앞 500자만 남긴다.
pub fn record(base: &Path, ent: Entry) {
    let dir = base.join("logs");
    if let Err(e) = fs::create_dir_all(&dir) {
        eprintln!("claudy: 감사 로그 폴더 생성 실패: {e}");
        return;
    }
    let file = dir.join(format!("{}.jsonl", crate::util::local_now().format("%Y-%m-%d")));
    let text: String = safety::redact(ent.text).chars().take(500).collect();
    let line = json!({
        "ts": util::now_iso(),
        "tool": ent.tool,
        "host": ent.host,
        "text": text,
        "exit_code": ent.exit_code,
        "duration_ms": ent.duration_ms,
        "denied": ent.denied,
    });
    match OpenOptions::new().create(true).append(true).open(&file) {
        Ok(mut f) => {
            let _ = writeln!(f, "{line}");
        }
        Err(e) => eprintln!("claudy: 감사 로그 쓰기 실패: {e}"),
    }
}
