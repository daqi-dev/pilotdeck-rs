use pilotdeck_yaml::{
    Options, parse_document,
    value::{Container, Value},
};

#[test]
fn alias_index_resolves_the_last_preceding_anchor_including_collection_keys() {
    let doc = parse_document(
        &"[&a first, *a, &a second, *a, { ? [*a] : value }]".into(),
        Options::default(),
    );
    assert!(doc.errors.is_empty());
    let graph = doc.to_json().unwrap();
    let Value::Ref(root) = graph.root else {
        panic!()
    };
    let Container::Array(items) = &graph.nodes[root] else {
        panic!()
    };
    for (i, expected) in [(0, "first"), (1, "first"), (2, "second"), (3, "second")] {
        assert!(matches!(&items[i], Value::String(s) if s.equals_str(expected)));
    }
    let Value::Ref(map) = items[4] else { panic!() };
    let Container::Object(entries) = &graph.nodes[map] else {
        panic!()
    };
    assert_eq!(entries[0].0.to_utf8().unwrap(), "[ *a ]");
    let doc = parse_document(&"[*a, &a later]".into(), Options::default());
    assert_eq!(doc.to_json().unwrap_err().name, "ReferenceError");
}

#[test]
fn wide_omap_and_merge_tables_preserve_order_and_precedence() {
    let pairs = (0..40)
        .map(|i| format!("{{ k{i}: {i} }}"))
        .collect::<Vec<_>>()
        .join(", ");
    let doc = parse_document(&format!("!!omap [{pairs}]").into(), Options::default());
    assert!(doc.errors.is_empty());
    let graph = doc.to_json().unwrap();
    let Value::Ref(root) = graph.root else {
        panic!()
    };
    let Container::Map(entries) = &graph.nodes[root] else {
        panic!()
    };
    assert_eq!(entries.len(), 40);
    assert!(matches!(&entries[39], (Value::String(k), Value::Number(39.0)) if k.equals_str("k39")));
    let doc = parse_document(
        &format!("!!omap [{pairs}, {{ k0: again }}]").into(),
        Options::default(),
    );
    assert_eq!(doc.errors.last().unwrap().code, "TAG_RESOLVE_FAILED");
    let doc = parse_document(
        &"!!omap [{ .nan: a }, { .NaN: b }]".into(),
        Options::default(),
    );
    assert_eq!(doc.errors.last().unwrap().code, "TAG_RESOLVE_FAILED");
    let entries = (0..40)
        .map(|i| format!("k{i}: {i}"))
        .collect::<Vec<_>>()
        .join(", ");
    let doc = parse_document(
        &format!("%YAML 1.1\n---\na: &a {{{entries}}}\nb: {{ <<: *a, k0: changed }}").into(),
        Options::default(),
    );
    assert!(doc.errors.is_empty());
    let graph = doc.to_json().unwrap();
    let Value::Ref(root) = graph.root else {
        panic!()
    };
    let Container::Object(entries) = &graph.nodes[root] else {
        panic!()
    };
    let Value::Ref(b) = entries[1].1 else {
        panic!()
    };
    let Container::Object(entries) = &graph.nodes[b] else {
        panic!()
    };
    assert_eq!(entries.len(), 40);
    assert_eq!(entries[0].0.to_utf8().unwrap(), "k0");
    assert!(matches!(&entries[0].1, Value::String(s) if s.equals_str("changed")));
}

#[test]
fn many_tag_handles_resolve_after_overwrite_without_reordering() {
    let mut source = (0..128)
        .map(|i| format!("%TAG !t{i}! tag:x{i}:\n"))
        .collect::<String>();
    source.push_str("%TAG !t64! tag:changed:\n---\n");
    for i in 0..128 {
        source.push_str(&format!("- !t{i}!value a\n"));
    }
    let doc = parse_document(&source.into(), Options::default());
    assert!(doc.errors.is_empty());
    assert_eq!(doc.warnings.len(), 128);
    assert_eq!(doc.directives.tags.len(), 129);
    assert_eq!(doc.directives.tags[65].0.to_utf8().unwrap(), "!t64!");
    let pilotdeck_yaml::nodes::NodeKind::Seq(items) = &doc.nodes[doc.contents.unwrap()].kind else {
        panic!();
    };
    for (i, id) in items.iter().enumerate() {
        let expected = if i == 64 {
            "tag:changed:value".to_string()
        } else {
            format!("tag:x{i}:value")
        };
        assert_eq!(
            doc.nodes[*id].tag.as_ref().unwrap().to_utf8().unwrap(),
            expected
        );
    }
}
