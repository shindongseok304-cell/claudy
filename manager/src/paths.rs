//! 데이터 폴더 위치. %USERPROFILE%\.claudy 가 기본이다.
//! .claudy 가 없고 .claudly 가 있으면 서버와 똑같이 옮긴다.
//! (.claudly 를 .claudy 로 이름 바꾸고, .claudly 에서 .claudy 로 가는 정션을 만든다)

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::model::{self, Model};

#[derive(Debug, Clone)]
pub struct DataDir {
    pub dir: PathBuf,
}

pub fn home_dir() -> PathBuf {
    env::var_os("USERPROFILE")
        .or_else(|| env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

impl DataDir {
    pub fn hosts_file(&self) -> PathBuf {
        self.dir.join("hosts.toml")
    }
    pub fn logs_dir(&self) -> PathBuf {
        self.dir.join("logs")
    }
    pub fn jobs_dir(&self) -> PathBuf {
        self.dir.join("jobs")
    }
}

/// link 에서 target 으로 가는 정션을 만든다. Windows 는 `cmd /C mklink /J`, 그 밖에는 시험용 심볼릭 링크.
fn make_junction(link: &Path, target: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(crate::proc::decode(&out.stdout).trim().to_string() + &crate::proc::decode(&out.stderr))
        }
    }
    #[cfg(not(windows))]
    {
        std::os::unix::fs::symlink(target, link).map_err(|e| e.to_string())
    }
}

/// 시작할 때 데이터 폴더를 정리하고 위치를 돌려준다. 두 번째 값은 사용자에게 알릴 문제 목록이다.
pub fn prepare(home: &Path) -> (DataDir, Vec<String>) {
    let new = home.join(".claudy");
    let old = home.join(".claudly");
    let mut notes = Vec::new();
    if new.exists() {
        return (DataDir { dir: new }, notes);
    }
    if old.exists() {
        match fs::rename(&old, &new) {
            Ok(()) => {
                if let Err(e) = make_junction(&old, &new) {
                    notes.push(format!(".claudly 정션을 만들지 못했다: {e}"));
                }
                return (DataDir { dir: new }, notes);
            }
            Err(e) => {
                notes.push(format!(".claudly 를 .claudy 로 옮기지 못했다: {e}"));
                return (DataDir { dir: old }, notes);
            }
        }
    }
    if let Err(e) = fs::create_dir_all(&new) {
        notes.push(format!(".claudy 폴더를 만들지 못했다: {e}"));
    } else if let Err(e) = model::save(&new.join("hosts.toml"), &Model::initial()) {
        notes.push(format!("기본 hosts.toml 을 만들지 못했다: {e}"));
    }
    (DataDir { dir: new }, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(name: &str) -> PathBuf {
        let b = std::env::temp_dir().join(format!("claudy-paths-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&b);
        fs::create_dir_all(&b).unwrap();
        b
    }

    #[test]
    fn moves_old_folder_and_leaves_link() {
        let b = base("move");
        fs::create_dir_all(b.join(".claudly")).unwrap();
        fs::write(b.join(".claudly").join("hosts.toml"), "x").unwrap();
        let (d, notes) = prepare(&b);
        assert!(notes.is_empty(), "{notes:?}");
        assert!(d.dir.ends_with(".claudy"));
        assert_eq!(fs::read_to_string(b.join(".claudy").join("hosts.toml")).unwrap(), "x");
        assert!(b.join(".claudly").exists(), "옛 이름은 새 폴더를 가리키는 링크로 남는다");
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn creates_default_when_none_exist() {
        let b = base("fresh");
        let (d, notes) = prepare(&b);
        assert!(notes.is_empty(), "{notes:?}");
        let m = model::load(&d.hosts_file()).unwrap().expect("hosts.toml 생성");
        assert_eq!(m.hosts[0].name, "this-pc");
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn existing_new_folder_is_kept_as_is() {
        let b = base("keep");
        fs::create_dir_all(b.join(".claudy")).unwrap();
        fs::create_dir_all(b.join(".claudly")).unwrap();
        let (d, notes) = prepare(&b);
        assert!(notes.is_empty());
        assert!(d.dir.ends_with(".claudy"));
        assert!(!b.join(".claudy").join("hosts.toml").exists());
        let _ = fs::remove_dir_all(&b);
    }
}
