//! Reading the file safely.
//!
//! The configuration can name executables, so a file that someone else could
//! have written or changed must not be used. The checks are made on the open
//! handle, as [`crate::provider`] does for the settings files it reads for deny
//! rules, so nothing can be swapped in between the check and the read:
//!
//! 1. The directory that holds the file is opened, then the entry is looked at
//!    without following a link and opened relative to that directory handle.
//! 2. What is read must be a regular file, owned by the current user, and not
//!    writable by group or others. A device, a pipe or a directory is refused
//!    before anything is read, and the open does not block.
//! 3. The directory that holds the file must be owned by the user and not
//!    writable by group or others, link or not, and a regular file is opened with
//!    `O_NOFOLLOW`. A symbolic link at the file is followed only when the user owns the link
//!    and the directory that holds it, and that directory is not writable by
//!    group or others. Another user can plant a link only in a directory they
//!    can write, so a link the user owns in a directory only the user can write
//!    was put there by the user, as a dotfiles manager does. The target must
//!    then pass rule 2 on its own handle. A link that fails a condition is
//!    refused with the condition named, and a link to nothing is refused. This
//!    is the rule `delega` applies to a link in a repository root when it reads
//!    Claude Code settings, with one difference: there a failing link is
//!    skipped, here the file is never skipped, because a skipped configuration
//!    would silently drop what the user set.
//! 4. At most [`MAX_CONFIG_BYTES`](super::MAX_CONFIG_BYTES) are read.

use std::io::Read as _;
use std::path::Path;

use rustix::fs::{AtFlags, FileType, Mode, OFlags, fstat, open, openat, statat};
use rustix::io::Errno;

use super::MAX_CONFIG_BYTES;

/// The mode bits of a `stat`, as a `u32` on every platform: `st_mode` is a
/// `u16` on macOS and already a `u32` on Linux, where the conversion is a
/// no-op that clippy would otherwise flag.
#[allow(clippy::useless_conversion)]
fn mode_bits(st_mode: rustix::fs::RawMode) -> u32 {
    u32::from(st_mode)
}

/// What reading gave.
#[derive(Debug)]
pub enum Read {
    /// Nothing is at the path, or a directory above it does not exist.
    Missing,
    /// The file's content.
    Bytes(Vec<u8>),
}

fn kind(e: Errno) -> std::io::ErrorKind {
    std::io::Error::from(e).kind()
}

fn others_can_write(mode: u32) -> bool {
    mode & 0o022 != 0
}

/// Read the file at the absolute `path` as the user `euid`. The error is why it
/// cannot be used, without the path.
pub fn read(path: &Path, euid: u32) -> Result<Read, String> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err("it is not a path to a file".to_string());
    };
    let dir = match open(
        parent,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(dir) => dir,
        Err(Errno::NOENT | Errno::NOTDIR) => return Ok(Read::Missing),
        Err(e) => return Err(format!("its directory cannot be opened ({})", kind(e))),
    };
    let entry = match statat(&dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(entry) => entry,
        Err(Errno::NOENT) => return Ok(Read::Missing),
        Err(e) => return Err(format!("it cannot be inspected ({})", kind(e))),
    };
    let entry_type = FileType::from_raw_mode(entry.st_mode);
    let is_link = entry_type == FileType::Symlink;
    // A device, a pipe, a directory or a socket is refused before it is opened:
    // opening some of them has side effects.
    if !is_link && entry_type != FileType::RegularFile {
        return Err("it is not a regular file".to_string());
    }
    if is_link && entry.st_uid != euid {
        return Err("it is a symbolic link that the current user does not own".to_string());
    }
    // Whoever can write the directory can swap the entry or plant a link or a
    // hard link, so the directory must be the user's and closed to group and
    // others, whether or not the entry is a link.
    let holder =
        fstat(&dir).map_err(|e| format!("its directory cannot be examined ({})", kind(e)))?;
    if holder.st_uid != euid || others_can_write(mode_bits(holder.st_mode)) {
        return Err(if is_link {
            "it is a symbolic link in a directory that the current user does not own or \
             that group or others can write, so the link is not trusted"
        } else {
            "the directory that holds it is not owned by the current user or can be written \
             by group or others, so the file is not trusted"
        }
        .to_string());
    }
    // Without blocking, so a pipe swapped in after the look above cannot hold
    // the command, and without following a link when the entry was not one, so
    // a link swapped in is not followed; the handle is checked below.
    let mut flags = OFlags::RDONLY | OFlags::NONBLOCK | OFlags::CLOEXEC;
    if !is_link {
        flags |= OFlags::NOFOLLOW;
    }
    let fd = match openat(&dir, name, flags, Mode::empty()) {
        Ok(fd) => fd,
        Err(Errno::NOENT) if is_link => {
            return Err("it is a symbolic link that points at nothing".to_string());
        }
        Err(Errno::NOENT) => return Ok(Read::Missing),
        Err(Errno::LOOP) => return Err("it was replaced by a symbolic link".to_string()),
        Err(e) => return Err(format!("it cannot be opened ({})", kind(e))),
    };
    let handle = fstat(&fd).map_err(|e| format!("it cannot be examined ({})", kind(e)))?;
    if FileType::from_raw_mode(handle.st_mode) != FileType::RegularFile {
        return Err("it is not a regular file".to_string());
    }
    if handle.st_uid != euid {
        return Err("it is owned by another user".to_string());
    }
    let mode = mode_bits(handle.st_mode);
    if others_can_write(mode) {
        return Err(format!(
            "it can be written by group or others (mode {:03o}); run `chmod go-w` on it",
            mode & 0o777
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::from(fd)
        .take(MAX_CONFIG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("it cannot be read ({})", e.kind()))?;
    if bytes.len() > MAX_CONFIG_BYTES {
        return Err(format!(
            "it is larger than the limit of {MAX_CONFIG_BYTES} bytes"
        ));
    }
    Ok(Read::Bytes(bytes))
}
