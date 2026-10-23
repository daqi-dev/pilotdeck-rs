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

// #594: the standalone skill loader reads its single SKILL.md through the same
// checked handle. Expected values come from `standalone-toctou.json` (real TS
// `loadStandaloneSkill`); a standalone skill is one file, so a refused read
// fails the whole skill rather than being skipped past.
fn standalone_fixture() -> Value {
    let text = include_str!("../../../../../tests/fixtures/plugin-loader/standalone-toctou.json");
    crate::test_baseline::oracle(text);
    serde_json::from_str(text).unwrap()
}

fn standalone_case(id: &str) -> Value {
    fixture_case(
        include_str!("../../../../../tests/fixtures/plugin-loader/standalone-toctou.json"),
        id,
    )
}

/// Same files as `generate-standalone-toctou.mts` `layout(id)`; returns the
/// standalone skill directory (its basename is the model-facing name).
fn standalone_layout(id: &str) -> (Scratch, PathBuf) {
    // Short prefix: the socket case binds directly under the scratch root.
    let scratch = Scratch::new("594");
    let skill_dir = scratch.path().join(id).join("skill");
    fs::create_dir_all(skill_dir.join("references")).unwrap();
    fs::create_dir_all(skill_dir.parent().unwrap().join("outside")).unwrap();
    fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: docx\ndescription: edit docs\n---\n\n# DOCX skill\n",
    )
    .unwrap();
    fs::write(skill_dir.join("references/workflows.md"), "# Workflows\n").unwrap();
    fs::write(
        skill_dir.join("real.md"),
        "---\nname: real\n---\n\n# In-root target\n",
    )
    .unwrap();
    fs::write(
        skill_dir.parent().unwrap().join("outside/secret.md"),
        "SECRET",
    )
    .unwrap();
    (scratch, skill_dir)
}

/// Loads the standalone skill with `swap(skill_dir)` at the selected window;
/// panics unless the hook fired on SKILL.md itself.
fn raced_standalone(
    skill_dir: &Path,
    swap: impl FnOnce(&Path) + 'static,
    before_lstat: bool,
) -> Result<LoadedPluginCommand, PluginRuntimeError> {
    let target = skill_dir.join("SKILL.md").canonicalize().unwrap();
    let fired = Rc::new(RefCell::new(None::<PathBuf>));
    let seen = Rc::clone(&fired);
    let swap_dir = skill_dir.to_path_buf();
    let hook: Box<dyn FnOnce(&Path)> = Box::new(move |canonical: &Path| {
        *seen.borrow_mut() = Some(canonical.to_path_buf());
        swap(&swap_dir);
    });
    let window = if before_lstat {
        &BEFORE_LSTAT
    } else {
        &BEFORE_OPEN
    };
    window.with(|h| *h.borrow_mut() = Some((target.clone(), hook)));
    let result = load_standalone_skill("skill", skill_dir);
    window.with(|h| h.borrow_mut().take());
    assert_eq!(
        fired.borrow().as_deref(),
        Some(target.as_path()),
        "race must fire on SKILL.md itself"
    );
    result
}

/// A statically out-of-root symlink leaf: its realpath escapes the skill root,
/// so the checked open refuses it. TS follows the link and reads the out-of-root
/// secret (`standalone-toctou.json` leaf-symlink).
#[test]
fn standalone_static_leaf_symlink_outside_is_refused() {
    let case = standalone_case("leaf-symlink");
    assert_eq!(
        case["observed"]["skill"]["content"], "SECRET",
        "TS reads out-of-root content"
    );
    let (_scratch, skill_dir) = standalone_layout("leaf-symlink");
    let leaf = skill_dir.join("SKILL.md");
    fs::remove_file(&leaf).unwrap();
    symlink("../outside/secret.md", &leaf).unwrap();
    assert!(
        load_standalone_skill("skill", &skill_dir).is_err(),
        "out-of-root leaf symlink must be refused, not read"
    );
}

/// An in-root symlink leaf: its realpath stays inside the skill root, so the
/// hardened read follows it exactly like TS (`standalone-toctou.json`
/// in-root-symlink). Guards against the fix over-refusing ordinary in-root links.
#[test]
fn standalone_static_in_root_symlink_is_read() {
    let case = standalone_case("in-root-symlink");
    let expected = case["observed"]["skill"]["content"].as_str().unwrap();
    let (_scratch, skill_dir) = standalone_layout("in-root-symlink");
    let leaf = skill_dir.join("SKILL.md");
    fs::remove_file(&leaf).unwrap();
    symlink("./real.md", &leaf).unwrap();
    let loaded = load_standalone_skill("skill", &skill_dir).expect("in-root link reads like TS");
    assert_eq!(
        loaded.content, expected,
        "in-root symlink content matches TS"
    );
}

/// A FIFO SKILL.md is not a regular file and is refused before any open; the
/// whole skill fails without blocking. TS `readFile` blocks instead
/// (`standalone-toctou.json` static-fifo).
#[test]
fn standalone_static_fifo_is_refused_without_blocking() {
    let case = standalone_case("static-fifo");
    assert_eq!(
        case["observed"]["timedOut"], true,
        "TS readFile blocks on a FIFO"
    );
    let (_scratch, skill_dir) = standalone_layout("static-fifo");
    let leaf = skill_dir.join("SKILL.md");
    fs::remove_file(&leaf).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&leaf)
            .status()
            .unwrap()
            .success()
    );
    assert!(fs::symlink_metadata(&leaf).unwrap().file_type().is_fifo());
    let error = load_standalone_skill("skill", &skill_dir)
        .err()
        .expect("FIFO skill must be refused without blocking");
    assert!(
        error.to_string().contains("EACCES"),
        "FIFO refusal must report EACCES: {error}"
    );
}

/// A socket SKILL.md is refused before any open. TS `readFile`'s open fails and
/// rejects the load too (`standalone-toctou.json` static-socket).
#[test]
fn standalone_static_socket_is_refused() {
    let case = standalone_case("static-socket");
    assert_eq!(
        case["observed"]["rejected"], true,
        "TS readFile rejects a socket"
    );
    let (scratch, skill_dir) = standalone_layout("static-socket");
    let leaf = skill_dir.join("SKILL.md");
    fs::remove_file(&leaf).unwrap();
    // Bind on a short path (sun_path limit), then move the socket into place.
    let bound = scratch.path().join("s");
    let _listener = std::os::unix::net::UnixListener::bind(&bound).unwrap();
    fs::rename(&bound, &leaf).unwrap();
    assert!(fs::symlink_metadata(&leaf).unwrap().file_type().is_socket());
    let error = load_standalone_skill("skill", &skill_dir)
        .err()
        .expect("socket skill must be refused");
    assert!(
        error.to_string().contains("EACCES"),
        "socket refusal must report EACCES: {error}"
    );
}

/// A leaf swapped to an out-of-root symlink after the boundary check: only the
/// leaf's NOFOLLOW keeps the read inside the skill root. The unraced load first
/// replays the genuine SKILL.md content.
#[test]
fn standalone_leaf_symlink_raced_after_check_is_refused() {
    let case = standalone_case("leaf-symlink");
    assert_eq!(
        case["observed"]["skill"]["content"], "SECRET",
        "TS via link"
    );
    let checked = standalone_fixture()["checked"]["content"]
        .as_str()
        .unwrap()
        .to_owned();
    let (_scratch, skill_dir) = standalone_layout("leaf-symlink");
    assert_eq!(
        load_standalone_skill("skill", &skill_dir).unwrap().content,
        checked,
        "unraced load replays TS checked content"
    );
    let result = raced_standalone(
        &skill_dir,
        |dir| {
            let leaf = dir.join("SKILL.md");
            fs::remove_file(&leaf).unwrap();
            symlink("../outside/secret.md", &leaf).unwrap();
        },
        false,
    );
    assert!(
        result.is_err(),
        "leaf symlink raced after check must be refused"
    );
}

/// A leaf swapped to a different inode (hardlink to an out-of-root file) after
/// the check: only the post-open (dev, ino) recheck rejects it.
#[test]
fn standalone_leaf_hardlink_raced_after_check_is_refused() {
    let (_scratch, skill_dir) = standalone_layout("leaf-hardlink");
    let result = raced_standalone(
        &skill_dir,
        |dir| {
            let leaf = dir.join("SKILL.md");
            let outside = dir.parent().unwrap().join("outside/secret.md");
            fs::remove_file(&leaf).unwrap();
            fs::hard_link(&outside, &leaf).unwrap();
            // Leave a single name so the independent nlink guard cannot mask
            // a broken checked-identity comparison.
            fs::remove_file(&outside).unwrap();
        },
        false,
    );
    assert!(
        result.is_err(),
        "hardlinked different inode must be refused by identity recheck"
    );
}

/// Deleting SKILL.md after the check surfaces the real ENOENT, matching TS
/// `readFile` rejecting the whole skill (`standalone-toctou.json`
/// deleted-after-check), rather than a generic refusal.
#[test]
fn standalone_deleted_after_check_rejects_like_ts() {
    let case = standalone_case("deleted-after-check");
    assert_eq!(
        case["observed"]["rejected"], true,
        "TS readFile rejects after deletion"
    );
    let code = case["observed"]["code"].as_str().unwrap();
    let (_scratch, skill_dir) = standalone_layout("deleted-after-check");
    let result = raced_standalone(
        &skill_dir,
        |dir| {
            fs::remove_file(dir.join("SKILL.md")).unwrap();
        },
        true,
    );
    assert!(result.is_err(), "deletion after check must reject");
    let err = result.err().unwrap().to_string();
    assert!(err.contains(code), "error matches TS code {code}: {err}");
}

#[test]
fn standalone_dangling_symlink_preserves_enoent() {
    let (_scratch, dir) = standalone_layout("dangling");
    let leaf = dir.join("SKILL.md");
    fs::remove_file(&leaf).unwrap();
    symlink("missing.md", &leaf).unwrap();
    let expected = super::super::io_error("open", &leaf, &fs::read(&leaf).err().unwrap());
    let error = load_standalone_skill("skill", &dir).err().unwrap();
    assert_eq!(
        error.to_string(),
        expected.to_string(),
        "dangling link keeps ENOENT and lexical path"
    );
}

#[test]
fn standalone_resolution_errors_keep_non_refusal_codes() {
    let file = Path::new("skill/SKILL.md");
    for (input, output) in [
        (libc::ENOENT, libc::ENOENT),
        (libc::ENOTDIR, libc::ENOTDIR),
        (libc::EIO, libc::EIO),
        (libc::ELOOP, libc::EACCES),
        (libc::EACCES, libc::EACCES),
        (libc::EXDEV, libc::EACCES),
    ] {
        let expected =
            super::super::io_error("open", file, &std::io::Error::from_raw_os_error(output));
        assert_eq!(
            skill_io_error(file, std::io::Error::from_raw_os_error(input)).to_string(),
            expected.to_string(),
            "resolution errno {input} classification"
        );
    }
}

#[test]
fn standalone_self_symlink_is_refused() {
    let (_scratch, dir) = standalone_layout("self-link");
    let leaf = dir.join("SKILL.md");
    fs::remove_file(&leaf).unwrap();
    symlink("SKILL.md", &leaf).unwrap();
    assert_eq!(
        fs::canonicalize(&leaf).err().unwrap().raw_os_error(),
        Some(libc::ELOOP)
    );
    let expected = super::super::io_error(
        "open",
        &leaf,
        &std::io::Error::from_raw_os_error(libc::EACCES),
    );
    assert_eq!(
        load_standalone_skill("skill", &dir)
            .err()
            .unwrap()
            .to_string(),
        expected.to_string(),
        "ELOOP is a refused resolution"
    );
}

#[test]
fn standalone_unresolvable_root_preserves_enoent() {
    let (_scratch, dir) = standalone_layout("missing-root");
    let leaf = dir.join("SKILL.md");
    fs::remove_dir_all(&dir).unwrap();
    let expected = super::super::io_error(
        "open",
        &leaf,
        &std::io::Error::from_raw_os_error(libc::ENOENT),
    );
    assert_eq!(
        read_checked_skill(&dir, &leaf).err().unwrap().to_string(),
        expected.to_string(),
        "missing root keeps ENOENT"
    );
}

#[test]
fn standalone_in_root_symlink_read_failure_keeps_lexical_path() {
    let (_scratch, dir) = standalone_layout("link-deleted");
    let leaf = dir.join("SKILL.md");
    fs::remove_file(&leaf).unwrap();
    symlink("real.md", &leaf).unwrap();
    for before_lstat in [true, false] {
        fs::write(dir.join("real.md"), "safe").unwrap();
        let error = raced_standalone(
            &dir,
            |dir| fs::remove_file(dir.join("real.md")).unwrap(),
            before_lstat,
        )
        .err()
        .unwrap();
        let expected = super::super::io_error(
            "open",
            &leaf,
            &std::io::Error::from_raw_os_error(libc::ENOENT),
        );
        assert_eq!(
            error.to_string(),
            expected.to_string(),
            "in-root link error uses lexical SKILL.md"
        );
    }
}

#[test]
fn standalone_static_outside_hardlink_is_refused() {
    use std::os::unix::fs::MetadataExt;
    assert_eq!(
        standalone_case("leaf-hardlink")["observed"]["skill"]["content"],
        "SECRET",
        "TS reads the static external hardlink"
    );
    let (_scratch, dir) = standalone_layout("static-hardlink");
    let leaf = dir.join("SKILL.md");
    let outside = dir.parent().unwrap().join("outside/secret.md");
    fs::remove_file(&leaf).unwrap();
    fs::hard_link(&outside, &leaf).unwrap();
    let stat = fs::metadata(&outside).unwrap();
    let entry = fs::read_dir(&dir)
        .unwrap()
        .map(Result::unwrap)
        .find(|e| e.path() == leaf)
        .unwrap()
        .metadata()
        .unwrap();
    assert_eq!(
        (entry.dev(), entry.ino()),
        (stat.dev(), stat.ino()),
        "external inode is already in root dirents"
    );
    assert_eq!(entry.nlink(), 2);
    let expected = super::super::io_error(
        "open",
        &leaf,
        &std::io::Error::from_raw_os_error(libc::EACCES),
    );
    assert_eq!(
        load_standalone_skill("skill", &dir)
            .err()
            .unwrap()
            .to_string(),
        expected.to_string(),
        "static outside hardlink must be refused before reading SECRET"
    );
}

#[test]
fn standalone_in_root_hardlink_is_conservatively_refused() {
    let (_scratch, dir) = standalone_layout("inside-hardlink");
    fs::hard_link(dir.join("SKILL.md"), dir.join("alias.md")).unwrap();
    assert!(
        load_standalone_skill("skill", &dir).is_err(),
        "all multiply linked skills are refused regardless of alias location"
    );
    fs::remove_file(dir.join("alias.md")).unwrap();
    assert!(
        load_standalone_skill("skill", &dir).is_ok(),
        "single-name regular file remains readable"
    );
}

#[test]
fn standalone_hardlink_added_after_check_is_refused() {
    let (_scratch, dir) = standalone_layout("added-hardlink");
    let error = raced_standalone(
        &dir,
        |dir| {
            fs::hard_link(
                dir.join("SKILL.md"),
                dir.parent().unwrap().join("outside/alias.md"),
            )
            .unwrap();
        },
        false,
    )
    .err()
    .unwrap();
    assert!(
        error.to_string().contains("EACCES"),
        "post-open nlink catches unchanged inode with a newly added alias"
    );
}
