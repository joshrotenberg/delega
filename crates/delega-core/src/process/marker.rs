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
//! `ps -E`. Both read the process's environment area as the kernel exposes it:
//! `unsetenv` does not remove the marker from it, while a process that
//! overwrites that area can.
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
                let block = std::fs::read(format!("/proc/{pid}/environ"));
                #[cfg(test)]
                if block.is_ok() {
                    examined::note(&token, &[pid]);
                }
                let marked = block.is_ok_and(|block| environ_has(&block, &token));
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
    #[cfg(test)]
    let mut parsed_pids = Vec::new();
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
        #[cfg(test)]
        parsed_pids.push(pid);
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
    #[cfg(test)]
    examined::note(token, &parsed_pids);
    Ok(marked)
}

/// Test-only record of which processes a marker reading examined, per run
/// token: on Linux each pid whose `/proc/<pid>/environ` the reader read, on
/// macOS each pid of a `ps -E` listing that parsed. It holds pids only, never
/// environment text, and is not compiled outside tests.
#[cfg(test)]
pub(crate) mod examined {
    use std::collections::{BTreeSet, HashMap};
    use std::sync::{Mutex, OnceLock};

    fn table() -> &'static Mutex<HashMap<String, BTreeSet<i32>>> {
        static TABLE: OnceLock<Mutex<HashMap<String, BTreeSet<i32>>>> = OnceLock::new();
        TABLE.get_or_init(Default::default)
    }

    pub(crate) fn note(token: &str, pids: &[i32]) {
        let mut table = table().lock().unwrap_or_else(|e| e.into_inner());
        table
            .entry(token.to_owned())
            .or_default()
            .extend(pids.iter().copied());
    }

    /// Whether a marker reading for `token` examined `pid`.
    pub(crate) fn contains(token: &str, pid: i32) -> bool {
        let table = table().lock().unwrap_or_else(|e| e.into_inner());
        table.get(token).is_some_and(|pids| pids.contains(&pid))
    }
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

    /// A value from another process's environment, as `ps -E` shows it.
    const SECRET: &str = "leakprobe5ec2e7c0ffee5ec2e7c0ffee";

    fn listing_with_secret() -> String {
        format!(
            "  13 S    Sun Nov 22 17:41:04 2026 sleep 60 {MARKER_ENV}={T}\n\
               14 S    Sun Nov 22 17:41:04 2026 other PROBE={SECRET}\n"
        )
    }

    fn assert_no_secret(error: &InspectError) {
        assert!(
            !error.to_string().contains(SECRET) && !format!("{error:?}").contains(SECRET),
            "the error carries listing text ({} bytes of it)",
            SECRET.len()
        );
    }

    /// The error of a call that must fail, without formatting what it returned
    /// when it did not.
    fn failure<T>(result: Result<T, InspectError>, what: &str) -> InspectError {
        match result {
            Ok(_) => panic!("{what} succeeded"),
            Err(error) => error,
        }
    }

    /// Assert the error is a failed inspection whose message holds `kind`,
    /// without printing the message (a regression could put listing text in
    /// it).
    fn assert_kind(error: &InspectError, kind: &str) {
        assert!(
            matches!(error, InspectError::Failed(message) if message.contains(kind)),
            "the error is not the failed inspection that says {kind:?}"
        );
    }

    #[test]
    fn a_marker_listing_line_that_cannot_be_parsed_is_not_quoted_in_the_error() {
        let text = format!(
            "{}  15 S    SECRET={SECRET} cannot be parsed\n",
            listing_with_secret()
        );
        let error = failure(parse_marked(&text, T, 13), "parsing a broken listing");
        assert_no_secret(&error);
        assert_kind(&error, "could not be parsed");
    }

    #[test]
    fn a_marker_listing_without_this_process_does_not_quote_the_listing_in_the_error() {
        let error = failure(
            parse_marked(&listing_with_secret(), T, 99),
            "parsing a listing without this process",
        );
        assert_no_secret(&error);
        assert_kind(&error, "this process");
    }

    fn printing(script: &str) -> tokio::process::Command {
        let mut command = tokio::process::Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[tokio::test]
    async fn a_listing_program_that_fails_after_printing_does_not_quote_its_output() {
        let error = super::super::tree::run_listing(
            printing(&format!(
                "echo PROBE={SECRET}; echo PROBE={SECRET} >&2; exit 3"
            )),
            1 << 20,
        )
        .await;
        let error = failure(error, "a listing that exits non-zero");
        assert_no_secret(&error);
        assert_kind(&error, "exit");
    }

    #[tokio::test]
    async fn a_listing_too_large_does_not_quote_its_output() {
        let error = super::super::tree::run_listing(
            printing(&format!(
                "i=0; while [ $i -lt 200 ]; do echo PROBE={SECRET}; i=$((i+1)); done"
            )),
            64,
        )
        .await;
        let error = failure(error, "a listing over the bound");
        assert_no_secret(&error);
        assert_kind(&error, "too large");
    }
}
