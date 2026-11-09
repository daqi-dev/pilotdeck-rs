//! Native seedAgentReadState: re-read the file and publish both freshness maps.
use super::*;
use crate::runtime::tools::builtin::{is_absolute, join, normalize, relative};
use sha2::{Digest, Sha256};
use std::{
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    time::UNIX_EPOCH,
};

pub(super) fn apply(
    target: &SeedReadTarget,
    input: &JsString,
    expected: f64,
) -> Result<bool, Error> {
    if input.is_empty() || input.units().contains(&0) {
        return Err(tool_error(
            "invalid_tool_input",
            "Path must be a non-empty string without null bytes.",
        ));
    }
    let absolute = if is_absolute(input) {
        normalize(input)
    } else {
        join(&target.cwd, input)
    };
    let path = PathBuf::from(text(&absolute));
    let real = resolve(target, input, &absolute, &path)?;
    if blocked(&text(&absolute)) || unsupported_type(&path) {
        return Err(tool_error(
            "invalid_tool_input",
            "seedReadState supports only text files previously read by read_file.",
        ));
    }
    let before = fs::metadata(&path).map_err(|e| io_error(e, "stat", &path))?;
    regular(&before, &path)?;
    if mtime(&before)? != expected {
        return Ok(false);
    }
    // NONBLOCK avoids hanging if a regular path is exchanged for a FIFO.
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW)
        .open(&real)
        .map_err(|e| io_error(e, "open", &path))?;
    let opened = file.metadata().map_err(|e| io_error(e, "fstat", &path))?;
    regular(&opened, &path)?;
    if opened.dev() != before.dev() || opened.ino() != before.ino() || mtime(&opened)? != expected {
        return Ok(false);
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|e| io_error(e, "read", &path))?;
    if bytes.contains(&0) {
        return Err(tool_error(
            "invalid_tool_input",
            format!("{} appears to be a binary file.", path.display()),
        ));
    }
    let after = fs::metadata(&path).map_err(|e| io_error(e, "stat", &path))?;
    regular(&after, &path)?;
    let final_real = path
        .canonicalize()
        .map_err(|e| io_error(e, "realpath", &path))?;
    if after.dev() != opened.dev()
        || after.ino() != opened.ino()
        || mtime(&after)? != expected
        || final_real != real
    {
        return Ok(false);
    }
    let content = String::from_utf8_lossy(&bytes);
    let snapshot = object([
        ("absolutePath", JsValue::String(absolute.clone())),
        ("mtimeMs", JsValue::Number(expected)),
        (
            "contentHash",
            JsValue::String(format!("{:x}", Sha256::digest(content.as_bytes())).into()),
        ),
    ]);
    let mut key = absolute.units().to_vec();
    key.extend("::text::1::all::".encode_utf16());
    // Both locks are acquired before changing either map. They are the same
    // maps read by tools and serialized by HostToolCheckpoint::snapshot.
    let mut reads = target
        .checkpoint
        .read_file_state
        .lock()
        .map_err(|_| plain("Error", "Read state lock poisoned."))?;
    let mut writes = target
        .checkpoint
        .write_snapshots
        .lock()
        .map_err(|_| plain("Error", "Write snapshot lock poisoned."))?;
    reads.insert(
        JsString::from_utf16(key),
        object([
            ("mtimeMs", JsValue::Number(expected)),
            ("kind", "text".into()),
        ]),
    );
    writes.insert(absolute.clone(), snapshot);
    drop(writes);
    drop(reads);
    target.checkpoint.allow_read_files(&[absolute]);
    Ok(true)
}

fn tool_error(code: &str, message: impl Into<JsString>) -> Error {
    named("PilotDeckToolRuntimeError", code, message)
}
fn regular(metadata: &fs::Metadata, path: &Path) -> Result<(), Error> {
    if metadata.is_file() {
        Ok(())
    } else {
        Err(tool_error(
            "file_conflict",
            format!("{} is not a regular file.", path.display()),
        ))
    }
}
fn mtime(metadata: &fs::Metadata) -> Result<f64, Error> {
    let modified = metadata
        .modified()
        .map_err(|e| plain("Error", e.to_string()))?;
    Ok(match modified.duration_since(UNIX_EPOCH) {
        Ok(d) => (d.as_secs_f64() * 1000.0).floor(),
        Err(e) => (-e.duration().as_secs_f64() * 1000.0).floor(),
    })
}
fn within(candidate: &JsString, root: &JsString) -> bool {
    let value = relative(root, candidate);
    !value.units().starts_with(&[b'.' as u16, b'.' as u16]) && !is_absolute(&value)
}
fn canonical(path: &Path, input: &JsString) -> Result<PathBuf, Error> {
    path.canonicalize().map_err(|e| {
        if e.kind() == io::ErrorKind::NotFound {
            tool_error(
                "file_not_found",
                format!("File {} does not exist.", text(input)),
            )
        } else {
            io_error(e, "realpath", path)
        }
    })
}
fn resolve(
    target: &SeedReadTarget,
    input: &JsString,
    absolute: &JsString,
    path: &Path,
) -> Result<PathBuf, Error> {
    let real = canonical(path, input)?;
    if target.bypass_permissions {
        return Ok(real);
    }
    let mut roots = std::iter::once(&target.cwd)
        .chain(target.additional_working_directories.iter())
        .map(normalize);
    if let Some(root) = roots.find(|root| within(absolute, root)) {
        let root_path = PathBuf::from(text(&root));
        let real_root = root_path
            .canonicalize()
            .map_err(|e| io_error(e, "realpath", &root_path))?;
        if !within(
            &real.to_string_lossy().as_ref().into(),
            &real_root.to_string_lossy().as_ref().into(),
        ) {
            return Err(tool_error(
                "path_not_allowed",
                format!(
                    "Path {} resolves outside the PilotDeck workspace.",
                    text(input)
                ),
            ));
        }
        return Ok(real);
    }
    let state = target
        .checkpoint
        .snapshot()
        .map_err(|e| plain("Error", e.to_string()))?;
    for allowed in state.allowed_read_files.unwrap_or_default() {
        let path = PathBuf::from(text(&allowed));
        match path.canonicalize() {
            Ok(allowed_real) if allowed_real == real => return Ok(real),
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(io_error(e, "realpath", &path)),
        }
    }
    Err(tool_error(
        "path_not_allowed",
        format!("Path {} is outside the PilotDeck workspace.", text(input)),
    ))
}
fn unsupported_type(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "png"
            | "jpg"
            | "jpeg"
            | "gif"
            | "webp"
            | "pdf"
            | "ipynb"
            | "exe"
            | "dll"
            | "so"
            | "dylib"
            | "class"
            | "jar"
            | "bin"
            | "dat"
            | "zip"
            | "gz"
            | "tar"
            | "7z"
            | "rar"
            | "doc"
            | "docx"
            | "ppt"
            | "pptx"
            | "xls"
            | "xlsx"
            | "odt"
            | "ods"
            | "odp"
            | "pages"
            | "key"
            | "numbers"
            | "rp"
            | "mp3"
            | "mp4"
            | "mov"
            | "avi"
            | "wav"
            | "ogg"
            | "woff"
            | "woff2"
            | "ttf"
            | "otf"
            | "xlsm"
            | "xlt"
            | "xltx"
            | "xltm"
            | "docm"
            | "dot"
            | "dotx"
            | "dotm"
            | "pptm"
            | "pot"
            | "potx"
            | "potm"
            | "pps"
            | "ppsx"
            | "ppsm"
    )
}
fn blocked(path: &str) -> bool {
    let device = |name: &str| {
        matches!(name, "CON" | "PRN" | "AUX" | "NUL")
            || ["COM", "LPT"].iter().any(|p| {
                name.strip_prefix(p).is_some_and(|n| {
                    matches!(n, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
                })
            })
    };
    if matches!(
        path,
        "/dev/zero"
            | "/dev/random"
            | "/dev/urandom"
            | "/dev/full"
            | "/dev/stdin"
            | "/dev/tty"
            | "/dev/console"
            | "/dev/stdout"
            | "/dev/stderr"
            | "/dev/fd/0"
            | "/dev/fd/1"
            | "/dev/fd/2"
    ) || path.starts_with("/proc/")
        && ["/fd/0", "/fd/1", "/fd/2"]
            .iter()
            .any(|s| path.ends_with(s))
    {
        return true;
    }
    if let Some(rest) = path
        .strip_prefix("\\\\.\\")
        .or_else(|| path.strip_prefix("\\\\?\\"))
    {
        let name = rest
            .split(['\\', '/'])
            .next()
            .unwrap_or("")
            .to_ascii_uppercase();
        if device(&name)
            || name
                .strip_prefix("PHYSICALDRIVE")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            || name.len() == 2 && name.as_bytes()[0].is_ascii_uppercase() && name.ends_with(':')
        {
            return true;
        }
    }
    let basename = path.rsplit('/').next().unwrap_or("");
    let name = basename
        .rsplit_once('.')
        .map_or(basename, |(name, _)| name)
        .to_ascii_uppercase();
    device(&name)
}
