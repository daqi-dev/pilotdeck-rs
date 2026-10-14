use pilotdeck_protocol::JsString;
use pilotdeck_yaml::{Options, errors::Diagnostic, nodes::NodeKind, parse_document};
use serde_json::{Value, json};

fn source(shape: &str, depth: usize) -> JsString {
    match shape {
        "flow-seq" => format!("{}0{}", "[".repeat(depth), "]".repeat(depth)),
        "flow-map" => format!("{}0{}", "{k:".repeat(depth), "}".repeat(depth)),
        "block-seq" => format!("{}0", "- ".repeat(depth)),
        "block-map" => format!("{}0\n", "? ".repeat(depth)),
        "recovery" => format!("{}0{}, 7]", "[".repeat(depth), "]".repeat(depth - 1)),
        "catch-props" => format!(
            "{}&cut !!str {}0{}",
            "[".repeat(662),
            "[".repeat(depth - 662),
            "]".repeat(depth)
        ),
        "mixed" => {
            let blocks = depth.min(2);
            let mut prefix = String::new();
            let mut suffix = String::new();
            for i in 0..blocks {
                prefix.push_str(if i % 2 == 0 { "- " } else { "? " });
            }
            for i in 0..depth - blocks {
                prefix.push_str(if i % 2 == 0 { "[" } else { "{k:" });
                suffix.push(if i % 2 == 0 { ']' } else { '}' });
            }
            format!("{prefix}0{}\n", suffix.chars().rev().collect::<String>())
        }
        "alias-chain" => {
            use std::fmt::Write;
            let mut text = "[&a0 [0]".to_owned();
            for i in 0..depth {
                write!(text, ", &a{} [*a{i}]", i + 1).unwrap();
            }
            text.push(']');
            text
        }
        "merge-chain" => {
            use std::fmt::Write;
            let mut text = "%YAML 1.1\n---\n[&a0 {x: 0}".to_owned();
            for i in 0..depth {
                write!(text, ", &a{} {{<<: *a{i}}}", i + 1).unwrap();
            }
            text.push(']');
            text
        }
        _ => panic!("unknown shape {shape}"),
    }
    .into()
}

fn options(keep_source_tokens: bool) -> Options {
    Options {
        pretty_errors: false,
        keep_source_tokens,
        ..Options::default()
    }
}
fn diagnostics(errors: &[Diagnostic]) -> Value {
    json!(errors.iter().map(|e| json!({
        "name": e.name, "code": e.code, "message": e.message.to_utf8().unwrap(), "pos": e.pos
    })).collect::<Vec<_>>())
}
fn small_stack(f: impl FnOnce() + Send + 'static) {
    std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024)
        .spawn(f)
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn measured_boundaries_match_node_errors_positions_and_catch_nodes() {
    small_stack(|| {
        let fixture: Value = serde_json::from_str(include_str!("fixtures/depth.json")).unwrap();
        assert_eq!(fixture["node"], "v22.22.0");
        assert_eq!(fixture["yaml"], "2.9.0");
        assert_eq!(fixture["execArgv"], json!([]));
        for (shape, measured) in fixture["shapes"].as_object().unwrap() {
            assert_eq!(measured["runs"].as_array().unwrap().len(), 5);
            let Some(depth) = measured["minimum"].as_u64() else {
                assert_eq!(shape, "alias-chain");
                for run in measured["runs"].as_array().unwrap() {
                    let doc = parse_document(
                        &source(shape, run["searchedThrough"].as_u64().unwrap() as usize),
                        options(false),
                    );
                    assert!(doc.errors.is_empty());
                    let error = doc.to_json().unwrap_err();
                    assert_eq!(error.name, run["observation"]["conversion"]["name"]);
                    assert_eq!(
                        error.message.to_utf8().unwrap(),
                        run["observation"]["conversion"]["message"]
                    );
                }
                continue;
            };
            let minimum_run = measured["runs"]
                .as_array()
                .unwrap()
                .iter()
                .find(|run| run["firstResourceExhaustion"] == depth)
                .unwrap();
            for (depth, observation) in [
                (depth - 1, &minimum_run["below"]),
                (depth, &minimum_run["at"]),
            ] {
                for keep in [false, true] {
                    let doc = parse_document(&source(shape, depth as usize), options(keep));
                    assert_eq!(
                        diagnostics(&doc.errors),
                        observation["errors"],
                        "{shape} depth={depth} keep={keep}"
                    );
                    let mut id = doc.contents.unwrap();
                    let mut collections = 0;
                    loop {
                        let next = match &doc.nodes[id].kind {
                            NodeKind::Seq(items) => items.first().copied(),
                            NodeKind::Map(items) => items.first().map(|p| p.value.unwrap_or(p.key)),
                            _ => None,
                        };
                        if let Some(next) = next {
                            collections += 1;
                            id = next;
                        } else {
                            break;
                        }
                    }
                    let node = &doc.nodes[id];
                    let expected = &observation["replacement"];
                    assert_eq!(
                        node.range,
                        [
                            expected["range"][0].as_u64().unwrap() as usize,
                            expected["range"][1].as_u64().unwrap() as usize,
                            expected["range"][2].as_u64().unwrap() as usize
                        ]
                    );
                    assert_eq!(
                        collections,
                        expected["collections"].as_u64().unwrap() as usize
                    );
                    assert_eq!(node.src_token.is_some(), keep);
                    if !doc.errors.is_empty() {
                        assert!(matches!(
                            node.kind,
                            NodeKind::Scalar(pilotdeck_yaml::nodes::ScalarValue::Null)
                        ));
                    }
                }
            }
        }
    });
}

#[test]
fn ten_thousand_to_one_million_layers_return_node_diagnostics_without_aborting() {
    small_stack(|| {
        let fixture: Value =
            serde_json::from_str(include_str!("fixtures/depth-stress.json")).unwrap();
        let observations = fixture["observations"].as_array().unwrap();
        assert_eq!(observations.len(), 18);
        for observation in observations {
            let shape = observation["shape"].as_str().unwrap();
            let depth = observation["depth"].as_u64().unwrap() as usize;
            eprintln!("stress {shape} {depth}");
            let doc = parse_document(&source(shape, depth), options(true));
            if shape == "alias-chain" {
                assert!(doc.errors.is_empty());
                let error = doc.to_json().unwrap_err();
                assert_eq!(error.name, observation["conversion"]["name"]);
                assert_eq!(
                    error.message.to_utf8().unwrap(),
                    observation["conversion"]["message"]
                );
            } else {
                // V8's catch frame depends on runtime stack/JIT state. The
                // contract at these extreme depths is diagnostic code/message.
                let expected = observation["errors"].as_array().unwrap();
                assert!(!expected.is_empty());
                assert_eq!(doc.errors.len(), expected.len(), "{shape} {depth}");
                for (actual, expected) in doc.errors.iter().zip(expected) {
                    assert_eq!(actual.name, expected["name"]);
                    assert_eq!(actual.code, expected["code"]);
                    assert_eq!(actual.message.to_utf8().unwrap(), expected["message"]);
                }
            }
            // Drop runs on the small calling stack with retained deep CST.
            drop(doc);
        }
    });
}

#[test]
fn million_layer_cst_clone_stringify_visit_and_drop_use_no_native_recursion() {
    small_stack(|| {
        use pilotdeck_yaml::parse::{
            cst::CollectionItem, cst_stringify, cst_visit, parser::Parser,
        };
        let text = source("flow-seq", 1_000_000);
        let mut tokens = Parser::default().parse(&text, false);
        let mut doc = tokens.pop().unwrap();
        let copy = doc.clone();
        assert_eq!(doc, copy);
        assert_eq!(cst_stringify::stringify(&copy), text);
        drop(copy);
        let mut root = CollectionItem {
            start: std::mem::take(&mut doc.start),
            value: doc.value.take(),
            ..CollectionItem::default()
        };
        let mut count = 0;
        cst_visit::visit(&mut root, &mut |_, path| {
            count += 1;
            assert!(path.len() <= 1_000_000);
            cst_visit::Control::Continue
        });
        assert_eq!(count, 1_000_001);
        assert_eq!(cst_stringify::stringify_item(&root), text);
        drop(root);
        drop(doc);
        // An unfinished parser also owns recursive CST edges.
        let mut parser = Parser::default();
        let unfinished = format!(
            "{}\n# buffered",
            source("flow-seq", 100_000).to_utf8().unwrap()
        );
        assert!(parser.parse(&unfinished.into(), true).is_empty());
        drop(parser);

        // A single dedent lexeme closes every compact block frame. EOF-only
        // cases exercise finish(), while this exercises step()'s trampoline.
        let dedent = format!(
            "{}\n- tail\n",
            source("block-seq", 100_000).to_utf8().unwrap()
        );
        let doc = parse_document(&dedent.into(), options(true));
        assert_eq!(doc.errors[0].code, "RESOURCE_EXHAUSTION");
        let NodeKind::Seq(items) = &doc.nodes[doc.contents.unwrap()].kind else {
            panic!("sequence");
        };
        assert_eq!(items.len(), 2);
        assert!(
            matches!(&doc.nodes[items[1]].kind, NodeKind::Scalar(pilotdeck_yaml::nodes::ScalarValue::String(s)) if s.equals_str("tail"))
        );
    });
}

#[test]
fn arena_graph_and_manually_extended_ast_are_bounded_during_conversion() {
    small_stack(|| {
        use pilotdeck_yaml::{
            nodes::ScalarValue,
            value::{Container, Value as GraphValue, ValueGraph},
        };
        let graph = ValueGraph {
            root: GraphValue::Ref(0),
            nodes: (0..1_000_000)
                .map(|id| {
                    Container::Array(vec![if id == 999_999 {
                        GraphValue::Null
                    } else {
                        GraphValue::Ref(id + 1)
                    }])
                })
                .collect(),
        };
        let error = graph.to_js_value().unwrap_err();
        assert_eq!(error.name, "RangeError");
        assert!(error.message.equals_str("Maximum call stack size exceeded"));
        drop(graph);
        let mut doc = parse_document(&"0".into(), options(false));
        let template = doc.nodes[0].clone();
        doc.nodes = (0..100_000)
            .map(|id| {
                let mut node = template.clone();
                node.kind = if id == 99_999 {
                    NodeKind::Scalar(ScalarValue::Null)
                } else {
                    NodeKind::Seq(vec![id + 1])
                };
                node
            })
            .collect();
        let error = doc.to_json().unwrap_err();
        assert_eq!(error.name, "RangeError");
        assert!(error.message.equals_str("Maximum call stack size exceeded"));
    });
}

#[test]
fn iterative_cst_visitor_preserves_node_controls_and_mutations() {
    use pilotdeck_yaml::parse::{
        cst::CollectionItem,
        cst_stringify,
        cst_visit::{self, Control, Field},
        parser::Parser,
    };
    use std::{cell::RefCell, rc::Rc, sync::Arc};
    fn event(phase: &str, path: &[(Field, usize)]) -> Value {
        json!({"phase": phase, "path": path.iter().map(|(field, index)|
            json!([if *field == Field::Key { "key" } else { "value" }, index])).collect::<Vec<_>>()})
    }
    let fixture: Value = serde_json::from_str(include_str!("fixtures/traversal.json")).unwrap();
    for case in fixture["cases"].as_array().unwrap() {
        let mode = case["mode"].as_str().unwrap();
        let mut doc = Parser::default()
            .parse(&case["yaml"].as_str().unwrap().into(), false)
            .pop()
            .unwrap();
        let mut root = CollectionItem {
            start: std::mem::take(&mut doc.start),
            value: doc.value.take(),
            ..CollectionItem::default()
        };
        let events = Rc::new(RefCell::new(Vec::new()));
        cst_visit::visit(&mut root, &mut |item, path| {
            events.borrow_mut().push(event("enter", path));
            if mode == "break" && path.len() == 2 {
                Arc::make_mut(item.value.as_mut().unwrap()).source = Some("A".into());
                return Control::Break;
            }
            if path.len() == 1 && path[0].1 == 0 {
                match mode {
                    "skip" => return Control::Skip,
                    "remove" => return Control::Remove,
                    "index" => return Control::Index(2),
                    mode if mode.starts_with("next") => {
                        let events = events.clone();
                        return Control::Next(Box::new(move |_, path| {
                            events.borrow_mut().push(event("after-key", path));
                            match mode {
                                "next-skip" => Control::Skip,
                                "next-break" => Control::Break,
                                "next-remove" => Control::Remove,
                                _ => {
                                    let events = events.clone();
                                    Control::Next(Box::new(move |_, path| {
                                        events.borrow_mut().push(event("exit", path));
                                        Control::Continue
                                    }))
                                }
                            }
                        }));
                    }
                    _ => (),
                }
            }
            Control::Continue
        });
        assert_eq!(json!(*events.borrow()), case["events"], "mode={mode}");
        assert_eq!(
            cst_stringify::stringify_item(&root).to_utf8().unwrap(),
            case["source"],
            "mode={mode}"
        );
    }
}

#[test]
fn exhaustion_keeps_catch_props_warnings_and_continues_with_siblings() {
    small_stack(|| {
        let fixture: Value =
            serde_json::from_str(include_str!("fixtures/depth-recovery.json")).unwrap();
        for observation in fixture["observations"].as_array().unwrap() {
            let shape = observation["shape"].as_str().unwrap();
            let doc = parse_document(
                &source(shape, observation["depth"].as_u64().unwrap() as usize),
                options(true),
            );
            assert_eq!(diagnostics(&doc.warnings), observation["warnings"]);
            assert_eq!(json!(doc.range), observation["range"]);
            if shape == "recovery" {
                assert_eq!(diagnostics(&doc.errors), observation["errors"]);
                let NodeKind::Seq(items) = &doc.nodes[doc.contents.unwrap()].kind else {
                    panic!("sequence");
                };
                assert_eq!(items.len(), 2);
                let tail = &doc.nodes[items[1]];
                assert_eq!(json!(tail.range), observation["tail"]["range"]);
                assert!(matches!(
                    tail.kind,
                    NodeKind::Scalar(pilotdeck_yaml::nodes::ScalarValue::Number(7.0))
                ));
            } else {
                // V8 may overflow again in the empty-node fallback and append a
                // second diagnostic. The bounded Rust fallback cannot overflow.
                // Keep that observed runtime difference explicit (README).
                assert!(!doc.errors.is_empty());
                for actual in &doc.errors {
                    assert_eq!(actual.code, observation["errors"][0]["code"]);
                    assert_eq!(
                        actual.message.to_utf8().unwrap(),
                        observation["errors"][0]["message"]
                    );
                }
                assert_eq!(
                    json!(doc.errors.last().unwrap().pos),
                    observation["errors"].as_array().unwrap().last().unwrap()["pos"]
                );
                let mut id = doc.contents.unwrap();
                while let NodeKind::Seq(items) = &doc.nodes[id].kind {
                    id = items[0];
                }
                let replacement = &doc.nodes[id];
                assert_eq!(
                    json!(replacement.range),
                    observation["replacement"]["range"]
                );
                assert!(
                    matches!(&replacement.kind, NodeKind::Scalar(pilotdeck_yaml::nodes::ScalarValue::String(s)) if s.is_empty())
                );
                assert!(replacement.anchor.as_ref().unwrap().equals_str("cut"));
                assert!(
                    replacement
                        .tag
                        .as_ref()
                        .unwrap()
                        .equals_str("tag:yaml.org,2002:str")
                );
            }
        }
    });
}

#[test]
fn flat_merge_alias_chain_obeys_the_shared_conversion_budget() {
    small_stack(|| {
        let observation: Value =
            serde_json::from_str(include_str!("fixtures/merge-depth.json")).unwrap();
        let doc = parse_document(&source("merge-chain", 10_000), options(true));
        assert_eq!(diagnostics(&doc.errors), observation["errors"]);
        let error = doc.to_json().unwrap_err();
        assert_eq!(error.name, observation["conversion"]["name"]);
        assert_eq!(
            error.message.to_utf8().unwrap(),
            observation["conversion"]["message"]
        );
    });
}
