//! %USERPROFILE%\.claudy\manager.toml. 설정 프로그램 자신의 동작 설정이다.
//! 잠수함 모드와 시작 시 실행은 app.rs(Windows)·app_mac.rs(macOS)가, 역방향 터널은 tunnel.rs 가 적용한다.
//! 모르는 항목은 그대로 보존한다.

use std::fs;
use std::path::Path;

use toml::{Table, Value};

#[derive(Debug, Clone, PartialEq)]
pub struct Behavior {
    pub stealth_mode: bool,
    pub autostart: bool,
    /// 역방향 터널: 이 PC 의 SSH 를 경유 서버의 127.0.0.1:tunnel_remote_port 에 열어 둔다
    pub tunnel_enabled: bool,
    /// 터널을 여는 서버. hosts.toml 의 ssh 호스트 이름
    pub tunnel_via: String,
    pub tunnel_remote_port: u16,
    pub tunnel_local_port: u16,
    /// Windows: 한글 「접근 허용」 보안 창 자동 승인(기본 켜짐)
    pub hwp_guard: bool,
    raw: Table,
}

pub const DEFAULT_TUNNEL_REMOTE: u16 = 22022;

impl Default for Behavior {
    fn default() -> Self {
        Behavior {
            stealth_mode: false,
            autostart: false,
            tunnel_enabled: false,
            tunnel_via: String::new(),
            tunnel_remote_port: DEFAULT_TUNNEL_REMOTE,
            tunnel_local_port: 22,
            hwp_guard: true,
            raw: Table::new(),
        }
    }
}

impl Behavior {
    pub fn parse(text: &str) -> Result<Behavior, String> {
        let raw: Table = toml::from_str(text).map_err(|e| format!("TOML 해석 실패: {e}"))?;
        let b = |k: &str| raw.get(k).and_then(Value::as_bool).unwrap_or(false);
        let port = |k: &str, d: u16| {
            raw.get(k).and_then(Value::as_integer).and_then(|v| u16::try_from(v).ok()).filter(|v| *v > 0).unwrap_or(d)
        };
        Ok(Behavior {
            stealth_mode: b("stealth_mode"),
            autostart: b("autostart"),
            tunnel_enabled: b("tunnel_enabled"),
            tunnel_via: raw.get("tunnel_via").and_then(Value::as_str).unwrap_or("").to_string(),
            tunnel_remote_port: port("tunnel_remote_port", DEFAULT_TUNNEL_REMOTE),
            tunnel_local_port: port("tunnel_local_port", 22),
            hwp_guard: raw.get("hwp_guard").and_then(Value::as_bool).unwrap_or(true),
            raw,
        })
    }

    pub fn to_text(&self) -> Result<String, String> {
        let mut t = self.raw.clone();
        t.insert("stealth_mode".into(), Value::Boolean(self.stealth_mode));
        t.insert("autostart".into(), Value::Boolean(self.autostart));
        t.insert("tunnel_enabled".into(), Value::Boolean(self.tunnel_enabled));
        t.insert("tunnel_via".into(), Value::String(self.tunnel_via.clone()));
        t.insert("tunnel_remote_port".into(), Value::Integer(self.tunnel_remote_port as i64));
        t.insert("tunnel_local_port".into(), Value::Integer(self.tunnel_local_port as i64));
        t.insert("hwp_guard".into(), Value::Boolean(self.hwp_guard));
        let body = toml::to_string(&t).map_err(|e| format!("TOML 쓰기 실패: {e}"))?;
        Ok(format!("# Claudy 설정 프로그램의 동작 설정 (manager.toml)\n{body}"))
    }
}

/// 파일이 없으면 기본값. 읽을 수 없는 파일은 오류로 돌려준다.
pub fn load(path: &Path) -> Result<Behavior, String> {
    match fs::read_to_string(path) {
        Ok(text) => Behavior::parse(text.trim_start_matches('\u{feff}')),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Behavior::default()),
        Err(e) => Err(format!("읽기 실패: {e}")),
    }
}

pub fn save(path: &Path, b: &Behavior) -> Result<(), String> {
    let text = b.to_text()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("폴더 만들기 실패: {e}"))?;
    }
    fs::write(path, text).map_err(|e| format!("쓰기 실패: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_and_round_trip_keep_unknown() {
        let b = Behavior::parse("future = 3\nstealth_mode = true\n").unwrap();
        assert!(b.stealth_mode);
        assert!(!b.autostart);
        let mut b2 = b.clone();
        b2.autostart = true;
        let back = Behavior::parse(&b2.to_text().unwrap()).unwrap();
        assert!(back.autostart && back.stealth_mode);
        assert_eq!(back.raw.get("future"), Some(&Value::Integer(3)));
    }

    #[test]
    fn missing_file_is_default() {
        let p = std::env::temp_dir().join(format!("claudy-none-{}.toml", std::process::id()));
        assert_eq!(load(&p).unwrap(), Behavior::default());
    }
}
