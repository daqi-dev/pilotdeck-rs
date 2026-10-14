use pilotdeck_protocol::{JsString, JsValue, parse_json};
use pilotdeck_yaml::parse::lexer::Lexer;
use std::path::Path;

fn fixtures(test: impl FnMut(&JsValue) + Send) {
    // The reference CST decoder / graph observer are recursive test helpers.
    std::thread::scope(|scope| {
        std::thread::Builder::new()
            .stack_size(32 * 1024 * 1024)
            .spawn_scoped(scope, || fixtures_inner(test))
            .unwrap()
            .join()
            .unwrap()
    });
}
fn fixtures_inner(mut test: impl FnMut(&JsValue)) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/differential");
    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(root.join("manifest.json")).unwrap())
            .unwrap();
    let mut total = 0;
    for shard in manifest["shards"].as_array().unwrap() {
        let data = std::fs::read_to_string(root.join(shard["file"].as_str().unwrap())).unwrap();
        assert_eq!(data.len(), shard["bytes"].as_u64().unwrap() as usize);
        assert!(data.len() <= 2_000_000);
        use sha2::{Digest, Sha256};
        assert_eq!(
            format!("{:x}", Sha256::digest(data.as_bytes())),
            shard["sha256"].as_str().unwrap()
        );
        let value = parse_json(&data).unwrap();
        let JsValue::Array(cases) = &value else {
            panic!("cases")
        };
        assert_eq!(cases.len(), shard["cases"].as_u64().unwrap() as usize);
        for case in cases {
            test(case);
            total += 1;
        }
    }
    assert_eq!(total, 3099);
}
fn prop<'a>(v: &'a JsValue, name: &str) -> &'a JsValue {
    let JsValue::Object(o) = v else {
        panic!("object")
    };
    o.get(name).unwrap()
}
fn string(v: &JsValue) -> &JsString {
    let JsValue::String(s) = v else {
        panic!("string")
    };
    s
}
#[test]
fn lexer_matches_all_recordings() {
    fixtures(|case| {
        let source = string(prop(case, "yaml"));
        let actual = Lexer::default().lex(source, false);
        let JsValue::Array(expected) = prop(case, "lex") else {
            panic!("lex")
        };
        let expected: Vec<_> = expected.iter().map(|v| string(v).clone()).collect();
        assert_eq!(
            actual,
            expected,
            "{}",
            string(prop(case, "id")).to_utf8().unwrap()
        );
    });
}
use pilotdeck_yaml::parse::cst::{CollectionItem, Token, TokenType};
use pilotdeck_yaml::parse::parser::Parser;
fn kinds(name: &JsString) -> TokenType {
    use TokenType::*;
    [
        ByteOrderMark,
        DocMode,
        FlowErrorEnd,
        Scalar,
        DocStart,
        DocEnd,
        Newline,
        SeqItemInd,
        ExplicitKeyInd,
        MapValueInd,
        FlowMapStart,
        FlowMapEnd,
        FlowSeqStart,
        FlowSeqEnd,
        Comma,
        Space,
        Comment,
        DirectiveLine,
        Alias,
        Anchor,
        Tag,
        SingleQuotedScalar,
        DoubleQuotedScalar,
        BlockScalarHeader,
        Directive,
        Document,
        Error,
        BlockScalar,
        BlockMap,
        BlockSeq,
        FlowCollection,
    ]
    .into_iter()
    .find(|k| name.equals_str(k.as_str()))
    .unwrap()
}
fn tokens(v: &JsValue) -> Vec<Token> {
    let JsValue::Array(a) = v else {
        panic!("token array")
    };
    a.iter().map(token).collect()
}
fn token(v: &JsValue) -> Token {
    let kind = kinds(string(prop(v, "type")));
    let mut out = Token::new(kind, prop(v, "offset").as_number().unwrap() as usize);
    out.indent = v.get("indent").map(|v| v.as_number().unwrap() as usize);
    out.source = v
        .get("source")
        .map(|v| std::sync::Arc::new(string(v).clone()));
    if kind == TokenType::Document {
        out.start = tokens(prop(v, "start"));
    }
    if kind == TokenType::FlowCollection {
        out.flow_start = Some(std::sync::Arc::new(token(prop(v, "start"))));
    }
    if let Some(JsValue::Array(items)) = v.get("items") {
        out.items = items.iter().map(collection_item).collect();
    }
    if let Some(props) = v.get("props") {
        out.props = tokens(props);
    }
    if let Some(end) = v.get("end") {
        out.end = Some(tokens(end));
    }
    if let Some(value) = v.get("value") {
        out.value = Some(std::sync::Arc::new(token(value)));
    }
    if let Some(message) = v.get("message") {
        out.message = Some(string(message).clone());
    }
    out
}
fn collection_item(v: &JsValue) -> CollectionItem {
    CollectionItem {
        start: tokens(prop(v, "start")),
        key: v.get("key").map(|v| {
            if matches!(v, JsValue::Null) {
                None
            } else {
                Some(std::sync::Arc::new(token(v)))
            }
        }),
        sep: v.get("sep").map(tokens),
        value: v.get("value").map(|v| std::sync::Arc::new(token(v))),
        explicit_key: v
            .get("explicitKey")
            .and_then(JsValue::as_bool)
            .unwrap_or(false),
    }
}
#[test]
fn cst_matches_all_recordings() {
    fixtures(|case| {
        let mut parser = Parser::default();
        let actual = parser.parse(string(prop(case, "yaml")), false);
        let expected = tokens(prop(case, "cst"));
        assert_eq!(
            actual,
            expected,
            "{}",
            string(prop(case, "id")).to_utf8().unwrap()
        );
        let JsValue::Array(lines) = prop(case, "lineStarts") else {
            panic!("lines")
        };
        let expected: Vec<_> = lines
            .iter()
            .map(|v| v.as_number().unwrap() as usize)
            .collect();
        assert_eq!(
            parser.line_counter.line_starts,
            expected,
            "lines {:?}",
            prop(case, "id")
        );
    });
}
use pilotdeck_protocol::{JsObject, stringify_json};
use pilotdeck_yaml::{
    Options,
    errors::Diagnostic,
    parse_document,
    value::{Container, Value as YValue, ValueGraph},
};
fn object(entries: Vec<(&str, JsValue)>) -> JsValue {
    let mut o = JsObject::new();
    for (k, v) in entries {
        o.insert(k.into(), v);
    }
    JsValue::Object(o)
}
fn s(v: impl Into<JsString>) -> JsValue {
    JsValue::String(v.into())
}
fn n(v: usize) -> JsValue {
    JsValue::Number(v as f64)
}
fn diagnostics(list: &[Diagnostic]) -> JsValue {
    JsValue::Array(
        list.iter()
            .map(|d| {
                let mut v = vec![
                    ("name", s(d.name)),
                    ("code", s(d.code)),
                    ("message", s(d.message.clone())),
                    ("pos", JsValue::Array(d.pos.iter().map(|i| n(*i)).collect())),
                ];
                if let Some(lp) = d.line_pos {
                    v.push((
                        "linePos",
                        JsValue::Array(
                            lp.iter()
                                .map(|p| object(vec![("line", n(p.line)), ("col", n(p.col))]))
                                .collect(),
                        ),
                    ));
                }
                object(v)
            })
            .collect(),
    )
}
fn graph(g: &ValueGraph) -> JsValue {
    fn visit(
        g: &ValueGraph,
        v: &YValue,
        nodes: &mut Vec<JsValue>,
        seen: &mut std::collections::HashMap<usize, usize>,
    ) -> JsValue {
        match v {
            YValue::Null => object(vec![("type", s("null"))]),
            YValue::Bool(v) => object(vec![
                ("type", s("boolean")),
                ("value", s(if *v { "true" } else { "false" })),
            ]),
            YValue::Number(v) => {
                let text = if *v == 0.0 && v.is_sign_negative() {
                    "-0".to_owned()
                } else if v.is_nan() {
                    "NaN".into()
                } else if *v == f64::INFINITY {
                    "Infinity".into()
                } else if *v == f64::NEG_INFINITY {
                    "-Infinity".into()
                } else {
                    ryu_js::Buffer::new().format(*v).into()
                };
                object(vec![("type", s("number")), ("value", s(text))])
            }
            YValue::String(v) => object(vec![
                ("type", s("string")),
                (
                    "json",
                    s(stringify_json(&s(v.as_ref().clone())).unwrap().unwrap()),
                ),
            ]),
            YValue::Symbol { description: v, .. } => object(vec![
                ("type", s("symbol")),
                ("value", s(format!("Symbol({})", v.to_utf8().unwrap()))),
            ]),
            YValue::Ref(index) => {
                if let Some(id) = seen.get(index) {
                    return object(vec![("ref", n(*id))]);
                }
                let id = nodes.len();
                seen.insert(*index, id);
                nodes.push(JsValue::Null);
                let mut fields = Vec::new();
                match &g.nodes[*index] {
                    Container::Object(entries) => {
                        fields.push(("type", s("[object Object]")));
                        let mut entries: Vec<_> = entries.iter().collect();
                        fn array_index(s: &JsString) -> Option<u32> {
                            let s = s.to_utf8().ok()?;
                            let n = s.parse::<u32>().ok()?;
                            if n == u32::MAX || n.to_string() != s {
                                None
                            } else {
                                Some(n)
                            }
                        }
                        entries.sort_by_key(|(k, _)| array_index(k).map_or((1, 0), |n| (0, n)));
                        let entries = entries
                            .into_iter()
                            .map(|(k, v)| {
                                JsValue::Array(vec![
                                    s(stringify_json(&s(k.clone())).unwrap().unwrap()),
                                    visit(g, v, nodes, seen),
                                ])
                            })
                            .collect();
                        fields.push(("entries", JsValue::Array(entries)));
                    }
                    Container::Array(items) => {
                        fields.push(("type", s("[object Array]")));
                        let entries = items
                            .iter()
                            .enumerate()
                            .map(|(i, v)| {
                                JsValue::Array(vec![
                                    s(format!("\"{i}\"")),
                                    visit(g, v, nodes, seen),
                                ])
                            })
                            .collect();
                        fields.push(("entries", JsValue::Array(entries)));
                    }
                    Container::Map(entries) => {
                        fields.push(("type", s("[object Map]")));
                        fields.push(("entries", JsValue::Array(Vec::new())));
                        let map = entries
                            .iter()
                            .map(|(k, v)| {
                                JsValue::Array(vec![
                                    visit(g, k, nodes, seen),
                                    visit(g, v, nodes, seen),
                                ])
                            })
                            .collect();
                        fields.push(("map", JsValue::Array(map)));
                    }
                    Container::Set(items) => {
                        fields.push(("type", s("[object Set]")));
                        fields.push(("entries", JsValue::Array(Vec::new())));
                        fields.push((
                            "set",
                            JsValue::Array(
                                items.iter().map(|v| visit(g, v, nodes, seen)).collect(),
                            ),
                        ));
                    }
                }
                nodes[id] = object(fields);
                object(vec![("ref", n(id))])
            }
        }
    }
    let mut nodes = Vec::new();
    let root = visit(g, &g.root, &mut nodes, &mut Default::default());
    object(vec![("root", root), ("nodes", JsValue::Array(nodes))])
}
fn outcome(src: &JsString, pretty: bool) -> JsValue {
    let doc = parse_document(
        src,
        Options {
            pretty_errors: pretty,
            ..Default::default()
        },
    );
    document_outcome(&doc)
}
fn document_outcome(doc: &pilotdeck_yaml::Document) -> JsValue {
    let mut entries = Vec::new();
    if !doc.errors.is_empty() {
        entries.push(("phase", s("document.errors")));
        entries.push(("errors", diagnostics(&doc.errors)));
        entries.push(("warnings", diagnostics(&doc.warnings)));
    } else {
        let result = doc.to_json();
        entries.push((
            "phase",
            s(if result.is_ok() {
                "value"
            } else {
                "toJSON.throw"
            }),
        ));
        entries.push(("errors", diagnostics(&doc.errors)));
        entries.push(("warnings", diagnostics(&doc.warnings)));
        match result {
            Ok(value) => entries.push(("value", graph(&value))),
            Err(e) => entries.push((
                "error",
                object(vec![("name", s(e.name)), ("message", s(e.message))]),
            )),
        }
    }
    object(entries)
}
fn diff(a: &JsValue, b: &JsValue, path: &str) -> Option<String> {
    match (a, b) {
        (JsValue::Object(a), JsValue::Object(b)) => {
            if a.len() != b.len() {
                return Some(format!("{path}: property count {} != {}", a.len(), b.len()));
            }
            for (k, v) in a.entries() {
                let Some(other) = b.get_key(k) else {
                    return Some(format!("{path}: missing {k:?}"));
                };
                if let Some(d) = diff(v, other, &format!("{path}.{}", k.to_utf8().unwrap())) {
                    return Some(d);
                }
            }
            None
        }
        (JsValue::Array(a), JsValue::Array(b)) => {
            if a.len() != b.len() {
                return Some(format!("{path}: length {} != {}", a.len(), b.len()));
            }
            a.iter()
                .zip(b)
                .enumerate()
                .find_map(|(i, (a, b))| diff(a, b, &format!("{path}[{i}]")))
        }
        _ => {
            if a == b {
                None
            } else {
                Some(format!(
                    "{path}: actual {} != expected {}",
                    stringify_json(a).unwrap().unwrap(),
                    stringify_json(b).unwrap().unwrap()
                ))
            }
        }
    }
}
#[test]
fn document_matches_all_recordings() {
    let mut failures = Vec::new();
    fixtures(|case| {
        let src = string(prop(case, "yaml"));
        for (pretty, field) in [(false, "expected"), (true, "pretty")] {
            let actual = outcome(src, pretty);
            if let Some(d) = diff(&actual, prop(case, field), field) {
                failures.push(format!(
                    "{} {d}",
                    string(prop(case, "id")).to_utf8().unwrap()
                ));
            }
        }
    });
    let report = failures.join("\n");
    if let Ok(path) = std::env::var("PILOTDECK_YAML_REPORT") {
        std::fs::write(path, &report).unwrap();
    }
    if !failures.is_empty() {
        panic!(
            "{} mismatches:\n{}",
            failures.len(),
            failures
                .iter()
                .take(20)
                .cloned()
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
}

// A separate oracle entry lets the isolated mutation runner prove that the new
// review probes themselves catch each branch, even when older fixtures fail too.
#[test]
fn review_round4_branches_match_node() {
    let JsValue::Array(ref cases) =
        parse_json(include_str!("fixtures/differential/round4-000.json")).unwrap()
    else {
        panic!("round4 cases");
    };
    assert_eq!(cases.len(), 42);
    for case in cases {
        let source = string(prop(case, "yaml"));
        for (pretty, field) in [(false, "expected"), (true, "pretty")] {
            assert_eq!(
                diff(&outcome(source, pretty), prop(case, field), field),
                None,
                "{:?}",
                prop(case, "id")
            );
        }
    }
}
#[test]
fn original_71_cases_are_unchanged_and_pass() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/yaml.json");
    let original = std::fs::read_to_string(path).unwrap();
    use sha2::{Digest, Sha256};
    assert_eq!(
        format!("{:x}", Sha256::digest(original.as_bytes())),
        "d564c505dc5705f9543fb716361ed721f9c4d7181508f2f51ba1e8deae712d71"
    );
    let data = parse_json(&original).unwrap();
    let JsValue::Array(cases) = prop(&data, "cases") else {
        panic!("cases")
    };
    assert_eq!(cases.len(), 71);
    for case in cases {
        let actual = outcome(string(prop(case, "yaml")), false);
        let mut expected = JsObject::new();
        for (k, v) in case.as_object().unwrap().entries() {
            if !k.equals_str("id") && !k.equals_str("yaml") {
                expected.insert(k.clone(), v.clone());
            }
        }
        assert_eq!(
            diff(&actual, &JsValue::Object(expected), "expected"),
            None,
            "{:?}",
            prop(case, "id")
        );
    }
}
#[test]
fn cst_source_and_incremental_input_match_recordings() {
    let mut stream_cases = 0;
    fixtures(|case| {
        let source = string(prop(case, "yaml"));
        let mut parser = Parser::default();
        let cst = parser.parse(source, false);
        let actual = JsString::from_utf16(
            cst.iter()
                .flat_map(|t| {
                    pilotdeck_yaml::parse::cst_stringify::stringify(t)
                        .units()
                        .to_vec()
                })
                .collect(),
        );
        assert_eq!(
            &actual,
            string(prop(case, "cstSource")),
            "cst source {:?}",
            prop(case, "id")
        );
        let Some(JsValue::Array(streams)) = case.get("streaming") else {
            return;
        };
        for stream in streams {
            stream_cases += 1;
            let size = prop(stream, "size").as_number().unwrap() as usize;
            let mut lexer = Lexer::default();
            let mut parser = Parser::default();
            let mut lex = Vec::new();
            let mut cst = Vec::new();
            for chunk in source.units().chunks(size) {
                let chunk = JsString::from_utf16(chunk.to_vec());
                lex.extend(lexer.lex(&chunk, true));
                cst.extend(parser.parse(&chunk, true));
            }
            lex.extend(lexer.lex(&JsString::default(), false));
            cst.extend(parser.parse(&JsString::default(), false));
            let JsValue::Array(expected) = prop(stream, "lex") else {
                panic!("lex")
            };
            let expected: Vec<_> = expected.iter().map(|v| string(v).clone()).collect();
            assert_eq!(
                lex,
                expected,
                "incremental lexer {:?} size {size}",
                prop(case, "id")
            );
            assert_eq!(
                cst,
                tokens(prop(stream, "cst")),
                "incremental cst {:?} size {size}",
                prop(case, "id")
            );
            let JsValue::Array(lines) = prop(stream, "lineStarts") else {
                panic!("lines")
            };
            let lines: Vec<_> = lines
                .iter()
                .map(|n| n.as_number().unwrap() as usize)
                .collect();
            assert_eq!(
                parser.line_counter.line_starts.len(),
                lines.len(),
                "incremental lines length {:?} size={size}",
                prop(case, "id")
            );
            assert_eq!(
                parser.line_counter.line_starts,
                lines,
                "incremental lines {:?}",
                prop(case, "id")
            );
        }
    });
    assert_eq!(stream_cases, (71 + 21 + 42) * 4);
}
fn version(d: &pilotdeck_yaml::doc::directives::Directives) -> JsValue {
    object(vec![
        ("explicit", JsValue::Bool(d.explicit)),
        ("version", s(d.version)),
    ])
}
#[test]
fn all_document_values_and_metadata_match_recordings() {
    let mut failures = Vec::new();
    fixtures(|case| {
        let docs = pilotdeck_yaml::parse_all_documents(
            string(prop(case, "yaml")),
            Options {
                pretty_errors: false,
                ..Default::default()
            },
        );
        let expected_lines = prop(case, "lineStarts");
        let actual_lines = JsValue::Array(docs.first().map_or_else(Vec::new, |doc| {
            doc.line_counter
                .line_starts
                .iter()
                .map(|n| JsValue::Number(*n as f64))
                .collect()
        }));
        if !docs.is_empty() {
            assert_eq!(
                diff(&actual_lines, expected_lines, "allDocumentLines"),
                None
            );
            for doc in docs.iter().skip(1) {
                assert!(std::sync::Arc::ptr_eq(
                    &docs[0].line_counter,
                    &doc.line_counter
                ));
            }
        }
        let mut entries = vec![
            (
                "documents",
                JsValue::Array(docs.iter().map(document_outcome).collect()),
            ),
            (
                "versions",
                JsValue::Array(docs.iter().map(|d| version(&d.directives)).collect()),
            ),
        ];
        if let Some(info) = &docs.stream_info {
            entries.push((
                "empty",
                object(vec![
                    ("comment", s(info.comment.clone())),
                    ("yaml", version(&info.directives)),
                    (
                        "tags",
                        JsValue::Array(
                            info.directives
                                .tags
                                .iter()
                                .map(|(k, v)| JsValue::Array(vec![s(k.clone()), s(v.clone())]))
                                .collect(),
                        ),
                    ),
                    ("errors", diagnostics(&info.errors)),
                    ("warnings", diagnostics(&info.warnings)),
                ]),
            ));
        }
        let actual = object(entries);
        if let Some(d) = diff(&actual, prop(case, "allDocuments"), "allDocuments") {
            failures.push(format!("{:?}: {d}", prop(case, "id")));
        }
    });
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
#[test]
fn document_line_counter_matches_recordings() {
    let mut failures = Vec::new();
    fixtures(|case| {
        let doc = parse_document(
            string(prop(case, "yaml")),
            Options {
                pretty_errors: false,
                ..Default::default()
            },
        );
        let actual = JsValue::Array(
            doc.line_counter
                .line_starts
                .iter()
                .map(|n| JsValue::Number(*n as f64))
                .collect(),
        );
        if let Some(d) = diff(&actual, prop(case, "docLines"), "docLines") {
            failures.push(format!("{:?}: {d}", prop(case, "id")));
        }
    });
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
#[test]
fn unsupported_options_fail_explicitly() {
    for name in [
        "version",
        "schema",
        "customTags",
        "merge",
        "uniqueKeys",
        "strict",
        "stringKeys",
        "intAsBigInt",
        "maxAliasCount",
        "logLevel",
        "reviver",
        "compat",
        "mapAsMap",
        "unknown",
    ] {
        let mut options = JsObject::new();
        options.insert(name.into(), JsValue::Bool(false));
        let error = Options::from_js(&options).unwrap_err();
        assert!(error.name.equals_str(name));
        assert!(
            error
                .message
                .to_utf8()
                .unwrap()
                .starts_with("Unsupported YAML option: ")
        );
    }
    let mut options = JsObject::new();
    options.insert("prettyErrors".into(), JsValue::Bool(false));
    options.insert("keepSourceTokens".into(), JsValue::Bool(true));
    let options = Options::from_js(&options).unwrap();
    assert!(!options.pretty_errors);
    assert!(options.keep_source_tokens);
    let doc = parse_document(&"x: 1\n".into(), options);
    let root = doc.contents.unwrap();
    assert!(doc.nodes[root].src_token.is_some());
}
#[test]
fn supplied_line_counter_retains_utf16_positions() {
    let counter = std::rc::Rc::new(std::cell::RefCell::new(
        pilotdeck_yaml::parse::line_counter::LineCounter::default(),
    ));
    let source = JsString::from_utf16(vec![
        0xd83e, 0xdd80, 58, 32, 49, 13, 10, 0xd800, 58, 32, 50, 10,
    ]);
    let doc = parse_document(
        &source,
        Options {
            line_counter: Some(counter.clone()),
            ..Default::default()
        },
    );
    assert!(doc.errors.is_empty());
    assert_eq!(counter.borrow().line_starts, vec![0, 7, 12]);
    assert_eq!(
        counter.borrow().line_pos(7),
        pilotdeck_yaml::parse::line_counter::LinePos { line: 2, col: 1 }
    );
    assert_eq!(
        doc.line_counter.line_pos(9),
        pilotdeck_yaml::parse::line_counter::LinePos { line: 2, col: 3 }
    );
}

#[test]
fn cst_visit_and_scalar_resolution_match_recordings() {
    use pilotdeck_yaml::parse::{
        cst_scalar::resolve_as_scalar,
        cst_visit::{self, Control, Field},
    };
    fixtures(|case| {
        let cst = Parser::default().parse(string(prop(case, "yaml")), false);
        let docs = cst
            .into_iter()
            .filter(|t| t.kind == TokenType::Document)
            .map(|mut doc| {
                let mut root = CollectionItem {
                    start: std::mem::take(&mut doc.start),
                    value: doc.value.take(),
                    ..Default::default()
                };
                let original = root.clone();
                let mut items = Vec::new();
                cst_visit::visit(&mut root, &mut |item, path| {
                    assert_eq!(cst_visit::item_at_path(&original, path), Some(&*item));
                    if !path.is_empty() {
                        assert!(
                            cst_visit::parent_collection(&original, path)
                                .unwrap()
                                .is_collection()
                        );
                    }
                    let mut entry = vec![(
                        "path",
                        JsValue::Array(
                            path.iter()
                                .map(|(field, index)| {
                                    JsValue::Array(vec![
                                        s(match field {
                                            Field::Key => "key",
                                            Field::Value => "value",
                                        }),
                                        JsValue::Number(*index as f64),
                                    ])
                                })
                                .collect(),
                        ),
                    )];
                    for (field, token) in [
                        ("key", item.key.as_ref().and_then(|t| t.as_deref())),
                        ("value", item.value.as_deref()),
                    ] {
                        if let Some(result) = token.and_then(resolve_as_scalar) {
                            entry.push((
                                field,
                                object(vec![
                                    ("value", s(result.value)),
                                    ("type", s(result.style)),
                                    ("comment", s(result.comment)),
                                    (
                                        "range",
                                        JsValue::Array(
                                            result
                                                .range
                                                .into_iter()
                                                .map(|n| JsValue::Number(n as f64))
                                                .collect(),
                                        ),
                                    ),
                                    ("errors", diagnostics(&result.errors)),
                                ]),
                            ));
                        }
                    }
                    items.push(object(entry));
                    Control::Continue
                });
                JsValue::Array(items)
            })
            .collect();
        let actual = JsValue::Array(docs);
        assert_eq!(
            diff(&actual, prop(case, "cstRead"), "cstRead"),
            None,
            "{:?}",
            prop(case, "id")
        );
    });
}

#[test]
fn graph_to_js_value_retains_strings_numbers_and_rejects_non_tree_values() {
    let source = JsString::from("[&a {x: \"\\uD800\"}, *a, -0, .nan, .inf]");
    let doc = parse_document(&source, Options::default());
    assert!(doc.errors.is_empty());
    let graph = doc.to_json().unwrap();
    let value = graph.to_js_value().unwrap();
    let JsValue::Array(items) = &value else {
        panic!("array");
    };
    assert_eq!(items[0], items[1]);
    assert_eq!(string(items[0].get("x").unwrap()).units(), [0xd800]);
    assert_eq!(items[2].as_number().unwrap().to_bits(), (-0.0f64).to_bits());
    assert!(items[3].as_number().unwrap().is_nan());
    assert_eq!(items[4].as_number(), Some(f64::INFINITY));
    for source in [
        "&a [*a]",
        "!!set {a: null}",
        "!!omap [{a: 1}]",
        "!!merge <<",
    ] {
        let doc = parse_document(&source.into(), Options::default());
        assert!(doc.errors.is_empty());
        let graph = doc.to_json().unwrap();
        assert_eq!(graph.to_js_value().unwrap_err().name, "TypeError");
    }
}

#[test]
fn deep_document_works_with_a_two_megabyte_caller_stack() {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(|| {
            for keep_source_tokens in [false, true] {
                let source = format!("{}0{}", "[".repeat(512), "]".repeat(512));
                let counter = std::rc::Rc::new(std::cell::RefCell::new(
                    pilotdeck_yaml::parse::line_counter::LineCounter::default(),
                ));
                let doc = parse_document(
                    &source.into(),
                    Options {
                        pretty_errors: false,
                        keep_source_tokens,
                        line_counter: Some(counter.clone()),
                    },
                );
                assert!(doc.errors.is_empty());
                assert_eq!(counter.borrow().line_starts, [0]);
                assert_eq!(doc.nodes.len(), 513);
                assert_eq!(
                    doc.nodes[doc.contents.unwrap()].src_token.is_some(),
                    keep_source_tokens
                );
                let graph = doc.to_json().unwrap();
                assert_eq!(graph.nodes.len(), 512);
                assert!(matches!(graph.to_js_value().unwrap(), JsValue::Array(_)));
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
