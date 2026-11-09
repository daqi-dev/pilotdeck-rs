//! Typed internal attachment cache security boundary.
use super::services::{
    compose_attachment_turn, compose_attachment_turn_with_cache, swap_test_hook,
};
use crate::metadata::object;
use pilotdeck_protocol::{JsValue, stringify_json};
use std::path::{Path, PathBuf};
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        let p = std::env::temp_dir().join(format!("pdrs-channel-cache-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(p.join("project")).unwrap();
        std::fs::create_dir_all(p.join("home/im-attachments/weixin/chat/message")).unwrap();
        Self(p.canonicalize().unwrap())
    }
    fn project(&self) -> PathBuf {
        self.0.join("project")
    }
    fn base(&self) -> PathBuf {
        self.0.join("home")
    }
    fn root(&self) -> PathBuf {
        self.base().join("im-attachments/weixin")
    }
    fn leaf(&self) -> PathBuf {
        self.root().join("chat/message/input.txt")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn attachment(path: &Path) -> JsValue {
    object([
        ("path", JsValue::from(path.to_str().unwrap())),
        ("mimeType", JsValue::from("text/plain")),
        (
            "metadata",
            object([("channelKey", JsValue::from("weixin"))]),
        ),
    ])
}
fn array(v: &JsValue) -> Option<&[JsValue]> {
    match v {
        JsValue::Array(rows) => Some(rows),
        _ => None,
    }
}
fn rendered(v: &JsValue) -> String {
    stringify_json(v).unwrap().unwrap()
}
fn denied(v: &JsValue, secret: &str) {
    let text = rendered(v);
    assert!(!text.contains(secret), "untrusted content: {text}");
    assert!(
        v.get("allowedReadFiles")
            .and_then(array)
            .is_none_or(|v| v.is_empty()),
        "unexpected permission: {text}"
    );
}
#[test]
fn plain_ws_forged_channel_metadata_cannot_read_typed_cache() {
    let f = Fixture::new();
    let path = f.leaf();
    std::fs::write(&path, "PRIVATE-CACHE-BYTES").unwrap();
    let message = JsValue::from("read attached");
    let ws = compose_attachment_turn(&f.project(), &message, &[attachment(&path)]).unwrap();
    denied(&ws, "PRIVATE-CACHE-BYTES");
    let typed = compose_attachment_turn_with_cache(
        &f.project(),
        &message,
        &[attachment(&path)],
        Some((&f.base(), &f.root())),
    )
    .unwrap();
    assert!(rendered(&typed).contains("PRIVATE-CACHE-BYTES"));
    assert!(
        typed
            .get("allowedReadFiles")
            .and_then(array)
            .unwrap()
            .iter()
            .any(|v| v
                .as_string()
                .is_some_and(|s| s.to_utf8().ok().as_deref() == path.to_str()))
    );
}
#[test]
fn typed_cache_rejects_other_channel_despite_forged_metadata() {
    let f = Fixture::new();
    let path = f.base().join("im-attachments/feishu/chat/input.txt");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "OTHER-CHANNEL-SECRET").unwrap();
    let r = compose_attachment_turn_with_cache(
        &f.project(),
        &JsValue::from("read"),
        &[attachment(&path)],
        Some((&f.base(), &f.root())),
    )
    .unwrap();
    denied(&r, "OTHER-CHANNEL-SECRET");
}
#[cfg(unix)]
#[test]
fn typed_cache_rejects_root_and_ancestor_symlink_escape() {
    use std::os::unix::fs::symlink;
    for ancestor in [false, true] {
        let f = Fixture::new();
        let outside = f.0.join("outside");
        std::fs::create_dir_all(outside.join("chat/message")).unwrap();
        std::fs::write(
            outside.join("chat/message/input.txt"),
            "SYMLINK-ESCAPE-SECRET",
        )
        .unwrap();
        if ancestor {
            std::fs::remove_dir_all(f.base().join("im-attachments")).unwrap();
            std::fs::create_dir_all(outside.join("weixin/chat/message")).unwrap();
            std::fs::write(
                outside.join("weixin/chat/message/input.txt"),
                "SYMLINK-ESCAPE-SECRET",
            )
            .unwrap();
            symlink(&outside, f.base().join("im-attachments")).unwrap();
        } else {
            std::fs::remove_dir_all(f.root()).unwrap();
            symlink(&outside, f.root()).unwrap();
        }
        let r = compose_attachment_turn_with_cache(
            &f.project(),
            &JsValue::from("read"),
            &[attachment(&f.leaf())],
            Some((&f.base(), &f.root())),
        )
        .unwrap();
        denied(&r, "SYMLINK-ESCAPE-SECRET");
    }
}
#[cfg(unix)]
#[test]
fn typed_cache_keeps_fd_identity_guard_during_open_path_swap() {
    use std::os::unix::fs::symlink;
    use std::{cell::Cell, rc::Rc};
    let f = Fixture::new();
    let leaf = f.leaf();
    std::fs::write(&leaf, "APPROVED-CACHE-BYTES").unwrap();
    let outside = f.0.join("outside.txt");
    std::fs::write(&outside, "OUTSIDE-HANDLE-SECRET").unwrap();
    let count = Rc::new(Cell::new(0));
    let before = swap_test_hook::set(swap_test_hook::SwapWindow::BeforeOpen, {
        let leaf = leaf.clone();
        let outside = outside.clone();
        let count = count.clone();
        move |path| {
            assert_eq!(path, leaf);
            std::fs::remove_file(&leaf).unwrap();
            symlink(&outside, &leaf).unwrap();
            count.set(count.get() + 1);
        }
    });
    let after = swap_test_hook::set(swap_test_hook::SwapWindow::AfterOpen, {
        let leaf = leaf.clone();
        let count = count.clone();
        move |path| {
            assert_eq!(path, leaf);
            std::fs::rename(&outside, &leaf).unwrap();
            count.set(count.get() + 1);
        }
    });
    let r = compose_attachment_turn_with_cache(
        &f.project(),
        &JsValue::from("read"),
        &[attachment(&leaf)],
        Some((&f.base(), &f.root())),
    )
    .unwrap();
    drop(before);
    drop(after);
    assert_eq!(count.get(), 2, "both production hooks must execute");
    denied(&r, "OUTSIDE-HANDLE-SECRET");
    assert!(!rendered(&r).contains("APPROVED-CACHE-BYTES"));
}
