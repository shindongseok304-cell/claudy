use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use serde::Deserialize;

use crate::safety;
use crate::util;

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(default, rename = "host")]
    pub hosts: Vec<HostCfg>,
    #[serde(default)]
    pub policy: Policy,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Policy {
    #[serde(default)]
    pub deny: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HostCfg {
    pub name: String,
    pub kind: String,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub claude: Option<String>,
    #[serde(default)]
    pub workdir: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub identity_file: Option<String>,
    /// ssh 전용. true 면 처음 보는 호스트 키를 자동으로 등록한다 (StrictHostKeyChecking=accept-new). 기본 false.
    #[serde(default)]
    pub accept_new_host_key: Option<bool>,
    /// Claude Code 권한: skip(기본) | accept_edits | default
    #[serde(default)]
    pub claude_permissions: Option<String>,
    /// ssh 전용. 접속할 때마다 명령보다 먼저 실행할 명령(예 cd ~/site, source ~/.profile). 설정 앱의 「접속 후 명령」
    #[serde(default)]
    pub startup: Option<String>,
    /// ssh 전용. true 면 접속한 계정에서 sudo 로 root 가 되어 실행한다(비밀번호 없는 sudo 가 필요). 설정 앱의 「root 권한으로 실행」
    #[serde(default)]
    pub run_as_root: Option<bool>,
    /// Windows 원격(shell=powershell) 전용. true 면 명령을 로그온한 사용자 화면 세션에서 실행한다.
    /// SSH 세션(세션 0)에서는 한글 같은 화면 프로그램 자동화가 멈추므로, 그런 일을 하는 PC 는 켠다
    #[serde(default)]
    pub desktop: Option<bool>,
    /// ssh 전용. 이 호스트로 바로 갈 수 없을 때 먼저 거쳐 갈 ssh 호스트 이름(hosts.toml 의 name). 설정 앱의 「경유 호스트」
    #[serde(default)]
    pub jump: Option<String>,
    /// ssh 전용. 호스트 키를 찾을 때 쓸 이름. 경유로 127.0.0.1 같은 주소에 붙을 때 키가 섞이지 않게 한다
    #[serde(default)]
    pub host_key_alias: Option<String>,
    /// ssh 전용. host 로 연결되지 않을 때 쓸 다른 주소(예: 집 안 주소가 host, 바깥 주소가 alt_host)
    #[serde(default)]
    pub alt_host: Option<String>,
    /// 불러온 뒤 채운다: 경유 호스트의 접속 정보
    #[serde(skip)]
    pub jump_via: Option<Box<HostCfg>>,
}

pub const PERMISSION_MODES: [&str; 3] = ["skip", "accept_edits", "default"];

/// 접속 후 명령 여러 줄을 && 로 잇는다. 빈 줄과 # 주석 줄은 뺀다. 한 줄이라도 실패하면 뒤 줄은 실행하지 않는다.
pub fn startup_chain(text: &str) -> Option<String> {
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')).collect();
    if lines.is_empty() { None } else { Some(lines.join(" &&\n")) }
}

impl HostCfg {
    /// 접속 후 명령을 원격 스크립트 앞에 붙인다. 실패하면 본 명령을 실행하지 않는다.
    pub fn with_startup(&self, script: String) -> String {
        if self.remote_windows() {
            let lines: Vec<String> = self
                .startup
                .as_deref()
                .unwrap_or("")
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(|l| format!("{l}\nif (-not $?) {{ exit 97 }}"))
                .collect();
            return if lines.is_empty() { script } else { format!("{}\n{script}", lines.join("\n")) };
        }
        match startup_chain(self.startup.as_deref().unwrap_or("")) {
            Some(s) => format!("{s} || exit 97\n{script}"),
            None => script,
        }
    }

    /// ssh 호스트가 Windows(PowerShell)인지. shell = "powershell" 로 정한다
    pub fn remote_windows(&self) -> bool {
        self.kind == "ssh" && self.shell.as_deref() == Some("powershell")
    }

    pub fn desktop_session(&self) -> bool {
        self.remote_windows() && self.desktop.unwrap_or(false)
    }

    pub fn as_root(&self) -> bool {
        self.kind == "ssh" && self.run_as_root.unwrap_or(false)
    }

    pub fn claude_bin(&self) -> &str {
        self.claude.as_deref().filter(|s| !s.is_empty()).unwrap_or("claude")
    }

    pub fn accept_new_host_key(&self) -> bool {
        self.accept_new_host_key.unwrap_or(false)
    }

    pub fn permission_mode(&self) -> &str {
        self.claude_permissions.as_deref().filter(|s| !s.is_empty()).unwrap_or("skip")
    }

    pub fn workdir(&self) -> Option<&str> {
        self.workdir.as_deref().filter(|s| !s.is_empty())
    }

    pub fn shell_name(&self) -> &str {
        self.shell
            .as_deref()
            .filter(|s| !s.is_empty())
            .unwrap_or(if cfg!(windows) { "powershell" } else if cfg!(target_os = "macos") { "zsh" } else { "bash" })
    }

    pub fn target_label(&self) -> String {
        if self.kind == "ssh" {
{
            let mut t = format!(
                "{}@{}:{}",
                self.user.as_deref().unwrap_or(""),
                self.host.as_deref().unwrap_or(""),
                self.port.unwrap_or(22)
            );
            if let Some(j) = self.jump.as_deref().filter(|s| !s.is_empty()) {
                t.push_str(&format!(" (경유 {j})"));
            }
            if self.remote_windows() {
                t.push_str(if self.desktop_session() { " [Windows, 화면 세션]" } else { " [Windows]" });
            }
            t
        }
        } else {
            format!("이 PC (shell: {})", self.shell_name())
        }
    }
}

fn this_pc() -> HostCfg {
    HostCfg {
        name: "this-pc".into(),
        kind: "local".into(),
        shell: Some(if cfg!(windows) { "powershell" } else if cfg!(target_os = "macos") { "zsh" } else { "bash" }.into()),
        claude: Some("claude".into()),
        workdir: Some(String::new()),
        host: None,
        port: None,
        user: None,
        identity_file: None,
        accept_new_host_key: None,
        claude_permissions: None,
        startup: None,
        run_as_root: None,
        desktop: None,
        jump: None,
        host_key_alias: None,
        alt_host: None,
        jump_via: None,
    }
}

pub fn default_config() -> Config {
    Config { hosts: vec![this_pc()], policy: Policy::default() }
}

pub fn default_text() -> String {
    let shell = if cfg!(windows) { "powershell" } else if cfg!(target_os = "macos") { "zsh" } else { "bash" };
    format!(
        r#"# Claudy 호스트 설정 (hosts.toml)
# 호스트를 추가하면 Claude 채팅에서 host 이름으로 지정할 수 있다.

[[host]]
name = "this-pc"
kind = "local"
shell = "{shell}"      # local 만: powershell | cmd | sh
claude = "claude"      # Claude Code 실행 이름
workdir = ""           # 비면 사용자 홈
# claude_permissions = "skip"   # Claude Code 권한. skip(기본, --dangerously-skip-permissions) | accept_edits(편집 자동 허용, --permission-mode acceptEdits) | default(권한 인자 없음, 승인 없이는 도구 사용 불가)

# ssh 호스트 예시 (주석을 풀고 값을 맞춘다)
# [[host]]
# name = "pi"
# kind = "ssh"
# host = "server.example.com"
# port = 22
# user = "user"
# identity_file = ""   # 비면 ssh 기본 키
# claude = "claude"
# workdir = ""
# accept_new_host_key = false   # true 면 처음 보는 서버의 호스트 키를 자동 등록한다 (기본 false)
# claude_permissions = "skip"   # 위 설명 참고

# 차단 목록을 바꾸려면 아래를 푼다. 지정하면 기본 목록을 대체한다.
# [policy]
# deny = ["(?i)\\bmkfs"]
"#
    )
}

pub struct Loaded {
    pub config: Config,
    pub warning: Option<String>,
    pub path: PathBuf,
}

pub fn path() -> PathBuf {
    util::base_dir().join("hosts.toml")
}

/// 설정을 읽는다. 파일이 없으면 기본 파일을 만든다. 오류가 있으면 this-pc 만으로 동작하고 경고를 남긴다.
pub fn load() -> Loaded {
    let path = path();
    if !path.exists() {
        let made = fs::create_dir_all(util::base_dir()).and_then(|_| fs::write(&path, default_text()));
        if let Err(e) = made {
            eprintln!("claudy: 기본 설정 파일 생성 실패: {e}");
        }
    }
    match parse_file(&path) {
        Ok(config) => Loaded { config, warning: None, path },
        Err(e) => {
            let warning = format!("설정 파일 오류로 기본값(this-pc)만 사용한다: {e}");
            eprintln!("claudy: {warning}");
            Loaded { config: default_config(), warning: Some(warning), path }
        }
    }
}

fn parse_file(path: &PathBuf) -> Result<Config, String> {
    let text = fs::read_to_string(path).map_err(|e| format!("읽기 실패: {e}"))?;
    let mut cfg: Config = toml::from_str(&text).map_err(|e| format!("TOML 해석 실패: {e}"))?;
    validate(&cfg)?;
    resolve_jumps(&mut cfg)?;
    Ok(cfg)
}

/// 경유 호스트 이름을 실제 접속 정보로 바꿔 둔다. 경유는 한 단계만 허용한다
fn resolve_jumps(cfg: &mut Config) -> Result<(), String> {
    let snapshot = cfg.hosts.clone();
    for h in cfg.hosts.iter_mut() {
        let Some(j) = h.jump.as_deref().filter(|s| !s.is_empty()) else { continue };
        if h.kind != "ssh" {
            return Err(format!("host `{}` 는 ssh 가 아니라 경유 호스트를 쓸 수 없다", h.name));
        }
        if j == h.name {
            return Err(format!("host `{}` 가 자기 자신을 경유한다", h.name));
        }
        let via = snapshot
            .iter()
            .find(|x| x.name == j)
            .ok_or_else(|| format!("host `{}` 의 경유 호스트 `{j}` 가 목록에 없다", h.name))?;
        if via.kind != "ssh" {
            return Err(format!("host `{}` 의 경유 호스트 `{j}` 는 ssh 호스트여야 한다", h.name));
        }
        if via.jump.as_deref().is_some_and(|s| !s.is_empty()) {
            return Err(format!("경유 호스트 `{j}` 가 또 다른 호스트를 경유한다(한 단계만 된다)"));
        }
        h.jump_via = Some(Box::new(via.clone()));
    }
    Ok(())
}

#[cfg(test)]
pub fn resolve_jumps_for_test(cfg: &mut Config) -> Result<(), String> {
    resolve_jumps(cfg)
}

fn validate(cfg: &Config) -> Result<(), String> {
    if cfg.hosts.is_empty() {
        return Err("[[host]] 항목이 하나도 없다".into());
    }
    let mut names = HashSet::new();
    for h in &cfg.hosts {
        if h.name.trim().is_empty() {
            return Err("host name 이 비어 있다".into());
        }
        if !names.insert(h.name.clone()) {
            return Err(format!("host name 이 중복되었다: {}", h.name));
        }
        match h.kind.as_str() {
            "local" => {
                if let Some(s) = h.shell.as_deref().filter(|s| !s.is_empty()) {
                    if !matches!(s, "powershell" | "cmd" | "sh" | "zsh" | "bash") {
                        return Err(format!("host `{}` 의 shell 값이 잘못되었다: {s}", h.name));
                    }
                }
            }
            "ssh" => {
                let host_ok = h.host.as_deref().is_some_and(|s| !s.is_empty());
                let user_ok = h.user.as_deref().is_some_and(|s| !s.is_empty());
                if !host_ok || !user_ok {
                    return Err(format!("ssh host `{}` 는 host 와 user 가 필요하다", h.name));
                }
                if let Some(s) = h.shell.as_deref().filter(|s| !s.is_empty()) {
                    if !matches!(s, "bash" | "powershell") {
                        return Err(format!("ssh host `{}` 의 shell 은 bash(리눅스·macOS) 또는 powershell(Windows) 이어야 한다: {s}", h.name));
                    }
                }
            }
            other => return Err(format!("host `{}` 의 kind 는 local 또는 ssh 여야 한다 (받은 값: {other})", h.name)),
        }
        if let Some(p) = h.claude_permissions.as_deref().filter(|s| !s.is_empty()) {
            if !PERMISSION_MODES.contains(&p) {
                return Err(format!("host `{}` 의 claude_permissions 값이 잘못되었다: {p} (skip, accept_edits, default 중 하나)", h.name));
            }
        }
    }
    if let Some(d) = &cfg.policy.deny {
        safety::compile_deny(d)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_text_parses_and_validates() {
        let cfg: Config = toml::from_str(&default_text()).expect("default parses");
        validate(&cfg).expect("default validates");
        assert_eq!(cfg.hosts.len(), 1);
        assert_eq!(cfg.hosts[0].name, "this-pc");
    }

    #[test]
    fn ssh_host_requires_fields() {
        let cfg: Config = toml::from_str("[[host]]\nname = \"pi\"\nkind = \"ssh\"\n").unwrap();
        assert!(validate(&cfg).is_err());
    }

    #[test]
    fn permission_mode_is_validated() {
        let ok: Config = toml::from_str("[[host]]\nname = \"a\"\nkind = \"local\"\nclaude_permissions = \"accept_edits\"\n").unwrap();
        validate(&ok).expect("accept_edits valid");
        assert_eq!(ok.hosts[0].permission_mode(), "accept_edits");
        let none: Config = toml::from_str("[[host]]\nname = \"a\"\nkind = \"local\"\n").unwrap();
        assert_eq!(none.hosts[0].permission_mode(), "skip");
        let bad: Config = toml::from_str("[[host]]\nname = \"a\"\nkind = \"local\"\nclaude_permissions = \"yolo\"\n").unwrap();
        assert!(validate(&bad).is_err());
    }

    #[test]
    fn accept_new_defaults_to_false() {
        let c: Config = toml::from_str("[[host]]\nname = \"pi\"\nkind = \"ssh\"\nhost = \"h\"\nuser = \"u\"\naccept_new_host_key = true\n").unwrap();
        assert!(c.hosts[0].accept_new_host_key());
        let d: Config = toml::from_str("[[host]]\nname = \"pi\"\nkind = \"ssh\"\nhost = \"h\"\nuser = \"u\"\n").unwrap();
        assert!(!d.hosts[0].accept_new_host_key());
    }

    #[test]
    fn bad_regex_rejected() {
        let cfg: Config = toml::from_str("[[host]]\nname = \"a\"\nkind = \"local\"\n[policy]\ndeny = [\"(\"]\n").unwrap();
        assert!(validate(&cfg).is_err());
    }
}
