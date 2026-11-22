//! 시험 격리 상태 (Windows)
//!
//! Claudy 서버가 격리 실행한 프로그램(숨은 화면·가상 데스크톱)의 목록과 감시기 상태를 보여 주고,
//! 사용자가 「모두 닫기」로 정리할 수 있게 한다. 대상 목록은 서버 도우미가 쓰는 ~/.claudy/isolate/pids.txt 다

use std::collections::HashMap;
use std::os::windows::process::CommandExt;
use std::path::Path;

use serde_json::{Value, json};
use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
use windows_sys::Win32::System::Threading::OpenMutexW;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// pid → (부모 pid, 실행 파일 이름)
fn procs() -> HashMap<u32, (u32, String)> {
    let mut m = HashMap::new();
    unsafe {
        let s = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if s == INVALID_HANDLE_VALUE {
            return m;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        if Process32FirstW(s, &mut e) != 0 {
            loop {
                let n = e.szExeFile.iter().position(|&c| c == 0).unwrap_or(e.szExeFile.len());
                m.insert(e.th32ProcessID, (e.th32ParentProcessID, String::from_utf16_lossy(&e.szExeFile[..n])));
                if Process32NextW(s, &mut e) == 0 {
                    break;
                }
            }
        }
        CloseHandle(s);
    }
    m
}

/// 격리 대상(pid, 방식). 방식은 「숨은 화면」 또는 가상 데스크톱 이름
fn watched(dir: &Path) -> Vec<(u32, String)> {
    std::fs::read_to_string(dir.join("isolate").join("pids.txt"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let pid = it.next()?.trim().parse().ok()?;
            let name = it.next().unwrap_or("").trim();
            Some((pid, if name == "@hidden" { "숨은 화면".to_string() } else { name.to_string() }))
        })
        .collect()
}

/// 살아 있는 격리 프로세스와 그 자손. (pid, 실행 파일, 방식)
fn alive(dir: &Path) -> Vec<(u32, String, String)> {
    let ps = procs();
    let w: HashMap<u32, String> = watched(dir).into_iter().collect();
    let mut out = Vec::new();
    for (&pid, (_, exe)) in &ps {
        let mut p = pid;
        for _ in 0..6 {
            if let Some(m) = w.get(&p) {
                out.push((pid, exe.clone(), m.clone()));
                break;
            }
            match ps.get(&p) {
                Some((pp, _)) if *pp > 4 => p = *pp,
                _ => break,
            }
        }
    }
    out.sort();
    out
}

fn guard_running() -> bool {
    let name: Vec<u16> = "Local\\ClaudyDeskGuard\0".encode_utf16().collect();
    unsafe {
        let h = OpenMutexW(0x0010_0000, 0, name.as_ptr());
        if h.is_null() {
            return false;
        }
        CloseHandle(h);
        true
    }
}

pub fn status(dir: &Path) -> Value {
    let list: Vec<Value> = alive(dir).into_iter().map(|(pid, exe, mode)| json!({"pid": pid, "exe": exe, "mode": mode})).collect();
    json!({"guard": guard_running(), "procs": list})
}

/// 격리 프로그램 모두 강제 종료. 닫은 프로세스 수
pub fn close_all(dir: &Path) -> usize {
    let mut n = 0;
    for (pid, _, _) in alive(dir) {
        let ok = std::process::Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if ok {
            n += 1;
        }
    }
    n
}
