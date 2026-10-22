//! Node v22.22.0 `fs.rmSync(path, { recursive: true, force: true })`.
//!
//! `lib/internal/fs/rimraf.js:177-308`: default maxRetries=0, so each retry
//! loop has one attempt and never sleeps. In particular EACCES is not a reason
//! to descend. A child error stops traversal and is propagated unchanged.
use std::{fs, io, path::Path};

pub(super) fn rm_sync(path: &Path) -> io::Result<()> {
    let stats = match fs::symlink_metadata(path) {
        Ok(stats) => Some(stats),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => {
            #[cfg(windows)]
            if is_eperm(&e) {
                fix_win_eperm(path, e)?;
            }
            #[cfg(not(windows))]
            let _ = e;
            // Node ignores other lstat errors here and attempts unlink.
            None
        }
    };
    let result = if stats.is_some_and(|s| s.is_dir()) {
        rmdir(path, None)
    } else {
        fs::remove_file(path)
    };
    match result {
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(e) if is_eperm(&e) => {
            #[cfg(windows)]
            return fix_win_eperm(path, e);
            #[cfg(not(windows))]
            rmdir(path, Some(e))
        }
        Err(e) if e.kind() == io::ErrorKind::IsADirectory => rmdir(path, Some(e)),
        result => result,
    }
}

fn is_eperm(error: &io::Error) -> bool {
    #[cfg(unix)]
    return error.raw_os_error() == Some(libc::EPERM);
    #[cfg(windows)]
    return error.raw_os_error() == Some(5); // libuv maps ERROR_ACCESS_DENIED to EPERM.
    #[cfg(not(any(unix, windows)))]
    false
}

fn rmdir(path: &Path, original: Option<io::Error>) -> io::Result<()> {
    let error = match fs::remove_dir(path) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => e,
    };
    if error.kind() == io::ErrorKind::NotADirectory {
        return Err(original.unwrap_or(error));
    }
    if matches!(
        error.kind(),
        io::ErrorKind::DirectoryNotEmpty | io::ErrorKind::AlreadyExists
    ) || is_eperm(&error)
    {
        // Node's synchronous readdir uses libuv scandir (strcmp order), not
        // the OS's native readdir order or locale collation. Keep raw names.
        let mut children = fs::read_dir(path)?
            .map(|entry| entry.map(|e| e.file_name()))
            .collect::<io::Result<Vec<_>>>()?;
        children.sort();
        for child in children {
            rm_sync(&path.join(child))?;
        }
        return match fs::remove_dir(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        };
    }
    Err(original.unwrap_or(error))
}

#[cfg(windows)]
fn fix_win_eperm(path: &Path, original: io::Error) -> io::Result<()> {
    let result = fs::metadata(path).and_then(|m| {
        let mut permissions = m.permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions)
    });
    if let Err(e) = result {
        return if e.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(original)
        };
    }
    match fs::metadata(path) {
        Ok(s) if s.is_dir() => rmdir(path, Some(original)),
        Ok(_) => fs::remove_file(path),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(original),
    }
}
