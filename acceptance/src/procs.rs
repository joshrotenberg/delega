//! Process inspection through `ps`, signals, and the survivor check.
//!
//! Survivors are found by the case marker in the process table, independently
//! of anything `delega` reported. A process counts when its command line
//! carries the marker (every process the fakes start runs a script from the
//! case directory, and the relay passes paths in it to `perl`). The ledger the
//! fakes write maps a pid to a role for the report; a process the ledger does
//! not know is reported as `unrecorded`.

use std::path::Path;
use std::process::Command;
use std::time::Duration;

use rustix::process::{Pid, Signal, kill_process};

#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: i32,
    pub stat: String,
    pub command: String,
}

pub fn ps() -> Vec<Proc> {
    let Ok(out) = Command::new("/bin/ps")
        .args(["-axww", "-o", "pid=,stat=,command="])
        .output()
    else {
        return Vec::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let stat = it.next()?.to_owned();
            let command = it.collect::<Vec<_>>().join(" ");
            Some(Proc { pid, stat, command })
        })
        .collect()
}

/// `role pid` pairs the fakes wrote.
pub fn ledger(dir: &Path) -> Vec<(String, i32)> {
    std::fs::read_to_string(dir.join("pids"))
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let (role, pid) = l.split_once(' ')?;
            Some((role.to_owned(), pid.trim().parse().ok()?))
        })
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Survivor {
    pub role: String,
    pub pid: i32,
}

/// Live processes (zombies excluded) whose command line carries `marker`,
/// other than the pids in `except`.
pub fn survivors(marker: &str, dir: &Path, except: &[i32]) -> Vec<Survivor> {
    let ledger = ledger(dir);
    let mut found: Vec<Survivor> = ps()
        .into_iter()
        .filter(|p| !p.stat.starts_with('Z') && p.command.contains(marker))
        .filter(|p| !except.contains(&p.pid))
        .map(|p| Survivor {
            role: ledger
                .iter()
                .find(|(_, pid)| *pid == p.pid)
                .map_or_else(|| "unrecorded".to_owned(), |(r, _)| r.clone()),
            pid: p.pid,
        })
        .collect();
    found.sort_by_key(|s| s.pid);
    found
}

/// Several samples a short time apart, merged by pid. A relay process lives a
/// few milliseconds, so one listing can miss it.
pub fn survivors_sampled(marker: &str, dir: &Path, except: &[i32], samples: u32) -> Vec<Survivor> {
    let mut all: Vec<Survivor> = Vec::new();
    for i in 0..samples {
        if i > 0 {
            std::thread::sleep(Duration::from_millis(60));
        }
        for s in survivors(marker, dir, except) {
            if !all.iter().any(|a| a.pid == s.pid) {
                all.push(s);
            }
        }
    }
    all
}

/// The distinct roles, sorted, as a list of labels.
pub fn roles(list: &[Survivor]) -> Vec<String> {
    let mut roles: Vec<String> = list.iter().map(|s| s.role.clone()).collect();
    roles.sort();
    roles.dedup();
    roles
}

pub fn signal(pid: i32, sig: Signal) {
    if let Some(pid) = Pid::from_raw(pid) {
        let _ = kill_process(pid, sig);
    }
}

pub fn kill_all(list: &[Survivor]) {
    for s in list {
        signal(s.pid, Signal::KILL);
    }
}

/// Kill every process that carries `marker` until none is found in three
/// listings in a row, for at most `limit`. Returns what was still alive at
/// the end (a process the harness could not kill).
pub fn sweep(marker: &str, dir: &Path, except: &[i32], limit: Duration) -> Vec<Survivor> {
    let began = std::time::Instant::now();
    let mut quiet = 0;
    loop {
        let found = survivors(marker, dir, except);
        if found.is_empty() {
            quiet += 1;
            if quiet >= 3 {
                return found;
            }
        } else {
            quiet = 0;
            kill_all(&found);
        }
        if began.elapsed() > limit {
            return survivors(marker, dir, except);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
