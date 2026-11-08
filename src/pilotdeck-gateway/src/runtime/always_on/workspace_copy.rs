//! Descriptor-only snapshot copy and workspace directory admission.
use std::{fs::File, io::{self, Read, Write}, os::unix::{ffi::OsStrExt, fs::MetadataExt}, path::{Component, Path, PathBuf}, sync::Arc};
use pilotdeck_protocol::JsObject;
use rustix::fs::{AtFlags, Dir, FileType, Mode, OFlags, fchmod, mkdirat, openat, readlinkat, statat, symlinkat};
use super::{AlwaysOnPaths, storage};
const IGNORES: &[&str] = &[".git", "node_modules", "dist", ".pilotdeck", ".pilotdeck-always-on"];
#[derive(Clone, Copy)]
pub(super) struct CopyLimits { pub bytes: u64, pub entries: usize, pub depth: usize, pub path_bytes: usize }
impl Default for CopyLimits { fn default() -> Self { Self { bytes: 1024 * 1024 * 1024, entries: 100_000, depth: 128, path_bytes: 16 * 1024 * 1024 } } }
pub(super) fn pin_project(project: &Path) -> io::Result<Arc<File>> {
    let canonical = project.canonicalize()?;
    Ok(Arc::new(crate::project_files::open_directory_beneath(&canonical, &canonical)?))
}
fn directory(parent: &File, name: &std::ffi::OsStr) -> io::Result<File> {
    Ok(File::from(openat(parent, name, OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())?))
}
fn walk(root: &File, path: &Path) -> io::Result<File> {
    let mut current = root.try_clone()?;
    for part in path.components() {
        let Component::Normal(name) = part else { return Err(io::ErrorKind::PermissionDenied.into()); };
        current = directory(&current, name)?;
    }
    Ok(current)
}
fn base(paths: &AlwaysOnPaths, strategy: &str) -> io::Result<PathBuf> {
    match strategy { "git-worktree" => Ok(paths.worktrees_dir()), "snapshot-copy" => Ok(paths.snapshots_dir()), _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "Unknown Always-On workspace strategy.")) }
}
pub(super) fn new_target(paths: &AlwaysOnPaths, strategy: &str, run: &str) -> io::Result<(PathBuf, Arc<File>)> {
    let base = base(paths, strategy)?;
    let parent = storage::directory(&storage::root_for(&paths.state_file), &base, true)?;
    // Exclusivity makes a failed copy's cleanup own precisely this new tree.
    // Never merge into or recursively remove a pre-existing user directory.
    mkdirat(&parent, run, Mode::from_raw_mode(0o700))?;
    let target = directory(&parent, std::ffi::OsStr::new(run))?;
    Ok((base.join(run), Arc::new(target)))
}
pub(super) fn owned_workspace(paths: &AlwaysOnPaths, strategy: &str, cwd: &Path) -> io::Result<Arc<File>> {
    let base = base(paths, strategy)?;
    let canonical_base = base.canonicalize()?;
    let canonical = cwd.canonicalize()?;
    let suffix = canonical.strip_prefix(&canonical_base).map_err(|_| io::Error::new(io::ErrorKind::PermissionDenied, "Workspace is outside this project's selected workspace base."))?;
    if suffix.components().count() != 1 || !matches!(suffix.components().next(), Some(Component::Normal(_))) {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Workspace is not a direct run directory of this project."));
    }
    let file = storage::directory(&storage::root_for(&paths.state_file), &base.join(suffix), false)?;
    let actual = file.metadata()?; let expected = std::fs::metadata(&canonical)?;
    if actual.dev() != expected.dev() || actual.ino() != expected.ino() {
        return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Workspace directory changed during admission."));
    }
    Ok(Arc::new(file))
}
fn names(directory: &File, used: &mut usize, cap: usize) -> io::Result<Vec<std::ffi::OsString>> {
    let readable = self::directory(directory, std::ffi::OsStr::new("."))?;
    let mut names = Vec::new();
    for entry in Dir::read_from(&readable)? {
        let entry = entry?; let bytes = entry.file_name().to_bytes();
        if bytes == b"." || bytes == b".." { continue; }
        *used += 1;
        if *used > cap { return Err(storage::budget("snapshot entries")); }
        names.push(std::ffi::OsStr::from_bytes(bytes).to_owned());
    }
    names.sort(); Ok(names)
}
fn walk_files(source: &File, target: Option<&File>, limits: CopyLimits) -> io::Result<u64> {
    let mut pending = vec![(PathBuf::new(), 0usize)];
    let mut pending_bytes = 0usize; let mut entries = 0usize; let mut bytes = 0u64;
    while let Some((path, depth)) = pending.pop() {
        pending_bytes = pending_bytes.saturating_sub(path.as_os_str().len());
        let src = walk(source, &path)?;
        let dst = target.map(|target| walk(target, &path)).transpose()?;
        for name in names(&src, &mut entries, limits.entries)? {
            let ignored = depth == 0 && IGNORES.iter().any(|ignore| name == std::ffi::OsStr::new(ignore));
            // The size pass includes ignored data like TS du; copy excludes
            // only a root-level ignore head, not arbitrary nested names.
            if target.is_some() && ignored { continue; }
            let relative = path.join(&name);
            let stat = statat(&src, &name, AtFlags::SYMLINK_NOFOLLOW)?;
            match FileType::from_raw_mode(stat.st_mode) {
                FileType::Directory => {
                    if depth + 1 > limits.depth { return Err(storage::budget("snapshot depth")); }
                    if let Some(dst) = &dst { mkdirat(dst, &name, Mode::from_raw_mode(0o700))?; }
                    pending_bytes = pending_bytes.checked_add(relative.as_os_str().len()).ok_or_else(|| storage::budget("snapshot paths"))?;
                    if pending_bytes > limits.path_bytes { return Err(storage::budget("snapshot paths")); }
                    pending.push((relative, depth + 1));
                }
                FileType::RegularFile => {
                    let mut file = File::from(openat(&src, &name, OFlags::RDONLY | OFlags::NONBLOCK | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::empty())?);
                    let metadata = file.metadata()?;
                    if !metadata.is_file() { return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Snapshot source changed type during copy.")); }
                    if bytes.checked_add(metadata.len()).is_none_or(|total| total > limits.bytes) { return Err(storage::budget("snapshot source bytes")); }
                    if let Some(dst) = &dst {
                        let mut output = File::from(openat(dst, &name, OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC, Mode::from_raw_mode(0o600))?);
                        let mut buffer = [0u8; 64 * 1024]; let mut copied = 0u64;
                        loop {
                            let length = file.read(&mut buffer)?; if length == 0 { break; }
                            copied = copied.checked_add(length as u64).ok_or_else(|| storage::budget("snapshot source bytes"))?;
                            if bytes.checked_add(copied).is_none_or(|total| total > limits.bytes) { return Err(storage::budget("snapshot source bytes")); }
                            output.write_all(&buffer[..length])?;
                        }
                        #[allow(clippy::unnecessary_cast)] // Darwin mode_t is u16; Linux RawMode is u32.
                        let copied_mode = Mode::from_raw_mode((metadata.mode() & 0o777) as rustix::fs::RawMode);
                        fchmod(&output, copied_mode)?;
                        output.sync_all()?; bytes += copied;
                    } else { bytes += metadata.len(); }
                }
                FileType::Symlink => {
                    // Copy the link itself. No traversal outside the selected
                    // project and no external data is read by this operation.
                    let link = readlinkat(&src, &name, Vec::new())?;
                    bytes = bytes.checked_add(link.as_bytes().len() as u64).ok_or_else(|| storage::budget("snapshot source bytes"))?;
                    if bytes > limits.bytes { return Err(storage::budget("snapshot source bytes")); }
                    if let Some(dst) = &dst { symlinkat(link.as_c_str(), dst, &name)?; }
                }
                _ => return Err(io::Error::new(io::ErrorKind::PermissionDenied, "Snapshot source contains a non-regular special entry.")),
            }
        }
    }
    Ok(bytes)
}
#[allow(clippy::unnecessary_cast)] // stat dev/inode differ between Darwin and Linux.
pub(super) fn snapshot(paths: &AlwaysOnPaths, project: &Arc<File>, run: &str, limits: CopyLimits) -> io::Result<(PathBuf, JsObject, Arc<File>)> {
    let size = walk_files(project, None, limits)?;
    let base = paths.snapshots_dir();
    let source = Path::new(&paths.project_key).canonicalize()?;
    // Prevent a configured home beneath the source from making its own copy
    // recursively visible. Node fs.cp rejects a source-containing target too.
    let home = storage::root_for(&paths.state_file);
    let comparable = if let Ok(canonical_home) = home.canonicalize() {
        base.strip_prefix(&home).map(|suffix| canonical_home.join(suffix)).unwrap_or_else(|_| base.clone())
    } else { base.clone() };
    if comparable.starts_with(&source) { return Err(io::Error::new(io::ErrorKind::InvalidInput, "Snapshot target is inside its source project.")); }
    let (target, destination) = new_target(paths, "snapshot-copy", run)?;
    if let Err(error) = walk_files(project, Some(&destination), limits) {
        let cleanup = remove_created(paths,"snapshot-copy",&target,&destination);
        return match cleanup { Ok(()) => Err(error), Err(cleanup) => Err(io::Error::other(format!("{error}; owned snapshot cleanup failed: {cleanup}"))) };
    }
    let mut metadata = JsObject::new(); metadata.insert("copyStrategy".into(), "fs.cp".into()); metadata.insert("baseSize".into(), size.to_string().as_str().into());
    Ok((target, metadata, destination))
}
/// Rename captures one directory entry atomically before checking which object
/// moved. An inode check followed by pathname unlink is NOT an atomic CAS.
pub(super) struct CapturedWorkspace {
    pub path:PathBuf,
    pub directory:Arc<File>,
    parent:File,
    container:File,
    container_name:std::ffi::OsString,
}
fn same_directory(expected:&File,actual:&File)->io::Result<()> {
    let expected=expected.metadata()?;let actual=actual.metadata()?;
    if expected.dev()!=actual.dev()||expected.ino()!=actual.ino(){return Err(io::Error::new(io::ErrorKind::PermissionDenied,"Created workspace was replaced before cleanup."));}Ok(())
}
pub(super) fn capture_selected(paths:&AlwaysOnPaths,strategy:&str,cwd:&Path,expected:&File)->io::Result<Arc<CapturedWorkspace>>{
    capture_with(paths,strategy,cwd,expected,||Ok(()))
}
fn capture_with(paths:&AlwaysOnPaths,strategy:&str,cwd:&Path,expected:&File,before_move:impl FnOnce()->io::Result<()>)->io::Result<Arc<CapturedWorkspace>>{
    let current=owned_workspace(paths,strategy,cwd)?;same_directory(expected,&current)?;
    let base=base(paths,strategy)?;let original=cwd.canonicalize()?;let canonical_base=base.canonicalize()?;
    let leaf=original.strip_prefix(&canonical_base).map_err(|_|io::ErrorKind::PermissionDenied)?;
    if leaf.components().count()!=1{return Err(io::ErrorKind::PermissionDenied.into());}
    let parent=storage::directory(&storage::root_for(&paths.state_file),&base,false)?;
    let name=std::ffi::OsString::from(format!(".cleanup-{}",uuid::Uuid::new_v4()));
    mkdirat(&parent,&name,Mode::from_raw_mode(0o700))?;let container=directory(&parent,&name)?;
    before_move()?;
    if let Err(error)=rustix::fs::renameat_with(&parent,leaf,&container,"workspace",rustix::fs::RenameFlags::NOREPLACE){let _=rustix::fs::unlinkat(&parent,&name,AtFlags::REMOVEDIR);return Err(error.into());}
    let captured=directory(&container,std::ffi::OsStr::new("workspace")).map_err(|error|io::Error::other(format!("{error}; captured workspace preserved at {}",base.join(&name).display())))?;
    if let Err(error)=same_directory(expected,&captured){
        // Restore without overwriting any object which appeared at the old
        // path. If that cannot succeed, preserve everything and name the
        // private recovery location in the error.
        let restored=rustix::fs::renameat_with(&container,"workspace",&parent,leaf,rustix::fs::RenameFlags::NOREPLACE);
        if restored.is_ok(){let _=rustix::fs::unlinkat(&parent,&name,AtFlags::REMOVEDIR);}
        return Err(io::Error::other(format!("{error}; cleanup capture was not the selected object; restore={restored:?}; preserved at {}",base.join(&name).display())));
    }
    let path=base.join(&name).join("workspace");
    Ok(Arc::new(CapturedWorkspace{path,directory:Arc::new(captured),parent,container,container_name:name}))
}
impl CapturedWorkspace {
    fn finish_container(&self)->io::Result<()> {
        let named=directory(&self.parent,&self.container_name)?;same_directory(&self.container,&named)?;
        // REMOVEDIR only removes an empty container. Never recursively reopen
        // its path: an externally replaced nonempty container is preserved.
        rustix::fs::unlinkat(&self.parent,&self.container_name,AtFlags::REMOVEDIR)?;Ok(())
    }
    pub fn remove(&self)->io::Result<()> {
        let current=directory(&self.container,std::ffi::OsStr::new("workspace"))?;same_directory(&self.directory,&current)?;
        let mut used=0;
        for name in names(&self.directory,&mut used,CopyLimits::default().entries)? {
            // All recursive traversal starts from the original captured fd,
            // never from a newly admitted directory at the original run path.
            crate::project_files::remove_tree_at(&self.directory,Path::new(&name),&self.path.join(&name))?;
        }
        let current=directory(&self.container,std::ffi::OsStr::new("workspace"))?;same_directory(&self.directory,&current)?;
        rustix::fs::unlinkat(&self.container,"workspace",AtFlags::REMOVEDIR)?;self.finish_container()
    }
    pub fn finish_git_remove(&self)->io::Result<()> {
        if !directory_gone(&self.directory,&self.path)?{return Err(io::Error::other(format!("Could not verify Git cleanup of selected capture; preserving {}",self.path.display())));}
        self.finish_container()
    }
}
// APFS keeps st_nlink and the last F_GETPATH string after rmdir. F_GETPATH
// changes on rename, so reject a moved held object; only accept the removed
// captured entry after successful owned git cleanup. This is an observed
// postcondition, not an atomic unlink-by-inode guarantee.
#[cfg(target_vendor="apple")]
fn directory_gone(expected:&File,path:&Path)->io::Result<bool>{
    let held=rustix::fs::getpath(expected)?;let parent=path.parent().ok_or(io::ErrorKind::InvalidInput)?.canonicalize()?;let path=parent.join(path.file_name().ok_or(io::ErrorKind::InvalidInput)?);
    if Path::new(std::ffi::OsStr::from_bytes(held.to_bytes()))!=path{return Ok(false);}
    match std::fs::symlink_metadata(path){Err(error) if error.kind()==io::ErrorKind::NotFound=>Ok(true),Ok(_)=>Ok(false),Err(error)=>Err(error)}
}
#[cfg(target_os="linux")]
fn directory_gone(expected:&File,_path:&Path)->io::Result<bool>{Ok(expected.metadata()?.nlink()==0)}
#[cfg(not(any(target_vendor="apple",target_os="linux")))]
fn directory_gone(_expected:&File,_path:&Path)->io::Result<bool>{Ok(false)}
/// A missing pathname only proves removal when the original held object is
/// unlinked too; a renamed original remains an explicit uncertain cleanup.
pub(super) fn remove_created(paths:&AlwaysOnPaths,strategy:&str,cwd:&Path,expected:&File)->io::Result<()> {
    match capture_selected(paths,strategy,cwd,expected){Ok(captured)=>captured.remove(),Err(error) if error.kind()==io::ErrorKind::NotFound&&directory_gone(expected,cwd)?=>Ok(()),Err(error)=>Err(error)}
}

#[cfg(test)]
mod tests {
    use super::*;
    struct Scratch(PathBuf);
    impl Drop for Scratch { fn drop(&mut self) { let _ = std::fs::remove_dir_all(&self.0); } }
    fn setup() -> (Scratch, AlwaysOnPaths, Arc<File>) {
        let root = Scratch(std::env::temp_dir().join(format!("pdrs-workspace-copy-{}", uuid::Uuid::new_v4())));
        let project = root.0.join("project"); std::fs::create_dir_all(&project).unwrap();
        let project_dir = root.0.join("home/always-on/projects/fixture");
        let paths = AlwaysOnPaths { project_key: project.to_string_lossy().into_owned(), state_file: project_dir.join("state.json"), plans_dir: project_dir.join("plans"), plan_index_file: project_dir.join("plans/index.json"), cycle_index_file: project_dir.join("cycles/index.json"), reports_dir: project_dir.join("reports"), discovery_lock_file: project_dir.join("locks/discovery.lock") };
        let file = pin_project(&project).unwrap(); (root, paths, file)
    }
    #[test]
    fn real_snapshot_preserves_files_links_and_root_exclusions_without_reading_external_link_targets() {
        let (root, paths, project) = setup(); let source = Path::new(&paths.project_key);
        std::fs::write(source.join("a.txt"), b"COPY-INPUT").unwrap();
        std::fs::create_dir_all(source.join("node_modules")).unwrap(); std::fs::write(source.join("node_modules/ignored"), "IGNORED").unwrap();
        std::fs::create_dir_all(source.join("nested/node_modules")).unwrap(); std::fs::write(source.join("nested/node_modules/kept"), "KEPT").unwrap();
        std::fs::write(root.0.join("external"), "EXTERNAL").unwrap();
        std::os::unix::fs::symlink("../external", source.join("link")).unwrap();
        let (target, metadata, selected) = snapshot(&paths, &project, "run", CopyLimits::default()).unwrap();
        assert_eq!(std::fs::read(target.join("a.txt")).unwrap(), b"COPY-INPUT");
        assert!(!target.join("node_modules").exists());
        assert_eq!(std::fs::read(target.join("nested/node_modules/kept")).unwrap(), b"KEPT");
        assert_eq!(std::fs::read_link(target.join("link")).unwrap(), PathBuf::from("../external"));
        assert!(metadata.get("copyStrategy").and_then(|value| value.as_string()).unwrap().equals_str("fs.cp"));
        assert!(owned_workspace(&paths, "snapshot-copy", &target).is_ok());
        assert!(owned_workspace(&paths, "snapshot-copy", source).is_err());
        let before = std::fs::read(root.0.join("external")).unwrap();
        remove_created(&paths, "snapshot-copy", &target, &selected).unwrap();
        assert_eq!(std::fs::read(root.0.join("external")).unwrap(), before);
    }
    #[test]
    fn sparse_source_over_default_budget_refuses_before_creating_a_workspace() {
        let (_root, paths, project) = setup();
        let file = File::create(Path::new(&paths.project_key).join("sparse")).unwrap();
        file.set_len(CopyLimits::default().bytes + 1).unwrap();
        let result = snapshot(&paths, &project, "over-budget", CopyLimits::default());
        assert!(result.is_err()); assert!(!paths.snapshots_dir().exists());
    }
    #[test]
    fn injected_count_depth_and_byte_boundaries_are_real_and_keep_existing_targets() {
        let (_root, paths, project) = setup(); let source = Path::new(&paths.project_key);
        std::fs::write(source.join("a"), [b'x'; 16]).unwrap();
        let limits = CopyLimits { bytes: 16, entries: 1, depth: 1, path_bytes: 64 };
        assert!(snapshot(&paths, &project, "exact", limits).is_ok());
        std::fs::write(source.join("a"), [b'x'; 17]).unwrap();
        assert!(snapshot(&paths, &project, "extra", limits).is_err());
        assert_eq!(std::fs::read(paths.snapshots_dir().join("exact/a")).unwrap(), [b'x'; 16]);
        std::fs::remove_file(source.join("a")).unwrap(); std::fs::create_dir_all(source.join("a/b")).unwrap();
        assert!(snapshot(&paths, &project, "depth", CopyLimits { bytes: 16, entries: 8, depth: 1, path_bytes: 64 }).is_err());
        assert!(snapshot(&paths, &project, "count", CopyLimits { bytes: 16, entries: 1, depth: 8, path_bytes: 64 }).is_err());
        assert!(!paths.snapshots_dir().join("extra").exists()); assert!(!paths.snapshots_dir().join("depth").exists()); assert!(!paths.snapshots_dir().join("count").exists());
    }
}

#[cfg(test)]mod capture_tests {
    use super::*;
    fn fixture()->(std::path::PathBuf,AlwaysOnPaths){let root=std::env::temp_dir().join(format!("pdrs-workspace-capture-{}",uuid::Uuid::new_v4()));let project=root.join("project");std::fs::create_dir_all(&project).unwrap();let base=root.join("home/always-on/projects/project");let paths=AlwaysOnPaths{project_key:project.to_string_lossy().into_owned(),state_file:base.join("state.json"),plans_dir:base.join("plans"),plan_index_file:base.join("plans/index.json"),cycle_index_file:base.join("cycles/index.json"),reports_dir:base.join("reports"),discovery_lock_file:base.join("locks/discovery.lock")};(root,paths)}
    #[test]
    fn capture_detects_swap_inside_the_old_check_to_move_window_without_deleting_foreign_data(){
        let(root,paths)=fixture();let(target,selected)=new_target(&paths,"snapshot-copy","run").unwrap();std::fs::write(target.join("owned"),"OWNED").unwrap();let moved=target.with_extension("kept");let path=target.clone();let kept=moved.clone();
        let result=capture_with(&paths,"snapshot-copy",&target,&selected,move||{std::fs::rename(&path,&kept)?;std::fs::create_dir(&path)?;std::fs::write(path.join("foreign"),"FOREIGN")?;Ok(())});
        assert!(result.is_err());assert_eq!(std::fs::read(target.join("foreign")).unwrap(),b"FOREIGN");assert_eq!(std::fs::read(moved.join("owned")).unwrap(),b"OWNED");std::fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn cleanup_uses_captured_object_when_a_new_original_path_appears(){
        let(root,paths)=fixture();let(target,selected)=new_target(&paths,"snapshot-copy","run").unwrap();std::fs::write(target.join("owned"),"OWNED").unwrap();let capture=capture_selected(&paths,"snapshot-copy",&target,&selected).unwrap();std::fs::create_dir(&target).unwrap();std::fs::write(target.join("foreign"),"FOREIGN").unwrap();capture.remove().unwrap();assert_eq!(std::fs::read(target.join("foreign")).unwrap(),b"FOREIGN");assert!(!capture.path.exists());assert!(!capture.path.exists());std::fs::remove_dir_all(root).unwrap();
    }
}
