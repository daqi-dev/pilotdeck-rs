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
fn verify(raw: &str) -> Vec<String> {
    let mut failures = vec![];
    for test in cases(raw) {
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
        // Fixed Node cases whose filtered sorting keys contain ill-formed UTF-16.
        // Keep the original response bytes; the host owns these comparisons.
        const HOST_COLLATION_CASES: &[&str] = &[
            "diag-2458-catalog-filters",
            "diag-2459-catalog-filters",
            "diag-2462-catalog-filters",
            "diag-2466-catalog-filters",
            "diag-2470-catalog-filters",
            "diag-2471-locale",
            "diag-2473-locale",
            "diag-2477-locale",
            "diag-2478-locale",
            "diag-2479-locale",
            "diag-2480-locale",
            "diag-2483-locale",
            "diag-2485-locale",
            "diag-2489-locale",
            "diag-2491-locale",
            "diag-2495-locale",
            "diag-2497-locale",
        ];
        if HOST_COLLATION_CASES.contains(&id.as_str()) {
            assert!(matches!(route, crate::dispatch::Route::Forward), "{id}");
            continue;
        }
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
    let failures = verify(include_str!("../../tests/fixtures/config/catalog.json"));
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
#[test]
fn configuration_diagnostics_and_catalog_wire() {
    let failures = verify(include_str!("../../tests/fixtures/config/diagnostics.json"));
    let total = failures.len();
    assert!(
        failures.is_empty(),
        "{total} differences; first entries:\n{}",
        failures.into_iter().take(12).collect::<Vec<_>>().join("\n")
    );
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
