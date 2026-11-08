use super::*;
use serde_json::{Value, json};
use std::{cell::RefCell, rc::Rc};

struct FakeBackend {
    options: Value,
    calls: Rc<RefCell<Vec<Value>>>,
}
impl Backend for FakeBackend {
    fn check(&mut self) -> Result<Status, Error> {
        self.calls.borrow_mut().push(json!("check"));
        let reason = self
            .options
            .pointer("/status/reason")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Ok(Status {
            can_update: self
                .options
                .pointer("/status/canUpdate")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            reason,
            latest: Some(Release {
                tag_name: "v2026.09.07".into(),
                source_sha: "a".repeat(40),
            }),
        })
    }
    fn apply(&mut self, target: &Release, output: &mut dyn FnMut(&str)) -> Result<(), Error> {
        self.calls
            .borrow_mut()
            .push(json!(["apply",{"tagName":target.tag_name,"sourceSha":target.source_sha}]));
        if let Some(e) = self.options.get("applyError") {
            return Err(Error::new(
                e["reason"].as_str().unwrap(),
                e["message"].as_str().unwrap(),
            ));
        }
        output("Prepared test build.");
        Ok(())
    }
    fn identity(&self) -> (&str, &str) {
        ("test-install", "old-generation")
    }
    fn target_generation(&self) -> Option<&str> {
        Some("new-generation")
    }
}
struct FakeRuntime {
    options: Value,
    calls: Rc<RefCell<Vec<Value>>>,
}
impl RuntimeClient for FakeRuntime {
    fn available(&mut self, _id: &str, _generation: &str) -> Result<bool, Error> {
        Ok(!self.options["noRestart"].as_bool().unwrap_or(false))
    }
    fn restart(&mut self, request: &RestartRequest) -> Result<RestartReply, Error> {
        self.calls.borrow_mut().push(json!("restart"));
        assert_eq!(request.install_id, "test-install");
        assert_eq!(request.expected_generation, "old-generation");
        assert_eq!(request.target_generation, "new-generation");
        if let Some(e) = self.options["restartError"].as_str() {
            return Err(Error::new("test", e));
        }
        Ok(RestartReply {
            status: "accepted".into(),
            restart_id: "isolated-id".into(),
            target_generation: request.target_generation.clone(),
        })
    }
}
#[test]
fn command_policy_replays_twenty_two_fixed_ts_cases() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../tests/fixtures/update/oracle.json")).unwrap();
    assert_eq!(
        fixture["baseline"],
        "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"
    );
    assert_eq!(fixture["node"], "v22.22.0");
    let mut tested = 0;
    for row in fixture["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r.get("options").is_some())
    {
        let options = row["options"].clone();
        let calls = Rc::new(RefCell::new(Vec::new()));
        let mut backend = FakeBackend {
            options: options.clone(),
            calls: calls.clone(),
        };
        let mut runtime = FakeRuntime {
            options: options.clone(),
            calls: calls.clone(),
        };
        let mut stdout = String::new();
        let mut output = |s: &str| {
            stdout.push_str(s);
            stdout.push('\n');
        };
        let result = execute(
            Flags {
                check_only: options["checkOnly"].as_bool().unwrap_or(false),
                restart: options["restart"].as_bool().unwrap_or(false),
            },
            &mut backend,
            Some(&mut runtime),
            &mut output,
        );
        let (code, stderr) = match result {
            Ok(c) => (c, String::new()),
            Err(e) => (1, format!("{}\n", e.message)),
        };
        assert_eq!(
            code as u64,
            row["code"].as_u64().unwrap(),
            "{}",
            row["name"]
        );
        assert_eq!(stdout, row["stdout"].as_str().unwrap(), "{}", row["name"]);
        assert_eq!(stderr, row["stderr"].as_str().unwrap(), "{}", row["name"]);
        assert_eq!(
            *calls.borrow(),
            row["calls"].as_array().unwrap().clone(),
            "{}",
            row["name"]
        );
        tested += 1;
    }
    assert_eq!(tested, 22);
}
#[test]
fn parser_and_restart_identity_are_strict() {
    assert!(Flags::parse(&["--force".into()]).is_err());
    assert!(Flags::parse(&["--check=x".into()]).is_err());
    let flags = Flags::parse(&["--check".into(), "--restart".into(), "--check".into()]).unwrap();
    assert!(flags.check_only && flags.restart);
    struct WrongRuntime;
    impl RuntimeClient for WrongRuntime {
        fn available(&mut self, _i: &str, _g: &str) -> Result<bool, Error> {
            Ok(true)
        }
        fn restart(&mut self, _r: &RestartRequest) -> Result<RestartReply, Error> {
            Ok(RestartReply {
                status: "accepted".into(),
                restart_id: "id".into(),
                target_generation: "another".into(),
            })
        }
    }
    let calls = Rc::new(RefCell::new(Vec::new()));
    let mut backend = FakeBackend {
        options: json!({}),
        calls,
    };
    let error = execute(
        Flags {
            check_only: false,
            restart: true,
        },
        &mut backend,
        Some(&mut WrongRuntime),
        &mut |_| {},
    )
    .unwrap_err();
    assert_eq!(error.reason, "restartFailed");
}

#[cfg(unix)]
mod filesystem {
    use super::*;
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
    };
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let p = std::env::temp_dir()
                .join(format!("pilotdeck-update-test-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    struct MockBuilder {
        old: String,
        new: String,
        remote: String,
        fail_build: bool,
        unconfirmed: bool,
        bundled_skills: bool,
        skills_as_file: bool,
        builds: Vec<(PathBuf, String)>,
    }
    impl Builder for MockBuilder {
        fn git(&mut self, cwd: &Path, args: &[&str]) -> Result<String, Error> {
            match args {
                ["rev-parse", "HEAD"] => Ok(if cwd.join(".new-source").exists() {
                    self.new.clone()
                } else {
                    self.old.clone()
                }),
                ["status", ..] => Ok(String::new()),
                ["rev-parse", "--is-shallow-repository"] => Ok("false".into()),
                ["remote", "get-url", "origin"] => Ok(self.remote.clone()),
                ["fetch", ..] => Ok(String::new()),
                ["rev-parse", "FETCH_HEAD^{commit}"] => Ok(self.new.clone()),
                ["merge-base", ..] => Ok(self.old.clone()),
                ["clone", ..] => {
                    let dest = Path::new(args.last().unwrap());
                    fs::create_dir(dest).unwrap();
                    fs::create_dir(dest.join(".git")).unwrap();
                    if !args[args.len() - 2].starts_with('/') {
                        fs::write(dest.join(".new-source"), b"fixture").unwrap();
                    }
                    Ok(String::new())
                }
                ["checkout", "--detach", sha] => {
                    if *sha == self.new {
                        fs::write(cwd.join(".new-source"), b"fixture").unwrap();
                    }
                    if self.bundled_skills {
                        if self.skills_as_file {
                            fs::write(cwd.join("skills"), b"invalid skill root").unwrap();
                        } else {
                            fs::create_dir_all(cwd.join("skills/example")).unwrap();
                            fs::write(cwd.join("skills/example/SKILL.md"), sha).unwrap();
                        }
                    }
                    Ok(String::new())
                }
                ["remote", "set-url", ..] => Ok(String::new()),
                _ => panic!("unexpected Git args {args:?}"),
            }
        }
        fn build(
            &mut self,
            source: &Path,
            _output: &mut dyn FnMut(&str),
        ) -> Result<PathBuf, Error> {
            let sha = if source.join(".new-source").exists() {
                self.new.clone()
            } else {
                self.old.clone()
            };
            self.builds.push((source.into(), sha.clone()));
            if self.unconfirmed {
                return Err(Error::new("processStopFailed", "fixture child unconfirmed"));
            }
            if self.fail_build {
                return Err(Error::new("buildFailed", "fixture build failed"));
            }
            let out = source.join(".update-target/release");
            fs::create_dir_all(&out).unwrap();
            let label = if sha == self.old { "old" } else { "new" };
            for name in BINARIES {
                fs::write(
                    out.join(name),
                    format!("#!/bin/sh\nprintf '%s' '{label}-{name}'\n"),
                )
                .unwrap();
                fs::set_permissions(out.join(name), fs::Permissions::from_mode(0o700)).unwrap();
            }
            Ok(out)
        }
    }
    fn fixture(scratch: &Scratch) -> (Installation, MockBuilder) {
        let source = scratch.0.join("original-source");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join(".git")).unwrap();
        let mut builder = MockBuilder {
            old: "1".repeat(40),
            new: "2".repeat(40),
            remote: "https://github.com/daqi-dev/pilotdeck-rs.git".into(),
            fail_build: false,
            unconfirmed: false,
            bundled_skills: false,
            skills_as_file: false,
            builds: Vec::new(),
        };
        let policy = InstallManifest {
            schema_version: 1,
            install_id: "test-install".into(),
            approved_remote: builder.remote.clone(),
            approved_ref: "refs/heads/main".into(),
        };
        let old = builder.old.clone();
        let installation = bootstrap(
            &scratch.0.join("installed"),
            policy,
            &source,
            &old,
            &mut builder,
        )
        .unwrap();
        assert_eq!(builder.builds.len(), 1);
        assert_eq!(builder.builds[0].1, old);
        assert_ne!(builder.builds[0].0, source);
        assert!(builder.builds[0].0.ends_with("source"));
        (installation, builder)
    }
    #[test]
    fn first_install_and_update_publish_four_matching_binaries() {
        let s = Scratch::new();
        let (i, b) = fixture(&s);
        let old = i.current.generation.clone();
        let root = i.root.clone();
        assert_eq!(
            i.current.source_checkout,
            root.join("generations").join(&old).join("source")
        );
        assert_eq!(
            fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let mut e = Engine::new(i, b);
        let status = e.check().unwrap();
        let target = status.latest.unwrap();
        assert!(status.can_update);
        e.apply(&target, &mut |_| {}).unwrap();
        let new = e.target_generation().unwrap();
        assert_ne!(new, old);
        let actual = Installation::open(&root, new).unwrap();
        assert_eq!(actual.current.source_sha, "2".repeat(40));
        assert_eq!(e.builder.builds.len(), 2);
        assert_eq!(e.builder.builds[1].1, "2".repeat(40));
        assert!(
            !actual
                .current
                .source_checkout
                .to_string_lossy()
                .contains(".prepare-")
        );
        for name in BINARIES {
            assert!(
                String::from_utf8(fs::read(root.join("bin").join(name)).unwrap())
                    .unwrap()
                    .contains(&format!("new-{name}"))
            );
            assert!(actual.current.bin_hashes.contains_key(name));
        }
        assert!(!root.join("update.lock").exists());
        assert!(root.join("generations").join(old).exists());
    }
    #[test]
    fn build_failure_and_lock_contention_leave_previous_installation() {
        let s = Scratch::new();
        let (i, mut b) = fixture(&s);
        let old = fs::read_link(i.root.join("current")).unwrap();
        let root = i.root.clone();
        b.fail_build = true;
        let mut e = Engine::new(i, b);
        let target = e.check().unwrap().latest.unwrap();
        assert_eq!(
            e.apply(&target, &mut |_| {}).unwrap_err().reason,
            "buildFailed"
        );
        assert_eq!(fs::read_link(root.join("current")).unwrap(), old);
        assert!(!root.join("update.lock").exists());
        fs::create_dir(root.join("update.lock")).unwrap();
        assert_eq!(e.check().err().unwrap().reason, "lockBusy");
        assert!(root.join("update.lock").exists());
    }
    #[test]
    fn unconfirmed_process_cleanup_retains_lock_and_staging() {
        let s = Scratch::new();
        let (i, mut b) = fixture(&s);
        let root = i.root.clone();
        let old = fs::read_link(root.join("current")).unwrap();
        b.unconfirmed = true;
        let mut e = Engine::new(i, b);
        let target = e.check().unwrap().latest.unwrap();
        assert_eq!(
            e.apply(&target, &mut |_| {}).unwrap_err().reason,
            "processStopFailed"
        );
        assert_eq!(fs::read_link(root.join("current")).unwrap(), old);
        assert!(root.join("update.lock").exists());
        assert!(fs::read_dir(root).unwrap().any(|r| {
            r.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".prepare-")
        }));
    }
    #[test]
    fn tampered_bundle_and_stale_runtime_are_refused() {
        let s = Scratch::new();
        let (i, b) = fixture(&s);
        let root = i.root.clone();
        let old = i.expected_generation.clone();
        fs::write(root.join("bin/pilotdeck-gateway"), b"tampered").unwrap();
        assert_eq!(
            Installation::open(&root, &old).err().unwrap().reason,
            "runtimeMismatch"
        );
        drop(b);
        let s = Scratch::new();
        let (mut i, b) = fixture(&s);
        i.expected_generation = "another-generation".into();
        let mut e = Engine::new(i, b);
        assert_eq!(e.check().err().unwrap().reason, "restartRequired");
    }
    #[test]
    fn published_bootstrap_confirmation_failures_are_explicit_and_preserved() {
        for fail_open in [false, true] {
            let scratch = Scratch::new();
            let (i, mut builder) = fixture(&scratch);
            let source = i.current.source_checkout.clone();
            let root = scratch.0.join("second-install");
            let policy = i.policy.clone();
            let sha = builder.old.clone();
            let error = super::super::install::bootstrap_with_confirmation(
                &root,
                policy,
                &source,
                &sha,
                &mut builder,
                |published, generation| {
                    assert!(published.join("current").is_symlink());
                    if fail_open {
                        fs::write(published.join("install.json"), b"invalid metadata").unwrap();
                        Installation::open(published, generation)
                    } else {
                        Err(Error::new(
                            "applyFailed",
                            "injected parent directory fsync failure",
                        ))
                    }
                },
            )
            .err()
            .unwrap();
            assert_eq!(error.reason, "publicationUnconfirmed");
            assert!(error.message.contains(root.to_str().unwrap()));
            assert!(error.message.contains("Do not retry or overwrite"));
            assert!(root.join("current").is_symlink());
            assert!(root.join("bin/pilotdeck-control").exists());
            let retry = bootstrap(&root, i.policy, &source, &sha, &mut builder)
                .err()
                .unwrap();
            assert_eq!(retry.reason, "alreadyInstalled");
        }
    }
    #[test]
    fn invalid_bundled_skill_root_never_publishes_an_install_or_update() {
        let scratch = Scratch::new();
        let (first, mut builder) = fixture(&scratch);
        builder.bundled_skills = true;
        builder.skills_as_file = true;
        let root = scratch.0.join("bad-bundled-install");
        let old_sha = builder.old.clone();
        let error = bootstrap(
            &root,
            first.policy.clone(),
            &scratch.0.join("original-source"),
            &old_sha,
            &mut builder,
        )
        .err()
        .unwrap();
        assert_eq!(error.reason, "invalidSkillsRoot");
        assert!(!root.exists());
        let previous = fs::read_link(first.root.join("current")).unwrap();
        let mut engine = Engine::new(first, builder);
        let target = engine.check().unwrap().latest.unwrap();
        let error = engine.apply(&target, &mut |_| {}).unwrap_err();
        assert_eq!(error.reason, "invalidSkillsRoot");
        assert_eq!(
            fs::read_link(engine.installation.root.join("current")).unwrap(),
            previous
        );
        assert!(engine.target_generation().is_none());
        for (parent, prefix) in [
            (&scratch.0, ".pilotdeck-install-"),
            (&engine.installation.root, ".prepare-"),
        ] {
            assert!(
                fs::read_dir(parent).unwrap().all(|entry| {
                    !entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(prefix)
                }),
                "failed skills validation left staging files"
            );
        }
    }
    #[test]
    fn bundled_skills_follow_the_published_generation_and_keep_old_bytes() {
        let scratch = Scratch::new();
        let (first, mut builder) = fixture(&scratch);
        builder.bundled_skills = true;
        let root = scratch.0.join("bundled-install");
        let old_sha = builder.old.clone();
        let installation = bootstrap(
            &root,
            first.policy,
            &scratch.0.join("original-source"),
            &old_sha,
            &mut builder,
        )
        .unwrap();
        let old_generation = installation.current.generation.clone();
        let current_skill = root.join("current/skills/example/SKILL.md");
        assert_eq!(fs::read_to_string(&current_skill).unwrap(), old_sha);
        assert_eq!(
            fs::canonicalize(root.join("current/bin/../skills")).unwrap(),
            fs::canonicalize(root.join("current/source/skills")).unwrap()
        );
        let mut engine = Engine::new(installation, builder);
        let target = engine.check().unwrap().latest.unwrap();
        engine.apply(&target, &mut |_| {}).unwrap();
        assert_eq!(
            fs::read_to_string(&current_skill).unwrap(),
            target.source_sha
        );
        assert_eq!(
            fs::read_to_string(
                root.join("generations")
                    .join(old_generation)
                    .join("skills/example/SKILL.md")
            )
            .unwrap(),
            old_sha
        );
        assert_eq!(
            fs::read_link(root.join("current/skills")).unwrap(),
            Path::new("source/skills")
        );
    }
    #[test]
    fn explicit_external_skills_survive_update_without_copying() {
        let scratch = Scratch::new();
        let (first, mut builder) = fixture(&scratch);
        builder.bundled_skills = true;
        let original = scratch.0.join("original-source");
        let external = scratch.0.join("outside-skills");
        fs::create_dir(&external).unwrap();
        fs::create_dir(external.join("example")).unwrap();
        let skill = external.join("example/SKILL.md");
        fs::write(&skill, b"self-written fixture").unwrap();
        let root = scratch.0.join("with-skills");
        let old = builder.old.clone();
        let installation = bootstrap_with_external_skills(
            &root,
            first.policy.clone(),
            &original,
            &old,
            &mut builder,
            Some(&external),
        )
        .unwrap();
        let link = root.join("generations/skills");
        assert_eq!(
            fs::read_link(&link).unwrap(),
            external.canonicalize().unwrap()
        );
        assert_eq!(
            fs::read(link.join("example/SKILL.md")).unwrap(),
            b"self-written fixture"
        );
        assert_eq!(
            fs::read(root.join("current/skills/example/SKILL.md")).unwrap(),
            b"self-written fixture"
        );
        let mut engine = Engine::new(installation, builder);
        let target = engine.check().unwrap().latest.unwrap();
        engine.apply(&target, &mut |_| {}).unwrap();
        let generation = engine.target_generation().unwrap();
        let actual = Installation::open(&root, generation).unwrap();
        let bin = actual.root.join("generations").join(generation).join("bin");
        assert_eq!(
            fs::canonicalize(bin.join("../../skills")).unwrap(),
            external.canonicalize().unwrap()
        );
        assert_eq!(fs::read(&skill).unwrap(), b"self-written fixture");
        assert_eq!(
            fs::read(root.join("current/skills/example/SKILL.md")).unwrap(),
            b"self-written fixture"
        );
        let bad_root = scratch.0.join("invalid-install");
        let result = bootstrap_with_external_skills(
            &bad_root,
            first.policy.clone(),
            &original,
            &old,
            &mut engine.builder,
            Some(&scratch.0.join("does-not-exist")),
        );
        assert_eq!(result.err().unwrap().reason, "invalidSkillsRoot");
        assert!(!bad_root.exists());
        let external_file = scratch.0.join("external-file");
        fs::write(&external_file, b"not a skill directory").unwrap();
        let result = bootstrap_with_external_skills(
            &bad_root,
            first.policy,
            &original,
            &old,
            &mut engine.builder,
            Some(&external_file),
        );
        assert_eq!(result.err().unwrap().reason, "invalidSkillsRoot");
        assert!(!bad_root.exists());
    }
}
