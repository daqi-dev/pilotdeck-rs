//! Local skill request handling for G0's injected, already assembled facade.
//! The returned object is the response body (`ok` and `result` or `error`);
//! G0 adds the frame's `type` and `id`. Only unknown methods return `None`.
//! Host-required errors are terminal local errors, never a reason to forward.

use super::{SkillError, SkillMethod, SkillsFacade};
use pilotdeck_protocol::{JsObject, JsValue};

/// Synchronous domain calls perform filesystem I/O; G0 owns scheduling them.
/// No environment, paths, configuration, or upstream transport is created here.
pub fn handle<F>(method: &str, params: &JsValue, facade: &SkillsFacade<F>) -> Option<JsValue>
where
    F: Fn(Option<&JsValue>) -> Result<JsValue, SkillError>,
{
    // SkillMethod covers exactly the eight wire methods; anything else is G0's.
    let method = SkillMethod::from_wire(method)?;
    Some(encode(facade.call(method, params)))
}

fn encode(outcome: Result<JsValue, SkillError>) -> JsValue {
    let mut body = JsObject::new();
    body.insert("ok".into(), JsValue::Bool(outcome.is_ok()));
    match outcome {
        Ok(result) => {
            body.insert("result".into(), result);
        }
        Err(error) => {
            let code = if error.code.is_empty() && matches!(error.name, "TypeError" | "RangeError")
            {
                "gateway_request_failed"
            } else {
                &error.code
            };
            let mut fields = JsObject::new();
            fields.insert("code".into(), code.into());
            fields.insert("message".into(), error.message.as_str().into());
            if let Some(validation) = error.validation {
                fields.insert("validation".into(), *validation);
            }
            if error.requires_host {
                fields.insert("requiresHost".into(), JsValue::Bool(true));
            }
            body.insert("error".into(), JsValue::Object(fields));
        }
    }
    JsValue::Object(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skills::{SkillManager, SkillManagerOptions};
    use pilotdeck_protocol::parse_json;
    use std::{fs, path::PathBuf};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../../_tmp/20261027-g-skill/fixtures")
                .join(uuid::Uuid::new_v4().to_string());
            fs::create_dir_all(&root).unwrap();
            Self(root)
        }
        fn manager(&self) -> SkillManager {
            let mut options = SkillManagerOptions::new(self.0.to_string_lossy());
            options.env = Some(Default::default());
            options.home_dir = Some(self.0.to_string_lossy().into_owned());
            SkillManager::new(options).unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    fn json(input: &str) -> JsValue {
        parse_json(input).unwrap()
    }

    #[test]
    fn skill_write_persists_content_and_rejects_builtin() {
        let fixture = Fixture::new();
        let facade = SkillsFacade::new(fixture.manager(), |_| Ok(JsValue::Undefined));
        for content in ["first", "updated"] {
            let params = super::super::object([
                ("scope", "user".into()),
                ("slug", "sample".into()),
                ("content", content.into()),
            ]);
            let response = handle("skill_write", &params, &facade).unwrap();
            assert_eq!(response.get("ok"), Some(&JsValue::Bool(true)));
            assert_eq!(
                fs::read_to_string(fixture.0.join("skills/sample/SKILL.md")).unwrap(),
                content
            );
            let read = handle("skill_read", &params, &facade).unwrap();
            assert_eq!(
                read.get("result").unwrap().get("content"),
                Some(&content.into())
            );
        }
        let response = handle(
            "skill_write",
            &json(r#"{"scope":"builtin","slug":"sample","content":"blocked"}"#),
            &facade,
        )
        .unwrap();
        assert_eq!(
            response.get("error").unwrap().get("code"),
            Some(&"read_only".into())
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join("skills/sample/SKILL.md")).unwrap(),
            "updated"
        );
    }

    #[test]
    fn skill_scan_lists_skill_folders_first_and_reports_bad_parent() {
        let fixture = Fixture::new();
        let facade = SkillsFacade::new(fixture.manager(), |_| Ok(JsValue::Undefined));
        let parent = fixture.0.join("browse");
        for (dir, skill) in [("zebra", false), ("alpha", true)] {
            let folder = parent.join(dir);
            fs::create_dir_all(&folder).unwrap();
            if skill {
                fs::write(
                    folder.join("SKILL.md"),
                    "---\nname: Alpha\ndescription: Scanned folder with a skill file\n---\n",
                )
                .unwrap();
            }
        }
        let params = super::super::object([(
            "parentPath",
            parent.to_string_lossy().into_owned().as_str().into(),
        )]);
        let response = handle("skill_scan", &params, &facade).unwrap();
        assert_eq!(response.get("ok"), Some(&JsValue::Bool(true)));
        let result = response.get("result").unwrap();
        let Some(JsValue::Array(folders)) = result.get("folders") else {
            panic!("folders must be an array")
        };
        assert_eq!(folders.len(), 2);
        let name = |f: &JsValue| {
            f.get("folderName")
                .and_then(JsValue::as_string)
                .and_then(|s| s.to_utf8().ok())
                .unwrap()
        };
        assert_eq!(name(&folders[0]), "alpha");
        assert_eq!(folders[0].get("hasSkillMd"), Some(&JsValue::Bool(true)));

        let response = handle(
            "skill_scan",
            &super::super::object([("parentPath", "/nonexistent/pdrs-scan".into())]),
            &facade,
        )
        .unwrap();
        assert_eq!(
            response.get("error").unwrap().get("code"),
            Some(&"not_found".into())
        );
    }

    #[test]
    fn skill_validate_reports_disk_checks_without_creating_targets() {
        let fixture = Fixture::new();
        let facade = SkillsFacade::new(fixture.manager(), |_| Ok(JsValue::Undefined));
        let good = fixture.0.join("check/good");
        fs::create_dir_all(&good).unwrap();
        fs::write(
            good.join("SKILL.md"),
            "---\nname: Good\ndescription: A well formed skill directory for tests\n---\nBody.\n",
        )
        .unwrap();
        let params = super::super::object([(
            "sourcePath",
            good.to_string_lossy().into_owned().as_str().into(),
        )]);
        let response = handle("skill_validate", &params, &facade).unwrap();
        assert_eq!(response.get("ok"), Some(&JsValue::Bool(true)));
        let result = response.get("result").unwrap();
        assert_eq!(result.get("ok"), Some(&JsValue::Bool(true)));
        assert_eq!(result.get("hardFails"), Some(&JsValue::Array(vec![])));
        assert!(!fixture.0.join("skills").exists());

        let bad = fixture.0.join("check/bad");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("SKILL.md"), "no frontmatter\n").unwrap();
        let response = handle(
            "skill_validate",
            &super::super::object([(
                "sourcePath",
                bad.to_string_lossy().into_owned().as_str().into(),
            )]),
            &facade,
        )
        .unwrap();
        let result = response.get("result").unwrap();
        assert_eq!(result.get("ok"), Some(&JsValue::Bool(false)));
        let codes: Vec<String> = result
            .get("hardFails")
            .and_then(|fails| match fails {
                JsValue::Array(items) => Some(items),
                _ => None,
            })
            .expect("hardFails must be an array")
            .iter()
            .filter_map(|f| f.get("code"))
            .filter_map(JsValue::as_string)
            .filter_map(|s| s.to_utf8().ok())
            .collect();
        assert!(
            codes.contains(&"frontmatter_missing".to_string()),
            "{codes:?}"
        );
    }

    #[test]
    fn skill_import_copies_valid_bundle_and_rejects_missing_source() {
        let fixture = Fixture::new();
        let facade = SkillsFacade::new(fixture.manager(), |_| Ok(JsValue::Undefined));
        let source = fixture.0.join("incoming/http-tool");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("SKILL.md"),
            "---\nname: Http Tool\ndescription: Imported through the route test path\n---\nBody.\n",
        )
        .unwrap();
        let params = super::super::object([
            ("scope", "user".into()),
            (
                "sourcePath",
                source.to_string_lossy().into_owned().as_str().into(),
            ),
        ]);
        let response = handle("skill_import", &params, &facade).unwrap();
        assert_eq!(response.get("ok"), Some(&JsValue::Bool(true)));
        assert_eq!(
            response.get("result").unwrap().get("mode"),
            Some(&"copy".into())
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join("skills/http-tool/SKILL.md")).unwrap(),
            "---\nname: Http Tool\ndescription: Imported through the route test path\n---\nBody.\n"
        );
        let missing = super::super::object([
            ("scope", "user".into()),
            ("sourcePath", "/nonexistent/pdrs-import".into()),
        ]);
        let response = handle("skill_import", &missing, &facade).unwrap();
        assert_eq!(
            response.get("error").unwrap().get("code"),
            Some(&"source_missing".into())
        );
    }

    #[test]
    fn skill_delete_removes_tree_and_is_idempotent() {
        let fixture = Fixture::new();
        let facade = SkillsFacade::new(fixture.manager(), |_| Ok(JsValue::Undefined));
        let dir = fixture.0.join("skills/gone");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("SKILL.md"),
            "---\nname: Gone\ndescription: Deleted by the delete route test\n---\n",
        )
        .unwrap();
        fs::write(dir.join("extra.txt"), "files go too").unwrap();
        let params = json(r#"{"scope":"user","slug":"gone"}"#);
        let response = handle("skill_delete", &params, &facade).unwrap();
        assert_eq!(response.get("ok"), Some(&JsValue::Bool(true)));
        assert!(!dir.exists());
        let again = handle("skill_delete", &params, &facade).unwrap();
        assert_eq!(again.get("ok"), Some(&JsValue::Bool(true)));
        let builtin = handle(
            "skill_delete",
            &json(r#"{"scope":"builtin","slug":"gone"}"#),
            &facade,
        )
        .unwrap();
        assert_eq!(
            builtin.get("error").unwrap().get("code"),
            Some(&"read_only".into())
        );
    }

    #[test]
    fn skill_create_makes_dir_once_and_conflicts_afterwards() {
        let fixture = Fixture::new();
        let facade = SkillsFacade::new(fixture.manager(), |_| Ok(JsValue::Undefined));
        let params = super::super::object([("scope", "user".into()), ("slug", "fresh".into())]);
        let response = handle("skill_create", &params, &facade).unwrap();
        assert_eq!(response.get("ok"), Some(&JsValue::Bool(true)));
        let path = response
            .get("result")
            .unwrap()
            .get("skillPath")
            .and_then(JsValue::as_string)
            .and_then(|s| s.to_utf8().ok())
            .unwrap();
        assert!(path.ends_with("/skills/fresh"), "{path}");
        let written = fs::read_to_string(fixture.0.join("skills/fresh/SKILL.md")).unwrap();
        assert!(written.contains("name: fresh"), "{written}");
        let again = handle("skill_create", &params, &facade).unwrap();
        assert_eq!(
            again.get("error").unwrap().get("code"),
            Some(&"conflict".into())
        );
        assert_eq!(
            fs::read_to_string(fixture.0.join("skills/fresh/SKILL.md")).unwrap(),
            written
        );
    }

    #[test]
    fn skill_read_reads_disk_and_preserves_not_found() {
        let fixture = Fixture::new();
        let dir = fixture.0.join("skills/sample");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "# read fixture\n").unwrap();
        let facade = SkillsFacade::new(fixture.manager(), |_| Ok(JsValue::Undefined));
        let params = json(r#"{"scope":"user","slug":"sample"}"#);
        let response = handle("skill_read", &params, &facade).unwrap();
        assert_eq!(response.get("ok"), Some(&JsValue::Bool(true)));
        assert_eq!(
            response.get("result").unwrap().get("content"),
            Some(&"# read fixture\n".into())
        );
        fs::remove_file(dir.join("SKILL.md")).unwrap();
        let response = handle("skill_read", &params, &facade).unwrap();
        assert_eq!(
            response.get("error").unwrap().get("code"),
            Some(&"not_found".into())
        );
    }

    #[test]
    fn every_skill_method_is_handled_locally() {
        let fixture = Fixture::new();
        let facade = SkillsFacade::new(fixture.manager(), |_| Ok(JsValue::Undefined));
        for method in SkillMethod::ALL {
            let body = handle(method.wire(), &json("{}"), &facade)
                .unwrap_or_else(|| panic!("{} was not handled locally", method.wire()));
            assert!(matches!(body.get("ok"), Some(JsValue::Bool(_))));
        }
        assert!(handle("skill_unknown", &json("{}"), &facade).is_none());
    }

    #[test]
    fn skill_list_is_local_and_unknown_methods_do_not_call_or_write() {
        let fixture = Fixture::new();
        let facade = SkillsFacade::new(fixture.manager(), |_| Ok(JsValue::Undefined));
        let result = handle("skill_list", &json("{}"), &facade).unwrap();
        assert_eq!(result.get("ok"), Some(&JsValue::Bool(true)));
        assert_eq!(
            result.get("result").unwrap().get("items"),
            Some(&JsValue::Array(vec![]))
        );
        let untouched = SkillsFacade::new(fixture.manager(), |_| {
            panic!("unknown method called facade")
        });
        assert!(handle("skill_unknown", &JsValue::Null, &untouched).is_none());
        assert_eq!(fs::read_dir(&fixture.0).unwrap().count(), 0);
    }

    #[test]
    fn skill_list_keeps_business_and_runtime_error_codes() {
        let fixture = Fixture::new();
        let facade = SkillsFacade::new(fixture.manager(), |_| Ok(JsValue::Undefined));
        for (params, code) in [
            (json(r#"{"limit":51}"#), "invalid_input"),
            (json(r#"{"query":1}"#), "gateway_request_failed"),
        ] {
            let response = handle("skill_list", &params, &facade).unwrap();
            assert_eq!(response.get("ok"), Some(&JsValue::Bool(false)));
            assert_eq!(
                response.get("error").unwrap().get("code"),
                Some(&code.into())
            );
        }
    }
}
