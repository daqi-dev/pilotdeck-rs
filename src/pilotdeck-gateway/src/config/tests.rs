use super::*;
use crate::metadata::Stat;
use pilotdeck_protocol::{parse_json, stringify_json};
use std::{
    io,
    path::{Path, PathBuf},
};
struct FixtureFiles {
    data: Vec<u8>,
    kind: Option<String>,
}
impl Files for FixtureFiles {
    fn entries(&self, _: &Path) -> io::Result<Vec<(String, bool)>> {
        Err(io::Error::from_raw_os_error(libc::ENOENT))
    }
    fn read(&self, _: &Path) -> io::Result<Vec<u8>> {
        if self.kind.as_deref() == Some("directory") {
            return Err(io::Error::from_raw_os_error(libc::EISDIR));
        }
        Ok(self.data.clone())
    }
    fn stat(&self, _: &Path) -> io::Result<Stat> {
        if self.kind.as_deref() == Some("missing") {
            return Err(io::Error::from_raw_os_error(libc::ENOENT));
        }
        Ok(Stat {
            directory: false,
            size: self.data.len() as u64,
            birth_ms: 0.0,
            ctime_ms: 0.0,
            mtime_ms: 0.0,
        })
    }
}
fn encode(v: &JsValue) -> String {
    stringify_json(v).unwrap().unwrap()
}
fn cases(raw: &str) -> Vec<JsValue> {
    let root = parse_json(raw).unwrap();
    let JsValue::Array(cases) = field(&root, "cases") else {
        panic!("cases")
    };
    cases.clone()
}
fn assert_fixture_complete(tests: &[JsValue], expected_count: usize) {
    assert_eq!(tests.len(), expected_count, "fixture count changed");
    let mut ids = std::collections::HashSet::new();
    for test in tests {
        let id = text(field(test, "id")).expect("fixture id");
        assert!(!id.is_empty(), "empty fixture id");
        assert!(ids.insert(id.clone()), "duplicate fixture id: {id}");
    }
}
fn verify(raw: &str, expected_count: usize) -> Vec<String> {
    let tests = cases(raw);
    assert_fixture_complete(&tests, expected_count);
    let mut failures = vec![];
    for test in tests {
        let id = text(field(&test, "id")).unwrap();
        let yaml = text(field(&test, "yaml")).unwrap();
        let kind = text(field(&test, "sourceKind"));
        let mut bytes = yaml.into_bytes();
        if kind.as_deref() == Some("bytes") {
            bytes.extend(b"unused: \"");
            bytes.extend([0xff, 0xfe, 0x80]);
            bytes.extend(b"\"\n");
        }
        let mut env: Env = entries(field(&test, "env"))
            .into_iter()
            .map(|(k, v)| (k.to_utf8().unwrap(), text(v).unwrap()))
            .collect();
        match kind.as_deref() {
            Some("blank") => {
                env.insert("PILOTDECK_CONFIG_PATH".into(), "".into());
            }
            Some("spaces") => {
                env.insert("PILOTDECK_CONFIG_PATH".into(), "  ".into());
            }
            Some("relative") => {
                env.insert("PILOTDECK_CONFIG_PATH".into(), "relative.yaml".into());
            }
            Some("alternate") => {
                env.insert(
                    "PILOTDECK_CONFIG_PATH".into(),
                    "  <PILOT_HOME>/other.yaml  ".into(),
                );
            }
            _ => {}
        }
        let metadata = Metadata {
            home: PathBuf::from("<PILOT_HOME>"),
            cwd: PathBuf::from("/offline"),
            files: FixtureFiles { data: bytes, kind },
        };
        let expected_load = field(&test, "load");
        let load = match super::load(&metadata, &env) {
            Ok(c) => object([
                ("ok", JsValue::Bool(true)),
                ("diagnostics", JsValue::Array(c.diagnostics)),
            ]),
            Err(e) => {
                let mut o = JsObject::new();
                o.insert("ok".into(), JsValue::Bool(false));
                o.insert("name".into(), e.name.into());
                if e.name == "PilotConfigError" {
                    o.insert("code".into(), JsValue::String(e.code.into()));
                }
                o.insert("message".into(), JsValue::String(e.message));
                if e.name == "PilotConfigError" {
                    o.insert("diagnostics".into(), JsValue::Array(e.diagnostics));
                }
                JsValue::Object(o)
            }
        };
        let expected = encode(&canonical(expected_load));
        let actual = encode(&canonical(&load));
        if actual != expected {
            failures.push(format!("{id} load\nexpected {expected}\nactual   {actual}"));
        }
        let request = text(field(&test, "request")).unwrap();
        let response = text(field(&test, "response")).unwrap();
        let frame = parse_json(&request).unwrap();
        assert!(crate::dispatch::is_local(&frame));
        assert!(crate::dispatch::needs_files(&frame));
        let route = crate::dispatch::route_catalog(&frame, Some(&metadata), &env);
        let actual = match route {
            crate::dispatch::Route::LocalResponse(bytes) => bytes,
            _ => panic!("catalog must terminate locally"),
        };
        if actual != response.as_bytes() {
            failures.push(format!(
                "{id} wire\nexpected {response}\nactual   {}",
                String::from_utf8_lossy(&actual)
            ));
        }
    }
    failures
}
#[test]
fn catalog_32_wire_and_load() {
    let failures = verify(include_str!("../../tests/fixtures/config/catalog.json"), 32);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
#[test]
fn configuration_diagnostics_and_catalog_wire() {
    let failures = verify(
        include_str!("../../tests/fixtures/config/diagnostics.json"),
        2696,
    );
    let total = failures.len();
    assert!(
        failures.is_empty(),
        "{total} differences; first entries:\n{}",
        failures.into_iter().take(12).collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn config_fixture_completeness_rejects_empty_truncated_and_duplicate() {
    let original = cases(include_str!("../../tests/fixtures/config/catalog.json"));
    assert_fixture_complete(&original, 32);
    let mut truncated = original.clone();
    truncated.pop();
    let mut duplicate = original.clone();
    duplicate[1] = duplicate[0].clone();
    for invalid in [vec![], truncated, duplicate] {
        assert!(std::panic::catch_unwind(|| assert_fixture_complete(&invalid, 32)).is_err());
    }
}

fn canonical(v: &JsValue) -> JsValue {
    match v {
        JsValue::Object(o) => {
            let mut keys: Vec<_> = o
                .entries()
                .iter()
                .filter(|(_, v)| !v.is_undefined())
                .collect();
            keys.sort_by(|a, b| a.0.cmp(&b.0));
            let mut out = JsObject::new();
            for (k, v) in keys {
                out.insert(k.clone(), canonical(v));
            }
            JsValue::Object(out)
        }
        JsValue::Array(a) => JsValue::Array(a.iter().map(canonical).collect()),
        _ => v.clone(),
    }
}

#[test]
fn host_intrinsics_keep_upstream_ownership() {
    let tests = cases(include_str!(
        "../../tests/fixtures/config/host-boundaries.json"
    ));
    assert_eq!(tests.len(), 18);
    for test in tests {
        let yaml = text(field(&test, "yaml")).unwrap();
        let metadata = Metadata {
            home: PathBuf::from("<PILOT_HOME>"),
            cwd: PathBuf::from("/offline"),
            files: FixtureFiles {
                data: yaml.into_bytes(),
                kind: None,
            },
        };
        let env = Env::new();
        let error = super::load(&metadata, &env)
            .err()
            .expect("requires JS host");
        assert!(error.requires_host);
        let request = parse_json(&text(field(&test, "request")).unwrap()).unwrap();
        assert!(matches!(
            crate::dispatch::route_catalog(&request, Some(&metadata), &env),
            crate::dispatch::Route::Forward
        ));
        // The separate Node oracle records the host response; no synthetic Rust
        // response is compared or counted as a native wire match.
        assert!(!text(field(&test, "response")).unwrap().is_empty());
    }
}

#[test]
fn catalog_unicode_ids_and_filtered_lists_stay_local() {
    use crate::dispatch::{Route, route_catalog};
    for (provider, model) in [
        ("parity", "中"),
        ("parity", "é"),
        ("parity", "😀"),
        ("中文", "ascii"),
    ] {
        let mut yaml = "schemaVersion: 1\nagent:\n  model: parity/ascii\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        ascii:\n          displayName: 中文 é 😀\n".to_owned();
        if provider == "parity" {
            yaml.push_str(&format!("        {model}: {{}}\n"));
        } else {
            yaml.push_str(&format!("    {provider}:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        {model}: {{}}\n"));
        }
        let metadata = Metadata {
            home: "<PILOT_HOME>".into(),
            cwd: "/offline".into(),
            files: FixtureFiles {
                data: yaml.into_bytes(),
                kind: None,
            },
        };
        let env = Env::new();
        let request = |params| {
            object([
                ("type", "request".into()),
                ("id", "catalog".into()),
                ("method", "model_catalog_list".into()),
                ("params", params),
            ])
        };
        assert!(
            matches!(
                route_catalog(&request(object([])), Some(&metadata), &env),
                Route::LocalResponse(_)
            ),
            "{provider}/{model}"
        );
        for params in [
            object([("query", "ascii".into()), ("provider", "parity".into())]),
            object([("query", "absent".into())]),
        ] {
            let Route::LocalResponse(bytes) =
                route_catalog(&request(params), Some(&metadata), &env)
            else {
                panic!("filtered ASCII catalog forwarded")
            };
            let frame = parse_json(std::str::from_utf8(&bytes).unwrap()).unwrap();
            assert_eq!(frame.get("ok").and_then(JsValue::as_bool), Some(true));
        }
    }
}

/// #304: the role-limited `load_project` projection must carry
/// `webui.runtime.httpsProxy` so the legacy proxy migration diagnostic matches
/// TS `loadPilotConfig`. Expectations recorded from the fixed baseline
/// (`_tmp/20261019-p2-sweep-6/probe-304.mts`, real TS run).
#[test]
fn load_project_reports_legacy_webui_proxy_migration_like_ts() {
    const BASE: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n";
    const MIGRATED: &str = "webui.runtime.httpsProxy has been migrated to the top-level proxy.url field. Please update your pilotdeck.yaml to use proxy.url instead.";
    for (extra, expected) in [
        (
            "webui: {runtime: {httpsProxy: http://legacy}}\n",
            vec![(
                "CONFIG_PROXY_MIGRATED",
                "info",
                MIGRATED,
                "webui.runtime.httpsProxy",
            )],
        ),
        (
            "proxy: http://new\nwebui: {runtime: {httpsProxy: http://legacy}}\n",
            vec![],
        ),
        ("webui: {runtime: {httpsProxy: ''}}\n", vec![]),
        ("webui: {runtime: {httpsProxy: 5}}\n", vec![]),
        ("webui: {runtime: [a]}\n", vec![]),
        // #330: TS checks `raw === undefined`, not falsiness. `null` therefore
        // stops the legacy migration and is rejected as a non-record; an empty
        // string still migrates nothing but is a valid (empty) proxy string.
        (
            "proxy: null\nwebui: {runtime: {httpsProxy: http://legacy}}\n",
            vec![(
                "CONFIG_PROXY_INVALID",
                "warning",
                "proxy must be a string or an object with a url field.",
                "proxy",
            )],
        ),
        (
            "proxy: ''\nwebui: {runtime: {httpsProxy: http://legacy}}\n",
            vec![],
        ),
        (
            "webui: {staffdeckCopy: {enabled: true}, runtime: {other: {x: 1}}}\n",
            vec![],
        ),
    ] {
        let metadata = Metadata {
            home: PathBuf::from("<PILOT_HOME>"),
            cwd: PathBuf::from("/offline"),
            files: FixtureFiles {
                data: format!("{BASE}{extra}").into_bytes(),
                kind: None,
            },
        };
        let config = load_project_with_files(&metadata, &Env::new(), None).unwrap();
        let actual: Vec<_> = config
            .diagnostics
            .iter()
            .map(|d| {
                ["code", "severity", "message", "path"].map(|key| text(field(d, key)).unwrap())
            })
            .collect();
        let expected: Vec<_> = expected
            .iter()
            .map(|(a, b, c, d)| [a, b, c, d].map(|s| s.to_string()))
            .collect();
        assert_eq!(actual, expected, "{extra}");
    }
}

/// #327 (sibling of #310): `load_project` anchors a relative `cwd` like
/// `path.resolve`, exactly as `load_snapshot` does, so reported paths are the
/// same as with the absolute cwd.
#[test]
fn load_project_anchors_relative_cwd_like_load_snapshot() {
    let root = std::env::temp_dir().join(format!("pdrs-load-project-cwd-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&root).expect("own temp root");
    struct Cleanup(PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    std::fs::create_dir_all(root.join("proj/.pilotdeck")).unwrap();
    std::fs::write(root.join("proj/.pilotdeck/pilotdeck.yaml"), "agent: [\n").unwrap();
    std::fs::write(
        root.join("pilotdeck.yaml"),
        "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: http://127.0.0.1:28794/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n",
    )
    .unwrap();
    let process_cwd = std::env::current_dir().unwrap();
    let mut relative = PathBuf::new();
    for _ in process_cwd.components().skip(1) {
        relative.push("..");
    }
    relative.push(root.strip_prefix("/").unwrap());
    assert!(relative.is_relative());
    let env = Env::from([("PILOT_HOME".to_owned(), root.to_string_lossy().into_owned())]);
    let diagnostics = |cwd: &Path| match load_project(&root, cwd, &env, Some("proj")) {
        Ok(config) => stringify_json(&JsValue::Array(config.diagnostics))
            .unwrap()
            .unwrap(),
        Err(error) => stringify_json(&JsValue::Array(error.diagnostics))
            .unwrap()
            .unwrap(),
    };
    let absolute = diagnostics(&root);
    assert!(absolute.contains(&format!(
        "{}/proj/.pilotdeck/pilotdeck.yaml",
        root.display()
    )));
    assert_eq!(diagnostics(&relative), absolute);
}

/// #2427: the snapshot's platform-key table and the loader's
/// `CHANNEL_LOADER_KEYS` have no derivation tying them together — a loader
/// key missing from the whitelist never reaches `config.adapters`, so the
/// loader's `cfg?.enabled` gate never even looks at it and the channel
/// silently skips (#1853 form; on this seam there is no console.error line at
/// all). The seam drives the real `load_snapshot` over a scratch home that
/// configures all 16 platform keys, and holds the two tables together from
/// the output side: every loader key plus the `wecom` bundle key (the one
/// platform-table member the loader table legitimately lacks) must appear in
/// the snapshot's `adapters`. Deriving the snapshot table from the loader
/// table was rejected: TS hardcodes its own `PLATFORM_KEYS`
/// (`parseGatewayConfig.ts`), so a Rust derivation would break the literal
/// mirror of the TS baseline the two lists both are.
#[test]
fn snapshot_adapters_cover_every_channel_loader_key() {
    let root = std::env::temp_dir().join(format!("pdrs-2427-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("#2427 scratch home");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    let mut yaml = String::from(
        "schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: test-only\n      models:\n        test: {}\nadapters:\n",
    );
    for key in super::snapshot::PLATFORM_SNAPSHOT_KEYS {
        yaml.push_str(&format!("  {key}:\n    enabled: true\n"));
    }
    std::fs::write(root.join("pilotdeck.yaml"), yaml).expect("#2427 scratch config");
    let env = Env::from([("PILOT_HOME".to_owned(), root.to_string_lossy().into_owned())]);
    let snapshot = load_snapshot(&root, &root, &env, None, 1.0, "1970-01-01T00:00:00.000Z")
        .expect("#2427 the all-keys scratch config loads");
    let adapters = snapshot
        .get("config")
        .and_then(|config| config.get("adapters"))
        .expect("#2427 the snapshot records an adapters object");
    for key in crate::channel::loader::CHANNEL_LOADER_KEYS {
        crate::negative_assert!(
            adapters.get(key).is_some(),
            "#2427 the config snapshot carries loader key {key} into config.adapters"
        );
    }
    crate::negative_assert!(
        adapters.get("wecom").is_some(),
        "#2427 the config snapshot carries the wecom bundle key into config.adapters"
    );
}

/// #2428 review r1 (arbitrated against the TS source): the TS snapshot layer
/// itself folds a non-string `apiKey` — `parsePlatformAdapter` reads it with
/// `stringField` (`parseGatewayConfig.ts:102-112`), which keeps only strings
/// and otherwise yields `undefined` — and Rust's `platform()` mirrors that
/// with `as_string()`. The never-fold guarantee #2428 pins therefore lives at
/// the loader's option/`extra` mount seam (where
/// `non_string_api_keys_never_fold_to_unset` drives it); this pin holds the
/// snapshot side of the split, so the two layers cannot silently swap roles
/// and leave the mount test vacuously green.
#[test]
fn snapshot_folds_a_non_string_platform_api_key_like_string_field() {
    let root = std::env::temp_dir().join(format!("pdrs-2428-fold-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).expect("#2428 scratch home");
    struct Cleanup(std::path::PathBuf);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _cleanup = Cleanup(root.clone());
    std::fs::write(
        root.join("pilotdeck.yaml"),
        "schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:1\n      apiKey: test-only\n      models:\n        test: {}\nadapters:\n  apiServer:\n    enabled: true\n    apiKey: 123\n  telegram:\n    enabled: true\n    apiKey: []\n",
    )
    .expect("#2428 scratch config");
    let env = Env::from([("PILOT_HOME".to_owned(), root.to_string_lossy().into_owned())]);
    let snapshot = load_snapshot(&root, &root, &env, None, 1.0, "1970-01-01T00:00:00.000Z")
        .expect("#2428 the non-string apiKey config loads");
    for key in ["apiServer", "telegram"] {
        let folded = snapshot
            .get("config")
            .and_then(|config| config.get("adapters"))
            .and_then(|adapters| adapters.get(key))
            .and_then(|adapter| adapter.get("apiKey"));
        crate::negative_assert!(
            matches!(folded, None | Some(JsValue::Undefined)),
            "#2428 the snapshot folds a non-string platform apiKey to undefined ({key})"
        );
    }
}

/// #261: config-source merging must read `output[key]` through the explicit
/// prototype chain and keep the key's UTF-16 units. Here a lone-surrogate key
/// lives on the prototype object and is then overridden: the old
/// `to_utf8().unwrap_or_default()` read `""`, replaced the record instead of
/// merging it. Expected value from the real TS `mergeConfigSources`
/// (`_tmp/20261020-p2-sweep-7/probe-261.out`).
#[test]
fn merge_reads_prototype_values_with_utf16_keys_like_javascript() {
    let key = JsString::from_utf16(vec![0xd800]);
    let mut prototype = JsObject::new();
    prototype.insert(key.clone(), super::object([("a", JsValue::Number(1.0))]));
    let mut right = JsObject::new();
    right.insert("__proto__".into(), JsValue::Object(prototype));
    right.insert(key.clone(), super::object([("b", JsValue::Number(2.0))]));
    let mut left = JsValue::Object(JsObject::new());
    super::merge(&mut left, JsValue::Object(right));
    let merged = left
        .as_object()
        .unwrap()
        .get_key(&key)
        .expect("merged surrogate key");
    assert_eq!(field(merged, "a"), &JsValue::Number(1.0), "{left:?}");
    assert_eq!(field(merged, "b"), &JsValue::Number(2.0));
}

/// #2301 fix-r3 review P1 #2: the real merge path installs the null
/// prototype. Two config sources hold `adapters.wecomCallback.extra.port` as
/// records (`{}` and `{__proto__: null}`), so the nested merge reaches the
/// `__proto__` arm with a null value and sets it as the merged port's
/// prototype — exactly the `mergeConfigSources` `output[key] = value` setter
/// behavior the TS side shows (Node v22.22.0 probe
/// `_tmp/20261026-issue-2301/fix-r3/node-probe/probe2.out`: after the real
/// yaml → merge → loadEnabledChannels pipeline, `Object.getPrototypeOf(port)`
/// is null and the loader records
/// `[adapters] Failed to load channel "wecomCallback": TypeError: Cannot
/// convert object to primitive value` while slack/bluebubbles/sms still
/// construct). The shared number guard must reject the merged shape, not only
/// a hand-installed prototype.
#[test]
fn merged_null_prototype_port_reports_the_number_conversion_error() {
    let mut left = parse_json(r#"{"adapters": {"wecomCallback": {"extra": {"port": {}}}}}"#)
        .expect("#2301 merge fixture base parses");
    let right =
        parse_json(r#"{"adapters": {"wecomCallback": {"extra": {"port": {"__proto__": null}}}}}"#)
            .expect("#2301 merge fixture override parses");
    super::merge(&mut left, right);
    let port = left
        .get("adapters")
        .and_then(|adapters| adapters.get("wecomCallback"))
        .and_then(|wecom| wecom.get("extra"))
        .and_then(|extra| extra.get("port"))
        .expect("#2301 the merged port is present");
    let JsValue::Object(object) = port else {
        panic!("#2301 the merged port is an object");
    };
    assert_eq!(
        object.prototype(),
        Some(&JsValue::Null),
        "#2301 the real merge installs null as the port's prototype"
    );
    assert_eq!(
        crate::js_value::number_conversion_error(port),
        Some("TypeError: Cannot convert object to primitive value".to_owned()),
        "#2301 a port whose chain terminates at null finds neither coercion method — \
         Number() throws like TS, the channel must not construct on the default port"
    );
}

/// #934 P1-3: the StaffDeck binding path message must keep the shared
/// descriptor's raw UTF-16 units instead of `from_utf16_lossy`.
#[test]
fn binding_path_argument_preserves_lone_surrogate_units() {
    let value = parse_json(r#"{"constructor":{"name":"\ud800"}}"#).unwrap();
    let received = match crate::metadata::argument_received(&value) {
        crate::metadata::ArgumentReceived::Text(received) => received,
        crate::metadata::ArgumentReceived::Throws { .. } => panic!("#934 unexpected throw"),
    };
    let error = binding::invalid_path_argument(&received);
    assert_eq!(
        error.code, "ERR_INVALID_ARG_TYPE",
        "#934 binding path argument must keep the Node code"
    );
    assert_eq!(
        error.message.units().last(),
        Some(&0xd800),
        "#934 binding path argument must keep the lone surrogate unit"
    );
    assert!(
        String::from_utf16_lossy(error.message.units())
            .starts_with("The \"path\" argument must be of type string. Received an instance of "),
        "#934 binding path argument must keep the Node wording"
    );
}

/// #1550 修 r5: every diagnostic message is decoded by `DiagnosticText`, which
/// reads U+001F as its own escape prefix and hits `unreachable!` on any other
/// tail. `js_string` therefore has to escape arrays exactly like scalars; the
/// r4 explicit-stack rewrite handed the decoder raw UTF-16 and a config value
/// `schemaVersion: ["\u001fX"]` aborted the gateway.
#[test]
fn array_stringification_keeps_the_diagnostic_escape() {
    let value = parse_json("[\"\\u001fX\",\"\\ud800\"]").unwrap();
    assert_eq!(
        DiagnosticText::from(js_string(&value)).0.units(),
        &[0x1f, u16::from(b'X'), u16::from(b','), 0xd800],
        "array elements must keep the injective diagnostic escape"
    );
}

/// The same input through the real config entry, where the message goes
/// through `Diagnostics::check` and back out of `load`.
#[test]
fn config_array_values_keep_control_units_in_diagnostics() {
    let metadata = Metadata {
        home: PathBuf::from("<PILOT_HOME>"),
        cwd: PathBuf::from("/offline"),
        files: FixtureFiles {
            data: "schemaVersion: [\"\\x1fX\",\"\\ud800\"]\n"
                .as_bytes()
                .to_vec(),
            kind: None,
        },
    };
    let error = match super::load(&metadata, &Env::new()) {
        Ok(_) => panic!("fatal schemaVersion diagnostic must fail the load"),
        Err(error) => error,
    };
    assert_eq!(error.code, "CONFIG_SCHEMA_VERSION_UNSUPPORTED");
    assert_eq!(
        error.message.units(),
        "Unsupported schemaVersion "
            .encode_utf16()
            .chain([
                0x1f,
                u16::from(b'X'),
                u16::from(b','),
                0xd800,
                u16::from(b'.')
            ])
            .collect::<Vec<_>>(),
        "the fatal message must carry the array's units unchanged"
    );
}

/// A YAML `!!set` is a real JS `Set` (`String(set)` is `[object Set]`, measured
/// on the baseline `yaml@2.9.0`), and the config graph keeps that tag on the
/// `JsValue` object. Inside an array the joined text must read the tag too:
/// the r4 explicit-stack join hardcoded `[object Object]`, so a diagnostic
/// carrying a set element printed the wrong object class (#1550 修 r6).
#[test]
fn array_stringification_keeps_the_yaml_collection_tag() {
    let mut set = pilotdeck_protocol::JsObject::new();
    set.set_native_tag("Set");
    let value = JsValue::Array(vec![JsValue::Object(set), JsValue::Number(3.0)]);
    assert_eq!(
        DiagnosticText::from(js_string(&value)).0.units(),
        "[object Set],3".encode_utf16().collect::<Vec<_>>(),
        "#1833 C1: the joiner keeps the collection tag (#1550 修 r6)"
    );
}

/// The same tag through the real config entry.
#[test]
fn config_array_set_values_keep_the_collection_tag() {
    let metadata = Metadata {
        home: PathBuf::from("<PILOT_HOME>"),
        cwd: PathBuf::from("/offline"),
        files: FixtureFiles {
            data: "schemaVersion: [!!set {a: }, 3]\n".as_bytes().to_vec(),
            kind: None,
        },
    };
    let error = match super::load(&metadata, &Env::new()) {
        Ok(_) => panic!("fatal schemaVersion diagnostic must fail the load"),
        Err(error) => error,
    };
    assert_eq!(error.code, "CONFIG_SCHEMA_VERSION_UNSUPPORTED");
    assert_eq!(
        error.message.units(),
        "Unsupported schemaVersion [object Set],3."
            .encode_utf16()
            .collect::<Vec<_>>()
    );
}

/// A bare tagged object takes the `js_string` object arm, not the array join:
/// the two implementations share one tag source, so this arm must keep the
/// class too (#1833 C1).
#[test]
fn config_object_value_keeps_the_collection_tag() {
    let metadata = Metadata {
        home: PathBuf::from("<PILOT_HOME>"),
        cwd: PathBuf::from("/offline"),
        files: FixtureFiles {
            data: "schemaVersion: !!set {a: }\n".as_bytes().to_vec(),
            kind: None,
        },
    };
    let error = match super::load(&metadata, &Env::new()) {
        Ok(_) => panic!("fatal schemaVersion diagnostic must fail the load"),
        Err(error) => error,
    };
    assert_eq!(error.code, "CONFIG_SCHEMA_VERSION_UNSUPPORTED", "#1833 C1");
    assert_eq!(
        error.message.units(),
        "Unsupported schemaVersion [object Set]."
            .encode_utf16()
            .collect::<Vec<_>>(),
        "#1833 C1: the object arm reads the same tag source as the joiner"
    );
}

/// Both YAML load sites (`mod.rs` schemaVersion, `sections.rs`
/// alwaysOn.language) must pass the class through; the third
/// `require_string_coercible` caller (`binding.rs`'s `encodeURIComponent`) is
/// pinned by its own binding test
/// (`uri_component_coercion_is_bounded_instead_of_aborting`). Testing
/// `string_coercible` alone left them free to re-collapse the error into a
/// `TypeError`; a YAML mapping with an own `toString: null` is the reachable
/// thrower, and TS calls `String(value)` on it unguarded (#1550 修 r6, r6 评审
/// P2).
#[test]
fn coercion_error_class_survives_both_config_call_sites() {
    for (site, data) in [
        ("schemaVersion", "schemaVersion: {toString: }\n"),
        (
            "alwaysOn.language",
            concat!(
                "schemaVersion: 1\n",
                "agent: {model: parity/deterministic}\n",
                "model:\n",
                "  providers:\n",
                "    parity:\n",
                "      protocol: openai\n",
                "      url: http://127.0.0.1:28794/v1\n",
                "      apiKey: mock-only\n",
                "      models:\n",
                "        deterministic: {}\n",
                "alwaysOn:\n",
                "  language: {toString: }\n",
            ),
        ),
    ] {
        let metadata = Metadata {
            home: PathBuf::from("<PILOT_HOME>"),
            cwd: PathBuf::from("/offline"),
            files: FixtureFiles {
                data: data.as_bytes().to_vec(),
                kind: None,
            },
        };
        let error = match super::load(&metadata, &Env::new()) {
            Ok(_) => panic!("#1833 {site}: a throwing String(value) must fail the load"),
            Err(error) => error,
        };
        assert_eq!(
            error.name, "TypeError",
            "#1833 {site}: the coercion error class must survive the load"
        );
        assert_eq!(
            error.message.to_utf8().ok().as_deref(),
            Some("Cannot convert object to primitive value"),
            "#1833 {site}: the V8 wording must survive the load"
        );
    }
}

/// `schemaVersion` / `alwaysOn.language` reject a throwing `String(value)`
/// before reading it, so the kind has to survive `string_coercible`: TS calls
/// `String(value)` there and reports `RangeError` past the depth bound,
/// `TypeError` for an own `toString` (#1550 修 r5). YAML caps nesting at 782
/// (`pilotdeck-yaml` `CONVERSION_DEPTH`), so the deep case is only reachable
/// if a JSON config source lands later.
#[test]
fn string_coercion_errors_keep_their_class() {
    let mut deep = JsValue::Number(1.0);
    for _ in 0..crate::js_value::MAX_ARRAY_DEPTH + 1 {
        deep = JsValue::Array(vec![deep]);
    }
    assert_eq!(
        string_coercible(&deep).map_err(|kind| kind.name()),
        Err("RangeError")
    );
    assert_eq!(
        string_coercible(&object([("toString", JsValue::Null)])).map_err(|kind| kind.name()),
        Err("TypeError")
    );
    assert_eq!(string_coercible(&JsValue::from("1")), Ok(()));
}

/// #1833 B1: the shared check-and-map both YAML load sites (and
/// `encodeURIComponent`) go through. The load-level tests above can only
/// produce `NotPrimitive` — the YAML depth cap (782) keeps a `StackOverflow`
/// out of a real `load` — so the helper pins both kinds directly: a mapping
/// collapsed back to one hardcoded kind fails here even while every
/// load-level test stays green.
#[test]
fn require_string_coercible_keeps_both_error_kinds() {
    let mut deep = JsValue::Number(1.0);
    for _ in 0..crate::js_value::MAX_ARRAY_DEPTH + 1 {
        deep = JsValue::Array(vec![deep]);
    }
    let error = require_string_coercible(&deep)
        .expect_err("#1833 nesting past the shared depth bound must throw");
    assert_eq!(
        error.name, "RangeError",
        "#1833 StackOverflow must map to RangeError, not a collapsed class"
    );
    assert_eq!(
        error.message.to_utf8().ok().as_deref(),
        Some(crate::js_value::StackOverflow::MESSAGE),
        "#1833 the RangeError must keep the V8 wording"
    );
    let error = require_string_coercible(&object([("toString", JsValue::Null)]))
        .expect_err("#1833 an own toString property must throw");
    assert_eq!(
        error.name, "TypeError",
        "#1833 NotPrimitive must map to TypeError, not a collapsed class"
    );
    assert_eq!(
        error.message.to_utf8().ok().as_deref(),
        Some(crate::js_value::ToStringError::NOT_PRIMITIVE),
        "#1833 the TypeError must keep the V8 wording"
    );
    assert!(
        require_string_coercible(&JsValue::from("1")).is_ok(),
        "#1833 a coercible value must not throw"
    );
}
