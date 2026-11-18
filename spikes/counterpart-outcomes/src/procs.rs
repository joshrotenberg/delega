//! Process inspection through `ps`, and the survivor check.

use std::{fs, path::Path, process::Command};

#[derive(Debug, Clone)]
pub struct Proc {
    pub pid: i32,
    pub stat: String,
    pub command: String,
}

pub fn ps() -> Vec<Proc> {
    let out = Command::new("ps")
        .args(["-axww", "-o", "pid=,ppid=,pgid=,stat=,command="])
        .output()
        .expect("run ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|line| {
            let mut it = line.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let _ppid: i32 = it.next()?.parse().ok()?;
            let _pgid: i32 = it.next()?.parse().ok()?;
            let stat = it.next()?.to_owned();
            let command = it.collect::<Vec<_>>().join(" ");
            Some(Proc { pid, stat, command })
        })
        .collect()
}

/// Role and pid pairs the fake providers wrote.
pub fn ledger(dir: &Path) -> Vec<(String, i32)> {
    fs::read_to_string(dir.join("pids"))
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
    pub zombie: bool,
}

impl Survivor {
    pub fn label(&self) -> String {
        if self.zombie {
            format!("{}(zombie)", self.role)
        } else {
            self.role.clone()
        }
    }
}

/// Live processes started by the fake provider for one case. A process counts
/// when its pid is in the ledger and its command line still carries the case
/// marker (this rules out pid reuse), or when its command line carries the
/// marker without being in the ledger.
pub fn survivors(marker: &str, dir: &Path) -> Vec<Survivor> {
    let table = ps();
    let ledger = ledger(dir);
    let mut found = Vec::new();
    for p in &table {
        let zombie = p.stat.starts_with('Z');
        let in_ledger = ledger.iter().find(|(_, pid)| *pid == p.pid);
        // A zombie's command line is only "(name)", so identify it by the ledger.
        if !(p.command.contains(marker) || (zombie && in_ledger.is_some())) {
            continue;
        }
        let role = in_ledger
            .map(|(r, _)| r.clone())
            .unwrap_or_else(|| "unrecorded".to_owned());
        found.push(Survivor {
            role,
            pid: p.pid,
            zombie,
        });
    }
    found.sort_by_key(|s| s.pid);
    found
}

pub fn kill_all(list: &[Survivor]) {
    for s in list {
        unsafe { libc::kill(s.pid, libc::SIGKILL) };
    }
}

pub fn render(list: &[Survivor]) -> String {
    if list.is_empty() {
        "none".into()
    } else {
        let mut roles: Vec<String> = list.iter().map(Survivor::label).collect();
        roles.sort();
        roles.join(",")
    }
}
