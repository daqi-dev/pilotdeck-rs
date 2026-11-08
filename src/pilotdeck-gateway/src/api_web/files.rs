//! #1348 the TS `adapters/web/projectFiles.ts` workspace-safe file service
//! (baseline cc7c4e6): tree/read/write against a project root, every method
//! boundary-checked like `resolveSafe`. The TS methods are `async` only to
//! await other fs promises; the replica is synchronous std::fs. `path.resolve`
//! comes from `metadata`'s shared Node replica; the Node base64 decoder is a
//! deliberate copy of the measured one in `project_files.rs` (that file is
//! outside this unit's editable set, so the copy cannot drift from the oracle
//! it was measured against).
//! #1675 hardening: reads, writes and tree enumeration run through the
//! A4-FILE-TOCTOU-001 NOFOLLOW descriptor walk once the realpath gate passes,
//! so a component swapped inside the window fails closed instead of carrying
//! the request out of the workspace. Registered in
//! `specs/gateway-parity/diffs/A8.4-API-WEB-002.md` (approved by its R1
//! correction, `A8.4-API-WEB-002-R1`).
use base64::Engine;
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    cmp::Ordering,
    fs, io,
    path::{Component, Path, PathBuf},
    time::UNIX_EPOCH,
};

/// `DEFAULT_MAX_READ_BYTES`: a bigger file answers metadata only.
const DEFAULT_MAX_READ_BYTES: u64 = 1024 * 1024;

/// `TEXT_EXTENSIONS` (projectFiles.ts:88-117): the extensions `readFile`
/// serves as UTF-8 text; every other extension answers base64.
const TEXT_EXTENSIONS: [&str; 28] = [
    "ts", "tsx", "js", "jsx", "json", "md", "mdx", "yml", "yaml", "txt", "css", "html", "py", "go",
    "rs", "java", "c", "cpp", "h", "hpp", "sh", "bash", "toml", "ini", "csv", "tsv", "env", "lock",
];

#[derive(Debug)]
pub(crate) enum FilesError {
    /// `WorkspaceBoundaryError`: the router renders its fixed message.
    Workspace { raw: String },
    /// Any other thrown `Error`: the message text under `internal_error`.
    /// A Rust string cannot carry the lone surrogates a Node message can,
    /// so a message that keeps them travels as [`FilesError::MessageUnits`].
    Message(String),
    /// #2455: the message as the JS string Node produced, lone surrogates
    /// intact. The NUL refusal's 128 UTF-16 unit cut can split a surrogate
    /// pair, and the router renders the message through the well-formed JSON
    /// encoder so the wire keeps Node's `\ud83d` escape instead of the
    /// U+FFFD a lossy UTF-16 → UTF-8 boundary would leave behind.
    MessageUnits(JsString),
}

pub(crate) struct ProjectFileService {
    /// `realpathSync(resolve(options.projectRoot))`, falling back to the
    /// canonical form of the deepest existing ancestor when the root does not
    /// exist yet — the lexical spelling (`/tmp/...` on macOS) would otherwise
    /// disagree with every `canonical_prefix` comparison below.
    root: PathBuf,
    max_read_bytes: u64,
    /// The compiled `api-web-boundary-off` switch, read once here so the
    /// file holds exactly one mutant site for every boundary throw.
    boundary: bool,
    /// The compiled `api-web-nofollow-off` switch (#1675): the descriptor
    /// walk below is on unless that control forces the measured path form.
    nofollow: bool,
}

impl ProjectFileService {
    pub(crate) fn new(project_root: &str) -> Self {
        let resolved = match std::env::current_dir() {
            Ok(cwd) => crate::metadata::resolve(&cwd, project_root),
            Err(_) => PathBuf::from(project_root),
        };
        Self {
            // `canonical_prefix` already canonicalizes the deepest existing
            // ancestor and re-appends the missing tail, which is exactly the
            // form every containment check below compares against.
            root: canonical_prefix(&resolved),
            max_read_bytes: DEFAULT_MAX_READ_BYTES,
            boundary: !crate::mutants::on("api-web-boundary-off"),
            nofollow: !crate::mutants::on("api-web-nofollow-off"),
        }
    }

    /// `readTree`: entries with optional `size`/`modifiedAt` (a child `stat`
    /// error omits them), directories first, then `name.localeCompare`.
    pub(crate) fn read_tree(&self, relative: &str) -> Result<JsValue, FilesError> {
        let target = self.resolve_safe(relative)?;
        let stats = fs::metadata(&target).map_err(|error| sys_error(&error, "stat", &target))?;
        if !stats.is_dir() {
            return Err(FilesError::Message(format!("Not a directory: {relative}")));
        }
        // Deterministic boundary-probe seam: the realpath/stat gate above has
        // run, so a test may replace the listed directory before either walk
        // opens it — the seam sits ahead of the switch so the measured path
        // form is exercised by the same swap.
        #[cfg(test)]
        test_hook::notify("tree");
        let mut rows = self.tree_rows(relative, &target)?;
        sort_tree_rows(&mut rows)?;
        let entries = rows
            .into_iter()
            .map(|(_, _, entry)| JsValue::Object(entry))
            .collect::<Vec<_>>();
        let mut result = JsObject::new();
        result.insert(
            "projectKey".into(),
            JsValue::from(self.root.to_string_lossy().to_string().as_str()),
        );
        result.insert("entries".into(), JsValue::Array(entries));
        Ok(JsValue::Object(result))
    }

    /// `readFile`: text extensions answer UTF-8 content, everything else
    /// base64, and a file past `maxReadBytes` answers metadata only — with
    /// the text/base64 encoding the full read would have used.
    pub(crate) fn read_file(&self, relative: &str) -> Result<JsValue, FilesError> {
        let target = self.resolve_safe(relative)?;
        let stats = fs::metadata(&target).map_err(|error| sys_error(&error, "stat", &target))?;
        if !stats.is_file() {
            return Err(FilesError::Message(format!("Not a file: {relative}")));
        }
        // The extension comes from the raw input, not the resolved target,
        // so a symlink to a text file keeps the link's own encoding.
        let ext = relative.split('.').next_back().unwrap_or("").to_lowercase();
        let text =
            !crate::mutants::on("api-web-text-ext-off") && TEXT_EXTENSIONS.contains(&ext.as_str());
        let over =
            !crate::mutants::on("api-web-read-limit-off") && stats.len() > self.max_read_bytes;
        let mut result = JsObject::new();
        result.insert(
            "projectKey".into(),
            JsValue::from(self.root.to_string_lossy().to_string().as_str()),
        );
        result.insert(
            "path".into(),
            JsValue::from(self.to_relative(&target).as_str()),
        );
        result.insert("size".into(), JsValue::Number(stats.len() as f64));
        if over {
            result.insert(
                "encoding".into(),
                JsValue::from(if text { "utf8" } else { "base64" }),
            );
            result.insert("binary".into(), JsValue::Bool(!text));
            return Ok(JsValue::Object(result));
        }
        // Deterministic boundary-probe seam: every path-based check above has
        // run, so a test may replace a component before the content open.
        #[cfg(test)]
        test_hook::notify("read");
        let bytes = self.read_bytes(relative, &target)?;
        if text {
            result.insert("encoding".into(), JsValue::from("utf8"));
            // Node's utf8 read replaces invalid sequences with U+FFFD.
            result.insert(
                "content".into(),
                JsValue::from(String::from_utf8_lossy(&bytes).to_string().as_str()),
            );
        } else {
            result.insert("encoding".into(), JsValue::from("base64"));
            result.insert(
                "content".into(),
                JsValue::from(
                    base64::engine::general_purpose::STANDARD
                        .encode(&bytes)
                        .as_str(),
                ),
            );
            result.insert("binary".into(), JsValue::Bool(true));
        }
        Ok(JsValue::Object(result))
    }

    /// `writeFile`: base64 through Node's lenient decoder, UTF-8 otherwise;
    /// the parent directories appear first.
    pub(crate) fn write_file(
        &self,
        relative: &str,
        content: &JsString,
        base64: bool,
    ) -> Result<(), FilesError> {
        let (literal, target) = self.resolve_safe_writing(relative)?;
        let bytes = if base64 {
            node_base64_decode(content.units())
        } else {
            String::from_utf16_lossy(content.units()).into_bytes()
        };
        // Deterministic boundary-probe seam: the lexical/realpath checks above
        // have run, so a test may replace a component before the write.
        #[cfg(test)]
        test_hook::notify("write");
        // #1764: Node's own write-path argument validation, in TS order.
        if !crate::mutants::on("api-web-node-write-refusal-off") {
            node_write_refusal(&literal)?;
        }
        self.write_bytes(relative, &literal, &target, &bytes)
    }

    /// One `readTree` entry: the directory-first flag, the collation key and
    /// the response object, shared by both walkers. `stats` is `None` when the
    /// follow-`stat` failed, which omits `size`/`modifiedAt` like the TS code.
    fn tree_row(
        &self,
        target: &Path,
        name: &str,
        kind: EntryKind,
        stats: Option<(i64, i64)>,
    ) -> (bool, String, JsObject) {
        let child = crate::metadata::resolve(target, name);
        let mut entry = JsObject::new();
        entry.insert("name".into(), JsValue::from(name));
        entry.insert(
            "path".into(),
            JsValue::from(self.to_relative(&child).as_str()),
        );
        entry.insert("type".into(), JsValue::from(kind.name()));
        if let Some((size, mtime)) = stats {
            entry.insert("size".into(), JsValue::Number(size as f64));
            // #1691: `Date#toISOString` of the stat mtime is the transport's
            // shared `format_epoch_iso` (Node v22.22.0-equivalent for the
            // whole epoch domain, extended years included — the local copy
            // this file used to carry is deleted; the equivalence is pinned
            // in the transport's own tests and below).
            entry.insert(
                "modifiedAt".into(),
                JsValue::from(pilotdeck_transport::system_clock::format_epoch_iso(mtime).as_str()),
            );
        }
        (kind == EntryKind::Directory, name.to_owned(), entry)
    }

    /// The `readTree` rows: the #1675 descriptor walk when it is armed, the
    /// measured path-based walk otherwise.
    fn tree_rows(
        &self,
        relative: &str,
        target: &Path,
    ) -> Result<Vec<(bool, String, JsObject)>, FilesError> {
        #[cfg(unix)]
        if self.beneath(target).is_some() {
            return self.rows_beneath(relative, target);
        }
        let _ = relative;
        self.rows_by_path(target)
    }

    /// `fs.readdir` with `withFileTypes`: the entry type is the directory
    /// entry's own type and a child `stat` supplies `size`/`modifiedAt`.
    fn rows_by_path(&self, target: &Path) -> Result<Vec<(bool, String, JsObject)>, FilesError> {
        let mut rows = Vec::new();
        for dirent in fs::read_dir(target).map_err(|error| sys_error(&error, "scandir", target))? {
            let dirent = dirent.map_err(|error| sys_error(&error, "scandir", target))?;
            // readdir withFileTypes reports the link type itself (std falls
            // back to lstat when the filesystem answers DT_UNKNOWN).
            let kind = dirent
                .file_type()
                .map_err(|error| sys_error(&error, "stat", &dirent.path()))?;
            let name = dirent.file_name().to_string_lossy().into_owned();
            let child = crate::metadata::resolve(target, &name);
            let stats = fs::metadata(&child)
                .ok()
                .map(|stats| (stats.len() as i64, mtime_ms(&stats)));
            rows.push(self.tree_row(target, &name, EntryKind::from(kind), stats));
        }
        Ok(rows)
    }

    /// The #1675 form: enumerate and `stat` through one descriptor opened with
    /// the A4-FILE-TOCTOU-001 NOFOLLOW walk, so a directory swapped after the
    /// realpath gate is refused instead of listed.
    #[cfg(unix)]
    fn rows_beneath(
        &self,
        relative: &str,
        target: &Path,
    ) -> Result<Vec<(bool, String, JsObject)>, FilesError> {
        use rustix::fs::{AtFlags, Dir, FileType, statat};
        let directory = crate::project_files::open_beneath(&self.root, target)
            .map_err(|error| self.refusal("scandir", relative, target, error))?;
        let mut rows = Vec::new();
        let entries = Dir::read_from(&directory)
            .map_err(|error| sys_error(&io::Error::from(error), "scandir", target))?;
        for dirent in entries {
            let dirent =
                dirent.map_err(|error| sys_error(&io::Error::from(error), "scandir", target))?;
            let name = dirent.file_name().to_string_lossy().into_owned();
            // getdents reports the two dot entries; readdir never yields them.
            if name == "." || name == ".." {
                continue;
            }
            let child = crate::metadata::resolve(target, &name);
            // readdir withFileTypes reports the link type itself; DT_UNKNOWN
            // falls back to the pinned parent's NOFOLLOW stat, like std/Node.
            let kind = match dirent.file_type() {
                FileType::Unknown => {
                    statat(&directory, dirent.file_name(), AtFlags::SYMLINK_NOFOLLOW)
                        .map(|stat| FileType::from_raw_mode(stat.st_mode))
                }
                kind => Ok(kind),
            }
            .map_err(|error| sys_error(&io::Error::from(error), "stat", &child))?;
            let stats = statat(&directory, dirent.file_name(), AtFlags::empty())
                .ok()
                // Same millisecond truncation as `mtime_ms`, and the same
                // `as f64` shape `project_files::native_stat` uses because
                // `st_mtime_nsec` is `u32` on Linux and `i64` on macOS.
                .map(|stat| {
                    (
                        stat.st_size,
                        (stat.st_mtime as f64 * 1000.0 + stat.st_mtime_nsec as f64 / 1_000_000.0)
                            as i64,
                    )
                });
            rows.push(self.tree_row(target, &name, EntryKind::from(kind), stats));
        }
        Ok(rows)
    }

    /// The file bytes: the #1675 descriptor open when it is armed, the
    /// measured path open otherwise.
    fn read_bytes(&self, relative: &str, target: &Path) -> Result<Vec<u8>, FilesError> {
        #[cfg(unix)]
        if self.beneath(target).is_some() {
            return self.read_beneath(relative, target);
        }
        let _ = relative;
        fs::read(target).map_err(|error| sys_error(&error, "open", target))
    }

    /// Open the realpath-gated target below a NOFOLLOW descriptor walk and
    /// read the handle: a component swapped after the checks fails closed.
    #[cfg(unix)]
    fn read_beneath(&self, relative: &str, target: &Path) -> Result<Vec<u8>, FilesError> {
        use std::io::Read as _;
        let mut file = crate::project_files::open_beneath(&self.root, target)
            .map_err(|error| self.refusal("open", relative, target, error))?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)
            .map_err(|error| self.refusal("read", relative, target, error))?;
        Ok(bytes)
    }

    /// Write the bytes: the #1675 descriptor mkdir + inode-checked overwrite
    /// when it is armed, the measured path form otherwise. `literal` is TS's
    /// argument spelling — every Node diagnostic below names it, never the
    /// canonical form the armed walk resolves in-root links to (Node opens the
    /// literal path and reports that one).
    fn write_bytes(
        &self,
        relative: &str,
        literal: &Path,
        target: &Path,
        bytes: &[u8],
    ) -> Result<(), FilesError> {
        #[cfg(unix)]
        if let Some(beneath) = self.beneath(target) {
            return self.write_beneath(relative, literal, beneath, bytes);
        }
        let _ = relative;
        let _ = target;
        if let Some(parent) = literal.parent() {
            fs::create_dir_all(parent).map_err(|error| sys_error(&error, "mkdir", parent))?;
        }
        fs::write(literal, bytes).map_err(|error| sys_error(&error, "open", literal))
    }

    /// `createDirectories` + `overwrite` relative to one pinned root
    /// descriptor: no component is ever followed, and the leaf is only
    /// truncated after its inode proves a private regular file. A directory
    /// target is Node's own EISDIR refusal from the write's `open` — the leaf
    /// `openat(O_WRONLY)` below returns it naturally — so the #1675 window
    /// never re-introduces a follow-stat that could classify a raced component
    /// (a swapped link to an outside directory answers the boundary refusal,
    /// not an EISDIR oracle on the outside leaf).
    #[cfg(unix)]
    fn write_beneath(
        &self,
        relative: &str,
        literal: &Path,
        beneath: &Path,
        bytes: &[u8],
    ) -> Result<(), FilesError> {
        // `path: "."` (or a spelling that normalizes to it) targets the
        // pinned root itself, so the leaf open below has no relative name
        // to open — `write_root_itself` decides between Node's EISDIR on an
        // existing root and #2456's create-the-root-as-a-file cell.
        if beneath.as_os_str().is_empty() {
            return self.write_root_itself(relative, literal, bytes);
        }
        let root = self.root_beneath(relative)?;
        if let Some(parent) = beneath.parent()
            && !parent.as_os_str().is_empty()
        {
            crate::project_files::create_directories_at(
                &root,
                parent,
                rustix::fs::Mode::from_raw_mode(0o777),
            )
            .map_err(|error| self.refusal("mkdir", relative, literal, error))?;
        }
        crate::project_files::overwrite_at(&root, beneath, bytes)
            .map_err(|error| self.refusal("open", relative, literal, error))
    }

    /// `path: "."` on the pinned root itself. An existing root is a
    /// directory the walk proved, and Node's own `writeFile` open refuses it
    /// with EISDIR on the literal path; a root this write has to create is
    /// #2456's cell — TS's `mkdir(dirname(target), {recursive: true})`
    /// creates only the root's *ancestors*, and the write's own open then
    /// creates the root as a regular file and answers 200.
    #[cfg(unix)]
    fn write_root_itself(
        &self,
        relative: &str,
        literal: &Path,
        bytes: &[u8],
    ) -> Result<(), FilesError> {
        match crate::project_files::open_directory_beneath(&self.root, &self.root) {
            Ok(_root) => Err(sys_error(
                &io::Error::from_raw_os_error(libc::EISDIR),
                "open",
                literal,
            )),
            // A root this write has to create: the ancestors become
            // directories (the `mkdir(dirname(target))` half), the root
            // itself is the write's own regular-file leaf.
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                self.create_root_file(relative, literal, bytes)
            }
            // Any other root-open refusal keeps the same mapping as
            // `root_beneath`: the ELOOP → boundary refusal included.
            Err(error) => Err(self.root_open_refusal(relative, error)),
        }
    }

    /// The #2456 missing-root `path: "."` write. `created_root` would `mkdir`
    /// the root itself and the write would then answer EISDIR; Node's open
    /// instead creates the root as a regular file and answers 200. The
    /// ancestors are created below the same deepest-existing NOFOLLOW anchor
    /// `created_root` pins, and the leaf is `overwrite_at`'s own
    /// inode-checked open — no follow-open anywhere, so an ancestor swapped
    /// for a link inside the window fails closed instead of carrying the
    /// write out of the workspace. The finished root is a file; nothing in
    /// this write's course re-opens the walk under a directory assumption.
    #[cfg(unix)]
    fn create_root_file(
        &self,
        relative: &str,
        literal: &Path,
        bytes: &[u8],
    ) -> Result<(), FilesError> {
        // Deterministic boundary-probe seam: the missing-root open has
        // already failed, so a test may swap an ancestor before the pin
        // below (same phase as `created_root`).
        #[cfg(test)]
        test_hook::notify("create-root");
        let ancestor = deepest_existing(&self.root);
        let pinned = crate::project_files::open_directory_beneath(&ancestor, &ancestor)
            .map_err(|error| self.root_open_refusal(relative, error))?;
        let tail = self
            .root
            .strip_prefix(&ancestor)
            .map_err(|_| sys_error(&io::Error::from_raw_os_error(libc::EACCES), "open", literal))?;
        // The root came to exist inside the window between the missing-root
        // open above and the pin (the shape a racing `mkdir` leaves), so the
        // write's target is the root itself again and `write_root_itself`
        // decides — Node's EISDIR on a directory root, the boundary refusal
        // on a link swapped in — instead of an empty relative path reaching
        // the leaf open's `UNKNOWN` diagnostic.
        if tail.as_os_str().is_empty() {
            return self.write_root_itself(relative, literal, bytes);
        }
        // `mkdir(dirname(target), {recursive: true})`: every component of
        // the missing tail except the root itself; the root is this write's
        // own leaf below.
        if let Some(dirs) = tail.parent()
            && !dirs.as_os_str().is_empty()
        {
            crate::project_files::create_directories_at(
                &pinned,
                dirs,
                rustix::fs::Mode::from_raw_mode(0o777),
            )
            .map_err(|error| self.refusal("mkdir", relative, literal, error))?;
        }
        crate::project_files::overwrite_at(&pinned, tail, bytes)
            .map_err(|error| self.refusal("open", relative, literal, error))
    }

    /// Pin the project root itself for the write walk. The root is opened by
    /// the same from-`/` NOFOLLOW walk as every other component, so a root
    /// swapped for a link inside the window fails closed; a root that simply
    /// cannot be opened keeps the Node diagnostic on the root path.
    #[cfg(unix)]
    fn root_beneath(&self, relative: &str) -> Result<fs::File, FilesError> {
        let pinned = match crate::project_files::open_directory_beneath(&self.root, &self.root) {
            // TS `writeFile` runs `mkdir(dirname(target), {recursive: true})`
            // first, so a project root that does not exist yet is created and
            // the write succeeds instead of answering ENOENT (the `path: "."`
            // spelling on a missing root is #2456's separate cell in
            // `write_root_itself`).
            Err(error) if error.kind() == io::ErrorKind::NotFound => self.created_root(),
            other => other,
        };
        pinned.map_err(|error| self.root_open_refusal(relative, error))
    }

    /// The root-open refusal mapping `root_beneath` answers: a NOFOLLOW
    /// refusal is the boundary error, anything else keeps the Node diagnostic
    /// on the root path.
    #[cfg(unix)]
    fn root_open_refusal(&self, relative: &str, error: io::Error) -> FilesError {
        if crate::project_files::nofollow_loop(&error)
            || error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
        {
            return FilesError::Workspace {
                raw: relative.to_owned(),
            };
        }
        sys_error(&error, "open", &self.root)
    }

    /// Create the project root this write found missing, through the same
    /// pinned walk as every other write step: the deepest existing ancestor is
    /// pinned NOFOLLOW first, the missing tail is `mkdirat`-ed below that
    /// descriptor, and the finished root is re-pinned by that same NOFOLLOW
    /// walk — no follow-open ever happens, so ancestry swapped for a link
    /// inside the window fails closed instead of carrying the root (and the
    /// write) out of the workspace. A rename that substitutes a *real*
    /// directory still resolves by path, exactly as Node does.
    #[cfg(unix)]
    fn created_root(&self) -> io::Result<fs::File> {
        // Deterministic boundary-probe seam: the missing-root open has already
        // failed, so a test may swap an ancestor before the pin below.
        #[cfg(test)]
        test_hook::notify("create-root");
        let ancestor = deepest_existing(&self.root);
        let pinned = crate::project_files::open_directory_beneath(&ancestor, &ancestor)?;
        let tail = self
            .root
            .strip_prefix(&ancestor)
            .map_err(|_| io::Error::from_raw_os_error(libc::EACCES))?;
        if !tail.as_os_str().is_empty() {
            crate::project_files::create_directories_at(
                &pinned,
                tail,
                rustix::fs::Mode::from_raw_mode(0o777),
            )?;
        }
        crate::project_files::open_directory_beneath(&self.root, &self.root)
    }

    /// The target below the root when the #1675 descriptor walk is armed. The
    /// boundary gate already proved containment, so a strip failure means that
    /// gate itself was compiled off (its negative control) and the measured
    /// path form has to run instead.
    #[cfg(unix)]
    fn beneath<'a>(&self, target: &'a Path) -> Option<&'a Path> {
        if !self.armed() {
            return None;
        }
        target.strip_prefix(&self.root).ok()
    }

    #[cfg(not(unix))]
    fn beneath(&self, _target: &Path) -> Option<&Path> {
        None
    }

    /// The #1675 descriptor walk is armed on Unix unless the switch forces the
    /// measured path form; non-Unix keeps the measured form.
    fn armed(&self) -> bool {
        cfg!(unix) && self.nofollow
    }

    /// A NOFOLLOW refusal after the realpath gate passed means a component was
    /// swapped inside the window: answer the boundary error, never the raced
    /// path (TS follows it). Any other failure keeps the Node diagnostic.
    fn refusal(
        &self,
        syscall: &str,
        relative: &str,
        target: &Path,
        error: io::Error,
    ) -> FilesError {
        if crate::project_files::nofollow_loop(&error)
            || error.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error())
        {
            return FilesError::Workspace {
                raw: relative.to_owned(),
            };
        }
        sys_error(&error, syscall, target)
    }

    /// `resolveSafe`: the lexical boundary checks, then the realpath check
    /// when the target exists — a symlink that lands outside the root is an
    /// escape too. ENOENT answers the lexical target (the write-before-create
    /// case); any other realpath error rethrows.
    fn resolve_safe(&self, raw: &str) -> Result<PathBuf, FilesError> {
        let lexical = self.lexical_target(raw)?;
        match fs::canonicalize(&lexical) {
            Ok(real) if self.boundary && !self.is_inside_root(&real) => {
                Err(FilesError::Workspace {
                    raw: raw.to_owned(),
                })
            }
            Ok(real) => Ok(real),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(lexical),
            Err(error) => Err(sys_error(&error, "realpath", &lexical)),
        }
    }

    /// `resolveSafeWriting`: the lexical checks, plus the realpath of the
    /// deepest existing ancestor when the #1675 walk is armed. TS never
    /// realpaths here, so an **in-root** link keeps its write-through; only a
    /// link that lands outside the root (or a raced replacement) is refused.
    /// The realpath never loosens the lexical gate above. The answer is
    /// `(literal, resolved)`: the literal target is TS's argument spelling
    /// (what Node's write-path diagnostics report), the resolved one is where
    /// the armed walk writes; unarmed the two are the same lexical path.
    fn resolve_safe_writing(&self, raw: &str) -> Result<(PathBuf, PathBuf), FilesError> {
        let lexical = self.lexical_target(raw)?;
        if !self.armed() {
            return Ok((lexical.clone(), lexical));
        }
        let resolved = canonical_prefix(&lexical);
        if self.boundary && !self.is_inside_root(&resolved) {
            return Err(FilesError::Workspace {
                raw: raw.to_owned(),
            });
        }
        Ok((lexical, resolved))
    }

    /// The shared lexical gate of both resolve forms: `isAbsolute`, a
    /// `normalize` that starts with `..` (which `..foo` also does), and the
    /// containment re-check of the resolved target.
    fn lexical_target(&self, raw: &str) -> Result<PathBuf, FilesError> {
        if self.boundary && (raw.starts_with('/') || node_normalize(raw).starts_with("..")) {
            return Err(FilesError::Workspace {
                raw: raw.to_owned(),
            });
        }
        let target = crate::metadata::resolve(&self.root, &node_normalize(raw));
        if self.boundary && !self.is_inside_root(&target) {
            return Err(FilesError::Workspace {
                raw: raw.to_owned(),
            });
        }
        Ok(target)
    }

    /// `isInsideRoot`: `relative(root, path)` may not start with `..` and is
    /// never absolute.
    fn is_inside_root(&self, absolute: &Path) -> bool {
        let rel = node_relative(&self.root, absolute);
        !rel.starts_with("..") && !rel.starts_with('/')
    }

    /// `toRelative`: `relative(root, path)`, "" as "." (POSIX: no backslash
    /// to normalize away).
    fn to_relative(&self, absolute: &Path) -> String {
        let rel = node_relative(&self.root, absolute);
        if rel.is_empty() { ".".to_owned() } else { rel }
    }
}

/// The realpath of the deepest existing ancestor of an absolute path, with the
/// not-yet-created tail re-appended. `canonicalize` answers ENOENT for a write
/// target that does not exist yet, but its existing prefix still has to be
/// resolved so an in-root link keeps TS's write-through while an outward one
/// is caught. Any other realpath error leaves the path lexical and lets the
/// write report it.
fn canonical_prefix(path: &Path) -> PathBuf {
    let mut existing = path.to_path_buf();
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    loop {
        match fs::canonicalize(&existing) {
            Ok(mut real) => {
                for component in tail.iter().rev() {
                    real.push(component);
                }
                return real;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                match (existing.file_name(), existing.parent()) {
                    (Some(name), Some(parent)) => {
                        tail.push(name.to_owned());
                        existing = parent.to_path_buf();
                    }
                    _ => return path.to_path_buf(),
                }
            }
            Err(_) => return path.to_path_buf(),
        }
    }
}

/// The deepest ancestor of `path` that exists, so a missing tail can be
/// created below a pinned descriptor instead of below a reopened path. Only
/// the existence question is asked here; the pinned NOFOLLOW walk decides
/// whether the ancestor is usable.
#[cfg(unix)]
fn deepest_existing(path: &Path) -> PathBuf {
    let mut existing = path;
    loop {
        if fs::metadata(existing).is_ok() {
            return existing.to_path_buf();
        }
        match existing.parent() {
            Some(parent) => existing = parent,
            None => return PathBuf::from("/"),
        }
    }
}

/// A `readdir` entry type: `withFileTypes` reports the directory entry's own
/// kind, so a link is a link and everything else is `other`.
#[derive(Clone, Copy, PartialEq)]
enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

impl EntryKind {
    fn name(self) -> &'static str {
        match self {
            Self::Directory => "directory",
            Self::File => "file",
            Self::Symlink => "symlink",
            Self::Other => "other",
        }
    }
}

impl From<fs::FileType> for EntryKind {
    fn from(kind: fs::FileType) -> Self {
        if kind.is_dir() {
            Self::Directory
        } else if kind.is_file() {
            Self::File
        } else if kind.is_symlink() {
            Self::Symlink
        } else {
            Self::Other
        }
    }
}

#[cfg(unix)]
impl From<rustix::fs::FileType> for EntryKind {
    fn from(kind: rustix::fs::FileType) -> Self {
        match kind {
            rustix::fs::FileType::Directory => Self::Directory,
            rustix::fs::FileType::RegularFile => Self::File,
            rustix::fs::FileType::Symlink => Self::Symlink,
            _ => Self::Other,
        }
    }
}

/// Node `path.normalize` on the inputs the boundary check sees: dot segments
/// collapse, leading `..` survive, and a trailing slash stays as a marker
/// ("" → ".", "sub/../" → "./").
fn node_normalize(path: &str) -> String {
    if path.is_empty() {
        return ".".to_owned();
    }
    let absolute = path.starts_with('/');
    let trailing = path.ends_with('/');
    let mut out: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => (),
            ".." => {
                if out.last().is_some_and(|last| *last != "..") {
                    out.pop();
                } else if !absolute {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    let joined = out.join("/");
    if absolute {
        format!("/{joined}")
    } else if joined.is_empty() {
        if trailing {
            "./".to_owned()
        } else {
            ".".to_owned()
        }
    } else if trailing {
        format!("{joined}/")
    } else {
        joined
    }
}

/// Node `path.relative(from, to)` over two already-normalized absolutes.
fn node_relative(from: &Path, to: &Path) -> String {
    let from = from.components().collect::<Vec<Component>>();
    let to = to.components().collect::<Vec<Component>>();
    let mut shared = 0;
    while shared < from.len() && shared < to.len() && from[shared] == to[shared] {
        shared += 1;
    }
    let mut parts = vec!["..".to_owned(); from.len() - shared];
    parts.extend(
        to[shared..]
            .iter()
            .map(|component| component.as_os_str().to_string_lossy().to_string()),
    );
    parts.join("/")
}

/// `name.localeCompare` ordering of the tree rows: directories first, then
/// the shared comparator's unit remap. #1740: the locale-resolved collator is
/// cached per thread per locale — `node_collator`'s value holds only baked
/// `'static` ICU data, so it stashes whole in a thread-local map. The locale
/// itself is still resolved from the environment on every call, so a `LANG`
/// change takes effect on the next tree; the sort answers are unchanged, only
/// the per-request rebuild goes away.
fn sort_tree_rows(rows: &mut [(bool, String, JsObject)]) -> Result<(), FilesError> {
    thread_local! {
        static COLLATORS: std::cell::RefCell<
            std::collections::HashMap<icu_locale_core::Locale, icu_collator::CollatorBorrowed<'static>>,
        > = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    COLLATORS.with_borrow_mut(|cache| {
        let locale = crate::node_locale::node_locale(|key| std::env::var(key).ok());
        if !cache.contains_key(&locale) {
            let collator = crate::project_files::node_collator(&locale)
                .map_err(|_| FilesError::Message("Collation data unavailable.".to_owned()))?;
            cache.insert(locale.clone(), collator);
        }
        let collator = cache.get(&locale).expect("the locale was just inserted");
        rows.sort_by(|(left_dir, left_name, _), (right_dir, right_name, _)| {
            match (left_dir, right_dir) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                // localeCompare keeps unpaired-surrogate order through the
                // shared comparator's unit remap.
                _ => crate::project_files::node_compare_utf16(
                    collator,
                    &JsString::from(left_name.as_str()),
                    &JsString::from(right_name.as_str()),
                ),
            }
        });
        Ok(())
    })
}

fn mtime_ms(stats: &fs::Metadata) -> i64 {
    stats
        .modified()
        .map(|when| match when.duration_since(UNIX_EPOCH) {
            Ok(delta) => delta.as_millis() as i64,
            Err(before) => -(before.duration().as_millis() as i64),
        })
        .unwrap_or_default()
}

/// `Buffer.from(value, "base64")` — a copy of the measured decoder in
/// `project_files.rs` (that frozen file cannot be edited to export it):
/// whitespace and other non-alphabet characters are skipped, `=` stops the
/// run, the standard and URL-safe alphabets both decode, each UTF-16 unit
/// contributes its low byte, and a dangling sextet is dropped.
fn node_base64_decode(value: &[u16]) -> Vec<u8> {
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
    .unwrap_or_default()
}

/// Test seam for the boundary probes (#1675): the hook fires once the
/// path-based checks have run and before the content open/write, so a test
/// can deterministically swap a component inside the race window. It mirrors
/// `runtime::memory::edgeclaw::file_memory::read_test_hook`.
#[cfg(test)]
pub(crate) mod test_hook {
    use std::cell::RefCell;
    type Hook = Box<dyn Fn(&str)>;
    thread_local! { static HOOK: RefCell<Option<Hook>> = const { RefCell::new(None) }; }
    pub(crate) struct Guard;
    pub(crate) fn set(hook: impl Fn(&str) + 'static) -> Guard {
        HOOK.with(|slot| {
            assert!(slot.borrow().is_none());
            *slot.borrow_mut() = Some(Box::new(hook));
        });
        Guard
    }
    pub(crate) fn notify(phase: &str) {
        HOOK.with(|slot| {
            if let Some(hook) = slot.borrow().as_ref() {
                hook(phase);
            }
        });
    }
    impl Drop for Guard {
        fn drop(&mut self) {
            HOOK.with(|slot| *slot.borrow_mut() = None);
        }
    }
}

/// A Node one-path syscall diagnostic, the `error.message` the 500 carries.
fn sys_error(error: &io::Error, syscall: &str, path: &Path) -> FilesError {
    FilesError::Message(
        crate::node_io::diagnostic_or_unknown(error, syscall, Some(&path.to_string_lossy())).1,
    )
}

/// #1764: the argument validation Node's write path runs before any syscall,
/// in TS order. TS writes run `mkdir(dirname(target), {recursive: true})`
/// first and `writeFile(target)` second, and both calls validate their own
/// path argument (no NUL bytes) before touching the filesystem. A directory
/// target is NOT pre-checked here: Node's EISDIR is the write's own `open`
/// refusal, so this replica answers it from the same place — the armed walk's
/// leaf `openat(O_WRONLY)` (or the plain path open below it) returns EISDIR
/// naturally, on the literal path. Measured against node v22.22.0
/// (`_tmp/20261026-issue-2354/ts-probe/probe-output.txt` and
/// `_tmp/20261026-issue-2354/fix-r1/ts-probe/node-write-refusal.txt`): the
/// old replica answered a bare InvalidInput for the root itself and an EINVAL
/// for a NUL.
fn node_write_refusal(target: &Path) -> Result<(), FilesError> {
    // The mkdir's argument is checked first: a NUL in a directory component
    // of the target is refused with the parent path as received.
    if let Some(parent) = target.parent() {
        let parent = parent.to_string_lossy();
        if parent.contains('\0') {
            return Err(null_byte_refusal(&parent));
        }
    }
    let text = target.to_string_lossy();
    if text.contains('\0') {
        return Err(null_byte_refusal(&text));
    }
    Ok(())
}

/// Node's ERR_INVALID_ARG_VALUE text for a NUL-bearing path argument: the
/// shared `node_io::path_error` renderer — `util.inspect`'s quoting (the
/// backtick tier for a value with both quote kinds included) and the error
/// constructor's 128 UTF-16 unit cut, not a local subset copy. The value is a
/// Rust string here, so the *inspected value* holds no lone surrogates (the
/// JSON boundary already replaced them with U+FFFD, like Node's own
/// JS-to-fs boundary); the 128-unit cut itself can still split a surrogate
/// pair (#2455), so the message keeps its UTF-16 units to the JSON encoder
/// instead of a lossy `String` fold.
fn null_byte_refusal(path: &str) -> FilesError {
    let message = crate::node_io::nul_message_for(&JsString::from(path), "path");
    FilesError::MessageUnits(message)
}

#[cfg(test)]
mod tests {
    use super::node_base64_decode;

    /// #1348 the low-byte rule is measured, not assumed: Node writes the
    /// string one byte per UTF-16 unit (V8 `String::WriteOneByte`, which
    /// truncates above U+00FF) and only then skips non-alphabet bytes, so
    /// `Ł` U+0141 decodes exactly like `A`. Raw v22.22.0 output:
    /// `Buffer.from('ŁAAA','base64')` → `000000`, `'łAAA'` → `040000`,
    /// `'éAAA'` → `0000`, `'😀AAA'` → ``, `'AA=AA'` → `00`, `'a-b_'` →
    /// `6be6ff`, `'!!!!'` → ``. A UTF-8 byte domain would answer `0`
    /// bytes for the first three and is the wrong model here.
    #[test]
    fn node_base64_truncates_units_to_low_bytes() {
        let cases: &[(&[u16], &[u8])] = &[
            (&[0x41, 0x41, 0x41], &[0, 0]),
            (&[0x0141, 0x41, 0x41, 0x41], &[0, 0, 0]),
            (&[0x0142, 0x41, 0x41, 0x41], &[4, 0, 0]),
            (&[0x00e9, 0x41, 0x41, 0x41], &[0, 0]),
            (&[0xd83d, 0xde00, 0x41, 0x41, 0x41], &[]),
            (&[0x41, 0x41, 0x3d, 0x41, 0x41], &[0]),
            (&[0x21, 0x21, 0x21, 0x21], &[]),
            (&[0x61, 0x2d, 0x62, 0x5f], &[0x6b, 0xe6, 0xff]),
        ];
        for (units, expected) in cases {
            assert_eq!(
                node_base64_decode(units),
                *expected,
                "#1348 node base64 low-byte decode of {units:04x?}"
            );
        }
    }
}
