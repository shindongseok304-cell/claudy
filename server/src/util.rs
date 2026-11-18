use std::path::PathBuf;

use base64::Engine;
use encoding_rs::EUC_KR;

/// 도구 출력 한도(문자 수)
pub const OUTPUT_LIMIT: usize = 30_000;

pub fn truncate(s: &str) -> String {
    let total = s.chars().count();
    if total <= OUTPUT_LIMIT {
        return s.to_string();
    }
    let head: String = s.chars().take(OUTPUT_LIMIT).collect();
    format!("{head}\n...[잘림: 전체 {total}자 중 {OUTPUT_LIMIT}자만 표시]")
}

/// 자식 프로세스 출력 바이트를 문자열로 바꾼다. Windows 에서는 UTF-8 이 아니면 CP949(EUC-KR 계열)로 본다.
pub fn decode_bytes(bytes: &[u8]) -> String {
    decode_with(bytes, cfg!(windows))
}

/// allow_cp949 가 참이면 UTF-8 이 아닌 바이트를 CP949 로 해석한다. 아니면 깨진 부분을 대체 문자로 바꾼다.
pub fn decode_with(bytes: &[u8], allow_cp949: bool) -> String {
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_string(),
        Err(_) if allow_cp949 => EUC_KR.decode(bytes).0.into_owned(),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

pub fn base_dir() -> PathBuf {
    home_dir().join(".claudy")
}

/// 0.1.4 까지의 설정 폴더 ~/.claudly 를 ~/.claudy 로 옮긴다. 새 폴더가 이미 있으면 아무것도 하지 않는다.
/// 옮긴 뒤 옛 경로에는 연결(Windows 정션)을 남겨, 옛 경로로 로드된 확장 프로그램이 계속 파일을 찾게 한다.
pub fn migrate_legacy_dir() -> Option<String> {
    let new = base_dir();
    let old = home_dir().join(".claudly");
    if new.exists() || !old.is_dir() {
        return None;
    }
    if let Err(e) = std::fs::rename(&old, &new) {
        return Some(format!("옛 설정 폴더 이동 실패: {e}"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J", &old.to_string_lossy(), &new.to_string_lossy()])
            .creation_flags(0x0800_0000)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
    Some(format!("설정 폴더를 {} 로 옮겼다", new.to_string_lossy()))
}

/// POSIX 셸용 작은따옴표 인용
pub fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn b64(s: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(s.as_bytes())
}

pub fn now_iso() -> String {
    crate::util::local_now().to_rfc3339_opts(chrono::SecondsFormat::Secs, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quote_handles_single_quote() {
        assert_eq!(sh_quote("a'b"), "'a'\\''b'");
        assert_eq!(sh_quote("/home/x y"), "'/home/x y'");
    }

    #[test]
    fn decode_keeps_valid_utf8() {
        assert_eq!(decode_with("한글 ok".as_bytes(), true), "한글 ok");
        assert_eq!(decode_with("한글 ok".as_bytes(), false), "한글 ok");
    }

    #[test]
    fn decode_reads_cp949_on_windows_path() {
        let cp949 = [0xC7u8, 0xD1, 0xB1, 0xDB];
        assert_eq!(decode_with(&cp949, true), "한글");
        assert_ne!(decode_with(&cp949, false), "한글");
    }

    #[test]
    fn truncate_limits_chars() {
        let long = "가".repeat(OUTPUT_LIMIT + 5);
        let t = truncate(&long);
        assert!(t.contains("잘림"));
        assert_eq!(truncate("짧음"), "짧음");
    }
}

/// 현지 시각. macOS 에서 CoreFoundation 없이 쓰려고 libc localtime_r 로 시차를 구한다.
pub fn local_now() -> chrono::DateTime<chrono::FixedOffset> {
    let utc = chrono::Utc::now();
    let off = local_offset_secs(utc.timestamp());
    utc.with_timezone(&chrono::FixedOffset::east_opt(off).unwrap_or_else(|| chrono::FixedOffset::east_opt(0).unwrap()))
}

#[cfg(unix)]
fn local_offset_secs(ts: i64) -> i32 {
    unsafe {
        let t: libc::time_t = ts as libc::time_t;
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() { return 0; }
        tm.tm_gmtoff as i32
    }
}

#[cfg(not(unix))]
fn local_offset_secs(ts: i64) -> i32 {
    use chrono::TimeZone;
    chrono::Local.timestamp_opt(ts, 0).single().map(|d| d.offset().local_minus_utc()).unwrap_or(0)
}

/// Windows PowerShell 이 출력을 CLIXML(#< CLIXML, <Objs ...>)로 감싸 보낸 경우 일반 글로 푼다.
/// 감싸지 않은 글은 그대로 두고, <Objs> 덩어리는 그 안의 글(S 요소)만 남긴다. 진행률 기록은 버린다.
pub fn decode_clixml(s: &str) -> String {
    if !s.contains("#< CLIXML") {
        return s.to_string();
    }
    let s = s.replace("#< CLIXML\r\n", "").replace("#< CLIXML\n", "").replace("#< CLIXML", "");
    let mut out = String::new();
    let mut r = s.as_str();
    while let Some(start) = r.find("<Objs ") {
        out.push_str(&r[..start]);
        let rest = &r[start..];
        let end = rest.find("</Objs>").map(|e| e + 7).unwrap_or(rest.len());
        let block = &rest[..end];
        let mut b = block;
        let mut texts = String::new();
        while let Some(i) = b.find("<S S=\"") {
            b = &b[i..];
            let Some(gt) = b.find('>') else { break };
            let Some(e) = b.find("</S>") else { break };
            if e > gt {
                texts.push_str(&b[gt + 1..e]);
            }
            b = &b[e + 4..];
        }
        let t = texts
            .replace("_x000D__x000A_", "\n")
            .replace("_x000A_", "\n")
            .replace("_x000D_", "")
            .replace("&lt;", "<")
            .replace("&gt;", ">")
            .replace("&quot;", "\"")
            .replace("&apos;", "'")
            .replace("&amp;", "&");
        // PowerShell 이 붙이는 위치 안내(위치 줄·+ ~~~)는 원인 파악에 쓸모가 적어 뺀다
        let t = t
            .lines()
            .filter(|l| {
                let x = l.trim_start();
                !(x.starts_with("위치 줄:") || x.starts_with("At line:") || x.starts_with("+ ") || x == "+" || x.starts_with("~"))
            })
            .collect::<Vec<_>>()
            .join("\n");
        out.push_str(&t);
        r = &rest[end..];
    }
    out.push_str(r);
    out.trim().to_string()
}
