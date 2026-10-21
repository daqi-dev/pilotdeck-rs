//! A small mutable DOM built by html5ever (the WHATWG tree builder), standing
//! in for `@mixmark-io/domino` under turndown. Nodes live in one arena with
//! sibling/parent links, so building, walking and dropping a deep tree never
//! recurses.
use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::{
    Attribute, ParseOpts, QualName, local_name, ns,
    tree_builder::{ElemName, ElementFlags, NodeOrText, QuirksMode, TreeSink},
};
use std::{borrow::Cow, cell::RefCell};

pub(crate) type Id = usize;

#[derive(Debug)]
pub(crate) enum Kind {
    Document,
    /// A `<template>`'s content fragment (not a child of anything).
    Fragment,
    Doctype,
    Comment,
    Text(String),
    Element {
        name: QualName,
        /// `(qualified name, value)` in source order.
        attrs: Vec<(String, String)>,
        template: Option<Id>,
        mathml_html: bool,
    },
}

#[derive(Debug)]
pub(crate) struct Node {
    pub kind: Kind,
    pub parent: Option<Id>,
    pub first: Option<Id>,
    pub last: Option<Id>,
    pub prev: Option<Id>,
    pub next: Option<Id>,
}

#[derive(Debug, Default)]
pub(crate) struct Dom {
    pub nodes: Vec<Node>,
}

pub(crate) const DOCUMENT: Id = 0;

impl Dom {
    fn push(&mut self, kind: Kind) -> Id {
        self.nodes.push(Node {
            kind,
            parent: None,
            first: None,
            last: None,
            prev: None,
            next: None,
        });
        self.nodes.len() - 1
    }

    pub(crate) fn detach(&mut self, id: Id) {
        let (parent, prev, next) = {
            let node = &self.nodes[id];
            (node.parent, node.prev, node.next)
        };
        let Some(parent) = parent else {
            return;
        };
        match prev {
            Some(prev) => self.nodes[prev].next = next,
            None => self.nodes[parent].first = next,
        }
        match next {
            Some(next) => self.nodes[next].prev = prev,
            None => self.nodes[parent].last = prev,
        }
        let node = &mut self.nodes[id];
        node.parent = None;
        node.prev = None;
        node.next = None;
    }

    fn append_child(&mut self, parent: Id, child: Id) {
        self.detach(child);
        let last = self.nodes[parent].last;
        self.nodes[child].parent = Some(parent);
        self.nodes[child].prev = last;
        match last {
            Some(last) => self.nodes[last].next = Some(child),
            None => self.nodes[parent].first = Some(child),
        }
        self.nodes[parent].last = Some(child);
    }

    fn insert_before(&mut self, sibling: Id, child: Id) {
        self.detach(child);
        let parent = self.nodes[sibling].parent.expect("sibling has a parent");
        let prev = self.nodes[sibling].prev;
        self.nodes[child].parent = Some(parent);
        self.nodes[child].prev = prev;
        self.nodes[child].next = Some(sibling);
        self.nodes[sibling].prev = Some(child);
        match prev {
            Some(prev) => self.nodes[prev].next = Some(child),
            None => self.nodes[parent].first = Some(child),
        }
    }

    fn text_mut(&mut self, id: Option<Id>) -> Option<&mut String> {
        match &mut self.nodes[id?].kind {
            Kind::Text(text) => Some(text),
            _ => None,
        }
    }

    pub(crate) fn children(&self, id: Id) -> Vec<Id> {
        let mut out = Vec::new();
        let mut child = self.nodes[id].first;
        while let Some(c) = child {
            out.push(c);
            child = self.nodes[c].next;
        }
        out
    }

    /// Tree-order walk of `id`'s descendants (not `id`, not template content).
    pub(crate) fn descendants(&self, id: Id) -> impl Iterator<Item = Id> + '_ {
        let mut next = self.nodes[id].first;
        std::iter::from_fn(move || {
            let current = next?;
            next = self.nodes[current].first.or_else(|| {
                let mut up = Some(current);
                while let Some(node) = up {
                    if node == id {
                        return None;
                    }
                    if let Some(sibling) = self.nodes[node].next {
                        return Some(sibling);
                    }
                    up = self.nodes[node].parent;
                }
                None
            });
            Some(current)
        })
    }

    pub(crate) fn is_element(&self, id: Id) -> bool {
        matches!(self.nodes[id].kind, Kind::Element { .. })
    }
    pub(crate) fn is_html(&self, id: Id) -> bool {
        matches!(&self.nodes[id].kind, Kind::Element { name, .. } if name.ns == ns!(html))
    }
    pub(crate) fn is_text(&self, id: Id) -> bool {
        matches!(self.nodes[id].kind, Kind::Text(_))
    }

    /// `node.nodeName`: upper-case for HTML elements, the qualified name for
    /// foreign (SVG/MathML) ones, `#text` / `#comment` otherwise.
    pub(crate) fn node_name(&self, id: Id) -> String {
        match &self.nodes[id].kind {
            Kind::Element { name, .. } => {
                let local: &str = &name.local;
                let qualified = match &name.prefix {
                    Some(prefix) => format!("{}:{local}", &**prefix),
                    None => local.to_string(),
                };
                if name.ns == ns!(html) {
                    qualified.to_ascii_uppercase()
                } else {
                    qualified
                }
            }
            Kind::Text(_) => "#text".into(),
            Kind::Comment => "#comment".into(),
            Kind::Document => "#document".into(),
            Kind::Fragment => "#document-fragment".into(),
            Kind::Doctype => "html".into(),
        }
    }

    /// `element.getAttribute(name)` (HTML documents lower-case `name`).
    pub(crate) fn attr(&self, id: Id, name: &str) -> Option<&str> {
        match &self.nodes[id].kind {
            Kind::Element {
                attrs, name: tag, ..
            } => {
                let wanted = if tag.ns == ns!(html) {
                    name.to_ascii_lowercase()
                } else {
                    name.to_string()
                };
                attrs
                    .iter()
                    .find(|(key, _)| *key == wanted)
                    .map(|(_, value)| value.as_str())
            }
            _ => None,
        }
    }

    /// `document.getElementById(id)` in tree order.
    pub(crate) fn element_by_id(&self, value: &str) -> Option<Id> {
        self.descendants(DOCUMENT)
            .find(|&d| self.is_element(d) && self.attr(d, "id") == Some(value))
    }

    pub(crate) fn element_children(&self, id: Id) -> Vec<Id> {
        self.children(id)
            .into_iter()
            .filter(|&c| self.is_element(c))
            .collect()
    }

    pub(crate) fn remove(&mut self, id: Id) {
        self.detach(id);
    }

    pub(crate) fn set_text(&mut self, id: Id, value: String) {
        if let Kind::Text(text) = &mut self.nodes[id].kind {
            *text = value;
        }
    }
}

/// Owned element name handed back to the tree builder.
#[derive(Debug)]
pub(crate) struct Name(QualName);
impl ElemName for Name {
    fn ns(&self) -> &html5ever::Namespace {
        &self.0.ns
    }
    fn local_name(&self) -> &html5ever::LocalName {
        &self.0.local
    }
}

#[derive(Default)]
struct Sink {
    dom: RefCell<Dom>,
}

impl Sink {
    fn new() -> Self {
        let mut dom = Dom::default();
        dom.push(Kind::Document);
        Self {
            dom: RefCell::new(dom),
        }
    }
    fn create(&self, kind: Kind) -> Id {
        self.dom.borrow_mut().push(kind)
    }
}

impl TreeSink for Sink {
    type Handle = Id;
    type Output = Dom;
    type ElemName<'a> = Name;

    fn finish(self) -> Dom {
        self.dom.into_inner()
    }
    fn parse_error(&self, _msg: Cow<'static, str>) {}
    fn get_document(&self) -> Id {
        DOCUMENT
    }
    fn elem_name<'a>(&'a self, target: &'a Id) -> Name {
        match &self.dom.borrow().nodes[*target].kind {
            Kind::Element { name, .. } => Name(name.clone()),
            _ => Name(QualName::new(None, ns!(), local_name!(""))),
        }
    }
    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Id {
        let template = flags.template.then(|| self.create(Kind::Fragment));
        let attrs = attrs
            .into_iter()
            .map(|a| {
                let local: &str = &a.name.local;
                let key = match &a.name.prefix {
                    Some(prefix) => format!("{}:{local}", &**prefix),
                    None => local.to_string(),
                };
                (key, a.value.to_string())
            })
            .collect();
        self.create(Kind::Element {
            name,
            attrs,
            template,
            mathml_html: flags.mathml_annotation_xml_integration_point,
        })
    }
    fn is_mathml_annotation_xml_integration_point(&self, target: &Id) -> bool {
        matches!(
            self.dom.borrow().nodes[*target].kind,
            Kind::Element {
                mathml_html: true,
                ..
            }
        )
    }
    fn create_comment(&self, _text: StrTendril) -> Id {
        self.create(Kind::Comment)
    }
    fn create_pi(&self, _target: StrTendril, _data: StrTendril) -> Id {
        self.create(Kind::Comment)
    }
    fn append(&self, parent: &Id, child: NodeOrText<Id>) {
        let mut dom = self.dom.borrow_mut();
        match child {
            NodeOrText::AppendNode(node) => dom.append_child(*parent, node),
            NodeOrText::AppendText(text) => {
                let last = dom.nodes[*parent].last;
                if let Some(existing) = dom.text_mut(last) {
                    existing.push_str(&text);
                } else {
                    let node = dom.push(Kind::Text(text.to_string()));
                    dom.append_child(*parent, node);
                }
            }
        }
    }
    fn append_based_on_parent_node(&self, element: &Id, prev_element: &Id, child: NodeOrText<Id>) {
        let has_parent = self.dom.borrow().nodes[*element].parent.is_some();
        if has_parent {
            self.append_before_sibling(element, child);
        } else {
            self.append(prev_element, child);
        }
    }
    fn append_doctype_to_document(
        &self,
        _name: StrTendril,
        _public: StrTendril,
        _system: StrTendril,
    ) {
        let node = self.create(Kind::Doctype);
        self.dom.borrow_mut().append_child(DOCUMENT, node);
    }
    fn get_template_contents(&self, target: &Id) -> Id {
        match &self.dom.borrow().nodes[*target].kind {
            Kind::Element {
                template: Some(contents),
                ..
            } => *contents,
            _ => *target,
        }
    }
    fn same_node(&self, x: &Id, y: &Id) -> bool {
        x == y
    }
    fn set_quirks_mode(&self, _mode: QuirksMode) {}
    fn append_before_sibling(&self, sibling: &Id, new_node: NodeOrText<Id>) {
        let mut dom = self.dom.borrow_mut();
        match new_node {
            NodeOrText::AppendNode(node) => dom.insert_before(*sibling, node),
            NodeOrText::AppendText(text) => {
                let prev = dom.nodes[*sibling].prev;
                if let Some(existing) = dom.text_mut(prev) {
                    existing.push_str(&text);
                } else {
                    let node = dom.push(Kind::Text(text.to_string()));
                    dom.insert_before(*sibling, node);
                }
            }
        }
    }
    fn add_attrs_if_missing(&self, target: &Id, extra: Vec<Attribute>) {
        if let Kind::Element { attrs, .. } = &mut self.dom.borrow_mut().nodes[*target].kind {
            for attribute in extra {
                let key = attribute.name.local.to_string();
                if !attrs.iter().any(|(k, _)| *k == key) {
                    attrs.push((key, attribute.value.to_string()));
                }
            }
        }
    }
    fn associate_with_form(&self, _target: &Id, _form: &Id, _nodes: (&Id, Option<&Id>)) {}
    fn remove_from_parent(&self, target: &Id) {
        self.dom.borrow_mut().detach(*target);
    }
    fn reparent_children(&self, node: &Id, new_parent: &Id) {
        let mut dom = self.dom.borrow_mut();
        for child in dom.children(*node) {
            dom.append_child(*new_parent, child);
        }
    }
}

/// `domino.createDocument(html)`.
pub(crate) fn parse_document(html: &str) -> Dom {
    let mut options = ParseOpts::default();
    options.tree_builder.scripting_enabled = false;
    html5ever::parse_document(Sink::new(), options).one(html)
}
