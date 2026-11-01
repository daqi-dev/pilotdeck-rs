use crate::metadata::{Files, Metadata, Stat};
use pilotdeck_protocol::{JsValue, parse_json, stringify_json};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

const MATRIX: &str = include_str!("../surrogate-collation-matrix/oracle.json");
const BASE: &str = "schemaVersion: 1\nagent: {model: parity/deterministic}\nmodel:\n  providers:\n    parity:\n      protocol: openai\n      url: https://example.invalid/v1\n      apiKey: mock-only\n      models:\n        deterministic: {}\n        alternate: {}\n";
const DEADLINE: Duration = Duration::from_secs(5);

fn field<'a>(v: &'a JsValue, key: &str) -> &'a JsValue {
    v.get(key).expect("#132 fixture field exists")
}
fn text(v: &JsValue) -> String {
    v.as_string()
        .expect("#132 fixture string")
        .to_utf8()
        .expect("#132 fixture envelope UTF-8")
}
fn array(v: &JsValue) -> &[JsValue] {
    let JsValue::Array(items) = v else {
        panic!("#132 fixture array")
    };
    items
}
fn encode(v: &JsValue) -> String {
    stringify_json(v)
        .expect("#132 serializable fixture")
        .expect("#132 defined fixture")
}
fn matrix() -> JsValue {
    let v = parse_json(MATRIX).expect("#132 matrix JSON");
    assert_eq!(text(field(&v, "node")), "v22.22.0", "#132 pinned Node");
    assert_eq!(text(field(&v, "icu")), "78.2", "#132 pinned ICU");
    assert_eq!(array(field(&v, "locales")).len(), 5, "#132 five locales");
    v
}

struct MatrixFiles(BTreeMap<PathBuf, Option<Vec<u8>>>);
impl Files for MatrixFiles {
    fn entries(&self, path: &Path) -> io::Result<Vec<(String, bool)>> {
        if path == Path::new("/fixture/home/projects") {
            Ok(vec![("registered".into(), true)])
        } else {
            Err(io::Error::from_raw_os_error(libc::ENOENT))
        }
    }
    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        self.0
            .get(path)
            .and_then(Clone::clone)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))
    }
    fn stat(&self, path: &Path) -> io::Result<Stat> {
        let value = self
            .0
            .get(path)
            .ok_or_else(|| io::Error::from_raw_os_error(libc::ENOENT))?;
        Ok(Stat {
            directory: value.is_none(),
            size: value.as_ref().map_or(0, |b| b.len() as u64),
            birth_ms: 0.0,
            ctime_ms: 0.0,
            mtime_ms: 0.0,
        })
    }
}
fn metadata(case: &JsValue) -> Metadata<MatrixFiles> {
    let catalog = text(field(case, "method")) == "model_catalog_list";
    let yaml = if catalog {
        text(field(case, "yaml"))
    } else {
        BASE.into()
    };
    let mut m = Metadata {
        home: "/fixture/home".into(),
        cwd: "/fixture".into(),
        files: MatrixFiles(BTreeMap::from([
            ("/fixture/project".into(), None),
            ("/fixture/home/projects/registered".into(), None),
            (
                "/fixture/home/projects/registered/.cwd".into(),
                Some(b"/fixture/project\n".to_vec()),
            ),
            (
                "/fixture/home/pilotdeck.yaml".into(),
                Some(yaml.into_bytes()),
            ),
        ])),
    };
    if !catalog {
        let bytes = array(field(case, "entries"))
            .iter()
            .map(encode)
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        let path = m
            .process_chat_dir("/fixture/project")
            .expect("#132 transcript location")
            .join("web:fixture.jsonl");
        m.files.0.insert(path, Some(bytes.into_bytes()));
    }
    m
}

#[test]
fn native_matrix_matches_node() {
    let matrix = matrix();
    let mut count = 0;
    for row in array(field(&matrix, "locales")) {
        let lang = text(field(row, "lang"));
        let env = crate::config::Env::from([
            ("LANG".into(), lang.clone()),
            ("PILOT_HOME".into(), "/fixture/home".into()),
        ]);
        let cases = array(field(row, "cases"));
        assert_eq!(cases.len(), 20, "#132 twenty method/key cells per locale");
        for case in cases {
            let request = parse_json(&text(field(case, "request"))).expect("#132 request JSON");
            let m = metadata(case);
            let response = if text(field(case, "method")) == "model_catalog_list" {
                crate::model_catalog::response(&request, &m, &env)
            } else {
                crate::session_model::response(&request, &m, &env)
            };
            let actual = response.expect("#132 matrix must terminate natively");
            assert_eq!(
                actual,
                text(field(case, "response")).as_bytes(),
                "#132 matrix wire bytes {lang}/{}",
                text(field(case, "id"))
            );
            println!("#132 aligned {lang}/{}", text(field(case, "id")));
            count += 1;
        }
    }
    assert_eq!(count, 100, "#132 all original cells replayed");
}

#[test]
fn native_comparator_matches_node_all_pairs() {
    let oracle = parse_json(include_str!("oracle.json")).expect("#132 comparator oracle JSON");
    let keys = array(field(&oracle, "keys"));
    assert_eq!(keys.len(), 62, "#132 comparator key coverage");
    let rows = array(field(&oracle, "rows"));
    assert_eq!(rows.len(), 14, "#132 comparator locale coverage");
    let mut failures = Vec::new();
    for row in rows {
        let locale = text(field(row, "locale"));
        let collator =
            crate::project_files::node_collator(&locale.parse().expect("#132 locale syntax"))
                .expect("#132 locale data");
        let signs = array(field(row, "signs"));
        assert_eq!(signs.len(), keys.len(), "#132 comparator matrix rows");
        for (i, a) in keys.iter().enumerate() {
            let expected = array(&signs[i]);
            assert_eq!(expected.len(), keys.len(), "#132 comparator matrix columns");
            for (j, b) in keys.iter().enumerate() {
                let actual = crate::project_files::node_compare_utf16(
                    &collator,
                    a.as_string().expect("#132 raw left UTF-16"),
                    b.as_string().expect("#132 raw right UTF-16"),
                );
                let sign = match actual {
                    std::cmp::Ordering::Less => -1.0,
                    std::cmp::Ordering::Equal => 0.0,
                    std::cmp::Ordering::Greater => 1.0,
                };
                if Some(sign) != expected[j].as_number() {
                    failures.push(format!(
                        "{locale}/{i}/{j}: {} vs {} actual={sign} expected={}",
                        encode(a),
                        encode(b),
                        encode(&expected[j])
                    ));
                }
            }
        }
    }
    assert!(
        failures.is_empty(),
        "#132 comparator raw weights: {} mismatches\n{}",
        failures.len(),
        failures.into_iter().take(30).collect::<Vec<_>>().join("\n")
    );
}

async fn write(io: &mut DuplexStream, bytes: &[u8]) {
    tokio::time::timeout(DEADLINE, io.write_all(bytes))
        .await
        .expect("#132 pairing write deadline")
        .expect("#132 pairing write");
}

#[test]
fn preserving_identical_strength_keeps_normalization_and_ignorables() {
    use icu_collator::options::{CollatorOptions, Strength};
    let mut options = CollatorOptions::default();
    options.strength = Some(Strength::Identical);
    let collator = icu_collator::Collator::try_new(Default::default(), options)
        .expect("#132 identical collator data");
    assert_eq!(
        collator.compare_utf16_preserving_surrogates(&[0xd800, 0xe9], &[0xd800, 0x65, 0x301]),
        std::cmp::Ordering::Equal,
        "#132 identical canonical equivalence"
    );
    assert_eq!(
        collator.compare_utf16_preserving_surrogates(&[0xd800, 0], &[0xd800]),
        std::cmp::Ordering::Greater,
        "#132 identical retains primary ignorables"
    );
    assert_eq!(
        collator.compare_utf16_preserving_surrogates(
            &[0xd800, 0xfffe, 0xe9],
            &[0xd800, 0xfffe, 0x65, 0x301]
        ),
        std::cmp::Ordering::Equal,
        "#132 identical canonical equivalence across merge separator"
    );
}
async fn read(io: &mut DuplexStream, expected: &[u8]) {
    let mut actual = vec![0; expected.len()];
    tokio::time::timeout(DEADLINE, io.read_exact(&mut actual))
        .await
        .expect("#132 pairing response deadline: residual host boundary")
        .expect("#132 pairing read");
    assert_eq!(
        actual, expected,
        "#132 pairing wire bytes / zero business upstream"
    );
}

#[test]
fn matrix_pairing_has_zero_business_upstream() {
    let matrix = matrix();
    crate::test_process::run_cases(
        "pairing::native_collation_tests::matrix_pairing_worker",
        array(field(&matrix, "locales"))
            .iter()
            .map(|row| {
                let lang = text(field(row, "lang"));
                (
                    lang.clone(),
                    vec![
                        ("LANG".into(), lang),
                        ("PILOT_HOME".into(), "/fixture/home".into()),
                    ],
                )
            })
            .collect(),
    );
}

#[tokio::test]
#[ignore = "#132 locale-isolated worker; exercised by the parent test"]
async fn matrix_pairing_worker() {
    let lang = std::env::var("LANG").expect("#132 worker locale");
    let matrix = matrix();
    let row = array(field(&matrix, "locales"))
        .iter()
        .find(|row| text(field(row, "lang")) == lang)
        .expect("#132 recorded locale");
    let cases = array(field(row, "cases"));
    assert_eq!(cases.len(), 20, "#132 worker full locale row");
    for case in cases {
        let (mut client, downstream) = tokio::io::duplex(1024 * 1024);
        let (upstream, mut observer) = tokio::io::duplex(1024 * 1024);
        let task = tokio::spawn(super::run_with_metadata(
            downstream,
            upstream,
            Some(metadata(case)),
        ));
        let hello = crate::frame::client_frame(0x81, 126, br#"{"type":"hello","protocolVersion":"1.2","clientName":"web","clientVersion":"1","token":"fixture"}"#);
        write(&mut client, &hello).await;
        read(&mut observer, &hello).await;
        let hello_ok = crate::frame::server_frame(1, br#"{"type":"hello_ok","protocolVersion":"1.2","serverVersion":"fixture","serverInfo":{},"interactionBinding":{"connectionId":"ts-owned","generation":1}}"#);
        write(&mut observer, &hello_ok).await;
        read(&mut client, &hello_ok).await;
        let request =
            crate::frame::client_frame(0x81, 127, text(field(case, "request")).as_bytes());
        let expected = crate::frame::server_frame(1, text(field(case, "response")).as_bytes());
        write(&mut client, &request).await;
        read(&mut client, &expected).await;
        // A causal fence: the next upstream bytes must be this unrelated
        // forwarded method, never any of the locally answered business frame.
        let fence = crate::frame::client_frame(
            0x81,
            127,
            br#"{"type":"request","id":"132-fence","method":"describe_server"}"#,
        );
        write(&mut client, &fence).await;
        read(&mut observer, &fence).await;
        observer.shutdown().await.expect("#132 observer shutdown");
        tokio::time::timeout(DEADLINE, task)
            .await
            .expect("#132 pairing task deadline")
            .expect("#132 pairing task join")
            .expect("#132 pairing task result");
    }
    crate::test_process::receipt(&lang);
}
