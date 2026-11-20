//! Writing and deleting inside the managed directory without following links.

use std::collections::BTreeSet;
use std::fs;
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};

use super::marker;

/// The directory with every existing component resolved, so an alias or a dot
/// segment names the same directory the hosts will read back. Components that
/// do not exist yet are appended as written.
pub fn canonical_dir(path: &Path) -> Result<PathBuf, String> {
    let mut resolved = PathBuf::new();
    for part in path.components() {
        match part {
            Component::RootDir => resolved.push("/"),
            Component::ParentDir => {
                resolved.pop();
            }
            Component::CurDir => {}
            Component::Normal(part) => {
                resolved.push(part);
                match fs::canonicalize(&resolved) {
                    Ok(path) => resolved = path,
                    Err(error)
                        if error.kind() == ErrorKind::NotFound
                            && fs::symlink_metadata(&resolved).is_err() => {}
                    Err(error) => {
                        return Err(format!("cannot resolve {}: {error}", resolved.display()));
                    }
                }
            }
            _ => return Err("unsupported install path".into()),
        }
    }
    if resolved.file_name().is_none() {
        return Err("the filesystem root cannot be an install directory".into());
    }
    Ok(resolved)
}

/// An existing directory is reused only when an earlier `delega plugin install` wrote it (a
/// valid marker) or it is empty. Anything else is someone's data.
pub fn check_dir(dir: &Path) -> Result<(), String> {
    match fs::read_dir(dir) {
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("cannot read {}: {error}", dir.display())),
        Ok(mut entries) => {
            if marker::read(dir)?.is_some() || entries.next().is_none() {
                Ok(())
            } else {
                Err(format!(
                    "{} exists and was not created by delega plugin install; choose another --install-dir",
                    dir.display()
                ))
            }
        }
    }
}

/// Refuse a managed path that is, or passes through, a symbolic link.
/// Following one would write or delete outside the managed directory.
pub fn refuse_symlinks(dir: &Path, relative: &str) -> Result<(), String> {
    let mut path = dir.to_path_buf();
    for part in relative.split('/') {
        path.push(part);
        if fs::symlink_metadata(&path).is_ok_and(|meta| meta.file_type().is_symlink()) {
            return Err(format!(
                "{} is a symbolic link; delega does not write or delete through links",
                path.display()
            ));
        }
    }
    Ok(())
}

/// Write `bytes` to `relative` under `dir` with `mode`, through a staging file
/// beside the destination and a rename, so a host that is running the
/// installed executable never sees a partial file.
pub fn write(dir: &Path, relative: &str, bytes: &[u8], mode: u32) -> Result<(), String> {
    let path = dir.join(relative);
    let fail = |error: std::io::Error| format!("cannot write {}: {error}", path.display());
    refuse_symlinks(dir, relative)?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(fail)?;
    }
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let mut attempt = 0u32;
    let (staged, mut file) = loop {
        let staged =
            path.with_file_name(format!(".{name}.delega-{}-{attempt}", std::process::id()));
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(mode)
            .open(&staged)
        {
            Ok(file) => break (staged, file),
            Err(error) if error.kind() == ErrorKind::AlreadyExists && attempt < 64 => attempt += 1,
            Err(error) => return Err(fail(error)),
        }
    };
    let result = file
        .write_all(bytes)
        .and_then(|()| file.set_permissions(fs::Permissions::from_mode(mode)))
        .and_then(|()| {
            drop(file);
            fs::rename(&staged, &path)
        });
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result.map_err(fail)
}

/// Delete the listed files, then any directory left empty beneath `dir`. A
/// directory that still holds something is not ours to remove.
pub fn delete_owned(dir: &Path, files: &[String]) -> Result<(), String> {
    for relative in files {
        refuse_symlinks(dir, relative)?;
    }
    let mut parents = BTreeSet::new();
    for relative in files {
        let path = dir.join(relative);
        match fs::remove_file(&path) {
            Err(error) if error.kind() != ErrorKind::NotFound => {
                return Err(format!("cannot delete {}: {error}", path.display()));
            }
            _ => {}
        }
        let mut parent = Path::new(relative).parent();
        while let Some(part) = parent.filter(|part| !part.as_os_str().is_empty()) {
            parents.insert(part.to_path_buf());
            parent = part.parent();
        }
    }
    // Deepest first. `remove_dir` fails on a non-empty directory, which keeps
    // anything the user added.
    for parent in parents.iter().rev() {
        let _ = fs::remove_dir(dir.join(parent));
    }
    Ok(())
}
