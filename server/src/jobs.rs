use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime};

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::sync::mpsc;

use crate::exec::{self, Spawnable};
use crate::safety::LineRedactor;
use crate::util;

pub fn dir(base: &Path) -> PathBuf {
    base.join("jobs")
}

pub fn status_path(base: &Path, id: &str) -> PathBuf {
    dir(base).join(format!("{id}.json"))
}

pub fn log_path(base: &Path, id: &str) -> PathBuf {
    dir(base).join(format!("{id}.log"))
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric())
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()[..12].to_string()
}

fn write_status(path: &Path, v: &Value) {
    let tmp = path.with_extension("json.tmp");
    if fs::write(&tmp, v.to_string()).is_ok() {
        let _ = fs::rename(&tmp, path);
    }
}

/// 이 서버 프로세스 안에서 아직 돌고 있는 작업 id
fn running_here() -> &'static Mutex<HashSet<String>> {
    static RUNNING: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    RUNNING.get_or_init(|| Mutex::new(HashSet::new()))
}

fn is_running_here(id: &str) -> bool {
    running_here().lock().unwrap_or_else(|e| e.into_inner()).contains(id)
}

/// pid 프로세스가 살아 있는지 본다. Windows 는 OpenProcess + GetExitCodeProcess, Unix 는 kill(pid, 0) 을 쓴다.
#[cfg(windows)]
pub fn process_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError};
    use windows_sys::Win32::System::Threading::{GetExitCodeProcess, OpenProcess};
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const STILL_ACTIVE: u32 = 259;
    const ERROR_ACCESS_DENIED: u32 = 5;
    // SAFETY: OpenProcess 와 GetExitCodeProcess 는 인자만 읽고, 핸들은 아래에서 닫는다.
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            // 권한이 없어 열지 못한 경우는 프로세스가 있는 것이다
            return GetLastError() == ERROR_ACCESS_DENIED;
        }
        let mut code: u32 = 0;
        let ok = GetExitCodeProcess(h, &mut code);
        CloseHandle(h);
        ok != 0 && code == STILL_ACTIVE
    }
}

#[cfg(unix)]
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    // SAFETY: 신호 0 은 프로세스 존재 여부만 확인한다.
    let rc = unsafe { libc::kill(pid as i32, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// 상태가 끝나지 않은 채로 남았고, 이 서버에서 돌고 있지 않으며, pid 프로세스도 없으면 중단된 작업이다.
/// pid 가 없는 예전 상태 파일은 판단할 수 없으므로 중단으로 보지 않는다.
fn is_interrupted(status: &Value, here: bool, alive: impl Fn(u32) -> bool) -> bool {
    if status["finished"].as_bool() != Some(false) || here {
        return false;
    }
    match status["pid"].as_u64().and_then(|p| u32::try_from(p).ok()) {
        Some(pid) => !alive(pid),
        None => false,
    }
}

/// 중단된 작업이면 응답과 상태 파일을 끝난 것으로 고친다
fn reconcile(base: &Path, id: &str, status: &mut Value) {
    if !valid_id(id) || !is_interrupted(status, is_running_here(id), process_alive) {
        return;
    }
    status["finished"] = json!(true);
    status["interrupted"] = json!(true);
    if status["ended_at"].is_null() {
        status["ended_at"] = json!(util::now_iso());
    }
    write_status(&status_path(base, id), status);
}

pub fn read_status(base: &Path, id: &str) -> Result<Value, String> {
    if !valid_id(id) {
        return Err("job_id 형식이 올바르지 않다".into());
    }
    let text = fs::read_to_string(status_path(base, id)).map_err(|_| format!("작업을 찾을 수 없다: {id}"))?;
    let mut status: Value = serde_json::from_str(&text).map_err(|e| format!("상태 파일 해석 실패: {e}"))?;
    reconcile(base, id, &mut status);
    Ok(status)
}

/// 서버 시작 때 호출한다. 기준 시각보다 keep 만큼 오래된 .json, .log 파일을 지우고 지운 개수를 돌려준다.
pub fn cleanup_old(base: &Path, now: SystemTime, keep: Duration) -> usize {
    let Ok(entries) = fs::read_dir(dir(base)) else {
        return 0;
    };
    let mut removed = 0;
    for e in entries.flatten() {
        let p = e.path();
        if !matches!(p.extension().and_then(|x| x.to_str()), Some("json") | Some("log")) {
            continue;
        }
        let Ok(modified) = e.metadata().and_then(|m| m.modified()) else {
            continue;
        };
        let old = now.duration_since(modified).is_ok_and(|age| age > keep);
        if old && fs::remove_file(&p).is_ok() {
            removed += 1;
        }
    }
    removed
}

pub fn tail(path: &Path, n: usize) -> String {
    let text = fs::read_to_string(path).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}

/// 최근 작업 목록 (시작 시각 내림차순)
pub fn list(base: &Path, n: usize) -> Vec<Value> {
    let mut items: Vec<Value> = fs::read_dir(dir(base))
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.path().extension().and_then(|x| x.to_str()) == Some("json"))
        .filter_map(|e| fs::read_to_string(e.path()).ok())
        .filter_map(|t| serde_json::from_str::<Value>(&t).ok())
        .map(|mut v| {
            let id = v["job_id"].as_str().unwrap_or("").to_string();
            reconcile(base, &id, &mut v);
            v
        })
        .collect();
    items.sort_by(|a, b| b["started_at"].as_str().cmp(&a["started_at"].as_str()));
    items.truncate(n);
    items
}

async fn pump<R: AsyncRead + Unpin>(r: R, tx: mpsc::UnboundedSender<String>) {
    let mut reader = BufReader::new(r);
    let mut red = LineRedactor::default();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let raw = util::decode_bytes(&buf);
        let line = raw.trim_end_matches(['\r', '\n']);
        if let Some(out) = red.feed(line) {
            if tx.send(out).is_err() {
                break;
            }
        }
    }
}

/// 백그라운드 작업을 시작한다. 출력은 가린 뒤 로그 파일에 쓰고, 끝나면 상태 파일을 갱신한다.
pub fn start(base: &Path, id: &str, host: &str, kind: &str, plan: Spawnable) -> Result<(), String> {
    fs::create_dir_all(dir(base)).map_err(|e| format!("작업 폴더 생성 실패: {e}"))?;
    let spath = status_path(base, id);
    let lpath = log_path(base, id);
    fs::write(&lpath, "").map_err(|e| format!("로그 파일 생성 실패: {e}"))?;
    let mut status = json!({
        "job_id": id,
        "host": host,
        "kind": kind,
        "started_at": util::now_iso(),
        "finished": false,
        "exit_code": null,
        "ended_at": null,
        "pid": null,
    });
    write_status(&spath, &status);

    let mut child = match exec::spawn_child(&plan) {
        Ok(c) => c,
        Err(e) => {
            status["finished"] = json!(true);
            status["ended_at"] = json!(util::now_iso());
            status["spawn_error"] = json!(e.to_string());
            write_status(&spath, &status);
            return Err(format!("실행 실패 ({}): {e}", plan.program));
        }
    };
    exec::feed_stdin(&mut child, plan.stdin.as_deref());
    status["pid"] = json!(child.id());
    write_status(&spath, &status);
    running_here().lock().unwrap_or_else(|e| e.into_inner()).insert(id.to_string());
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let id = id.to_string();

    tokio::spawn(async move {
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        let mut readers = Vec::new();
        if let Some(s) = stdout {
            readers.push(tokio::spawn(pump(s, tx.clone())));
        }
        if let Some(s) = stderr {
            readers.push(tokio::spawn(pump(s, tx.clone())));
        }
        drop(tx);
        let lp = lpath.clone();
        let writer = tokio::spawn(async move {
            let Ok(mut f) = tokio::fs::OpenOptions::new().append(true).create(true).open(&lp).await else {
                return;
            };
            while let Some(line) = rx.recv().await {
                let _ = f.write_all(format!("{line}\n").as_bytes()).await;
            }
            let _ = f.flush().await;
        });
        let code = child.wait().await.ok().and_then(|s| s.code());
        for r in readers {
            let _ = r.await;
        }
        let _ = writer.await;
        status["finished"] = json!(true);
        status["exit_code"] = json!(code);
        status["ended_at"] = json!(util::now_iso());
        write_status(&spath, &status);
        running_here().lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_safe() {
        assert!(valid_id(&new_id()));
        assert!(!valid_id("../etc"));
        assert!(!valid_id(""));
    }

    #[test]
    fn own_process_is_alive_and_huge_pid_is_not() {
        assert!(process_alive(std::process::id()));
        assert!(!process_alive(999_999_999));
    }

    #[test]
    fn unfinished_job_with_dead_pid_is_interrupted() {
        let st = json!({"finished": false, "pid": 999999});
        assert!(is_interrupted(&st, false, |_| false));
        assert!(!is_interrupted(&st, false, |_| true), "살아 있는 pid 는 실행 중");
        assert!(!is_interrupted(&st, true, |_| false), "이 서버에서 돌고 있는 작업은 살아 있다");
        let done = json!({"finished": true, "pid": 999999});
        assert!(!is_interrupted(&done, false, |_| false));
        let old = json!({"finished": false});
        assert!(!is_interrupted(&old, false, |_| false), "pid 없는 예전 파일은 판단하지 않는다");
    }

    #[test]
    fn cleanup_removes_only_old_json_and_log() {
        let base = std::env::temp_dir().join(format!("claudy-cleanup-{}", new_id()));
        fs::create_dir_all(dir(&base)).unwrap();
        fs::write(dir(&base).join("a.json"), "{}").unwrap();
        fs::write(dir(&base).join("a.log"), "x").unwrap();
        fs::write(dir(&base).join("keep.txt"), "x").unwrap();
        let later = SystemTime::now() + Duration::from_secs(15 * 86400);
        let removed = cleanup_old(&base, later, Duration::from_secs(14 * 86400));
        assert_eq!(removed, 2);
        assert!(dir(&base).join("keep.txt").exists());
        assert!(!dir(&base).join("a.json").exists());
        assert_eq!(cleanup_old(&base, SystemTime::now(), Duration::from_secs(14 * 86400)), 0);
        let _ = fs::remove_dir_all(&base);
    }
}
