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
//! known to be stale. Nothing is hashed and no provider is run.

use std::fs::{File, Metadata};
use std::io::{ErrorKind, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::Serialize;
use serde_json::Value;

/// What a caller is told to do about a stale server.
pub const WHAT_TO_DO: &str = "Reconnect the delega MCP server (for example from /mcp in Claude \
    Code) or start a new session, so that the installed build is the one running.";

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

/// The `revision` of `<directory of the executable>/../IDENTITY.json`, where
/// `delega plugin install` writes the identity of the build it installed. Any
/// failure is `None`: a missing file, one that is not a regular file, one over
/// [`IDENTITY_LIMIT`], or one that is not a JSON object with a string
/// `revision`.
fn installed_revision(executable: &Path) -> Option<String> {
    let path = executable.parent()?.parent()?.join("IDENTITY.json");
    // Checked before opening: opening a FIFO would wait for a writer.
    if !std::fs::metadata(&path).ok()?.is_file() {
        return None;
    }
    let file = File::open(&path).ok()?;
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
}

impl Staleness {
    pub(crate) fn new(executable: Option<Executable>, running_revision: &str) -> Self {
        Self {
            executable,
            running_revision: running_revision.to_string(),
        }
    }

    /// The report, or `None` when the server is current or nothing was
    /// recorded. The file calls run off the runtime's threads, so a stuck
    /// filesystem holds up this call only; a check that fails to run reports
    /// nothing.
    pub(crate) async fn check(&self) -> Option<StaleServer> {
        let executable = self.executable.clone()?;
        let revision = self.running_revision.clone();
        tokio::task::spawn_blocking(move || executable.stale(&revision))
            .await
            .ok()
            .flatten()
    }
}
