//! Isolated release resource probes. See run_resource_scaling.py --help and README.
use pilotdeck_protocol::JsString;
use pilotdeck_yaml::{Options, parse_all_documents};
use std::{hint::black_box, time::Instant};

fn require_release() {
    #[cfg(debug_assertions)]
    panic!("run with --release");
}

#[test]
#[ignore = "release-only Node v22 resource comparison; requires PILOTDECK_YAML_BASELINE"]
fn resource_scaling() {
    require_release();
    let baseline = std::env::var("PILOTDECK_YAML_BASELINE").expect("baseline checkout path");
    let status = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/run_resource_scaling.py"
        ))
        .arg("--baseline")
        .arg(baseline)
        .arg("--rust-bin")
        .arg(std::env::current_exe().unwrap())
        .status()
        .unwrap();
    assert!(
        status.success(),
        "resource limits exceeded; see result table"
    );
}

#[test]
#[ignore = "isolated subprocess worker for resource_scaling"]
fn resource_worker() {
    require_release();
    let file = std::env::var("PILOTDECK_YAML_RESOURCE_INPUT").expect("worker input");
    let text = std::fs::read_to_string(file).unwrap();
    let source: JsString = text.as_str().into();
    drop(text);
    let start = Instant::now();
    let docs = parse_all_documents(&source, Options::default());
    let parsed = start.elapsed().as_secs_f64();
    let values: Vec<_> = docs.iter().map(|doc| doc.to_json()).collect();
    let elapsed = start.elapsed().as_secs_f64();
    let errors: usize = docs.iter().map(|doc| doc.errors.len()).sum();
    let warnings: usize = docs.iter().map(|doc| doc.warnings.len()).sum();
    let conversion_errors = values.iter().filter(|value| value.is_err()).count();
    println!(
        "RESOURCE_SAMPLE {}",
        serde_json::json!({"seconds": elapsed, "parse_seconds": parsed,
            "documents": docs.len(), "errors": errors, "warnings": warnings, "conversion_errors": conversion_errors})
    );
    black_box((&docs, &values));
}

#[test]
fn all_documents_share_lines_and_preserve_positions() {
    let counter = std::rc::Rc::new(std::cell::RefCell::new(Default::default()));
    let docs = parse_all_documents(
        &"---\n🦀: 1\n---\n\"\\q\"\n".into(),
        Options {
            line_counter: Some(counter.clone()),
            ..Default::default()
        },
    );
    assert_eq!(docs.len(), 2);
    assert!(std::sync::Arc::ptr_eq(
        &docs[0].line_counter,
        &docs[1].line_counter
    ));
    assert_eq!(
        docs[0].line_counter.line_starts,
        counter.borrow().line_starts
    );
    assert_eq!(
        docs[1].line_counter.line_pos(14),
        pilotdeck_yaml::parse::line_counter::LinePos { line: 4, col: 1 }
    );
    assert_eq!(docs[1].errors[0].line_pos.unwrap()[0].line, 4);
}

#[test]
fn collection_key_temporaries_do_not_escape_the_value_graph() {
    let doc = pilotdeck_yaml::parse_document(&"? ? [x, y]\n: 1\n".into(), Options::default());
    assert!(doc.errors.is_empty());
    let graph = doc.to_json().unwrap();
    assert_eq!(
        graph.nodes.len(),
        1,
        "converted collection keys are strings, not live objects"
    );
    let value = graph.to_js_value().unwrap();
    assert!(value.as_object().is_some());
}

#[test]
fn long_scalar_aliases_preserve_utf16_with_default_limit() {
    let source = format!(
        "[&a \"{}\", {}]",
        "x".repeat(80_000),
        vec!["*a"; 99].join(", ")
    );
    let doc = pilotdeck_yaml::parse_document(&source.into(), Options::default());
    assert!(doc.errors.is_empty());
    let graph = doc.to_json().unwrap();
    let pilotdeck_yaml::value::Value::Ref(root) = graph.root else {
        panic!("array root");
    };
    let pilotdeck_yaml::value::Container::Array(items) = &graph.nodes[root] else {
        panic!("array");
    };
    let pilotdeck_yaml::value::Value::String(first) = &items[0] else {
        panic!("string");
    };
    for value in items {
        let pilotdeck_yaml::value::Value::String(value) = value else {
            panic!("string alias");
        };
        assert_eq!(value, first);
        assert!(std::sync::Arc::ptr_eq(value, first));
        assert_eq!(value.len_utf16(), 80_000);
    }
}

#[test]
fn plain_scalar_storage_is_shared_only_inside_yaml() {
    use pilotdeck_yaml::{
        nodes::{NodeKind, ScalarValue},
        value::Value,
    };
    let source = JsString::from_utf16(vec![0xd800; 80_000]);
    let doc = pilotdeck_yaml::parse_document(
        &source,
        Options {
            keep_source_tokens: true,
            ..Default::default()
        },
    );
    assert!(doc.errors.is_empty());
    let node = &doc.nodes[doc.contents.unwrap()];
    let NodeKind::Scalar(ScalarValue::String(scalar)) = &node.kind else {
        panic!("string scalar");
    };
    assert!(std::sync::Arc::ptr_eq(scalar, &node.source));
    assert!(std::sync::Arc::ptr_eq(
        scalar,
        node.src_token.as_ref().unwrap().source.as_ref().unwrap()
    ));
    let graph = doc.to_json().unwrap();
    let Value::String(value) = &graph.root else {
        panic!("string value");
    };
    assert!(std::sync::Arc::ptr_eq(scalar, value));
    let value = graph.to_js_value().unwrap();
    assert_eq!(value.as_string().unwrap(), &source);
    drop(doc);
    assert_eq!(graph.to_js_value().unwrap(), value);
}
