//! The keeper registry (macOS): where each keeper publishes the unique ids
//! of the processes it took, so that a run does not count another run's
//! orphans as possibly its own.
//!
//! - The directory is the caller's (see `KeeperCommand::registry`). It must
//!   be a real directory (not a symlink), owned by this user, with no access
//!   for group or others; it is created with mode 0700 when missing, and
//!   checked before every use. Anything else and there is no registry.
//! - Each keeper writes one file, `keeper-<pid>-<unique id>`, created new
//!   with mode 0600 and without following a symlink, and appends the unique
//!   id of every process it proves its own, one per line. It removes the
//!   file when it exits; a keeper killed with SIGKILL leaves it behind.
//! - A file counts only while a process with its pid and its unique id is
//!   alive (`proc_pidinfo` flavor 17): a stale file, or one that names a pid
//!   with another unique id, exempts nothing. A stale file is removed: the
//!   unique id in its name is never reused, so its keeper is gone for good.
//! - An entry only exempts a process from being ambiguous for another run;
//!   no run signals anything because of the registry.

use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use crate::process::sys::darwin;

const PREFIX: &str = "keeper-";

/// Check that `dir` is a directory of this user's that nobody else can use:
/// not a symlink, owned by this user, no permission bits for group or
/// others. Creates it (mode 0700) when missing.
pub(super) fn checked_dir(dir: &Path) -> Result<(), String> {
    if !dir.is_absolute() {
        return Err("the registry directory is not an absolute path".into());
    }
    if std::fs::symlink_metadata(dir).is_err() {
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("could not create the registry's parent: {e}"))?;
        }
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("could not create the registry: {e}")),
        }
    }
    let meta = std::fs::symlink_metadata(dir)
        .map_err(|e| format!("could not read the registry directory: {e}"))?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err("the registry is not a directory (or is a symlink)".into());
    }
    if meta.uid() != rustix::process::getuid().as_raw() {
        return Err("the registry directory is owned by another user".into());
    }
    let mode = meta.permissions().mode();
    if mode & 0o077 != 0 {
        return Err(format!(
            "the registry directory is open to group or others (mode {:o})",
            mode & 0o7777
        ));
    }
    Ok(())
}

/// This keeper's entry in the registry.
pub(super) struct Registry {
    dir: PathBuf,
    file: File,
    own: String,
}

/// The path of this keeper's entry, for removing it when the keeper exits.
static ENTRY: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

/// Remove this keeper's entry, if it has one.
pub(super) fn unpublish() {
    if let Some(path) = ENTRY.get() {
        let _ = std::fs::remove_file(path);
    }
}

impl Registry {
    /// Create this keeper's entry in `dir`.
    pub(super) fn open(dir: &Path, pid: i32, uniqueid: u64) -> Result<Self, String> {
        checked_dir(dir)?;
        let own = format!("{PREFIX}{pid}-{uniqueid}");
        let path = dir.join(&own);
        let file = OpenOptions::new()
            .append(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&path)
            .map_err(|e| format!("could not create the registry entry: {e}"))?;
        let _ = ENTRY.set(path);
        Ok(Self {
            dir: dir.to_path_buf(),
            file,
            own,
        })
    }

    /// Publish a unique id this keeper proved its own. `false` when it could
    /// not be written.
    pub(super) fn publish(&mut self, uniqueid: u64) -> bool {
        self.file
            .write_all(format!("{uniqueid}\n").as_bytes())
            .is_ok()
    }

    /// The unique ids published by every other keeper that is alive with the
    /// unique id its entry names. An error when the directory cannot be
    /// checked or read: then nothing is exempt.
    pub(super) fn others(&self) -> Result<HashSet<u64>, String> {
        checked_dir(&self.dir)?;
        let mut out = HashSet::new();
        let entries =
            std::fs::read_dir(&self.dir).map_err(|e| format!("reading the registry: {e}"))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("reading the registry: {e}"))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name == self.own {
                continue;
            }
            let Some((pid, uniqueid)) = name
                .strip_prefix(PREFIX)
                .and_then(|rest| rest.split_once('-'))
                .and_then(|(p, u)| Some((p.parse::<i32>().ok()?, u.parse::<u64>().ok()?)))
            else {
                continue;
            };
            match darwin::identity(pid) {
                Ok(Some(keeper)) if keeper.uniqueid == uniqueid => {}
                // Its keeper is gone for good: a unique id is never reused.
                Ok(_) => {
                    let _ = std::fs::remove_file(entry.path());
                    continue;
                }
                Err(_) => continue,
            }
            // The keeper itself (an orphan once its owner is gone) is that
            // run's too.
            out.insert(uniqueid);
            let mut text = String::new();
            let opened = OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW)
                .open(entry.path());
            if let Ok(mut file) = opened
                && file
                    .metadata()
                    .is_ok_and(|m| m.uid() == rustix::process::getuid().as_raw())
                && file.read_to_string(&mut text).is_ok()
            {
                out.extend(text.lines().filter_map(|l| l.trim().parse::<u64>().ok()));
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn me() -> darwin::Ident {
        darwin::identity(rustix::process::getpid().as_raw_nonzero().get())
            .unwrap()
            .unwrap()
    }

    #[test]
    fn a_directory_open_to_others_or_a_symlink_is_refused() {
        let base = tempfile::tempdir().expect("a temporary directory");
        let open = base.path().join("open");
        std::fs::create_dir(&open).unwrap();
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(checked_dir(&open).is_err());
        let link = base.path().join("link");
        let real = base.path().join("real");
        std::fs::DirBuilder::new()
            .mode(0o700)
            .create(&real)
            .unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(checked_dir(&link).is_err());
        assert!(checked_dir(&real).is_ok());
        let made = base.path().join("made");
        assert!(checked_dir(&made).is_ok());
    }

    #[test]
    fn a_stale_or_forged_entry_exempts_nothing_and_a_live_one_does() {
        let base = tempfile::tempdir().expect("a temporary directory");
        let dir = base.path().join("registry");
        let own = me();
        // A reader whose own entry is a made-up keeper, so the test
        // process's real entry below counts as another keeper's.
        let reader = Registry::open(&dir, 1, 1).expect("a registry");
        // This process is alive with this unique id: its entry counts.
        let mut live = Registry::open(&dir, own.pid, own.uniqueid).expect("an entry");
        assert!(live.publish(4242));
        // A live pid with the wrong unique id, and a pid that is not alive.
        std::fs::write(
            dir.join(format!("{PREFIX}{}-{}", own.pid, own.uniqueid + 1)),
            "7\n",
        )
        .unwrap();
        std::fs::write(dir.join(format!("{PREFIX}{}-{}", i32::MAX - 7, 99)), "8\n").unwrap();
        let others = reader.others().expect("read");
        assert!(others.contains(&4242), "{others:?}");
        assert!(!others.contains(&7) && !others.contains(&8), "{others:?}");
    }
}
