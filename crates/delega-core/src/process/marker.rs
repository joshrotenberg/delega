//! The run marker: a value in the environment of every process a kept run
//! starts, so cleanup can find a descendant that left the provider's session
//! and whose parent is gone.
//!
//! The variable [`MARKER_ENV`] holds a list of values separated by `:`. Each
//! run adds a new random value to the list it inherited from the environment of
//! the process that runs it, so a `delega` started by a worker keeps the outer
//! run's value and its processes stay findable by the outer run. A process is
//! marked for a run when one element of the list equals the run's value.
//!
//! The value is read on Linux from `/proc/<pid>/environ` and elsewhere from
//! `ps -E`. Both read the memory where `exec` placed the process's environment,
//! not the process's current environment: `unsetenv` does not hide a process,
//! but a process that overwrites that memory in place can remove the marker,
//! and a process started with an environment that leaves the variable out does
//! not carry it.
//!
//! The worker can read the value. It grants nothing: knowing it only lets a
//! process make itself a target of that run's cleanup.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::{OsStrExt, OsStringExt};

use super::tree::{InspectError, ProcessRow};

/// The variable that carries the marker.
pub const MARKER_ENV: &str = "DELEGA_RUN_MARKER";

/// Bytes of randomness in one run's value.
const TOKEN_BYTES: usize = 16;

/// One run's marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Marker {
    token: String,
    value: OsString,
}

impl Marker {
    /// A new value, appended to the list in this process's own environment.
    pub(crate) fn new() -> std::io::Result<Self> {
        let mut bytes = [0u8; TOKEN_BYTES];
        std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        let inherited = std::env::var_os(MARKER_ENV).filter(|v| valid_list(v.as_bytes()));
        Ok(Self::with_inherited(token, inherited))
    }

    pub(crate) fn with_inherited(token: String, inherited: Option<OsString>) -> Self {
        let mut value = Vec::new();
        if let Some(outer) = inherited.filter(|v| !v.is_empty()) {
            value.extend_from_slice(outer.as_bytes());
            value.push(b':');
        }
        value.extend_from_slice(token.as_bytes());
        Self {
            token,
            value: OsString::from_vec(value),
        }
    }

    /// This run's own element of the list.
    pub(crate) fn token(&self) -> &str {
        &self.token
    }

    /// The whole value to set in the environment.
    pub(crate) fn value(&self) -> &OsStr {
        &self.value
    }
}

/// Whether a list holds only hex elements separated by `:`.
fn valid_list(value: &[u8]) -> bool {
    !value.is_empty()
        && value
            .split(|b| *b == b':')
            .all(|e| !e.is_empty() && e.iter().all(u8::is_ascii_hexdigit))
}

/// Whether a list value contains `token` as one element.
pub(crate) fn list_has(value: &[u8], token: &str) -> bool {
    !token.is_empty() && value.split(|b| *b == b':').any(|e| e == token.as_bytes())
}

/// Whether a NUL-separated environment block carries `token`.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) fn environ_has(block: &[u8], token: &str) -> bool {
    let prefix = format!("{MARKER_ENV}=");
    block
        .split(|b| *b == 0)
        .filter_map(|entry| entry.strip_prefix(prefix.as_bytes()))
        .any(|value| list_has(value, token))
}

/// Whether the command column of `ps -E` (arguments, then the environment,
/// separated by spaces) carries `token`.
pub(crate) fn command_has(command: &str, token: &str) -> bool {
    let prefix = format!("{MARKER_ENV}=");
    command
        .split_ascii_whitespace()
        .filter_map(|word| word.strip_prefix(prefix.as_str()))
        .any(|value| list_has(value.as_bytes(), token))
}

/// A live process that carries the marker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MarkedRow {
    pub pid: i32,
    /// The start time, compared only with other marker readings.
    pub started: String,
    pub stopped: bool,
    /// The start time in clock ticks where the system gives it (Linux).
    pub ticks: Option<u64>,
}

/// Every live process in `rows` that carries `token`, read from
/// `/proc/<pid>/environ`. A process whose environment cannot be read (another
/// user's, or gone) is not marked.
#[cfg(target_os = "linux")]
pub(crate) async fn marked(
    rows: &[ProcessRow],
    token: &str,
) -> Result<Vec<MarkedRow>, InspectError> {
    let candidates: Vec<(i32, String, bool)> = rows
        .iter()
        .filter(|r| !r.zombie && r.pid > 1)
        .map(|r| (r.pid, r.started.clone(), r.stopped))
        .collect();
    let token = token.to_string();
    tokio::task::spawn_blocking(move || {
        candidates
            .into_iter()
            .filter_map(|(pid, started, stopped)| {
                // The start time is read before and after the environment and
                // kept only when both agree, so the identity recorded is that
                // of the process whose environment carried the marker; a pid
                // replaced in between gets no start time, and then no signal.
                let before = super::sys::start_ticks(pid);
                let marked = std::fs::read(format!("/proc/{pid}/environ"))
                    .is_ok_and(|block| environ_has(&block, &token));
                let after = super::sys::start_ticks(pid);
                let ticks = before.filter(|t| after == Some(*t));
                marked.then_some((pid, started, stopped, ticks))
            })
            .map(|(pid, started, stopped, ticks)| MarkedRow {
                pid,
                started,
                stopped,
                ticks,
            })
            .collect()
    })
    .await
    .map_err(|_| InspectError::Failed("the marker reader panicked".into()))
}

/// Every live process of this user that carries `token`, from one run of
/// `ps -E`. A listing that fails or cannot be parsed is a failed inspection.
#[cfg(not(target_os = "linux"))]
pub(crate) async fn marked(
    _rows: &[ProcessRow],
    token: &str,
) -> Result<Vec<MarkedRow>, InspectError> {
    if std::env::var_os(super::tree::NO_LISTING_ENV).is_some() {
        return Err(InspectError::Unavailable(
            "process listing is switched off".into(),
        ));
    }
    let uid = rustix::process::getuid().as_raw().to_string();
    let mut command = tokio::process::Command::new("/bin/ps");
    command
        .args(["-U", &uid, "-Eww", "-o", "pid=,stat=,lstart=,command="])
        .env("LC_ALL", "C");
    let text = super::tree::run_listing(command, super::limits::MARKER_PS_MAX_BYTES).await?;
    parse_marked(&text, token, std::process::id())
}

/// Parse a `ps -o pid=,stat=,lstart=,command=` listing (with `LC_ALL=C`, so
/// the start time is five words) into the marked rows. The listing must hold
/// this process and every line must have a pid, a state and a start time.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn parse_marked(
    text: &str,
    token: &str,
    own_pid: u32,
) -> Result<Vec<MarkedRow>, InspectError> {
    let mut marked = Vec::new();
    let mut seen_own = false;
    let mut lines = 0usize;
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        lines += 1;
        let mut words = line.split_ascii_whitespace();
        let parsed = (|| {
            let pid: i32 = words.next()?.parse().ok()?;
            let state = words.next()?;
            let started: Vec<&str> = words.by_ref().take(5).collect();
            (started.len() == 5).then(|| (pid, state.to_string(), started.join(" ")))
        })();
        let Some((pid, state, started)) = parsed else {
            return Err(InspectError::Failed(
                "the marker listing had a line that could not be parsed".into(),
            ));
        };
        if u32::try_from(pid).is_ok_and(|p| p == own_pid) {
            seen_own = true;
        }
        let rest: Vec<&str> = words.collect();
        if state.starts_with('Z') || pid <= 1 {
            continue;
        }
        if command_has(&rest.join(" "), token) {
            marked.push(MarkedRow {
                pid,
                started,
                stopped: state.starts_with('T'),
                ticks: None,
            });
        }
    }
    if lines == 0 {
        return Err(InspectError::Failed(
            "the marker listing held no rows".into(),
        ));
    }
    if !seen_own {
        return Err(InspectError::Failed(
            "the marker listing does not contain this process".into(),
        ));
    }
    Ok(marked)
}

#[cfg(test)]
mod tests {
    use super::*;

    const T: &str = "00ff00ff00ff00ff00ff00ff00ff00ff";

    #[test]
    fn a_nested_run_keeps_the_outer_value_and_adds_its_own() {
        let marker = Marker::with_inherited("bb".into(), Some("aa".into()));
        assert_eq!(marker.value(), "aa:bb");
        assert!(list_has(marker.value().as_bytes(), "aa"));
        assert!(list_has(marker.value().as_bytes(), "bb"));
        assert!(!list_has(marker.value().as_bytes(), "a"));
        let marker = Marker::with_inherited("bb".into(), None);
        assert_eq!(marker.value(), "bb");
    }

    #[test]
    fn a_new_marker_has_a_fresh_hex_value() {
        let a = Marker::new().expect("a marker");
        let b = Marker::new().expect("a marker");
        assert_ne!(a.token(), b.token());
        assert_eq!(a.token().len(), TOKEN_BYTES * 2);
        assert!(valid_list(a.token().as_bytes()));
    }

    #[test]
    fn an_environment_block_carries_the_marker_only_as_an_exact_element() {
        let block = format!("PATH=/bin\0{MARKER_ENV}=aa:{T}\0HOME=/x\0");
        assert!(environ_has(block.as_bytes(), T));
        assert!(environ_has(block.as_bytes(), "aa"));
        assert!(!environ_has(block.as_bytes(), &T[..8]));
        let other = format!("X{MARKER_ENV}={T}\0");
        assert!(!environ_has(other.as_bytes(), T));
    }

    #[test]
    fn a_ps_command_column_carries_the_marker_as_a_word() {
        assert!(command_has(
            &format!("sleep 60 PATH=/bin {MARKER_ENV}={T}"),
            T
        ));
        assert!(!command_has(
            &format!("sleep 60 PATH=/bin {MARKER_ENV}x={T}"),
            T
        ));
        assert!(!command_has("sleep 60", T));
    }

    #[test]
    fn a_marker_listing_is_parsed_and_checked() {
        let text = format!(
            "  10 S    Sun Nov 22 17:41:03 2026 /bin/sh -c x {MARKER_ENV}={T}\n\
               11 T    Sun Nov 22 17:41:04 2026 sleep 60 {MARKER_ENV}={T}\n\
               12 Z    Sun Nov 22 17:41:04 2026 (sleep) {MARKER_ENV}={T}\n\
               13 S    Sun Nov 22 17:41:04 2026 sleep 60\n"
        );
        let rows = parse_marked(&text, T, 13).expect("parsed");
        assert_eq!(
            rows,
            vec![
                MarkedRow {
                    pid: 10,
                    started: "Sun Nov 22 17:41:03 2026".into(),
                    stopped: false,
                    ticks: None
                },
                MarkedRow {
                    pid: 11,
                    started: "Sun Nov 22 17:41:04 2026".into(),
                    stopped: true,
                    ticks: None
                },
            ]
        );
        assert!(parse_marked(&text, T, 99).is_err());
        assert!(parse_marked("", T, 13).is_err());
        assert!(parse_marked("13 S garbage\n", T, 13).is_err());
    }
}
