//! Whether the executable this server runs from is still the one on disk.
//!
//! `delega plugin install` writes the plugin's executable through a staged file
//! and a rename, so a refresh gives the path a new file. Claude Code runs the
//! server from that path. Codex runs it from a versioned cache directory that a
//! refresh replaces, so there the path disappears. In both cases the host goes
//! on running the old server until it is reconnected or a new session starts.
//!
//! [`Executable`] records the path and the identity of the file at start
//! (device, inode, size and modification time). Each `list` and each read of
//! `delega://identity` asks for the file's status once more and reports a
//! [`StaleServer`] when the file is gone or is another file. The check is one
//! `stat`; the neighbouring `IDENTITY.json` is read only once the server is
//! known to be stale. Nothing is hashed and no provider is run. A call waits
//! for the filesystem for [`CHECK_LIMIT`] at most and then reports nothing.
//!
//! The report means that the file changed, not that a newer build is installed:
//! `delega plugin uninstall` removes the file, and an install of identical
//! bytes replaces it.
//!
//! `IDENTITY.json` is looked for in the directory above the executable's
//! (`plugin/delega/IDENTITY.json`), opened without blocking, read only when it
//! is a regular file of at most 64 KiB, and ignored when it has no `revision`.
//! At most four checks may wait on a filesystem that does not answer; further
//! calls answer without the field until one returns.
//!
//! Limits of the check:
//!
//! - A file renamed over the executable after the process started and before
//!   [`Executable::current`] recorded it is taken as the running one, so that
//!   replacement is not reported. The window is the start of the process,
//!   before the configuration file is read.
//! - A server that could not find its executable's path, or that file's
//!   status, at start never reports stale.
//! - A later `stat` that fails for a reason other than the path being gone, or
//!   a check that does not finish within [`CHECK_LIMIT`], leaves the field out
//!   of that call only.
//! - A rewrite in place that keeps the size and the modification time is not
//!   seen.
//! - `delega plugin install` run from the managed executable itself skips
//!   writing it, so nothing is reported.
//! - A library caller's server reports only when it is given the executable
//!   (`router_with_executable`).
//!
//! `delega version --output json` never carries the field, and nothing else in
//! `list` or the identity changes when it is present.

use std::fs::{File, Metadata, OpenOptions};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::Semaphore;

/// What a caller is told to do about a stale server.
pub const WHAT_TO_DO: &str = "The executable this server was started from has been replaced or \
    removed since it started. Reconnect the delega MCP server (for example from /mcp in Claude \
    Code) or start a new session.";

/// How long a `list` or a read of the identity waits for the filesystem check.
/// Past it the call reports nothing about staleness and answers.
const CHECK_LIMIT: Duration = Duration::from_secs(2);

/// How many checks may be waiting on the filesystem at once. A check that
/// outlives [`CHECK_LIMIT`] keeps its thread until the filesystem answers;
/// while this many are held, further calls report nothing without starting
/// another.
const CHECKS_AT_ONCE: usize = 4;

/// The most of `IDENTITY.json` that is read. The file `delega plugin install`
/// writes is a few hundred bytes; a larger one is ignored.
const IDENTITY_LIMIT: u64 = 64 * 1024;

/// What identifies a file: a rename over the path changes the inode, and a
/// rewrite in place changes the size or the modification time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_secs: i64,
    modified_nanos: i64,
}

impl FileIdentity {
    fn of(metadata: &Metadata) -> Self {
        Self {
            device: metadata.dev(),
            inode: metadata.ino(),
            size: metadata.size(),
            modified_secs: metadata.mtime(),
            modified_nanos: metadata.mtime_nsec(),
        }
    }
}

/// The executable a server runs from, as it was when the server started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Executable {
    path: PathBuf,
    identity: FileIdentity,
}

impl Executable {
    /// The executable of this process. `None` when the path or the file's
    /// status cannot be had; a server built with `None` never reports stale.
    pub fn current() -> Option<Self> {
        Self::at(std::env::current_exe().ok()?)
    }

    /// The file at `path` as it is now, standing for the server's executable.
    /// `delega mcp` uses [`Executable::current`]; this is for a caller that
    /// knows the path another way, and for tests. `None` when the file's status
    /// cannot be had.
    pub fn at(path: impl Into<PathBuf>) -> Option<Self> {
        let path = path.into();
        let identity = FileIdentity::of(&std::fs::metadata(&path).ok()?);
        Some(Self { path, identity })
    }

    /// The recorded path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// What became of the file since it was recorded: `None` while it is the
    /// same file. One `stat`. An error other than the path no longer existing
    /// (permission denied, say) decides nothing and is not reported as stale.
    fn change(&self) -> Option<Change> {
        match std::fs::metadata(&self.path) {
            Ok(metadata) if FileIdentity::of(&metadata) == self.identity => None,
            Ok(_) => Some(Change::Replaced),
            Err(error)
                if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) =>
            {
                Some(Change::Removed)
            }
            Err(_) => None,
        }
    }

    /// The report for a server of revision `running_revision`, or `None` while
    /// the executable is unchanged.
    pub(crate) fn stale(&self, running_revision: &str) -> Option<StaleServer> {
        let change = self.change()?;
        Some(StaleServer {
            change,
            running_revision: running_revision.to_string(),
            installed_revision: installed_revision(&self.path),
            what_to_do: WHAT_TO_DO.to_string(),
        })
    }
}

/// What happened to the executable the server was started from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Change {
    /// Another file is at the path now, or the file was rewritten.
    Replaced,
    /// Nothing is at the path now.
    Removed,
}

/// Reported by `list` and by `delega://identity` when the executable this
/// server was started from has been replaced or removed since: the host is
/// still running the old build.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct StaleServer {
    /// `replaced` when another file is at the executable's path, `removed`
    /// when nothing is.
    pub change: Change,
    /// The source revision of the build that is running.
    pub running_revision: String,
    /// The revision in the `IDENTITY.json` beside the installed executable.
    /// Absent when that file cannot be read or names no revision.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installed_revision: Option<String>,
    /// What to do: reconnect the server or start a new session.
    pub what_to_do: String,
}

/// Why a file was not opened for reading by [`open_regular`].
#[derive(Debug)]
enum NotOpened {
    /// The open or the status call failed, a missing file included.
    Failed,
    /// Something is at the path that is not a regular file: a FIFO, a
    /// directory, a device.
    NotARegularFile,
}

/// Open `path` for reading if it is a regular file.
///
/// The open has `O_NONBLOCK`, which keeps it from waiting for a writer when
/// the path is a FIFO. The filesystem calls themselves (the open, the status
/// call and later reads) can still wait on a filesystem that does not answer,
/// which is why callers bound the whole check ([`CHECK_LIMIT`]). What kind of
/// file it is is asked of the open handle, so the answer is about the file
/// that will be read and not about what the path named a moment earlier.
fn open_regular(path: &Path) -> Result<File, NotOpened> {
    let nonblocking =
        i32::try_from(rustix::fs::OFlags::NONBLOCK.bits()).map_err(|_| NotOpened::Failed)?;
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(nonblocking)
        .open(path)
        .map_err(|_| NotOpened::Failed)?;
    let metadata = file.metadata().map_err(|_| NotOpened::Failed)?;
    if metadata.is_file() {
        Ok(file)
    } else {
        Err(NotOpened::NotARegularFile)
    }
}

/// The `revision` of `<directory of the executable>/../IDENTITY.json`, where
/// `delega plugin install` writes the identity of the build it installed. Any
/// failure is `None`: a missing file, one that is not a regular file (a FIFO
/// is not waited on), one over [`IDENTITY_LIMIT`], or one that is not a JSON
/// object with a string `revision`.
fn installed_revision(executable: &Path) -> Option<String> {
    let path = executable.parent()?.parent()?.join("IDENTITY.json");
    let file = open_regular(&path).ok()?;
    let mut bytes = Vec::new();
    file.take(IDENTITY_LIMIT + 1).read_to_end(&mut bytes).ok()?;
    if bytes.len() as u64 > IDENTITY_LIMIT {
        return None;
    }
    let document: Value = serde_json::from_slice(&bytes).ok()?;
    document.get("revision")?.as_str().map(str::to_string)
}

/// The check for a server: the recorded executable, if there is one, and the
/// revision of the running build.
#[derive(Debug, Clone)]
pub(crate) struct Staleness {
    executable: Option<Executable>,
    running_revision: String,
    /// Bounds the checks waiting on the filesystem ([`CHECKS_AT_ONCE`]).
    checks: Arc<Semaphore>,
}

impl Staleness {
    pub(crate) fn new(executable: Option<Executable>, running_revision: &str) -> Self {
        Self {
            executable,
            running_revision: running_revision.to_string(),
            checks: Arc::new(Semaphore::new(CHECKS_AT_ONCE)),
        }
    }

    /// The report, or `None` when the server is current or nothing was
    /// recorded. Returns within [`CHECK_LIMIT`]: a check that has not answered
    /// by then, or that fails to run, reports nothing.
    pub(crate) async fn check(&self) -> Option<StaleServer> {
        let executable = self.executable.clone()?;
        let revision = self.running_revision.clone();
        bounded(CHECK_LIMIT, &self.checks, move || {
            executable.stale(&revision)
        })
        .await
    }
}

/// Run `work`, which touches the filesystem, on a thread for blocking work and
/// wait for it for `limit` at most. `None` when it has not answered by then,
/// when it could not run, or when every permit of `checks` is held by work
/// that is still waiting. Work past its limit keeps its permit and its thread
/// until the filesystem answers, so a stuck filesystem costs a bounded number
/// of threads and never holds a call.
async fn bounded<T, F>(limit: Duration, checks: &Arc<Semaphore>, work: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> Option<T> + Send + 'static,
{
    let permit = checks.clone().try_acquire_owned().ok()?;
    let task = tokio::task::spawn_blocking(move || {
        let answer = work();
        drop(permit);
        answer
    });
    tokio::time::timeout(limit, task).await.ok()?.ok().flatten()
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{RecvTimeoutError, channel};
    use std::time::Instant;

    use super::*;

    /// Make a FIFO at `path` with the `mkfifo` system call: no child process,
    /// so nothing to wait for and no spawn lock to hold. The standard library
    /// has no call for it, and rustix leaves `mkfifoat` out on macOS.
    #[allow(unsafe_code)]
    fn make_fifo(path: &Path) {
        use std::os::unix::ffi::OsStrExt;
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).expect("a path");
        // SAFETY: `name` is a NUL-terminated string that outlives the call,
        // and `mkfifo` keeps no pointer to it.
        let made = unsafe { libc::mkfifo(name.as_ptr(), 0o600) };
        assert_eq!(made, 0, "mkfifo: {}", std::io::Error::last_os_error());
    }

    #[test]
    fn opening_a_fifo_with_no_writer_answers_not_a_regular_file_without_waiting() {
        let tmp = tempfile::tempdir().expect("create temporary directory");
        let path = tmp.path().join("IDENTITY.json");
        make_fifo(&path);
        // On a thread of its own, so that an open that waits for a writer
        // fails this test by the deadline below. That thread is then left
        // behind: nothing here tries to release or join it.
        let (answer, answered) = channel();
        let opened = path.clone();
        std::thread::spawn(move || {
            let _ = answer.send(open_regular(&opened).map(drop));
        });
        match answered.recv_timeout(Duration::from_secs(10)) {
            Ok(Err(NotOpened::NotARegularFile)) => {}
            Ok(other) => panic!("a FIFO was answered with {other:?}"),
            Err(RecvTimeoutError::Timeout) => panic!("the open waited on a FIFO with no writer"),
            Err(RecvTimeoutError::Disconnected) => panic!("the opening thread ended unanswered"),
        }
    }

    #[test]
    fn a_regular_file_is_opened_and_a_directory_or_a_missing_file_is_not() {
        let tmp = tempfile::tempdir().expect("create temporary directory");
        let file = tmp.path().join("IDENTITY.json");
        std::fs::write(&file, b"{}").expect("write");
        assert!(open_regular(&file).is_ok());
        assert!(matches!(
            open_regular(tmp.path()),
            Err(NotOpened::NotARegularFile)
        ));
        assert!(matches!(
            open_regular(&tmp.path().join("absent")),
            Err(NotOpened::Failed)
        ));
    }

    /// A long time for a test to wait before it calls something stuck.
    const STUCK: Duration = Duration::from_secs(60);

    #[tokio::test]
    async fn a_check_the_filesystem_does_not_answer_reports_nothing_and_does_not_hold_the_call() {
        let checks = Arc::new(Semaphore::new(2));
        let (release, held) = channel::<()>();
        let (release_second, held_second) = channel::<()>();
        let began = Instant::now();
        // Work that does not answer until released, as a stuck `stat` would.
        let first = bounded(Duration::from_millis(50), &checks, move || {
            held.recv().ok().map(|()| 1)
        })
        .await;
        assert_eq!(first, None);
        let second = bounded(Duration::from_millis(50), &checks, move || {
            held_second.recv().ok().map(|()| 2)
        })
        .await;
        assert_eq!(second, None);
        // Both permits are held by stuck work: the next call does not start
        // more, and answers without waiting even for its limit.
        let third = bounded(STUCK, &checks, || Some(3)).await;
        assert_eq!(third, None);
        assert!(began.elapsed() < STUCK, "a call was held");
        // Once the filesystem answers, checks work again.
        release.send(()).expect("release");
        release_second.send(()).expect("release");
        let mut answered = None;
        for _ in 0..6000 {
            answered = bounded(STUCK, &checks, || Some(4)).await;
            if answered.is_some() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        assert_eq!(answered, Some(4));
    }

    #[tokio::test]
    async fn a_check_that_answers_in_time_is_returned() {
        let checks = Arc::new(Semaphore::new(1));
        assert_eq!(
            bounded(STUCK, &checks, || Some("stale")).await,
            Some("stale")
        );
        assert_eq!(bounded(STUCK, &checks, || None::<u8>).await, None);
        // The permit came back each time.
        assert_eq!(checks.available_permits(), 1);
    }
}
