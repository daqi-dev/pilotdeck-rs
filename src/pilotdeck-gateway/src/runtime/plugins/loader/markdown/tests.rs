//! #564 / ROOT-001: checked Markdown read under deterministic races.
//!
//! Every race fires on `sub/b.md` itself (asserted): after its traversal
//! containment check, either before lstat or before the checked open. Layouts and
//! swaps mirror `generate-toctou.mts` and `generate-pre-lstat.mts`; expected
//! values come from their real TS observations (`checked` unraced, `control`
//! with the raced entry removed). Unix only: symlinks and the openat path.
use super::*;
use crate::test_scratch::Scratch;
use serde_json::{Value, json};
use std::{
    cell::RefCell,
    fs,
    os::unix::fs::{FileTypeExt, symlink},
    rc::Rc,
};

fn case(id: &str) -> Value {
    fixture_case(
        include_str!("../../../../../tests/fixtures/plugin-loader/toctou.json"),
        id,
    )
}

fn pre_lstat_case(id: &str) -> Value {
    fixture_case(
        include_str!("../../../../../tests/fixtures/plugin-loader/pre-lstat.json"),
        id,
    )
}

fn fixture_case(text: &str, id: &str) -> Value {
    crate::test_baseline::oracle(text);
    let fixture: Value = serde_json::from_str(text).unwrap();
    fixture["cases"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["id"] == id)
        .unwrap_or_else(|| panic!("fixture has no case {id}"))
        .clone()
}

fn project(commands: &[LoadedPluginCommand], root: &Path) -> Value {
    let root = root.to_str().unwrap();
    json!(commands.iter().map(|c| json!({"name": c.name, "path": c.path.to_string_lossy().replace(root, "<root>"), "content": c.content, "frontmatter": c.frontmatter, "isSkill": c.is_skill})).collect::<Vec<_>>())
}

/// Same files as the generator's `layout(id)`.
fn layout(id: &str) -> (Scratch, PathBuf, PathBuf) {
    // Short prefix: the socket case binds directly under the scratch root.
    let scratch = Scratch::new("564");
    let dir = scratch.path().join(id);
    let base = dir.join("p/commands");
    fs::create_dir_all(base.join("sub")).unwrap();
    fs::create_dir_all(dir.join("outside/sub")).unwrap();
    fs::write(base.join("a.md"), "safe").unwrap();
    fs::write(base.join("sub/b.md"), "---\nkind: inside\n---\ninside").unwrap();
    fs::write(dir.join("outside/secret.md"), "SECRET").unwrap();
    fs::write(dir.join("outside/sub/b.md"), "SECRET").unwrap();
    (scratch, dir, base)
}

/// Loads with `swap(dir, base)` at the selected pre-lstat / pre-open window and
/// returns the raced result; panics unless the hook fired on that file.
fn raced_load(
    id: &str,
    dir: &Path,
    base: &Path,
    swap: impl FnOnce(&Path, &Path) + 'static,
    before_lstat: bool,
) -> Result<Vec<LoadedPluginCommand>, PluginRuntimeError> {
    raced_load_with(id, dir, base, swap, before_lstat, || {
        load_plugin_commands("p", base)
    })
}

fn raced_load_with<T>(
    id: &str,
    dir: &Path,
    base: &Path,
    swap: impl FnOnce(&Path, &Path) + 'static,
    before_lstat: bool,
    load: impl FnOnce() -> T,
) -> T {
    let target = base.join("sub/b.md").canonicalize().unwrap();
    let fired = Rc::new(RefCell::new(None::<PathBuf>));
    let seen = Rc::clone(&fired);
    let (dir, swap_base) = (dir.to_path_buf(), base.to_path_buf());
    let hook: Box<dyn FnOnce(&Path)> = Box::new(move |canonical: &Path| {
        *seen.borrow_mut() = Some(canonical.to_path_buf());
        swap(&dir, &swap_base);
    });
    let window = if before_lstat {
        &BEFORE_LSTAT
    } else {
        &BEFORE_OPEN
    };
    window.with(|h| *h.borrow_mut() = Some((target.clone(), hook)));
    let result = load();
    window.with(|h| h.borrow_mut().take());
    assert_eq!(
        fired.borrow().as_deref(),
        Some(target.as_path()),
        "{id}: race must fire on sub/b.md itself"
    );
    result
}
/// The unraced layout replays TS `checked`; the raced load equals TS `control`
/// (raced entry removed): skipped, never read, and never a load error.
fn race(id: &str, swap: impl FnOnce(&Path, &Path) + 'static) {
    race_at(id, swap, false);
}

fn race_at(id: &str, swap: impl FnOnce(&Path, &Path) + 'static, before_lstat: bool) {
    let case = if before_lstat {
        pre_lstat_case(id)
    } else {
        case(id)
    };
    let (scratch, dir, base) = layout(id);
    let unraced = load_plugin_commands("p", &base).unwrap();
    assert_eq!(
        project(&unraced, scratch.path()),
        case["checked"],
        "{id}: unraced layout replays TS checked"
    );
    let raced = raced_load(id, &dir, &base, swap, before_lstat)
        .unwrap_or_else(|e| panic!("{id}: raced entry must be skipped, not fail: {e}"));
    assert_eq!(
        project(&raced, scratch.path()),
        case["control"],
        "{id}: raced load keeps only the TS control"
    );
}

/// Link to another (outside) file: NOFOLLOW and (dev, ino) both reject it.
#[test]
fn leaf_symlink_raced_after_check_is_skipped() {
    race("leaf-symlink", |_dir, base| {
        let leaf = base.join("sub/b.md");
        fs::remove_file(&leaf).unwrap();
        symlink("../../../outside/secret.md", &leaf).unwrap();
    });
}

#[test]
fn ancestor_symlink_raced_after_check_is_skipped() {
    race("ancestor-symlink", |dir, base| {
        fs::rename(base.join("sub"), dir.join("saved-sub")).unwrap();
        symlink("../../outside/sub", base.join("sub")).unwrap();
    });
}

/// The checked inode itself, moved outside the root behind a link: the
/// identity matches, so only the leaf's NOFOLLOW keeps the read in the root.
#[test]
fn leaf_moved_outside_behind_symlink_is_skipped() {
    race("leaf-moved-symlink", |dir, base| {
        let leaf = base.join("sub/b.md");
        fs::rename(&leaf, dir.join("outside/moved.md")).unwrap();
        symlink("../../../outside/moved.md", &leaf).unwrap();
    });
}

/// Same, for the parent directory: only the ancestors' NOFOLLOW rejects it.
#[test]
fn ancestor_moved_outside_behind_symlink_is_skipped() {
    race("ancestor-moved-symlink", |dir, base| {
        fs::rename(base.join("sub"), dir.join("saved-sub")).unwrap();
        symlink("../../saved-sub", base.join("sub")).unwrap();
    });
}

/// No link anywhere: another inode now carries the checked name, so only the
/// post-open (dev, ino) recheck rejects it.
#[test]
fn leaf_replaced_by_other_inode_is_skipped() {
    race("leaf-hardlink", |dir, base| {
        let leaf = base.join("sub/b.md");
        fs::remove_file(&leaf).unwrap();
        fs::hard_link(dir.join("outside/secret.md"), &leaf).unwrap();
    });
}

/// A socket `.md` is not a regular file and is skipped before any open; the
/// open would fail (EOPNOTSUPP/ENXIO). TS `readFile` rejects the whole load
/// instead (`toctou.json` static-socket); the retained set is the TS control.
#[test]
fn socket_markdown_is_skipped() {
    let case = case("static-socket");
    assert_eq!(case["socket"]["rejected"], true, "TS readFile rejects");
    let (scratch, _dir, base) = layout("static-socket");
    let leaf = base.join("sub/b.md");
    fs::remove_file(&leaf).unwrap();
    // Bind on a short path (sun_path limit), then move the socket into place.
    let bound = scratch.path().join("s");
    let _listener = std::os::unix::net::UnixListener::bind(&bound).unwrap();
    fs::rename(&bound, &leaf).unwrap();
    assert!(fs::symlink_metadata(&leaf).unwrap().file_type().is_socket());
    let result = load_plugin_commands("p", &base)
        .unwrap_or_else(|e| panic!("socket must be skipped, not fail: {e}"));
    assert_eq!(
        project(&result, scratch.path()),
        case["control"],
        "socket skipped like the TS control"
    );
}

/// A replacement before lstat matches lstat's identity, but not the original
/// traversal identity. Replay the same real TS hardlink control as the later race.
#[test]
fn hardlink_with_different_checked_identity_is_skipped() {
    race_at(
        "leaf-hardlink",
        |dir, base| {
            let leaf = base.join("sub/b.md");
            fs::remove_file(&leaf).unwrap();
            fs::hard_link(dir.join("outside/secret.md"), &leaf).unwrap();
        },
        true,
    );
}

/// TS readFile rejects after collection if the checked file was deleted;
/// swallowing this at lstat would keep the safe sibling instead of failing.
#[test]
fn deleted_after_check_propagates_error() {
    let case = pre_lstat_case("deleted-after-check");
    assert_eq!(case["deleted"]["rejected"], true, "TS readFile rejects");
    let (scratch, dir, base) = layout("deleted-after-check");
    assert_eq!(
        project(&load_plugin_commands("p", &base).unwrap(), scratch.path()),
        case["checked"]
    );
    let result = raced_load(
        "deleted-after-check",
        &dir,
        &base,
        |_dir, base| {
            fs::remove_file(base.join("sub/b.md")).unwrap();
        },
        true,
    );
    assert!(result.is_err(), "deletion after check must propagate error");
    let err = result.err().unwrap().to_string();
    assert!(
        err.contains(case["deleted"]["code"].as_str().unwrap()),
        "error matches TS: {err}"
    );
}

fn replace_ancestor_with_file(dir: &Path, base: &Path) {
    fs::rename(base.join("sub"), dir.join("saved-sub")).unwrap();
    fs::write(base.join("sub"), "ordinary file").unwrap();
    assert!(fs::symlink_metadata(base.join("sub")).unwrap().is_file());
}

fn ancestor_file_error(before_lstat: bool) {
    let case = pre_lstat_case("ancestor-file-after-check");
    assert_eq!(case["replaced"]["rejected"], true, "TS readFile rejects");
    let (scratch, dir, base) = layout("ancestor-file-after-check");
    assert_eq!(
        project(&load_plugin_commands("p", &base).unwrap(), scratch.path()),
        case["checked"]
    );
    let result = raced_load(
        "ancestor-file-after-check",
        &dir,
        &base,
        replace_ancestor_with_file,
        before_lstat,
    );
    assert!(
        result.is_err(),
        "ordinary-file ancestor must propagate ENOTDIR"
    );
    let err = result.err().unwrap().to_string();
    assert!(
        err.contains(case["replaced"]["code"].as_str().unwrap()),
        "error matches TS: {err}"
    );
}

/// Covers the ENOTDIR from openat(O_DIRECTORY), after the pre-open lstat.
#[test]
fn ancestor_file_after_lstat_propagates_error() {
    ancestor_file_error(false);
}

/// The earlier window produces ENOTDIR at lstat and also must propagate it.
#[test]
fn ancestor_file_before_lstat_propagates_error() {
    ancestor_file_error(true);
}

/// Replay the full TS PluginLoader boundary: lose a.md with its failed dir,
/// retain another configured commands dir and the other contribution families.
#[test]
fn ancestor_file_rejects_only_its_configured_directory() {
    use super::super::{LoadedPlugin, PluginSource, load_plugin_from_path};
    let case = pre_lstat_case("ancestor-file-after-check");
    let (scratch, dir, base) = layout("ancestor-file-after-check");
    let plugin_path = dir.join("p");
    fs::write(
        plugin_path.join("plugin.json"),
        r#"{"name":"p","version":"1.0.0","commands":["commands","other-commands"]}"#,
    )
    .unwrap();
    for (directory, file, content) in [
        ("other-commands", "c.md", "other"),
        ("skills/s", "SKILL.md", "skill"),
        ("output-styles", "style.md", "style"),
    ] {
        fs::create_dir_all(plugin_path.join(directory)).unwrap();
        fs::write(plugin_path.join(directory).join(file), content).unwrap();
    }
    let project_plugin = |plugin: &LoadedPlugin| {
        json!({
            "commands": project(&plugin.commands, scratch.path()),
            "skills": project(&plugin.skills, scratch.path()),
            "outputStyles": project(&plugin.output_styles, scratch.path()),
        })
    };
    let load = || load_plugin_from_path(&plugin_path, PluginSource::Global).unwrap();
    assert_eq!(project_plugin(&load()), case["configuredChecked"]);
    let raced = raced_load_with(
        "ancestor-file-after-check",
        &dir,
        &base,
        replace_ancestor_with_file,
        false,
        load,
    );
    assert_eq!(
        project_plugin(&raced),
        case["configuredRaced"],
        "ordinary-file ancestor rejects exactly its configured directory like TS"
    );
}
