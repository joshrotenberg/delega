//! Process inspection through `ps`, signals, and the survivor check.
//!
//! Survivors are found independently of anything `delega` reported, in two
//! ways. A process counts when its command line, as `ps -axww` shows it,
//! carries the case marker (every process the fakes start runs a script from
//! the case directory, and the relay passes paths in it to `perl`). A process
//! of a role in `BARE_ROLES` has no marker in its command line, so the ledger
//! the fakes write is read instead: it counts when its recorded pid is alive
//! and its command is still `sleep` (which rules out a reused pid). The
//! ledger also maps a pid to a role; a marked process it does not know is
//! reported as `unrecorded`.

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

/// Roles whose processes carry no marker in their command line.
const BARE_ROLES: [&str; 1] = ["bare"];

/// Whether a command line carries `marker`. The marker ends in `/`, so the
/// marker of case 1 is not found in the command line of case 10.
pub fn carries(command: &str, marker: &str) -> bool {
    command.contains(marker)
}

/// Live processes (zombies excluded) that belong to the case: whose command
/// line carries `marker`, or that the ledger names with a role in
/// `BARE_ROLES`; other than the pids in `except`.
pub fn survivors(marker: &str, dir: &Path, except: &[i32]) -> Vec<Survivor> {
    let ledger = ledger(dir);
    let table = ps();
    let mut found: Vec<Survivor> = table
        .iter()
        .filter(|p| !p.stat.starts_with('Z') && carries(&p.command, marker))
        .filter(|p| !except.contains(&p.pid))
        .map(|p| Survivor {
            role: ledger
                .iter()
                .find(|(_, pid)| *pid == p.pid)
                .map_or_else(|| "unrecorded".to_owned(), |(r, _)| r.clone()),
            pid: p.pid,
        })
        .collect();
    for (role, pid) in &ledger {
        let alive_bare = BARE_ROLES.contains(&role.as_str())
            && table.iter().any(|p| {
                p.pid == *pid && !p.stat.starts_with('Z') && p.command.starts_with("sleep ")
            });
        if alive_bare && !except.contains(pid) && !found.iter().any(|f| f.pid == *pid) {
            found.push(Survivor {
                role: role.clone(),
                pid: *pid,
            });
        }
    }
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

#[cfg(test)]
mod tests {
    use super::carries;

    #[test]
    fn a_marker_is_not_found_in_the_command_line_of_a_case_whose_number_starts_with_it() {
        let one = "accmk-777-1/";
        let ten = "sh /tmp/dacc-x/accmk-777-10/fake-claude.sh";
        let one_cmd = "sh /tmp/dacc-x/accmk-777-1/acc-child.sh child";
        assert!(!carries(ten, one));
        assert!(carries(one_cmd, one));
        assert!(carries(ten, "accmk-777-10/"));
        assert!(!carries(one_cmd, "accmk-77-1/"));
        assert!(!carries("sh /tmp/accmk-1777-1/x", one));
    }
}
