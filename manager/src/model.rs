//! hosts.toml 읽기·검사·쓰기. 필드 이름과 값 형식은 Claudy 서버(config.rs)와 같다.
//! 모르는 항목은 extra 와 raw 에 그대로 담아 다시 쓸 때 보존한다.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use toml::{Table, Value};

pub const PERMISSION_MODES: [&str; 3] = ["skip", "accept_edits", "default"];
pub const SHELLS: [&str; 5] = ["powershell", "cmd", "sh", "zsh", "bash"];

/// 서버 기본 차단 목록 (safety.rs DEFAULT_DENY 와 같다)
pub const DEFAULT_DENY: [&str; 5] = [
    r"(?i)\brm\s+-rf\s+/(\s|$)",
    r"(?i)\bmkfs",
    r"(?i)\b(shutdown|reboot|poweroff)\b",
    r"(?i)format\s+[a-z]:",
    r"(?i)Remove-Item\s+.*-Recurse.*\s[A-Za-z]:\\?\s*$",
];

const KNOWN_HOST_KEYS: [&str; 15] = [
    "name",
    "kind",
    "shell",
    "claude",
    "workdir",
    "host",
    "port",
    "user",
    "identity_file",
    "accept_new_host_key",
    "claude_permissions",
    "startup",
    "run_as_root",
    "jump",
    "desktop",
];

pub const HEADER: &str = "\
# Claudy 호스트 설정 (hosts.toml). Claudy 설정 프로그램이 쓴다. 손으로 고쳐도 된다.
# 모르는 항목은 설정 프로그램이 그대로 보존한다.
# name: 호스트 이름. Claude 채팅에서 이 이름으로 호스트를 고른다.
# kind: local(이 PC) 또는 ssh
# shell: 이 PC에서 명령을 실행할 셸. powershell | cmd | sh | zsh | bash
# claude: Claude Code 실행 이름 또는 경로
# workdir: 작업 폴더. 비우면 사용자 홈
# host, port, user, identity_file: ssh 접속 정보. identity_file 을 비우면 ssh 기본 키를 쓴다
# accept_new_host_key: true 면 처음 보는 서버의 호스트 키를 자동 등록한다
# startup: ssh 만. 접속할 때마다 명령보다 먼저 실행할 명령(예 cd ~/site)
# run_as_root: ssh 만. true 면 접속 계정에서 sudo 로 root 가 되어 실행한다(비밀번호 없는 sudo 필요)
# shell(ssh): 원격 셸. 비우거나 bash 면 Linux·macOS, powershell 이면 Windows
# jump: ssh 만. 먼저 거쳐 갈 ssh 호스트 이름(경유 호스트)
# desktop: Windows 원격만. true 면 로그온한 사용자 화면 세션에서 실행한다(한글 같은 화면 프로그램 자동화)
# claude_permissions: skip(전체 허용) | accept_edits(파일 편집만 자동 허용) | default(모두 확인)
# [policy] deny: 차단 명령 정규식 목록. 비우면 기본 목록을 쓴다. 실수 방지용이며 보안 경계가 아니다.

";

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Host {
    pub name: String,
    pub kind: String,
    pub shell: String,
    pub claude: String,
    pub workdir: String,
    pub host: String,
    pub port: String,
    pub user: String,
    pub identity_file: String,
    pub accept_new_host_key: bool,
    pub claude_permissions: String,
    pub startup: String,
    pub run_as_root: bool,
    /// 경유 호스트 이름(ssh). 비면 바로 접속
    pub jump: String,
    /// Windows 원격을 화면 세션에서 실행
    pub desktop: bool,
    /// 경유 호스트의 접속 정보(불러온 뒤 채운다. 파일에는 쓰지 않는다)
    pub jump_via: Option<Box<Host>>,
    /// 알 수 없는 항목. 다시 쓸 때 그대로 돌려준다.
    pub extra: Table,
}

fn default_shell() -> &'static str {
    if cfg!(windows) { "powershell" } else if cfg!(target_os = "macos") { "zsh" } else { "bash" }
}

impl Host {
    /// 호스트 키를 찾을 때 쓸 이름(host_key_alias, 알 수 없는 항목으로 보존된다)
    pub fn host_key_alias(&self) -> Option<String> {
        self.extra.get("host_key_alias").and_then(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(String::from)
    }

    /// Windows 원격(원격 셸 powershell)인지
    pub fn remote_windows(&self) -> bool {
        self.kind.trim() == "ssh" && self.shell.trim() == "powershell"
    }

    /// 경유 호스트 이름을 목록에서 찾아 jump_via 에 채운다
    pub fn with_jump(mut self, hosts: &[Host]) -> Host {
        let j = self.jump.trim().to_string();
        if !j.is_empty() && j != self.name.trim() {
            self.jump_via = hosts.iter().find(|x| x.name.trim() == j && x.kind.trim() == "ssh").map(|x| Box::new(x.clone()));
        }
        self
    }

    pub fn this_pc() -> Host {
        Host {
            name: "this-pc".into(),
            kind: "local".into(),
            shell: default_shell().into(),
            claude: "claude".into(),
            ..Default::default()
        }
    }

    pub fn from_table(t: &Table) -> Host {
        let s = |k: &str| t.get(k).and_then(Value::as_str).unwrap_or("").to_string();
        let mut extra = t.clone();
        for k in KNOWN_HOST_KEYS {
            extra.remove(k);
        }
        Host {
            name: s("name"),
            kind: s("kind"),
            shell: s("shell"),
            claude: s("claude"),
            workdir: s("workdir"),
            host: s("host"),
            port: t.get("port").and_then(Value::as_integer).map(|p| p.to_string()).unwrap_or_default(),
            user: s("user"),
            identity_file: s("identity_file"),
            accept_new_host_key: t.get("accept_new_host_key").and_then(Value::as_bool).unwrap_or(false),
            claude_permissions: s("claude_permissions"),
            startup: s("startup"),
            run_as_root: t.get("run_as_root").and_then(Value::as_bool).unwrap_or(false),
            jump: s("jump"),
            desktop: t.get("desktop").and_then(Value::as_bool).unwrap_or(false),
            jump_via: None,
            extra,
        }
    }

    /// 칸 하나에 붙일 검사 결과. (칸 이름, 메시지). 이름 중복은 호스트 목록이 필요해 따로 본다.
    pub fn field_errors(&self) -> Vec<(&'static str, String)> {
        let mut errs = Vec::new();
        if self.name.trim().is_empty() {
            errs.push(("name", "이름을 적어 주세요.".to_string()));
        }
        match self.kind.trim() {
            "local" => {
                let s = self.shell.trim();
                if !s.is_empty() && !SHELLS.contains(&s) {
                    errs.push(("shell", format!("셸 값이 잘못되었습니다: {s}")));
                }
            }
            "ssh" => {
                if self.host.trim().is_empty() {
                    errs.push(("host", "호스트 주소를 적어 주세요.".to_string()));
                }
                if self.user.trim().is_empty() {
                    errs.push(("user", "사용자 이름을 적어 주세요.".to_string()));
                }
                let s = self.shell.trim();
                if !s.is_empty() && s != "bash" && s != "powershell" {
                    errs.push(("shell", format!("원격 셸은 bash 또는 powershell 이어야 합니다 ({s})")));
                }
                if self.jump.trim() == self.name.trim() && !self.jump.trim().is_empty() {
                    errs.push(("jump", "자기 자신을 경유할 수 없습니다.".to_string()));
                }
            }
            other => errs.push(("kind", format!("연결 종류는 이 PC 또는 SSH 여야 합니다 ({other})"))),
        }
        let p = self.port.trim();
        if !p.is_empty() && !matches!(p.parse::<u32>(), Ok(v) if (1..=65535).contains(&v)) {
            errs.push(("port", "포트는 1~65535 사이의 숫자여야 합니다.".to_string()));
        }
        let perm = self.claude_permissions.trim();
        if !perm.is_empty() && !PERMISSION_MODES.contains(&perm) {
            errs.push(("claude_permissions", format!("권한 값이 잘못되었습니다: {perm}")));
        }
        errs
    }

    pub fn to_table(&self) -> Table {
        let mut t = self.extra.clone();
        let put = |t: &mut Table, k: &str, v: &str| {
            let v = v.trim();
            if v.is_empty() {
                t.remove(k);
            } else {
                t.insert(k.to_string(), Value::String(v.to_string()));
            }
        };
        t.insert("name".into(), Value::String(self.name.trim().to_string()));
        t.insert("kind".into(), Value::String(self.kind.trim().to_string()));
        put(&mut t, "shell", &self.shell);
        put(&mut t, "claude", &self.claude);
        put(&mut t, "workdir", &self.workdir);
        put(&mut t, "host", &self.host);
        put(&mut t, "user", &self.user);
        put(&mut t, "identity_file", &self.identity_file);
        put(&mut t, "claude_permissions", &self.claude_permissions);
        put(&mut t, "startup", &self.startup);
        if self.kind.trim() == "ssh" {
            put(&mut t, "jump", &self.jump);
        } else {
            t.remove("jump");
        }
        if self.desktop && self.kind.trim() == "ssh" && self.shell.trim() == "powershell" {
            t.insert("desktop".into(), Value::Boolean(true));
        } else {
            t.remove("desktop");
        }
        if self.run_as_root {
            t.insert("run_as_root".into(), Value::Boolean(true));
        } else {
            t.remove("run_as_root");
        }
        match self.port.trim().parse::<i64>() {
            Ok(p) => {
                t.insert("port".into(), Value::Integer(p));
            }
            Err(_) => {
                t.remove("port");
            }
        }
        if self.kind.trim() == "ssh" {
            t.insert("accept_new_host_key".into(), Value::Boolean(self.accept_new_host_key));
        }
        t
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Model {
    pub hosts: Vec<Host>,
    /// None 이면 [policy] deny 를 쓰지 않는다(서버 기본 목록 사용)
    pub deny: Option<Vec<String>>,
    /// 읽어 온 문서 전체. host 를 뺀 나머지 항목을 보존하는 데 쓴다.
    pub raw: Table,
}

/// 여러 줄 입력을 차단 목록으로 바꾼다. 빈 줄은 무시하고, 아무것도 없으면 None.
pub fn deny_from_text(text: &str) -> Option<Vec<String>> {
    let v: Vec<String> = text.lines().map(str::trim).filter(|l| !l.is_empty()).map(String::from).collect();
    if v.is_empty() { None } else { Some(v) }
}

/// 편집 칸 문자열을 차단 목록으로 바꾼다. 기본 목록과 같거나 비었으면 None(서버 기본 목록 사용).
pub fn deny_from_edit(text: &str) -> Option<Vec<String>> {
    match deny_from_text(text) {
        Some(v) if v.iter().map(String::as_str).eq(DEFAULT_DENY.iter().copied()) => None,
        other => other,
    }
}

/// 차단 목록을 편집 칸에 보일 문자열로 바꾼다. None 이면 기본 목록을 보인다.
pub fn deny_to_edit(deny: &Option<Vec<String>>) -> String {
    match deny {
        Some(v) => v.join("\n"),
        None => DEFAULT_DENY.join("\n"),
    }
}

pub fn deny_to_text(deny: &Option<Vec<String>>) -> String {
    deny.as_ref().map(|v| v.join("\n")).unwrap_or_default()
}

impl Model {
    pub fn initial() -> Model {
        Model { hosts: vec![Host::this_pc()], deny: None, raw: Table::new() }
    }

    pub fn parse(text: &str) -> Result<Model, String> {
        let doc: Table = toml::from_str(text).map_err(|e| format!("TOML 해석 실패: {e}"))?;
        let hosts = match doc.get("host") {
            None => Vec::new(),
            Some(Value::Array(arr)) => {
                let mut out = Vec::new();
                for v in arr {
                    let t = v.as_table().ok_or_else(|| "[[host]] 항목이 표가 아니다".to_string())?;
                    out.push(Host::from_table(t));
                }
                out
            }
            Some(_) => return Err("host 는 [[host]] 배열이어야 한다".into()),
        };
        let deny = match doc.get("policy") {
            None => None,
            Some(Value::Table(p)) => match p.get("deny") {
                None => None,
                Some(Value::Array(a)) => {
                    let mut out = Vec::new();
                    for v in a {
                        out.push(v.as_str().ok_or_else(|| "deny 는 문자열 목록이어야 한다".to_string())?.to_string());
                    }
                    Some(out)
                }
                Some(_) => return Err("deny 는 문자열 목록이어야 한다".into()),
            },
            Some(_) => return Err("policy 는 표여야 한다".into()),
        };
        Ok(Model { hosts, deny, raw: doc })
    }

    fn to_document(&self) -> Table {
        let mut doc = self.raw.clone();
        doc.remove("host");
        let arr: Vec<Value> = self.hosts.iter().map(|h| Value::Table(h.to_table())).collect();
        doc.insert("host".into(), Value::Array(arr));

        let mut policy = match doc.remove("policy") {
            Some(Value::Table(p)) => p,
            _ => Table::new(),
        };
        match &self.deny {
            Some(list) => {
                let arr = list.iter().map(|s| Value::String(s.clone())).collect();
                policy.insert("deny".into(), Value::Array(arr));
            }
            None => {
                policy.remove("deny");
            }
        }
        if !policy.is_empty() {
            doc.insert("policy".into(), Value::Table(policy));
        }
        doc
    }

    pub fn to_text(&self) -> Result<String, String> {
        let body = toml::to_string(&self.to_document()).map_err(|e| format!("TOML 쓰기 실패: {e}"))?;
        Ok(format!("{HEADER}{body}"))
    }

    /// 저장 전 검사. 빨간 상태줄에 그대로 보인다.
    pub fn validate(&self) -> Vec<String> {
        let mut errs = Vec::new();
        if self.hosts.is_empty() {
            errs.push("호스트가 하나도 없다. 최소 한 개가 있어야 한다.".to_string());
        }
        let mut seen = HashSet::new();
        for h in &self.hosts {
            let name = h.name.trim();
            if name.is_empty() {
                errs.push("이름이 비어 있는 호스트가 있다.".to_string());
                continue;
            }
            if !seen.insert(name) {
                errs.push(format!("이름이 중복되었다: {name}"));
            }
            match h.kind.trim() {
                "local" => {
                    let s = h.shell.trim();
                    if !s.is_empty() && !SHELLS.contains(&s) {
                        errs.push(format!("`{name}` 의 셸 값이 잘못되었다: {s}"));
                    }
                }
                "ssh" => {
                    if h.host.trim().is_empty() || h.user.trim().is_empty() {
                        errs.push(format!("ssh 호스트 `{name}` 은 호스트 이름과 사용자가 필요하다."));
                    }
                }
                other => errs.push(format!("`{name}` 의 종류는 local 또는 ssh 여야 한다 (현재: {other})")),
            }
            let p = h.port.trim();
            if !p.is_empty() && !matches!(p.parse::<u32>(), Ok(v) if (1..=65535).contains(&v)) {
                errs.push(format!("`{name}` 의 포트는 1~65535 사이의 숫자여야 한다."));
            }
            let j = h.jump.trim();
            if !j.is_empty() {
                match self.hosts.iter().find(|x| x.name.trim() == j) {
                    None => errs.push(format!("`{name}` 의 경유 호스트 `{j}` 가 목록에 없다.")),
                    Some(x) if x.kind.trim() != "ssh" || !x.jump.trim().is_empty() || j == name => {
                        errs.push(format!("`{name}` 의 경유 호스트 `{j}` 는 바로 접속하는 ssh 호스트여야 한다."))
                    }
                    _ => {}
                }
            }
            let perm = h.claude_permissions.trim();
            if !perm.is_empty() && !PERMISSION_MODES.contains(&perm) {
                errs.push(format!("`{name}` 의 권한 값이 잘못되었다: {perm}"));
            }
        }
        if let Some(list) = &self.deny {
            for p in list {
                if let Err(e) = regex::Regex::new(p) {
                    errs.push(format!("차단 정규식 오류 `{p}`: {e}"));
                }
            }
        }
        errs
    }
}

/// 파일이 없으면 None. 있으면 읽어서 Model 로 만든다.
pub fn load(path: &Path) -> Result<Option<Model>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Model::parse(&text).map(Some),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(format!("읽기 실패: {e}")),
    }
}

/// 쓰기 전에 기존 파일을 hosts.toml.bak 로 복사한다.
pub fn save(path: &Path, model: &Model) -> Result<(), String> {
    save_with(path, model, true)
}

/// backup 이 거짓이면 .bak 을 새로 만들지 않는다(세션 첫 저장에서만 백업하는 데 쓴다).
pub fn save_with(path: &Path, model: &Model, backup: bool) -> Result<(), String> {
    let text = model.to_text()?;
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir).map_err(|e| format!("폴더 만들기 실패: {e}"))?;
    }
    if backup && path.exists() {
        let bak = path.with_file_name("hosts.toml.bak");
        fs::copy(path, &bak).map_err(|e| format!("백업 실패: {e}"))?;
    }
    fs::write(path, text).map_err(|e| format!("쓰기 실패: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("claudy-manager-test-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    const SAMPLE: &str = r#"
# 예전 주석
top_unknown = "keep"

[[host]]
name = "this-pc"
kind = "local"
shell = "powershell"
claude = "claude"
future_flag = true

[[host]]
name = "pi"
kind = "ssh"
host = "example.invalid"
port = 2222
user = "tester"
identity_file = "C:\\keys\\id"
accept_new_host_key = true
claude_permissions = "accept_edits"
note_for_later = "x"

[policy]
deny = ["(?i)\\bmkfs"]
other_policy = 1
"#;

    #[test]
    fn round_trip_keeps_unknown_fields_and_writes_header() {
        let m = Model::parse(SAMPLE).expect("parse");
        assert_eq!(m.hosts.len(), 2);
        assert_eq!(m.hosts[1].port, "2222");
        let text = m.to_text().expect("text");
        assert!(text.starts_with("# Claudy 호스트 설정"));
        let back = Model::parse(&text).expect("reparse");
        assert_eq!(back.hosts[0].extra.get("future_flag"), Some(&Value::Boolean(true)));
        assert_eq!(back.hosts[1].extra.get("note_for_later"), Some(&Value::String("x".into())));
        assert_eq!(back.raw.get("top_unknown"), Some(&Value::String("keep".into())));
        assert_eq!(back.deny, Some(vec!["(?i)\\bmkfs".to_string()]));
        assert_eq!(back.hosts, m.hosts);
        let policy = back.raw.get("policy").and_then(Value::as_table).unwrap();
        assert_eq!(policy.get("other_policy"), Some(&Value::Integer(1)));
    }

    #[test]
    fn empty_fields_are_not_written() {
        let mut m = Model::initial();
        m.hosts[0].workdir = "   ".into();
        m.hosts[0].identity_file = String::new();
        let text = m.to_text().unwrap();
        assert!(!text.contains("\nworkdir ="));
        assert!(!text.contains("\nidentity_file ="));
        assert!(!text.contains("\n[policy]"));
    }

    #[test]
    fn deny_empty_removes_policy_table_and_text_round_trip() {
        let mut m = Model::parse(SAMPLE).unwrap();
        m.deny = None;
        let text = m.to_text().unwrap();
        assert!(!text.contains("\ndeny ="));
        assert!(text.contains("other_policy"));
        assert_eq!(deny_from_text("  \n\n"), None);
        assert_eq!(deny_from_text(" a \n\nb"), Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(deny_to_text(&Some(vec!["a".into(), "b".into()])), "a\nb");
    }

    #[test]
    fn deny_edit_treats_default_list_as_none() {
        let shown = deny_to_edit(&None);
        assert_eq!(shown.lines().count(), DEFAULT_DENY.len());
        assert_eq!(deny_from_edit(&shown), None);
        assert_eq!(deny_from_edit(""), None);
        assert_eq!(deny_from_edit("a\nb"), Some(vec!["a".to_string(), "b".to_string()]));
        assert_eq!(deny_to_edit(&Some(vec!["x".into()])), "x");
    }

    #[test]
    fn validation_rules() {
        let ok = Model::parse(SAMPLE).unwrap();
        assert!(ok.validate().is_empty(), "{:?}", ok.validate());

        let mut dup = ok.clone();
        dup.hosts[1].name = "this-pc".into();
        assert!(dup.validate().iter().any(|e| e.contains("중복")));

        let mut empty_name = ok.clone();
        empty_name.hosts[0].name = " ".into();
        assert!(empty_name.validate().iter().any(|e| e.contains("비어")));

        let mut no_user = ok.clone();
        no_user.hosts[1].user = String::new();
        assert!(no_user.validate().iter().any(|e| e.contains("사용자")));

        for bad in ["0", "65536", "abc", "-1"] {
            let mut m = ok.clone();
            m.hosts[1].port = bad.into();
            assert!(m.validate().iter().any(|e| e.contains("포트")), "port {bad}");
        }

        let mut perm = ok.clone();
        perm.hosts[0].claude_permissions = "yolo".into();
        assert!(perm.validate().iter().any(|e| e.contains("권한")));

        let mut re = ok.clone();
        re.deny = Some(vec!["(".into()]);
        assert!(re.validate().iter().any(|e| e.contains("정규식")));

        let mut none = ok.clone();
        none.hosts.clear();
        assert!(!none.validate().is_empty());
    }

    #[test]
    fn save_writes_backup_and_loads_back() {
        let dir = tmp("save");
        let path = dir.join("hosts.toml");
        fs::write(&path, SAMPLE).unwrap();
        let m = Model::parse(SAMPLE).unwrap();
        save(&path, &m).expect("save");
        let bak = dir.join("hosts.toml.bak");
        assert_eq!(fs::read_to_string(&bak).unwrap(), SAMPLE);
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.hosts, m.hosts);
        assert!(load(&dir.join("missing.toml")).unwrap().is_none());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn default_header_text_parses_with_server_fields() {
        let m = Model::initial();
        let t = m.to_text().unwrap();
        let back = Model::parse(&t).unwrap();
        assert_eq!(back.hosts[0].name, "this-pc");
        assert!(back.validate().is_empty());
    }
}
