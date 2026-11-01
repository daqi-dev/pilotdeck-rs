//! Disk-only file methods, measured against the pinned Node oracle.
use crate::metadata::{Files, Metadata, Stat, js_trim, object, resolve, text_field};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use icu_casemap::CaseMapper;
use icu_collator::{Collator, CollatorBorrowed, CollatorPreferences};
use icu_locale_core::{LanguageIdentifier, Locale};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering,
    collections::HashSet,
    fs::File,
    io::{self, Read},
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

type Error = (&'static str, String);
type Result<T> = std::result::Result<T, Error>;
const FILE_INDEX_LIMIT: usize = 100_000;
const OUTSIDE: &str = "File path is outside the project workspace.";
const ESCAPE: &str = "File path resolves outside the project workspace.";
const EXCLUDED: &[&str] = &[
    ".git",
    ".pilotdeck",
    ".tmp",
    "node_modules",
    "dist",
    "build",
    "coverage",
];
fn denied(message: &str) -> Error {
    ("PATH_NOT_ALLOWED", message.into())
}
fn index_limit() -> Error {
    (
        "FILE_INDEX_LIMIT",
        "Project file scan exceeded its resource limit.".into(),
    )
}
/// `Number(maxBytes)` on a too-deep array: V8's RangeError, which TS reports
/// as a plain gateway error (real TS + WS evidence: tests/fixtures/runtime-boundaries.json).
fn stack_overflow(_: crate::js_value::StackOverflow) -> Error {
    (
        "gateway_request_failed",
        crate::js_value::StackOverflow::MESSAGE.into(),
    )
}
fn encode(value: &JsValue) -> String {
    stringify_json(value).expect("JSON value").expect("defined")
}

// Only a NOFOLLOW open failure denotes our deliberate security divergence.
// Ledger: specs/gateway-parity/accepted-diffs.md A4-FILE-TOCTOU-001 (human-approved).
// A realpath ELOOP is an ordinary measured Node error, not evidence of a race.
#[derive(Debug)]
struct NoFollowLoop;
impl std::fmt::Display for NoFollowLoop {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("symlink replaced during checked open")
    }
}
impl std::error::Error for NoFollowLoop {}
#[cfg(unix)]
fn checked_open_error(error: io::Error) -> io::Error {
    if error.raw_os_error() == Some(libc::ELOOP) {
        io::Error::new(error.kind(), NoFollowLoop)
    } else {
        error
    }
}
pub(crate) fn nofollow_loop(error: &io::Error) -> bool {
    error.get_ref().is_some_and(|e| e.is::<NoFollowLoop>())
}
fn measured(method: &str, error: &io::Error) -> Result<()> {
    if nofollow_loop(error) || crate::node_io::lookup_for(method, error).is_some() {
        Ok(())
    } else {
        Err(("NODE_IO_REQUIRED", String::new()))
    }
}

/// Each component is opened relative to a pinned directory, never following a
/// symlink. The preceding realpath permits ordinary in-root links; a replacement
/// after that check fails closed. NONBLOCK prevents a raced-in FIFO hanging us.
pub(crate) fn open_beneath(root: &Path, canonical: &Path) -> io::Result<File> {
    open_checked(root, canonical, false)
}
/// Create a private file without following any component, preserving Node's
/// `wx` semantics. Reuse the same directory walk as reads; only the final open
/// gains write/create flags. Check the opened inode before callers write bytes.
#[cfg(unix)]
pub(crate) fn create_new_beneath(root: &Path, canonical: &Path) -> io::Result<File> {
    use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
    let parent = open_checked(
        root,
        canonical.parent().ok_or(io::ErrorKind::InvalidInput)?,
        true,
    )?;
    let file = File::from(
        openat(
            &parent,
            canonical.file_name().ok_or(io::ErrorKind::InvalidInput)?,
            OFlags::WRONLY
                | OFlags::CREATE
                | OFlags::EXCL
                | OFlags::CLOEXEC
                | OFlags::NOFOLLOW
                | OFlags::NONBLOCK,
            Mode::from_raw_mode(0o600),
        )
        .map_err(io::Error::from)
        .map_err(checked_open_error)?,
    );
    let stat = fstat(&file)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    Ok(file)
}
/// Node recursive mkdir semantics below a trusted project root. Every lookup
/// and mkdir is relative to the pinned parent; no symlink is ever followed.
#[cfg(unix)]
pub(crate) fn create_directories_beneath(root: &Path, target: &Path) -> io::Result<()> {
    let relative = target
        .strip_prefix(root)
        .map_err(|_| io::Error::from_raw_os_error(libc::EACCES))?;
    let parent = open_checked(root, root, true)?;
    create_directories_at(&parent, relative, rustix::fs::Mode::from_raw_mode(0o700))
}

/// Descriptor variant of the #596 mkdir walk, shared by memory mutations.
#[cfg(unix)]
pub(crate) fn create_directories_at(
    root: &File,
    relative: &Path,
    mode: rustix::fs::Mode,
) -> io::Result<()> {
    use rustix::fs::{AtFlags, FileType, Mode, mkdirat, openat, statat};
    let components: Vec<_> = relative.components().collect();
    let mut parent = root.try_clone()?;
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        };
        let name: &std::ffi::OsStr = name;
        let open = || openat(&parent, name, directory_flags(), Mode::empty());
        let opened = match open() {
            Err(rustix::io::Errno::NOENT) => {
                match mkdirat(&parent, name, mode) {
                    Ok(()) | Err(rustix::io::Errno::EXIST) => {}
                    Err(error) => return Err(error.into()),
                }
                open()
            }
            result => result,
        };
        parent = File::from(opened.map_err(|error| {
            // Distinguish a refused link from Node's ordinary non-directory
            // errors. statat also stays on the pinned parent, even in a race.
            if matches!(error, rustix::io::Errno::NOTDIR | rustix::io::Errno::LOOP)
                && let Ok(stat) = statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW)
            {
                let kind = FileType::from_raw_mode(stat.st_mode);
                if kind == FileType::Symlink {
                    return io::Error::new(io::ErrorKind::PermissionDenied, NoFollowLoop);
                }
                if kind != FileType::Directory && index + 1 == components.len() {
                    return io::Error::from_raw_os_error(libc::EEXIST);
                }
            }
            checked_open_error(error.into())
        })?);
    }
    Ok(())
}

#[cfg(unix)]
fn directory_flags() -> rustix::fs::OFlags {
    use rustix::fs::OFlags;
    // Traversing a directory needs search permission, not read permission.
    #[cfg(target_os = "linux")]
    let access = OFlags::PATH;
    #[cfg(target_vendor = "apple")]
    let access = OFlags::from_bits_retain(libc::O_SEARCH as u32);
    #[cfg(not(any(target_os = "linux", target_vendor = "apple")))]
    let access = OFlags::RDONLY;
    access | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW
}

// The Unix walk is the hardening behind A4-FILE-TOCTOU-001 and the memory
// mutations; non-Unix keeps the measured Node behavior below.
#[cfg(unix)]
fn open_checked(root: &Path, canonical: &Path, search_directory: bool) -> io::Result<File> {
    let relative = canonical
        .strip_prefix(root)
        .map_err(|_| io::Error::from_raw_os_error(libc::EACCES))?;
    // Walk the canonical root from / as well, using the same NOFOLLOW walk
    // as descriptor-relative consumers. No absolute path is reopened later.
    let filesystem = File::open("/")?;
    let relative = root
        .strip_prefix("/")
        .map_err(|_| io::ErrorKind::InvalidInput)?
        .join(relative);
    walk_beneath(&filesystem, &relative, search_directory)
}

/// The NOFOLLOW descriptor walk is the Unix-only hardening behind
/// A4-FILE-TOCTOU-001 (specs/gateway-parity/accepted-diffs.md). Non-Unix keeps
/// the measured Node behavior: a plain open of the realpath-checked path.
#[cfg(not(unix))]
fn open_checked(root: &Path, canonical: &Path, search_directory: bool) -> io::Result<File> {
    let _ = search_directory;
    canonical
        .strip_prefix(root)
        .map_err(|_| io::Error::from_raw_os_error(libc::EACCES))?;
    File::open(canonical)
}

#[cfg(unix)]
pub(crate) fn open_directory_beneath(root: &Path, path: &Path) -> io::Result<File> {
    open_checked(root, path, true)
}

/// A NOFOLLOW leaf open reports ELOOP, but a platform may report ENOTDIR
/// instead; confirm with a NOFOLLOW stat on the pinned parent before
/// deciding the leaf is a raced-in link. The stat stays on that parent,
/// even in a race.
#[cfg(unix)]
fn leaf_open_error(parent: &File, name: &std::ffi::OsStr, error: io::Error) -> io::Error {
    use rustix::fs::{AtFlags, FileType, statat};
    if error.raw_os_error() == Some(libc::ENOTDIR)
        && let Ok(stat) = statat(parent, name, AtFlags::SYMLINK_NOFOLLOW)
        && FileType::from_raw_mode(stat.st_mode) == FileType::Symlink
    {
        return io::Error::new(io::ErrorKind::PermissionDenied, NoFollowLoop);
    }
    checked_open_error(error)
}

#[cfg(unix)]
fn walk_beneath(root: &File, relative: &Path, search_directory: bool) -> io::Result<File> {
    use rustix::fs::{Mode, OFlags, openat};
    let mut file = root.try_clone()?;
    let components: Vec<_> = relative.components().collect();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(io::Error::from_raw_os_error(libc::EACCES));
        };
        let flags = if index + 1 < components.len() || search_directory {
            directory_flags()
        } else {
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK
        };
        file = File::from(
            openat(&file, *name, flags, Mode::empty())
                .map_err(io::Error::from)
                .map_err(|e| leaf_open_error(&file, name, e))?,
        );
    }
    Ok(file)
}
/// Node's default `w` semantics: open/create, verify the inode, then truncate.
/// Never truncate before rejecting a raced symlink, FIFO, or shared hardlink.
/// The nlink/type check is a deliberate, registered divergence from TS's
/// write-through: specs/gateway-parity/diffs/A6.6-EDGECLAW-WRITE-INODE-001.
#[cfg(unix)]
pub(crate) fn overwrite_at(root: &File, relative: &Path, bytes: &[u8]) -> io::Result<()> {
    use rustix::fs::{FileType, Mode, OFlags, fstat, openat};
    use std::io::Write;
    let parent = walk_beneath(
        root,
        relative.parent().ok_or(io::ErrorKind::InvalidInput)?,
        true,
    )?;
    let mut file = File::from(
        openat(
            &parent,
            relative.file_name().ok_or(io::ErrorKind::InvalidInput)?,
            OFlags::WRONLY | OFlags::CREATE | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::from_raw_mode(0o666),
        )
        .map_err(io::Error::from)
        .map_err(checked_open_error)?,
    );
    let stat = fstat(&file)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile || stat.st_nlink != 1 {
        return Err(io::Error::from_raw_os_error(libc::EACCES));
    }
    file.set_len(0)?;
    // The truncate rides the open in Node's `writeFileSync` (`w` implies
    // O_TRUNC); a descriptor write error carries no path, so stage it for the
    // callers' Node-style diagnostics.
    file.write_all(bytes)
        .map_err(|e| crate::node_io::descriptor_error(e, "write"))
}

#[cfg(unix)]
pub(crate) fn unlink_at(root: &File, relative: &Path) -> io::Result<()> {
    let parent = walk_beneath(
        root,
        relative.parent().ok_or(io::ErrorKind::InvalidInput)?,
        true,
    )?;
    rustix::fs::unlinkat(
        &parent,
        relative.file_name().ok_or(io::ErrorKind::InvalidInput)?,
        rustix::fs::AtFlags::empty(),
    )?;
    Ok(())
}

/// Read a file below a pinned root without following a raced-in link. The
/// leaf keeps NONBLOCK so a raced FIFO cannot hang the open or the read.
/// A search failure above the leaf is reported as missing: the file may
/// merely sit beneath an unsearchable or non-directory ancestor, which
/// Node's existsSync swallows to false (and the read then yields no record,
/// not a permission error). The leaf's own open errors stay errors.
#[cfg(unix)]
pub(crate) fn read_at(root: &File, relative: &Path) -> io::Result<Vec<u8>> {
    use rustix::fs::{Mode, OFlags, openat};
    let mut file = if relative.as_os_str().is_empty() {
        root.try_clone()?
    } else {
        let parent = match walk_beneath(
            root,
            relative.parent().ok_or(io::ErrorKind::InvalidInput)?,
            true,
        ) {
            Ok(parent) => parent,
            // NOFOLLOW may report ENOTDIR on a directory link, which the
            // walk wraps in NoFollowLoop. Preserve Node's readFile ELOOP
            // shape rather than treating this as an inaccessible ancestor.
            Err(e) if nofollow_loop(&e) => {
                return Err(io::Error::from_raw_os_error(libc::ELOOP));
            }
            Err(e) if matches!(e.raw_os_error(), Some(libc::EACCES) | Some(libc::ENOTDIR)) => {
                return Err(io::Error::from_raw_os_error(libc::ENOENT));
            }
            Err(e) => return Err(e),
        };
        let name = relative.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let open_result = openat(
            &parent,
            name,
            OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
            Mode::empty(),
        )
        .map_err(io::Error::from);
        File::from(match open_result {
            Ok(fd) => fd,
            Err(e) => {
                // On Linux, walk_beneath uses O_PATH which skips the x-bit
                // check on each opened component; a directory ancestor with
                // mode 0o600 (no x) is opened successfully but the *leaf*
                // openat then returns EACCES. Probe with statat to tell
                // apart "ancestor unreachable → report as missing" from
                // "leaf exists but unreadable → keep the error", matching
                // Node's existsSync-then-readFile semantics (existsSync
                // swallows EACCES to false → no record, not a permission
                // error). macOS O_SEARCH already fails at walk time and is
                // mapped to ENOENT above; this probe is a no-op there.
                if matches!(e.raw_os_error(), Some(libc::EACCES) | Some(libc::ENOTDIR)) {
                    use rustix::fs::{AtFlags, statat};
                    let reachable = statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).is_ok();
                    if !reachable {
                        return Err(io::Error::from_raw_os_error(libc::ENOENT));
                    }
                }
                return Err(leaf_open_error(&parent, name, e));
            }
        })
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// List the regular files of a directory below a pinned root, without
/// following a raced-in link. Enumeration needs read permission on the
/// descriptor, unlike the search-only walk (cf. remove_child_at).
#[cfg(unix)]
pub(crate) fn regular_files_at(root: &File, relative: &Path) -> io::Result<Vec<String>> {
    use rustix::fs::{Dir, Mode, OFlags, openat};
    let directory = if relative.as_os_str().is_empty() {
        root.try_clone()?
    } else {
        let parent = match walk_beneath(
            root,
            relative.parent().ok_or(io::ErrorKind::InvalidInput)?,
            true,
        ) {
            Ok(parent) => parent,
            // Match the existsSync probe used by memory directory reads.
            // Only ancestor search failures mean absent; leaf scandir and
            // NOFOLLOW failures retain their existing error semantics.
            Err(e) if matches!(e.raw_os_error(), Some(libc::EACCES) | Some(libc::ENOTDIR)) => {
                return Err(io::Error::from_raw_os_error(libc::ENOENT));
            }
            Err(e) => return Err(e),
        };
        let name = relative.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let open_result = openat(
            &parent,
            name,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC | OFlags::NOFOLLOW,
            Mode::empty(),
        )
        .map_err(io::Error::from);
        match open_result {
            Ok(fd) => File::from(fd),
            Err(e) => {
                // On Linux, walk_beneath uses O_PATH which skips x-bit checks on
                // each ancestor, so an inaccessible ancestor's EACCES/ENOTDIR
                // may surface only here at the leaf openat. Probe with statat to
                // distinguish "ancestor unreachable → treat as absent (ENOENT)"
                // from "leaf itself exists but is unreadable → keep the error".
                let raw = e.raw_os_error();
                if matches!(raw, Some(libc::EACCES) | Some(libc::ENOTDIR)) {
                    use rustix::fs::{AtFlags, statat};
                    let reachable = statat(&parent, name, AtFlags::SYMLINK_NOFOLLOW).is_ok();
                    if reachable {
                        // Leaf exists but is unreadable: preserve the original error.
                        return Err(leaf_open_error(&parent, name, e));
                    } else {
                        // Leaf unreachable (ancestor blocked): mirror existsSync → absent.
                        return Err(io::Error::from_raw_os_error(libc::ENOENT));
                    }
                }
                return Err(leaf_open_error(&parent, name, e));
            }
        }
    };
    let mut names = Vec::new();
    for entry in Dir::read_from(&directory).map_err(io::Error::from)? {
        let entry = entry.map_err(io::Error::from)?;
        if entry.file_type() == rustix::fs::FileType::RegularFile {
            names.push(node_dirent_name(entry.file_name()));
        }
    }
    Ok(names)
}

/// Recursive removal never follows a link, including one installed after stat.
/// Diagnostics retain Node's syscall and the original display path.
#[cfg(unix)]
pub(crate) fn remove_tree_at(root: &File, relative: &Path, display: &Path) -> io::Result<()> {
    let parent = walk_beneath(
        root,
        relative.parent().ok_or(io::ErrorKind::InvalidInput)?,
        true,
    )?;
    remove_child_at(
        &parent,
        relative.file_name().ok_or(io::ErrorKind::InvalidInput)?,
        display,
    )
}
/// Node's rimraf descends on a non-empty rmdir however it is reported:
/// ENOTEMPTY, EEXIST, or a sticky directory's EPERM (node_fs::rm_sync,
/// measured from libuv: EACCES is never a reason to descend).
#[cfg(unix)]
fn descends_after_rmdir(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::DirectoryNotEmpty
        || error.kind() == io::ErrorKind::AlreadyExists
        || crate::node_io::lookup(error).is_some_and(|(code, _)| code == "EPERM")
}

#[cfg(unix)]
fn remove_child_at(parent: &File, name: &std::ffi::OsStr, display: &Path) -> io::Result<()> {
    use rustix::fs::{AtFlags, Dir, Mode, OFlags, openat, unlinkat};
    use std::os::unix::ffi::OsStrExt;
    // Keep paths, not directory handles, across descent. Rewalking from the
    // pinned anchor costs O(depth) per entry but uses a constant number of
    // descriptors and checks every ancestor with NOFOLLOW on every visit.
    let mut pending = vec![(PathBuf::from(name), display.to_path_buf(), false)];
    while let Some((relative, display, visited)) = pending.pop() {
        let directory = walk_beneath(
            parent,
            relative.parent().ok_or(io::ErrorKind::InvalidInput)?,
            true,
        )?;
        let name = relative.file_name().ok_or(io::ErrorKind::InvalidInput)?;
        let unlink = |flags, syscall| {
            unlinkat(&directory, name, flags)
                .map_err(|e| crate::node_io::path_error(e.into(), syscall, &display))
        };
        if visited {
            unlink(AtFlags::REMOVEDIR, "rmdir")?;
            continue;
        }
        match unlink(AtFlags::REMOVEDIR, "rmdir") {
            Ok(()) => continue,
            Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
            Err(e) if e.kind() == io::ErrorKind::NotADirectory => {
                unlink(AtFlags::empty(), "unlink")?;
                continue;
            }
            // On Linux, unlinkat(AT_REMOVEDIR) on a regular file (or symlink)
            // whose parent directory is not writable returns EACCES rather than
            // ENOTDIR, so the ENOTDIR arm above is never reached. Similarly,
            // EPERM is returned for files in sticky directories (S_ISVTX) or
            // with immutable/append-only attributes. Probe with statat to tell
            // apart "non-directory entry → re-report as unlink" from "directory
            // → handle per error code", matching Node's rmSync which lstat()s
            // before choosing the syscall.
            // Use lookup() to match by code string: std maps both EACCES and
            // EPERM to PermissionDenied so we must distinguish them explicitly.
            Err(e)
                if crate::node_io::lookup(&e)
                    .is_some_and(|(code, _)| code == "EACCES" || code == "EPERM") =>
            {
                use rustix::fs::{AtFlags as AF2, statat};
                let is_dir = statat(&directory, name, AF2::SYMLINK_NOFOLLOW)
                    .map(|st| (st.st_mode & libc::S_IFMT) == libc::S_IFDIR)
                    .unwrap_or(true); // unknown → treat as dir, preserve error
                if !is_dir {
                    // Non-directory (regular file, symlink, …): re-report with
                    // "unlink" syscall label to match Node's lstat-first rmSync.
                    unlink(AtFlags::empty(), "unlink")?;
                    continue;
                }
                // Directory + EACCES: return immediately (Node does not descend
                // on EACCES, only on ENOTEMPTY/EEXIST/EPERM per descends_after_rmdir).
                if crate::node_io::lookup(&e).is_some_and(|(code, _)| code == "EACCES") {
                    return Err(e);
                }
                // Directory + EPERM (sticky dir or immutable): fall through to
                // the descends_after_rmdir arm which returns true for EPERM.
            }
            Err(e) if descends_after_rmdir(&e) => {}
            Err(e) => return Err(e),
        }
        // Open for reading only here: enumeration, unlike traversal, needs it.
        // Enumeration failures keep Node's scandir syscall and this directory's
        // display path, like the path-based node_fs::rm_sync they replace; a
        // raced link stays NoFollowLoop instead of gaining a scandir stage.
        let scandir = |error: io::Error| {
            if nofollow_loop(&error) {
                error
            } else {
                crate::node_io::path_error(error, "scandir", &display)
            }
        };
        let dir = File::from(
            openat(
                &directory,
                name,
                OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
                Mode::empty(),
            )
            .map_err(io::Error::from)
            .map_err(checked_open_error)
            .map_err(scandir)?,
        );
        let mut children = Dir::read_from(&dir)
            .map_err(io::Error::from)
            .and_then(|entries| {
                entries
                    .map(|entry| {
                        entry
                            .map(|entry| entry.file_name().to_owned())
                            .map_err(io::Error::from)
                    })
                    .collect::<std::result::Result<Vec<_>, _>>()
            })
            .map_err(scandir)?;
        children.sort();
        // Release the enumeration descriptor and this entry's parent before
        // processing children; no stack frame holds a descriptor for an ancestor.
        drop(dir);
        drop(directory);
        pending.push((relative.clone(), display.clone(), true));
        for child in children.into_iter().rev() {
            let child = std::ffi::OsStr::from_bytes(child.to_bytes());
            if child == std::ffi::OsStr::new(".") || child == std::ffi::OsStr::new("..") {
                continue;
            }
            pending.push((relative.join(child), display.join(child), false));
        }
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn native_stat(root: &Path, path: &Path) -> io::Result<(Stat, bool)> {
    use rustix::fs::{AtFlags, FileType, fstat, statat};
    let stat = if root == path {
        fstat(open_checked(root, root, true)?)?
    } else {
        let parent = open_checked(
            root,
            path.parent().ok_or(io::ErrorKind::InvalidInput)?,
            true,
        )?;
        statat(
            &parent,
            path.file_name().ok_or(io::ErrorKind::InvalidInput)?,
            AtFlags::SYMLINK_NOFOLLOW,
        )?
    };
    let kind = FileType::from_raw_mode(stat.st_mode);
    Ok((
        Stat {
            directory: kind == FileType::Directory,
            size: stat.st_size as u64,
            birth_ms: 0.0,
            ctime_ms: 0.0,
            mtime_ms: stat.st_mtime as f64 * 1000.0 + stat.st_mtime_nsec as f64 / 1_000_000.0,
        },
        kind == FileType::RegularFile,
    ))
}

/// Non-Unix keeps the measured Node behavior: lstat semantics on the
/// realpath-checked path, like the descriptor walk it mirrors.
#[cfg(not(unix))]
pub(crate) fn native_stat(root: &Path, path: &Path) -> io::Result<(Stat, bool)> {
    let _ = root;
    let stat = std::fs::symlink_metadata(path)?;
    let mtime_ms = stat
        .modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0.0, |elapsed| elapsed.as_millis() as f64);
    Ok((
        Stat {
            directory: stat.is_dir(),
            size: stat.len(),
            birth_ms: 0.0,
            ctime_ms: 0.0,
            mtime_ms,
        },
        stat.is_file(),
    ))
}

pub(crate) use crate::node_locale::node_locale;

fn lowercase(value: &str, locale: &LanguageIdentifier, substring: bool) -> String {
    // Node v22.22.0 / V8 12.4: Latin-1 fast casing ignores the locale only
    // for non-sliced strings. Substrings shorter than 13 UTF-16 units are
    // copied; longer ones remain slices. This is representation-dependent,
    // not a Unicode guarantee: remeasure on engine upgrades (round4/5 fixtures).
    let sliced = substring && value.encode_utf16().count() >= 13;
    let root = LanguageIdentifier::UNKNOWN;
    let locale = if !sliced && value.encode_utf16().all(|unit| unit <= 0xff) {
        &root
    } else {
        locale
    };
    CaseMapper::new()
        .lowercase_to_string(value, locale)
        .into_owned()
}

// Retained for independently scoped archive/storage host boundaries.
pub(crate) fn has_unpaired_surrogate(value: &pilotdeck_protocol::JsString) -> bool {
    char::decode_utf16(value.units().iter().copied()).any(|unit| unit.is_err())
}

/// Node/ICU4C collation: isolated UTF-16 units retain their own implicit CE,
/// while valid runs use ICU4X normalization and the pinned locale data.
pub(crate) fn node_compare_utf16(
    collator: &CollatorBorrowed<'static>,
    left: &pilotdeck_protocol::JsString,
    right: &pilotdeck_protocol::JsString,
) -> Ordering {
    collator.compare_utf16_preserving_surrogates(left.units(), right.units())
}

fn node_dirent_name(name: &std::ffi::CStr) -> String {
    name.to_string_lossy().into_owned()
}

// Generated ICU 78.2 / CLDR 48 collation data uses the pinned Node oracle's
// Unihan root, with matching tailorings. Unsupported collation extensions
// retain Node's fallback to the language default, then root.
pub(crate) fn node_collator(locale: &Locale) -> Result<CollatorBorrowed<'static>> {
    let mut preferences: CollatorPreferences = locale.clone().into();
    let create = |preferences: CollatorPreferences| {
        Collator::try_new(preferences, Default::default()).map(|collator| {
            // The pinned baked metadata omits fr-CA's backward-secondary bit.
            // Restore the Node/ICU78 default; explicit collations (e.g. emoji)
            // retain their own metadata. See native-collation/oracle.json.
            if locale.id.language.as_str() == "fr"
                && locale.id.script.is_none()
                && locale
                    .id
                    .region
                    .is_some_and(|region| region.as_str() == "CA")
                && preferences.collation_type.is_none()
            {
                collator.with_backward_second_level(true)
            } else {
                collator
            }
        })
    };
    create(preferences)
        .or_else(|_| {
            preferences.collation_type = None;
            create(preferences)
        })
        .or_else(|_| Collator::try_new(Default::default(), Default::default()))
        .map_err(|_| {
            (
                "gateway_request_failed",
                "Collation data unavailable.".into(),
            )
        })
}

#[cfg(unix)]
pub(crate) fn native_entries(root: &Path, path: &Path) -> io::Result<Vec<(String, bool)>> {
    let file = open_beneath(root, path)?;
    let dir = rustix::fs::Dir::read_from(&file)?;
    let mut entries = Vec::new();
    for entry in dir {
        let entry = entry?;
        let name = node_dirent_name(entry.file_name());
        if name == "." || name == ".." {
            continue;
        }
        entries.push((name, entry.file_type() == rustix::fs::FileType::Directory));
        // Enough entries to force the method's existing FILE_INDEX_LIMIT.
        if entries.len() > FILE_INDEX_LIMIT {
            break;
        }
    }
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(entries)
}

/// Non-Unix keeps the measured Node behavior: scandir on the realpath-checked
/// path; std's directory iterator already yields d_type without following links.
#[cfg(not(unix))]
pub(crate) fn native_entries(root: &Path, path: &Path) -> io::Result<Vec<(String, bool)>> {
    let _ = root;
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(path)? {
        let entry = entry?;
        entries.push((
            entry.file_name().to_string_lossy().into_owned(),
            entry.file_type()?.is_dir(),
        ));
        // Enough entries to force the method's existing FILE_INDEX_LIMIT.
        if entries.len() > FILE_INDEX_LIMIT {
            break;
        }
    }
    entries.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
    Ok(entries)
}
pub(crate) fn read_prefix(reader: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut output = Vec::new();
    let mut chunk = [0; 8192];
    while output.len() < limit {
        let count = (limit - output.len()).min(chunk.len());
        let read = match reader.read(&mut chunk[..count]) {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => other?,
        };
        if read == 0 {
            break;
        }
        output.extend_from_slice(&chunk[..read]);
    }
    Ok(output)
}

// Invalid JS argument shapes with util.inspect-specific errors remain TS-owned.
// Never forward a string path on security grounds: that check is local below.
pub(crate) fn execute<F: Files>(
    metadata: &Metadata<F>,
    method: &str,
    params: &JsValue,
) -> Option<Result<JsValue>> {
    let project = text_field(params, "projectKey")?;
    if method == "project_file_read" {
        // Node encodes lone UTF-16 surrogates as U+FFFD for filesystem access.
        // Keep all string paths on the local containment/NOFOLLOW path, while
        // preserving the original JS string in the response.
        let path = params.get("path")?.as_string()?;
        let path = String::from_utf16_lossy(path.units());
        let result = read(metadata, params, &project, &path);
        // A too-deep maxBytes is answered here too: forwarding would hand the
        // string path to TS's non-race-safe open.
        if matches!(&result, Err((_, m)) if m == crate::js_value::StackOverflow::MESSAGE) {
            return Some(result);
        }
        return local_result(result);
    }
    let query = match params.get("query") {
        None | Some(JsValue::Null | JsValue::Undefined) => String::new(),
        Some(value) => crate::metadata::string(value)?,
    };
    // A non-string truthy cursor has Node-specific Buffer argument diagnostics.
    if let Some(cursor) = params.get("cursor")
        && truthy(cursor)
        && cursor.as_string().is_none()
    {
        return None;
    }
    local_result(list(metadata, params, &project, &query))
}
// Node owns unmeasured platform-specific IO diagnostics. Keep those errors
// upstream rather than manufacturing a different errno/message. Security errors
// are already resolved locally and never take this fallback.
fn local_result(result: Result<JsValue>) -> Option<Result<JsValue>> {
    if matches!(
        &result,
        Err(("gateway_request_failed" | "NODE_IO_REQUIRED", _))
    ) {
        None
    } else {
        Some(result)
    }
}
fn registered<F: Files>(metadata: &Metadata<F>, project: &str, general: bool) -> Result<PathBuf> {
    let requested = crate::metadata::resolve_process_path(&metadata.cwd, Path::new(project))
        .map_err(|e| crate::metadata::process_cwd_diagnostic(&e))?;
    if general && requested == metadata.home {
        return Ok(requested);
    }
    // File RPCs enumerate the full web project list; unlike model/archive
    // registration they must not short-circuit at the requested direct id.
    let listed = metadata
        .list_projects()
        .map_err(|e| crate::metadata::process_cwd_diagnostic(&e))?;
    if let Some(JsValue::Array(projects)) = listed.get("projects") {
        for item in projects {
            if let Some(key) = text_field(item, "projectKey") {
                let key = crate::metadata::resolve_process_path(&metadata.cwd, Path::new(&key))
                    .map_err(|e| crate::metadata::process_cwd_diagnostic(&e))?;
                if key == requested {
                    return Ok(key);
                }
            }
        }
    }
    Err((
        "PROJECT_NOT_FOUND",
        format!("Unknown projectKey: {project}"),
    ))
}
fn read<F: Files>(
    metadata: &Metadata<F>,
    params: &JsValue,
    project: &str,
    path: &str,
) -> Result<JsValue> {
    let root = registered(metadata, project, true)?;
    let root = metadata
        .files
        .canonical(&root)
        .map_err(|e| fs_error("project_file_read", e, "realpath", &root))?;
    let absolute = resolve(&root, path);
    // TS pathSafety also rejects '..name' prefixes, unlike listProjectFiles.
    if !within_read(&root, &absolute) {
        return Err(denied(OUTSIDE));
    }
    let canonical = metadata.files.canonical(&absolute).map_err(|e| {
        measured("project_file_read", &e)
            .err()
            .unwrap_or_else(|| denied(ESCAPE))
    })?;
    if !within_read(&root, &canonical) {
        return Err(denied(ESCAPE));
    }
    let stat = match metadata.files.project_stat(&root, &canonical) {
        Ok((stat, true)) => stat,
        Ok(_) => return Ok(JsValue::Null),
        Err(e) => {
            measured("project_file_read", &e)?;
            return Ok(JsValue::Null);
        }
    };
    // Node readFile opens before checking size, then slices to maxBytes.
    // A zero-byte prefix performs our safe open without reading the huge file,
    // so EACCES (and NOFOLLOW failures) take precedence over the size error.
    if stat.size > 2_147_483_647 {
        metadata
            .files
            .project_read(&root, &canonical, 0)
            .map_err(|e| fs_error("project_file_read", e, "open", &canonical))?;
        return Err((
            "ERR_FS_FILE_TOO_LARGE",
            format!("File size ({}) is greater than 2 GiB", stat.size),
        ));
    }
    let number = match params.get("maxBytes") {
        None | Some(JsValue::Null | JsValue::Undefined) => 1_000_000.0,
        Some(value) => crate::js_value::to_number(value).map_err(stack_overflow)?,
    };
    let limit = if number.is_nan() {
        0
    } else {
        number.clamp(1.0, 10_000_000.0) as usize
    };
    let bytes = metadata
        .files
        .project_read(&root, &canonical, limit)
        .map_err(|e| fs_error("project_file_read", e, "open", &canonical))?;
    let (content, encoding) = if text_field(params, "encoding").as_deref() == Some("base64") {
        (STANDARD.encode(bytes), "base64")
    } else {
        (String::from_utf8_lossy(&bytes).into_owned(), "utf-8")
    };
    Ok(object([
        (
            "path",
            params.get("path").cloned().unwrap_or_else(|| path.into()),
        ),
        ("content", content.as_str().into()),
        ("encoding", encoding.into()),
    ]))
}
fn within_read(root: &Path, path: &Path) -> bool {
    path.strip_prefix(root)
        .is_ok_and(|p| !p.to_string_lossy().starts_with(".."))
}
fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Null | JsValue::Undefined => false,
        JsValue::Bool(v) => *v,
        JsValue::Number(n) => *n != 0.0 && !n.is_nan(),
        JsValue::String(s) => !s.is_empty(),
        _ => true,
    }
}
fn fs_error(method: &str, error: io::Error, operation: &str, path: &Path) -> Error {
    // NOFOLLOW detects a link installed after canonicalization. Never send that
    // request to the TS fallback, whose check/open sequence is not race-safe.
    if nofollow_loop(&error) {
        return denied(ESCAPE);
    }
    if let Err(error) = measured(method, &error) {
        return error;
    }
    crate::node_io::diagnostic(&error, operation, Some(&path.to_string_lossy()))
        .unwrap_or(("NODE_IO_REQUIRED", String::new()))
}
const FILE_INDEX_DEADLINE: Duration = Duration::from_secs(5);

fn list<F: Files>(
    metadata: &Metadata<F>,
    params: &JsValue,
    project: &str,
    query: &str,
) -> Result<JsValue> {
    list_with_deadline(metadata, params, project, query, FILE_INDEX_DEADLINE)
}

/// The scan deadline is a parameter so the 100k disappearing-entry replay can
/// prove the counter does not fail closed without racing a real 5 s wall clock
/// under parallel cargo load (#346).
fn list_with_deadline<F: Files>(
    metadata: &Metadata<F>,
    params: &JsValue,
    project: &str,
    query: &str,
    deadline: Duration,
) -> Result<JsValue> {
    let registered = registered(metadata, project, false)?;
    let root = metadata.files.canonical(&registered).map_err(|e| {
        if let Err(error) = measured("project_files_list", &e) {
            return error;
        }
        (
            "PROJECT_NOT_FOUND",
            format!("Project does not exist: {project}"),
        )
    })?;
    if !metadata
        .files
        .project_stat(&root, &root)
        .map_err(|e| fs_error("project_files_list", e, "stat", &root))?
        .0
        .directory
    {
        return Err((
            "PROJECT_NOT_FOUND",
            format!("Project is not a directory: {project}"),
        ));
    }
    let locale = node_locale(|key| std::env::var(key).ok());
    let trimmed = js_trim(query);
    let query = lowercase(trimmed, &locale.id, trimmed != query);
    if query.encode_utf16().count() > 256 {
        return Err((
            "INVALID_QUERY",
            "query must not exceed 256 characters.".into(),
        ));
    }
    let include_dirs = !matches!(params.get("includeDirs"), Some(JsValue::Bool(false)));
    let limit = match params.get("limit") {
        None | Some(JsValue::Undefined) => 100,
        Some(JsValue::Number(n)) if n.fract() == 0.0 && (1.0..=500.0).contains(n) => *n as usize,
        _ => {
            return Err((
                "INVALID_LIMIT",
                "limit must be an integer between 1 and 500.".into(),
            ));
        }
    };
    let signature = URL_SAFE_NO_PAD.encode(Sha256::digest(encode(&object([
        ("root", root.to_string_lossy().as_ref().into()),
        ("query", query.as_str().into()),
        ("includeDirs", JsValue::Bool(include_dirs)),
    ]))));
    let invalid_cursor = || {
        (
            "INVALID_CURSOR",
            "cursor is invalid or does not match the query.".into(),
        )
    };
    let offset = match params
        .get("cursor")
        .and_then(JsValue::as_string)
        .filter(|v| !v.is_empty())
    {
        None => 0.0,
        Some(cursor) => {
            let bytes = decode_base64(cursor.units()).ok_or_else(invalid_cursor)?;
            let parsed =
                parse_json(&String::from_utf8_lossy(&bytes)).map_err(|_| invalid_cursor())?;
            let offset = parsed
                .get("offset")
                .and_then(JsValue::as_number)
                .ok_or_else(invalid_cursor)?;
            if parsed.get("version").and_then(JsValue::as_number) != Some(1.0)
                || text_field(&parsed, "signature").as_deref() != Some(signature.as_str())
                || offset < 0.0
                || offset.fract() != 0.0
            {
                return Err(invalid_cursor());
            }
            offset
        }
    };
    let mut scan = Scan {
        metadata,
        root: &root,
        collator: node_collator(&locale)?,
        query: &query,
        locale: locale.id,
        include_dirs,
        started: Instant::now(),
        deadline,
        count: 0,
        visited: HashSet::from([root.clone()]),
        entries: Vec::new(),
    };
    scan.walk(&root, 0)?;
    scan.entries.sort_by(|a, b| {
        scan.collator.compare(
            &text_field(a, "relativePath").unwrap(),
            &text_field(b, "relativePath").unwrap(),
        )
    });
    let start = (offset as usize).min(scan.entries.len());
    let end = (start + limit).min(scan.entries.len());
    let next = if end < scan.entries.len() {
        URL_SAFE_NO_PAD.encode(encode(&object([
            ("version", JsValue::Number(1.0)),
            ("offset", JsValue::Number(end as f64)),
            ("signature", signature.as_str().into()),
        ])))
    } else {
        String::new()
    };
    let mut fields = vec![
        (
            "items",
            JsValue::Array(scan.entries.drain(start..end).collect()),
        ),
        ("projectKey", root.to_string_lossy().as_ref().into()),
    ];
    if !next.is_empty() {
        fields.push(("nextCursor", next.as_str().into()));
    }
    Ok(object(fields))
}
fn decode_base64(value: &[u16]) -> Option<Vec<u8>> {
    // Buffer.from(base64url) accepts standard alphabet, whitespace, missing
    // padding, and ignores other characters. A trailing sextet is discarded.
    // Buffer uses each UTF-16 code unit's low byte, including lone surrogates.
    let mut clean: String = value
        .iter()
        .map(|unit| char::from(*unit as u8))
        .take_while(|c| *c != '=')
        .filter_map(|c| match c {
            '+' => Some('-'),
            '/' => Some('_'),
            c if c.is_ascii_alphanumeric() || c == '-' || c == '_' => Some(c),
            _ => None,
        })
        .collect();
    if clean.len() % 4 == 1 {
        clean.pop();
    }
    use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
    GeneralPurpose::new(
        &base64::alphabet::URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_allow_trailing_bits(true)
            .with_decode_padding_mode(DecodePaddingMode::Indifferent),
    )
    .decode(clean)
    .ok()
}
struct Scan<'a, F> {
    metadata: &'a Metadata<F>,
    root: &'a Path,
    collator: CollatorBorrowed<'static>,
    query: &'a str,
    locale: LanguageIdentifier,
    include_dirs: bool,
    started: Instant,
    deadline: Duration,
    count: usize,
    visited: HashSet<PathBuf>,
    entries: Vec<JsValue>,
}
impl<F: Files> Scan<'_, F> {
    fn walk(&mut self, directory: &Path, depth: usize) -> Result<()> {
        if depth > 64 || self.count >= FILE_INDEX_LIMIT || self.started.elapsed() > self.deadline {
            return Err(index_limit());
        }
        let mut children = self
            .metadata
            .files
            .project_entries(self.root, directory)
            .map_err(|e| fs_error("project_files_list", e, "scandir", directory))?;
        children.sort_by(|a, b| self.collator.compare(&a.0, &b.0));
        for (name, is_directory) in children {
            self.count += 1;
            if self.count > FILE_INDEX_LIMIT || self.started.elapsed() > self.deadline {
                return Err(index_limit());
            }
            if is_directory && EXCLUDED.contains(&name.as_str()) {
                continue;
            }
            let canonical = match self.metadata.files.canonical(&directory.join(&name)) {
                Ok(path) => path,
                Err(e) => {
                    measured("project_files_list", &e)?;
                    continue;
                }
            };
            let Ok(relative) = canonical.strip_prefix(self.root) else {
                continue;
            };
            if relative.as_os_str().is_empty() {
                continue;
            }
            let (stat, is_file) = match self.metadata.files.project_stat(self.root, &canonical) {
                Ok(stat) => stat,
                Err(e) => {
                    measured("project_files_list", &e)?;
                    continue;
                }
            };
            if !stat.directory && !is_file {
                continue;
            }
            let relative = relative.to_string_lossy();
            let mut matches = Vec::new();
            if !self.query.is_empty() {
                append_matches(
                    &mut matches,
                    "name",
                    &name,
                    self.query,
                    &self.locale,
                    self.root,
                );
                append_matches(
                    &mut matches,
                    "relativePath",
                    &relative,
                    self.query,
                    &self.locale,
                    self.root,
                );
            }
            if (self.query.is_empty() || !matches.is_empty()) && (is_file || self.include_dirs) {
                let mut hash = Sha256::new();
                hash.update(self.root.to_string_lossy().as_bytes());
                hash.update([0]);
                hash.update(relative.as_bytes());
                let id = format!("sha256:{:x}", hash.finalize());
                let mut fields = vec![
                    ("id", id.as_str().into()),
                    (
                        "name",
                        canonical
                            .file_name()
                            .unwrap()
                            .to_string_lossy()
                            .as_ref()
                            .into(),
                    ),
                    ("relativePath", relative.as_ref().into()),
                    (
                        "kind",
                        if stat.directory {
                            "directory".into()
                        } else {
                            "file".into()
                        },
                    ),
                    (
                        "size",
                        JsValue::Number(if stat.directory {
                            0.0
                        } else {
                            stat.size as f64
                        }),
                    ),
                    ("mtimeMs", JsValue::Number(stat.mtime_ms)),
                ];
                if !matches.is_empty() {
                    fields.push(("matches", JsValue::Array(matches)));
                }
                self.entries.push(object(fields));
            }
            if stat.directory && self.visited.insert(canonical.clone()) {
                self.walk(&canonical, depth + 1)?;
            }
        }
        Ok(())
    }
}
fn append_matches(
    output: &mut Vec<JsValue>,
    field: &str,
    value: &str,
    query: &str,
    locale: &LanguageIdentifier,
    root: &Path,
) {
    // The baseline's relative(root, canonical).split(sep).join("/") keeps
    // a top-level substring. With a two-byte canonical root, any segment
    // slice of >=13 UTF-16 units also makes the joined path use locale casing.
    // Dirent.name is not a slice. These are measured V8 representation rules,
    // not exhaustive guarantees (see the fixture README's residual risk).
    let substring = field == "relativePath"
        && (!value.contains('/')
            || (root.to_string_lossy().chars().any(|c| c > '\u{ff}')
                && value
                    .split('/')
                    .any(|segment| segment.encode_utf16().count() >= 13)));
    let lower = lowercase(value, locale, substring);
    for (byte, _) in lower.match_indices(query) {
        let start = lower[..byte].encode_utf16().count();
        output.push(object([
            ("field", field.into()),
            ("start", JsValue::Number(start as f64)),
            (
                "end",
                JsValue::Number((start + query.encode_utf16().count()) as f64),
            ),
        ]));
    }
}
#[cfg(test)]
pub(crate) mod tests;
