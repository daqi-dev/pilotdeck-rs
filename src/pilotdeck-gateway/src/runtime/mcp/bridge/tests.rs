use super::*;
use pilotdeck_protocol::parse_json;
use std::{cell::Cell, fs, os::unix::fs::symlink};

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("pdrs-mcp-image-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

fn raw(link: &str) -> JsValue {
    JsValue::Array(vec![content_block(vec![
        ("type", "text".into()),
        (
            "text",
            JsValue::String(format!("keep [image]({link}) exactly\n").into()),
        ),
    ])])
}

fn assert_skipped(cwd: &Path, link: &str, roots: &[PathBuf]) {
    let raw = raw(link);
    assert_eq!(
        JsValue::Array(marshal_mcp_content_with_roots(&raw, cwd.to_str(), roots)),
        raw,
        "text must survive: {link}",
    );
}

fn assert_no_open(policy: &ImageReadPolicy, path: &Path) {
    let opens = Cell::new(0);
    let reads = Cell::new(0);
    assert!(
        policy
            .read_with(
                path,
                |_, _| {
                    opens.set(opens.get() + 1);
                    Err(io::ErrorKind::PermissionDenied.into())
                },
                |_| {
                    reads.set(reads.get() + 1);
                    Ok(Vec::new())
                }
            )
            .is_none()
    );
    assert_eq!(
        opens.get(),
        0,
        "unsafe candidate must never be opened: {path:?}"
    );
    assert_eq!(
        reads.get(),
        0,
        "unsafe candidate must never be read: {path:?}"
    );
}

#[test]
fn image_paths_use_shared_lexical_resolution() {
    assert_eq!(
        resolve_path("/a/./b", "../图.png").expect("#879 healthy image path"),
        Path::new("/a/图.png")
    );
    assert_eq!(
        resolve_path("/a", "/x/../img.png").expect("#879 healthy image path"),
        Path::new("/img.png")
    );
    assert_eq!(
        resolve_path("/a", "../../../img.png").expect("#879 healthy image path"),
        Path::new("/img.png")
    );
    let cwd = std::env::current_dir().unwrap();
    assert_eq!(
        resolve_path("a/../b", "./../img.png").expect("#879 healthy image path"),
        cwd.join("img.png")
    );
}

#[test]
fn non_array_content_and_fallbacks() {
    assert_eq!(
        marshal_mcp_content(&JsValue::Null, None),
        [parse_json(r#"{"type":"json","value":null}"#).unwrap()]
    );
    assert_eq!(
        marshal_mcp_content(&parse_json("[]").unwrap(), None),
        [parse_json(r#"{"type":"json","value":[]}"#).unwrap()]
    );
    assert_eq!(
        error_text(&JsValue::Null, "s", "t"),
        JsString::from("MCP server s/t returned isError")
    );
}

#[test]
fn image_links_are_lazy_and_case_insensitive() {
    let found: Vec<String> = IMAGE_LINK
        .captures_iter("[a](./x.PNG) [b](./y.txt) [c\n](./z.png) [d](./w.jpeg)")
        .map(|c| c[1].to_owned())
        .collect();
    assert_eq!(found, ["./x.PNG", "./w.jpeg"]);
}

#[test]
fn realpath_rejects_parent_absolute_and_symlink_escapes_without_open() {
    let dir = Scratch::new();
    let cwd = dir.path("cwd");
    fs::create_dir(&cwd).unwrap();
    fs::create_dir(dir.path("cwd-extra")).unwrap();
    fs::write(dir.path("outside.png"), b"secret").unwrap();
    fs::write(dir.path("cwd-extra/outside.png"), b"secret").unwrap();
    symlink("../outside.png", cwd.join("link.png")).unwrap();
    symlink("link.png", cwd.join("chain.png")).unwrap();
    symlink("..", cwd.join("parent")).unwrap();
    let policy = ImageReadPolicy::new(cwd.to_str().unwrap(), &[]);
    for link in [
        "../outside.png",
        "../cwd-extra/outside.png",
        "./link.png",
        "./chain.png",
        "./parent/outside.png",
    ] {
        assert_no_open(
            &policy,
            &resolve_path(cwd.to_str().unwrap(), link).expect("#879 healthy image path"),
        );
        assert_skipped(&cwd, link, &[]);
    }
    let absolute = dir.path("outside.png");
    assert_no_open(&policy, &absolute);
    assert_skipped(&cwd, absolute.to_str().unwrap(), &[]);
    // Even authorized absolute links are outside the baseline link grammar.
    fs::write(cwd.join("inside.png"), b"image").unwrap();
    assert_skipped(&cwd, cwd.join("inside.png").to_str().unwrap(), &[]);
}

#[test]
fn realpath_accepts_inside_links_chains_and_symlink_cwd() {
    let dir = Scratch::new();
    let cwd = dir.path("cwd");
    fs::create_dir(&cwd).unwrap();
    fs::write(cwd.join("inside.png"), b"image").unwrap();
    symlink("inside.png", cwd.join("link.png")).unwrap();
    symlink("link.png", cwd.join("chain.png")).unwrap();
    symlink(&cwd, dir.path("cwd-alias")).unwrap();
    let policy = ImageReadPolicy::new(dir.path("cwd-alias").to_str().unwrap(), &[]);
    for name in ["inside.png", "link.png", "chain.png"] {
        assert_eq!(policy.read(&cwd.join(name)), Some(b"image".to_vec()));
    }
}

#[test]
fn whitelist_is_explicit_canonical_directory_and_component_bounded() {
    let dir = Scratch::new();
    let cwd = dir.path("cwd");
    fs::create_dir(&cwd).unwrap();
    let allowed = dir.path("allowed");
    fs::create_dir(&allowed).unwrap();
    fs::create_dir(allowed.join("sub")).unwrap();
    fs::create_dir(dir.path("allowed-extra")).unwrap();
    fs::write(allowed.join("sub/image.png"), b"allowed").unwrap();
    fs::write(dir.path("allowed-extra/image.png"), b"secret").unwrap();
    symlink(&allowed, dir.path("allowed-alias")).unwrap();
    symlink("../allowed/sub/image.png", cwd.join("link.png")).unwrap();
    let roots = vec![dir.path("allowed-alias")];
    let policy = ImageReadPolicy::new(cwd.to_str().unwrap(), &roots);
    assert_eq!(
        policy.read(&cwd.join("link.png")),
        Some(b"allowed".to_vec())
    );
    assert_eq!(
        marshal_mcp_content_with_roots(&raw("./link.png"), cwd.to_str(), &roots).len(),
        2
    );
    assert_no_open(&policy, &dir.path("allowed-extra/image.png"));
    assert_skipped(&cwd, "../allowed-extra/image.png", &roots);
    assert_skipped(&cwd, "./link.png", &[]);
    for invalid in [
        allowed.join("sub/image.png"),
        dir.path("missing"),
        PathBuf::from("."),
    ] {
        let policy = ImageReadPolicy::new(cwd.to_str().unwrap(), &[invalid]);
        assert_eq!(
            policy.roots.len(),
            1,
            "invalid whitelist entry must grant nothing"
        );
        assert_no_open(&policy, &allowed.join("sub/image.png"));
    }
}

#[test]
fn non_regular_and_oversized_candidates_are_not_opened() {
    let dir = Scratch::new();
    fs::create_dir(dir.path("directory.png")).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(dir.path("fifo.png"))
            .status()
            .unwrap()
            .success()
    );
    File::create(dir.path("large.png"))
        .unwrap()
        .set_len(MCP_FILE_IMAGE_MAX_BYTES + 1)
        .unwrap();
    let policy = ImageReadPolicy::new(dir.0.to_str().unwrap(), &[]);
    for name in ["directory.png", "fifo.png", "large.png"] {
        assert_no_open(&policy, &dir.path(name));
        assert_skipped(&dir.0, &format!("./{name}"), &[]);
    }
    // Whitelisting /dev never authorizes character devices.
    let devices = ImageReadPolicy::new("/dev", &[]);
    assert_no_open(&devices, Path::new("/dev/null"));
}

#[test]
fn replacement_links_in_leaf_and_ancestor_are_not_followed() {
    for ancestor in [false, true] {
        let dir = Scratch::new();
        let cwd = dir.path("cwd");
        fs::create_dir_all(cwd.join("sub")).unwrap();
        fs::write(cwd.join("sub/image.png"), b"safe").unwrap();
        fs::create_dir(dir.path("outside")).unwrap();
        fs::write(dir.path("outside/image.png"), b"secret").unwrap();
        let policy = ImageReadPolicy::new(cwd.to_str().unwrap(), &[]);
        let reads = Cell::new(0);
        let bytes = policy.read_with(
            &cwd.join("sub/image.png"),
            |root, path| {
                if ancestor {
                    fs::rename(cwd.join("sub"), cwd.join("saved")).unwrap();
                    symlink(dir.path("outside"), cwd.join("sub")).unwrap();
                } else {
                    fs::remove_file(path).unwrap();
                    symlink(dir.path("outside/image.png"), path).unwrap();
                }
                crate::project_files::open_beneath(root, path)
            },
            |file| {
                reads.set(reads.get() + 1);
                read_limited_image(file)
            },
        );
        assert!(bytes.is_none());
        assert_eq!(reads.get(), 0, "raced-in secret must not be read");
    }
}

#[test]
fn opened_handle_identity_type_and_size_are_rechecked_before_read() {
    let dir = Scratch::new();
    fs::write(dir.path("image.png"), b"safe").unwrap();
    fs::write(dir.path("replacement.png"), b"secret").unwrap();
    let policy = ImageReadPolicy::new(dir.0.to_str().unwrap(), &[]);
    for replacement in [
        dir.path("replacement.png"),
        dir.0.clone(),
        PathBuf::from("/dev/null"),
    ] {
        let reads = Cell::new(0);
        assert!(
            policy
                .read_with(
                    &dir.path("image.png"),
                    |_, _| File::open(&replacement),
                    |_| {
                        reads.set(reads.get() + 1);
                        Ok(Vec::new())
                    }
                )
                .is_none()
        );
        assert_eq!(reads.get(), 0);
    }
    let reads = Cell::new(0);
    assert!(
        policy
            .read_with(
                &dir.path("image.png"),
                |root, path| {
                    File::options()
                        .write(true)
                        .open(path)?
                        .set_len(MCP_FILE_IMAGE_MAX_BYTES + 1)?;
                    crate::project_files::open_beneath(root, path)
                },
                |_| {
                    reads.set(reads.get() + 1);
                    Ok(Vec::new())
                }
            )
            .is_none()
    );
    assert_eq!(
        reads.get(),
        0,
        "same-inode growth must be rejected before reading"
    );
}

#[test]
fn read_is_bounded_even_if_file_grows_after_handle_metadata() {
    let dir = Scratch::new();
    fs::write(dir.path("image.png"), b"safe").unwrap();
    let policy = ImageReadPolicy::new(dir.0.to_str().unwrap(), &[]);
    assert!(
        policy
            .read_with(
                &dir.path("image.png"),
                crate::project_files::open_beneath,
                |file| {
                    File::options()
                        .write(true)
                        .open(dir.path("image.png"))?
                        .set_len(MCP_FILE_IMAGE_MAX_BYTES + 1)?;
                    read_limited_image(file)
                }
            )
            .is_none()
    );
    // Inclusive boundary: exactly the TS attachment cap is accepted.
    File::options()
        .write(true)
        .open(dir.path("image.png"))
        .unwrap()
        .set_len(MCP_FILE_IMAGE_MAX_BYTES)
        .unwrap();
    assert_eq!(
        policy.read(&dir.path("image.png")).unwrap().len() as u64,
        MCP_FILE_IMAGE_MAX_BYTES
    );
}

#[test]
fn reader_never_consumes_more_than_limit_plus_one() {
    struct Counter<'a>(&'a Cell<u64>);
    impl Read for Counter<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            // Finite input keeps the unbounded-reader negative control safe.
            let remaining = MCP_FILE_IMAGE_MAX_BYTES + 65_536 - self.0.get();
            let n = buf.len().min(remaining as usize);
            buf[..n].fill(0);
            self.0.set(self.0.get() + n as u64);
            Ok(n)
        }
    }
    let consumed = Cell::new(0);
    assert!(read_limited_image(Counter(&consumed)).is_err());
    assert_eq!(consumed.get(), MCP_FILE_IMAGE_MAX_BYTES + 1);
}
