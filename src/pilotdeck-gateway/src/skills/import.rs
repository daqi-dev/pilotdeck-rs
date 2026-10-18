//! SkillManager.ts:328–478 import and scan, plus the slice of Node 22's fs.cp
//! (lib/internal/fs/cp/cp.js) that import's `{ recursive: true, force: true,
//! dereference: false, errorOnExist: false }` call can reach.
use super::{
    Result, SkillError, SkillManager, assert_mutable, basename, field, invalid_input, invalid_slug,
    is_str, join, mkdir_p, object, read_skill_meta, remove_tree, resolve, trim_units, truthy,
    valid_slug, validate::validate_from_disk,
};
use crate::{js_value::display, project_files::node_collator};
use pilotdeck_protocol::{JsString, JsValue};
use std::{
    fs::{self, Metadata},
    io,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, symlink},
};

impl SkillManager {
    /// SkillManager.ts:328–427.
    pub fn import(&self, input: &JsValue) -> Result<JsValue> {
        let scope = field(input, "scope")?.clone();
        assert_mutable(&scope)?;
        let source = required_path(input, "sourcePath")?;
        let symlink_mode = is_str(field(input, "mode")?, "symlink");
        let source = resolve(&self.expand_home(&source))?;
        match fs::metadata(&source) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(SkillError::manager(
                    "source_missing",
                    format!("Source path does not exist: {source}"),
                ));
            }
            Err(e) => return Err(SkillError::io(&e, "stat", Some(&source))),
            Ok(m) if !m.is_dir() => {
                return Err(SkillError::manager(
                    "source_not_directory",
                    format!("Source path is not a directory: {source}"),
                ));
            }
            Ok(_) => {}
        }
        // fs.access(F_OK) follows symlinks, as stat does; any failure counts.
        if fs::metadata(join(&source, "SKILL.md")).is_err() {
            return Err(SkillError::manager(
                "no_skill_md",
                format!("Source folder does not contain a SKILL.md at the root: {source}"),
            ));
        }
        // `(input.slug && input.slug.trim()) || basename(resolvedSource)`
        let slug: JsString = match field(input, "slug")? {
            v if !truthy(v) => basename(&source).into(),
            JsValue::String(s) => match trim_units(s.units()) {
                [] => basename(&source).into(),
                trimmed => JsString::from_utf16(trimmed.to_vec()),
            },
            _ => {
                return Err(SkillError::type_error(
                    "",
                    "input.slug.trim is not a function",
                ));
            }
        };
        let slug = JsValue::String(slug);
        let Some(name) = valid_slug(&slug) else {
            return Err(invalid_slug(&display(&slug)?));
        };
        let root = self.resolve_scope_root(&scope, field(input, "projectKey")?)?;
        // `resolve(targetDir)`: the root is resolved and the slug one plain segment.
        let target = join(&root, &name);
        if target == source {
            return Err(SkillError::manager(
                "self_import",
                "Source and target resolve to the same path; pick a different slug or scope.",
            ));
        }
        let exists = fs::metadata(&target).is_ok();
        if exists && !truthy(field(input, "force")?) {
            return Err(SkillError::manager(
                "conflict",
                format!("Skill already exists at {target}. Re-run with force=true to overwrite."),
            ));
        }
        let validation = validate_from_disk(&source)?;
        if validation.get("ok") != Some(&JsValue::Bool(true)) {
            return Err(SkillError::validation(JsValue::Object(validation)));
        }
        if exists {
            remove_tree(&target)?;
        }
        mkdir_p(&root)?;
        if symlink_mode {
            symlink(&source, &target)
                .map_err(|e| SkillError::io_pair(&e, "symlink", &source, &target))?;
        } else {
            cp(&source, &target)?;
        }
        let skill = read_skill_meta(&target, &scope);
        Ok(object([
            ("ok", JsValue::Bool(true)),
            ("mode", if symlink_mode { "symlink" } else { "copy" }.into()),
            ("scope", scope),
            ("slug", slug),
            ("sourcePath", source.as_str().into()),
            ("skillPath", target.as_str().into()),
            ("skill", skill),
            ("validation", JsValue::Object(validation)),
        ]))
    }

    /// SkillManager.ts:429–478.
    pub fn scan(&self, input: &JsValue) -> Result<JsValue> {
        let parent = required_path(input, "parentPath")?;
        let root = resolve(&self.expand_home(&parent))?;
        let not_found = || SkillError::manager("not_found", format!("Directory not found: {root}"));
        match fs::metadata(&root) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(not_found()),
            Err(e) => return Err(SkillError::io(&e, "stat", Some(&root))),
            Ok(m) if !m.is_dir() => {
                return Err(SkillError::manager(
                    "not_directory",
                    format!("Path is not a directory: {root}"),
                ));
            }
            Ok(_) => {}
        }
        let mut entries = match fs::read_dir(&root).and_then(|r| r.collect::<io::Result<Vec<_>>>())
        {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(not_found()),
            Err(e) => return Err(SkillError::io(&e, "scandir", Some(&root))),
        };
        // fs.promises.readdir is uv_fs_scandir, whose uv__fs_scandir_sort is
        // strcmp: the same byte order as OsString, so the stable sort below
        // breaks ties exactly as TS does (CLAUDE.md 问题分级, 2026-10-14).
        // This holds only for scandir; fs.cp walks with opendir, which is
        // unsorted (see copy_entry).
        entries.sort_by_key(|e| e.file_name());
        let current = scan_folder(&root, basename(&root));
        let collator = node_collator(&self.locale)
            .map_err(|(code, message)| SkillError::host_failure(code, message))?;
        let mut children = vec![];
        for entry in entries {
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = join(&root, &name);
            let is_dir = match entry.file_type() {
                Ok(kind) if kind.is_dir() => true,
                Ok(kind) if kind.is_symlink() => fs::metadata(&path).is_ok_and(|m| m.is_dir()),
                _ => false,
            };
            if is_dir {
                children.push(scan_folder(&path, &name));
            }
        }
        // Folders with a SKILL.md first, then localeCompare (Array#sort is stable).
        children.sort_by(|a, b| {
            b.has_skill_md
                .cmp(&a.has_skill_md)
                .then_with(|| collator.compare_utf16(a.name.units(), b.name.units()))
        });
        let folders = current
            .has_skill_md
            .then_some(current)
            .into_iter()
            .chain(children)
            .map(|f| f.value)
            .collect();
        Ok(object([
            ("parentPath", root.as_str().into()),
            ("folders", JsValue::Array(folders)),
        ]))
    }
}

/// `typeof input.key !== "string" || !input.key.trim()` → invalid_input.
fn required_path(input: &JsValue, key: &str) -> Result<String> {
    match field(input, key)? {
        JsValue::String(s) if !trim_units(s.units()).is_empty() => {
            Ok(String::from_utf16_lossy(trim_units(s.units())))
        }
        _ => Err(invalid_input(format!("{key} is required."))),
    }
}

struct ScanFolder {
    has_skill_md: bool,
    name: JsString,
    value: JsValue,
}

/// SkillManager.ts:663 buildSkillScanFolder.
fn scan_folder(dir: &str, folder_name: &str) -> ScanFolder {
    let has_skill_md = fs::metadata(join(dir, "SKILL.md")).is_ok();
    let meta = if has_skill_md {
        read_skill_meta(dir, &"user".into())
    } else {
        JsValue::Null
    };
    let (file_count, total_size) = if has_skill_md {
        tree_files(dir)
    } else {
        (0.0, 0.0)
    };
    let pick = |key: &str| meta.get(key).cloned().unwrap_or(JsValue::Null);
    ScanFolder {
        has_skill_md,
        name: folder_name.into(),
        value: object([
            ("folderName", folder_name.into()),
            ("hasSkillMd", JsValue::Bool(has_skill_md)),
            ("name", pick("name")),
            ("description", pick("description")),
            ("sourcePath", dir.into()),
            ("fileCount", JsValue::Number(file_count)),
            ("totalSize", JsValue::Number(total_size)),
        ]),
    }
}

/// `fs.readdir(dir, { recursive: true })` (promises readdirRecursive), then a
/// following stat of every listed path. The recursive readdir descends every
/// entry its internalModuleStat sees as a directory, symlinks included, so a
/// link cycle unrolls until stat hits ELOOP; any listing failure rejects the
/// whole readdir, which scan swallows as zero files. Measured with a real
/// unreadable child in tests/record_runtime_boundaries.mts.
fn tree_files(dir: &str) -> (f64, f64) {
    let mut listed = vec![];
    let mut queue = vec![dir.to_owned()];
    while let Some(next) = queue.pop() {
        let Ok(entries) = fs::read_dir(&next).and_then(|r| r.collect::<io::Result<Vec<_>>>())
        else {
            return (0.0, 0.0);
        };
        for entry in entries {
            let path = join(&next, &entry.file_name().to_string_lossy());
            if fs::metadata(&path).is_ok_and(|m| m.is_dir()) {
                queue.push(path.clone());
            }
            listed.push(path);
        }
    }
    listed
        .iter()
        .filter_map(|p| fs::metadata(p).ok())
        .filter(Metadata::is_file)
        .fold((0.0, 0.0), |(count, size), m| {
            (count + 1.0, size + m.len() as f64)
        })
}

// ------------------------------------------------------------------- fs.cp

/// cp.js cpFn → checkPaths → checkParentPaths → checkParentDir. The parent
/// already exists (import created the scope root), so checkParentDir goes
/// straight to getStatsForCopy.
fn cp(src: &str, dest: &str) -> Result<()> {
    let (src_stat, dest_stat) = check_paths(src, dest)?;
    check_parent_paths(src, &src_stat, dest)?;
    copy_entry(src_stat, dest_stat, src, dest)
}

/// cp.js checkPaths with lstat on both sides (dereference: false).
fn check_paths(src: &str, dest: &str) -> Result<(Metadata, Option<Metadata>)> {
    let src_stat = fs::symlink_metadata(src).map_err(|e| SkillError::io(&e, "lstat", Some(src)))?;
    let dest_stat = match fs::symlink_metadata(dest) {
        Ok(m) => Some(m),
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => return Err(SkillError::io(&e, "lstat", Some(dest))),
    };
    if let Some(dest_stat) = &dest_stat {
        if identical(&src_stat, dest_stat) {
            return Err(einval("src and dest cannot be the same", dest));
        }
        if src_stat.is_dir() && !dest_stat.is_dir() {
            return Err(SkillError::cp(
                "ERR_FS_CP_DIR_TO_NON_DIR",
                "Cannot overwrite non-directory with directory",
                "EISDIR",
                &format!("cannot overwrite non-directory {dest} with directory {src}"),
                dest,
            ));
        }
        if !src_stat.is_dir() && dest_stat.is_dir() {
            return Err(SkillError::cp(
                "ERR_FS_CP_NON_DIR_TO_DIR",
                "Cannot overwrite directory with non-directory",
                "ENOTDIR",
                &format!("cannot overwrite directory {dest} with non-directory {src}"),
                dest,
            ));
        }
    }
    if src_stat.is_dir() && is_src_subdir(src, dest) {
        return Err(subdir_of_self(src, dest));
    }
    Ok((src_stat, dest_stat))
}

/// cp.js checkParentPaths: no ancestor of dest may be src itself.
fn check_parent_paths(src: &str, src_stat: &Metadata, dest: &str) -> Result<()> {
    let src_parent = dirname(src);
    let mut dest = dest.to_owned();
    loop {
        let dest_parent = dirname(&dest);
        if dest_parent == src_parent || dest_parent == "/" {
            return Ok(());
        }
        match fs::metadata(&dest_parent) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(SkillError::io(&e, "stat", Some(&dest_parent))),
            Ok(m) if identical(src_stat, &m) => return Err(subdir_of_self(src, &dest)),
            Ok(_) => dest = dest_parent,
        }
    }
}

/// Copy entries depth first without putting directory depth on the native
/// stack. Visit children in opendir order and chmod a new directory only after
/// all its children succeed, just like cp.js mkDirAndCopy.
/// Node 22 opendir ordering was measured in A6.8-pr4 gate 2:
/// `_tmp/20261018-a68-pr4/cp-order-probe.log` (sorting changes the first error).
fn copy_entry(
    src_stat: Metadata,
    dest_stat: Option<Metadata>,
    src: &str,
    dest: &str,
) -> Result<()> {
    enum Work {
        Entry(String, String),
        Mode(String, Metadata),
    }
    let mut pending = vec![Work::Entry(src.to_owned(), dest.to_owned())];
    let mut root_stats = Some((src_stat, dest_stat));
    while let Some(work) = pending.pop() {
        let (src, dest) = match work {
            Work::Mode(dest, stat) => {
                set_mode(&dest, &stat)?;
                continue;
            }
            Work::Entry(src, dest) => (src, dest),
        };
        let (src_stat, dest_stat) = match root_stats.take() {
            Some(stats) => stats,
            None => check_paths(&src, &dest)?,
        };
        if !src_stat.is_dir() {
            copy_non_directory(&src_stat, dest_stat.as_ref(), &src, &dest)?;
            continue;
        }
        if dest_stat.is_none() {
            fs::create_dir(&dest).map_err(|e| SkillError::io(&e, "mkdir", Some(&dest)))?;
            pending.push(Work::Mode(dest.clone(), src_stat));
        }
        let entries = fs::read_dir(&src)
            .and_then(|r| r.collect::<io::Result<Vec<_>>>())
            .map_err(|e| SkillError::io(&e, "opendir", Some(&src)))?;
        // LIFO scheduling preserves the native directory order. Delay each
        // child's checkPaths until its turn, so a later error cannot mask an
        // earlier child's copy failure or suppress its already-written files.
        for entry in entries.into_iter().rev() {
            let name = entry.file_name().to_string_lossy().into_owned();
            pending.push(Work::Entry(join(&src, &name), join(&dest, &name)));
        }
    }
    Ok(())
}

fn copy_non_directory(
    src_stat: &Metadata,
    dest_stat: Option<&Metadata>,
    src: &str,
    dest: &str,
) -> Result<()> {
    let kind = src_stat.file_type();
    if kind.is_file() || kind.is_char_device() || kind.is_block_device() {
        // onFile; force: true unlinks an existing dest first.
        if dest_stat.is_some() {
            fs::remove_file(dest).map_err(|e| SkillError::io(&e, "unlink", Some(dest)))?;
        }
        fs::copy(src, dest).map_err(|e| SkillError::io_pair(&e, "copyfile", src, dest))?;
        return set_mode(dest, src_stat);
    }
    if kind.is_symlink() {
        return copy_link(dest_stat.is_some(), src, dest);
    }
    let (code, prefix, what) = if kind.is_socket() {
        (
            "ERR_FS_CP_SOCKET",
            "Cannot copy a socket file",
            "a socket file",
        )
    } else if kind.is_fifo() {
        (
            "ERR_FS_CP_FIFO_PIPE",
            "Cannot copy a FIFO pipe",
            "a FIFO pipe",
        )
    } else {
        (
            "ERR_FS_CP_UNKNOWN",
            "Cannot copy an unknown file type",
            "an unknown file type",
        )
    };
    Err(SkillError::cp(
        code,
        prefix,
        "EINVAL",
        &format!("cannot copy {what}: {dest}"),
        dest,
    ))
}

/// cp.js onLink (verbatimSymlinks: false): a relative link target is made
/// absolute against the link's own directory. A dest that is not a link
/// (readlink EINVAL) gets no unlink: onLink calls symlink straight away and
/// lets it fail with EEXIST.
fn copy_link(dest_exists: bool, src: &str, dest: &str) -> Result<()> {
    let resolved_src = absolute_link(src)?;
    if dest_exists {
        let resolved_dest = match fs::read_link(dest) {
            Ok(_) => absolute_link(dest)?,
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => {
                return link(&resolved_src, dest);
            }
            Err(e) => return Err(SkillError::io(&e, "readlink", Some(dest))),
        };
        if is_src_subdir(&resolved_src, &resolved_dest) {
            return Err(einval(
                &format!("cannot copy {resolved_src} to a subdirectory of self {resolved_dest}"),
                dest,
            ));
        }
        let followed = fs::metadata(src).map_err(|e| SkillError::io(&e, "stat", Some(src)))?;
        if followed.is_dir() && is_src_subdir(&resolved_dest, &resolved_src) {
            return Err(SkillError::cp(
                "ERR_FS_CP_SYMLINK_TO_SUBDIRECTORY",
                "Cannot overwrite symlink in subdirectory of self",
                "EINVAL",
                &format!("cannot overwrite {resolved_dest} with {resolved_src}"),
                dest,
            ));
        }
        // onLinkCopy
        fs::remove_file(dest).map_err(|e| SkillError::io(&e, "unlink", Some(dest)))?;
    }
    link(&resolved_src, dest)
}
fn link(target: &str, path: &str) -> Result<()> {
    symlink(target, path).map_err(|e| SkillError::io_pair(&e, "symlink", target, path))
}
fn absolute_link(path: &str) -> Result<String> {
    let target = fs::read_link(path).map_err(|e| SkillError::io(&e, "readlink", Some(path)))?;
    let target = target.to_string_lossy();
    if target.starts_with('/') {
        Ok(target.into_owned())
    } else {
        resolve(&format!("{}/{target}", dirname(path)))
    }
}

/// cp.js setDestMode: chmod to the source's mode.
fn set_mode(dest: &str, src_stat: &Metadata) -> Result<()> {
    fs::set_permissions(dest, fs::Permissions::from_mode(src_stat.mode()))
        .map_err(|e| SkillError::io(&e, "chmod", Some(dest)))
}

/// cp.js areIdentical: same ino and dev.
fn identical(a: &Metadata, b: &Metadata) -> bool {
    a.ino() == b.ino() && a.dev() == b.dev()
}
/// cp.js isSrcSubdir: every segment of src prefixes dest.
fn is_src_subdir(src: &str, dest: &str) -> bool {
    let mut dest = dest.split('/').filter(|s| !s.is_empty());
    src.split('/')
        .filter(|s| !s.is_empty())
        .all(|s| dest.next() == Some(s))
}
fn subdir_of_self(src: &str, dest: &str) -> SkillError {
    einval(
        &format!("cannot copy {src} to a subdirectory of self {dest}"),
        dest,
    )
}
fn einval(detail: &str, dest: &str) -> SkillError {
    SkillError::cp(
        "ERR_FS_CP_EINVAL",
        "Invalid src or dest",
        "EINVAL",
        detail,
        dest,
    )
}
/// path.dirname of a resolved path.
fn dirname(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => "/".into(),
        Some(i) => path[..i].into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_copy_does_not_use_native_stack_per_level() {
        // The public import replay checks TS behavior on a normal thread. This
        // deliberately small stack catches reintroducing copy_dir recursion.
        let root = std::env::temp_dir().join(format!("pdrs-copy-depth-{}", std::process::id()));
        fs::create_dir(&root).unwrap();
        struct Cleanup(std::path::PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                fs::remove_dir_all(&self.0).unwrap();
            }
        }
        let _cleanup = Cleanup(root.clone());
        let relative = vec!["d"; 240].join("/");
        let source = root.join("src");
        let dest = root.join("dst");
        fs::create_dir_all(source.join(&relative)).unwrap();
        fs::write(source.join(&relative).join("leaf"), b"leaf").unwrap();
        std::thread::Builder::new()
            .stack_size(128 * 1024)
            .spawn(move || cp(source.to_str().unwrap(), dest.to_str().unwrap()))
            .unwrap()
            .join()
            .unwrap()
            .unwrap();
        assert_eq!(
            fs::read(root.join("dst").join(relative).join("leaf")).unwrap(),
            b"leaf"
        );
    }
}
