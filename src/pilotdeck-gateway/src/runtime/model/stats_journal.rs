//! Retention lock-timeout sidecars (A6.4-STATS-SIDECAR-001).
//!
//! A merge receipt and a hard link to the replacement journal are published
//! before replacing the main path. Inode equality is the commit bit: readers
//! never count both an included sidecar and the committed replacement. Keep
//! the receipt until all included sidecars have been durably removed.
use super::{JsValue, parse_stats_records, read_bounded_text};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

type Identity = (u64, u64);

#[derive(Clone, PartialEq, Eq)]
struct Receipt {
    manifest: String,
    journal: Identity,
}

fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    name.into()
}

fn identity(path: &Path) -> io::Result<Option<Identity>> {
    match fs::metadata(path) {
        Ok(meta) => Ok(Some((meta.dev(), meta.ino()))),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn sync_dir(path: &Path) -> io::Result<()> {
    File::open(path)?.sync_all()
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut file = OpenOptions::new().write(true).create_new(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Publish one immutable complete JSONL row. An incomplete `.tmp` is invisible.
pub(super) fn append_pending(jsonl: &Path, bytes: &[u8]) -> io::Result<()> {
    let directory = sibling(jsonl, ".pending");
    fs::create_dir_all(&directory)?;
    sync_dir(parent(jsonl))?;
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = format!(
        "{nanos:039}.{}.{id}",
        std::process::id(),
        id = uuid::Uuid::new_v4()
    );
    let temporary = directory.join(format!("{name}.tmp"));
    write_new(&temporary, bytes)?;
    fs::rename(&temporary, directory.join(format!("{name}.jsonl")))?;
    sync_dir(&directory)
}

fn pending_names(jsonl: &Path) -> io::Result<Vec<String>> {
    let entries = match fs::read_dir(sibling(jsonl, ".pending")) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry?;
        if let Some(name) = entry
            .file_name()
            .to_str()
            .filter(|name| name.ends_with(".jsonl"))
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    Ok(names)
}

fn receipt(jsonl: &Path) -> io::Result<Option<Receipt>> {
    let directory = sibling(jsonl, ".merge");
    loop {
        let manifest = match fs::read_to_string(directory.join("manifest")) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let journal = identity(&directory.join("journal"))?;
        match fs::read_to_string(directory.join("manifest")) {
            Ok(after) if after == manifest => {
                let journal =
                    journal.ok_or_else(|| io::Error::other("incomplete stats merge receipt"))?;
                return Ok(Some(Receipt { manifest, journal }));
            }
            Ok(_) => continue,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        }
    }
}

fn consumed(receipt: &Receipt) -> io::Result<Vec<&str>> {
    // First line is a unique transaction id, including when no sidecars exist.
    receipt
        .manifest
        .lines()
        .skip(1)
        .map(|name| {
            if name.ends_with(".jsonl") && !name.contains(['/', '\\']) && name != ".." {
                Ok(name)
            } else {
                Err(io::Error::other("invalid stats sidecar name"))
            }
        })
        .collect()
}

/// Lock-free disk snapshot. Retry if replacement or receipt cleanup straddled
/// the read. Sidecars published after the directory listing belong to a later
/// snapshot, just like appends after a TS readFileSync.
pub(super) fn snapshot(jsonl: &Path) -> Option<(Vec<JsValue>, Vec<String>)> {
    loop {
        // Pin the inode so a rapid sequence of replacements cannot reuse it
        // and fool the identity comparison (ABA).
        let pinned = match File::open(jsonl) {
            Ok(file) => Some(file),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(_) => return None,
        };
        let before = pinned
            .as_ref()
            .map(|file| file.metadata().map(|meta| (meta.dev(), meta.ino())))
            .transpose()
            .ok()?;
        let merge = receipt(jsonl).ok()?;
        let committed = merge.as_ref().filter(|merge| Some(merge.journal) == before);
        let included = committed
            .map(consumed)
            .transpose()
            .ok()?
            .unwrap_or_default();
        #[cfg(test)]
        checkpoint("read-receipt");
        let raw = if before.is_some() {
            read_bounded_text(jsonl, super::super::MAX_STRING_LENGTH)?
        } else {
            String::new()
        };
        #[cfg(test)]
        checkpoint("read-main");
        let names = pending_names(jsonl).ok()?;
        let mut records = parse_stats_records(&raw);
        let mut missing = false;
        for name in &names {
            if included.contains(&name.as_str()) {
                continue;
            }
            match read_bounded_text(
                &sibling(jsonl, ".pending").join(name),
                super::super::MAX_STRING_LENGTH,
            ) {
                Some(raw) => records.extend(parse_stats_records(&raw)),
                None => {
                    missing = true;
                    break;
                }
            }
        }
        if before != identity(jsonl).ok()? || merge != receipt(jsonl).ok()? {
            std::thread::yield_now();
            continue;
        }
        // A sidecar disappearing without a receipt transition is an IO error,
        // not an empty journal to be compacted over the original data.
        return (!missing).then_some((records, names));
    }
}

/// Under the retention lock, publish a recoverable replacement. No sidecar
/// is deleted until both the replacement and its commit bit are durable.
fn prepare_merge(jsonl: &Path, bytes: &[u8], names: &[String]) -> io::Result<()> {
    let staging = sibling(jsonl, &format!(".merge.{}.tmp", uuid::Uuid::new_v4()));
    fs::create_dir(&staging)?;
    write_new(&staging.join("journal"), bytes)?;
    let manifest = std::iter::once(uuid::Uuid::new_v4().to_string())
        .chain(names.iter().cloned())
        .collect::<Vec<_>>()
        .join("\n");
    write_new(&staging.join("manifest"), manifest.as_bytes())?;
    sync_dir(&staging)?;
    fs::rename(&staging, sibling(jsonl, ".merge"))?;
    sync_dir(parent(jsonl))
}

fn commit_merge(jsonl: &Path, merge: &Receipt) -> io::Result<()> {
    if identity(jsonl)? != Some(merge.journal) {
        let temporary = sibling(jsonl, &format!(".{}.tmp", uuid::Uuid::new_v4()));
        fs::hard_link(sibling(jsonl, ".merge").join("journal"), &temporary)?;
        fs::rename(temporary, jsonl)?;
    }
    sync_dir(parent(jsonl))
}

fn clean_merge(jsonl: &Path, merge: &Receipt) -> io::Result<()> {
    let pending = sibling(jsonl, ".pending");
    let names = consumed(merge)?;
    for name in &names {
        match fs::remove_file(pending.join(name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    if !names.is_empty() {
        sync_dir(&pending)?;
    }
    // Atomic retirement keeps readers from observing a partially removed receipt.
    let garbage = sibling(jsonl, &format!(".merge.{}.garbage", uuid::Uuid::new_v4()));
    fs::rename(sibling(jsonl, ".merge"), &garbage)?;
    sync_dir(parent(jsonl))?;
    fs::remove_dir_all(garbage)
}

/// Finish a pre-crash merge before any further locked write can change main.
pub(super) fn recover(jsonl: &Path) -> io::Result<()> {
    if let Some(merge) = receipt(jsonl)? {
        commit_merge(jsonl, &merge)?;
        clean_merge(jsonl, &merge)?;
    }
    Ok(())
}

pub(super) fn replace(jsonl: &Path, bytes: &[u8], names: &[String]) -> io::Result<()> {
    #[cfg(test)]
    checkpoint("snapshot");
    prepare_merge(jsonl, bytes, names)?;
    #[cfg(test)]
    checkpoint("prepared");
    let merge = receipt(jsonl)?.ok_or_else(|| io::Error::other("missing stats merge receipt"))?;
    commit_merge(jsonl, &merge)?;
    #[cfg(test)]
    checkpoint("committed");
    clean_merge(jsonl, &merge)
}

/// Explicit clear discards the published sidecars too. Its pre-existing
/// lock-timeout race with other clear/writers is unchanged.
pub(super) fn clear_pending(jsonl: &Path) -> io::Result<()> {
    for name in pending_names(jsonl)? {
        fs::remove_file(sibling(jsonl, ".pending").join(name))?;
    }
    Ok(())
}

#[cfg(test)]
type Checkpoint = Box<dyn FnMut(&str)>;
#[cfg(test)]
thread_local! { static CHECKPOINT: std::cell::RefCell<Option<Checkpoint>> = const { std::cell::RefCell::new(None) }; }
#[cfg(test)]
fn checkpoint(stage: &str) {
    CHECKPOINT.with_borrow_mut(|hook| {
        if let Some(hook) = hook {
            hook(stage);
        }
    });
}

#[cfg(test)]
#[path = "stats_journal_tests.rs"]
mod tests;
