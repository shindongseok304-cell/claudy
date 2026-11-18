use regex::Regex;
use std::sync::OnceLock;

/// 기본 차단 목록(hosts.toml 의 [policy] deny 로 대체 가능)
pub const DEFAULT_DENY: [&str; 5] = [
    r"(?i)\brm\s+-rf\s+/(\s|$)",
    r"(?i)\bmkfs",
    r"(?i)\b(shutdown|reboot|poweroff)\b",
    r"(?i)format\s+[a-z]:",
    r"(?i)Remove-Item\s+.*-Recurse.*\s[A-Za-z]:\\?\s*$",
];

pub fn compile_deny(patterns: &[String]) -> Result<Vec<Regex>, String> {
    patterns
        .iter()
        .map(|p| Regex::new(p).map_err(|e| format!("차단 정규식 오류 `{p}`: {e}")))
        .collect()
}

/// 명령이 차단 목록에 걸리면 사유를 돌려준다
pub fn check_deny(rules: &[Regex], text: &str) -> Option<String> {
    rules
        .iter()
        .find(|r| r.is_match(text))
        .map(|r| format!("차단 목록에 걸려 실행하지 않았다: {}", r.as_str()))
}

struct Pats {
    private_key: Regex,
    key_begin: Regex,
    api_key: Regex,
    kv: Regex,
}

fn pats() -> &'static Pats {
    static P: OnceLock<Pats> = OnceLock::new();
    P.get_or_init(|| Pats {
        private_key: Regex::new(r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?(?:-----END [A-Z ]*PRIVATE KEY-----|\z)")
            .expect("regex"),
        key_begin: Regex::new(r"-----BEGIN [A-Z ]*PRIVATE KEY-----").expect("regex"),
        api_key: Regex::new(r"sk-ant-[A-Za-z0-9_-]+").expect("regex"),
        kv: Regex::new(r"(?i)((?:password|passwd|pwd|token|secret|api[_-]?key)\s*[:=]\s*)\S+").expect("regex"),
    })
}

/// 출력과 로그에서 비밀 값을 가린다
pub fn redact(text: &str) -> String {
    let p = pats();
    let t: String = p.private_key.replace_all(text, "***").into_owned();
    let t: String = p.api_key.replace_all(&t, "***").into_owned();
    p.kv.replace_all(&t, "${1}***").into_owned()
}

/// 한 줄씩 들어오는 로그용. 개인 키 블록은 BEGIN 줄부터 END 줄까지 통째로 가린다
#[derive(Default)]
pub struct LineRedactor {
    in_key: bool,
}

impl LineRedactor {
    pub fn feed(&mut self, line: &str) -> Option<String> {
        if self.in_key {
            if line.contains("-----END ") && line.contains("PRIVATE KEY-----") {
                self.in_key = false;
            }
            return None;
        }
        if pats().key_begin.is_match(line) {
            if !line.contains("-----END ") {
                self.in_key = true;
            }
            return Some("***".to_string());
        }
        Some(redact(line))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules() -> Vec<Regex> {
        let v: Vec<String> = DEFAULT_DENY.iter().map(|s| s.to_string()).collect();
        compile_deny(&v).unwrap()
    }

    #[test]
    fn deny_blocks_dangerous_commands() {
        let r = rules();
        assert!(check_deny(&r, "rm -rf /").is_some());
        assert!(check_deny(&r, "sudo rm -rf /  ").is_some());
        assert!(check_deny(&r, "mkfs.ext4 /dev/sda").is_some());
        assert!(check_deny(&r, "shutdown -h now").is_some());
        assert!(check_deny(&r, "format D:").is_some());
        assert!(check_deny(&r, "Remove-Item -Recurse C:\\").is_some());
        assert!(check_deny(&r, "Remove-Item C:\\data -Recurse").is_none());
        assert!(check_deny(&r, "rm -rf ./build").is_none());
        assert!(check_deny(&r, "echo hello").is_none());
    }

    #[test]
    fn redact_masks_secrets() {
        assert_eq!(redact("key sk-ant-abc_DEF-123 end"), "key *** end");
        assert_eq!(redact("password=hunter2"), "password=***");
        assert_eq!(redact("API_KEY: xyz"), "API_KEY: ***");
        let pem = "a\n-----BEGIN RSA PRIVATE KEY-----\nMIIB\n-----END RSA PRIVATE KEY-----\nb";
        assert_eq!(redact(pem), "a\n***\nb");
        let open = "-----BEGIN OPENSSH PRIVATE KEY-----\nAAA";
        assert_eq!(redact(open), "***");
    }

    #[test]
    fn line_redactor_masks_key_block() {
        let mut r = LineRedactor::default();
        let lines = ["x", "-----BEGIN PRIVATE KEY-----", "MII", "-----END PRIVATE KEY-----", "y"];
        let out: Vec<String> = lines.iter().filter_map(|l| r.feed(l)).collect();
        assert_eq!(out, vec!["x", "***", "y"]);
    }
}
