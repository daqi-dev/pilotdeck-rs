//! #2115: the one proc-macro2 lexer behind every Python-side Rust-name check.
//!
//! `test_metadata_io_driver.py` used to count `fn` definitions with literal
//! regexes; both edges of that missed (a comment between `fn` and the name, an
//! `r#` prefix) and both edges of false positives (an `fn x(` inside a comment
//! or a string literal) are token questions, not text questions. This example
//! is the small Rust entry that answers them: it reads a JSON document
//! `{"files": {name: content}}` from stdin and prints, per file, the
//! `(mod path, fn name)` item set the driver's guards consume — the same
//! `tests/common/source_inputs.rs` lexer `unsafe_boundary.rs` uses (#449 /
//! #1166), never a hand-written one. Content travels through stdin, not
//! paths, because the guards may pass a sources dict whose text was rewritten
//! in memory.
//!
//! Fail-closed policy, mirrored on the Python side:
//!  * a file proc-macro2 cannot lex is reported with an `error` and the
//!    consumer must refuse;
//!  * `fn $name` inside a `macro_rules!` body, a macro-shaped `mod`
//!    declaration, or a real `include!` token leave the file's fn-name set
//!    statically incomplete — reported under `unresolved` so the caller can
//!    refuse where completeness is the question;
//!  * macro *invocation* arguments are reported only for macros this batch
//!    defines with a metavariable `fn`, because expansion happens at the
//!    invocation site: those idents are the names the macro can instantiate
//!    in that file, and an exception keyed by such a name must not be provable.
//!
//! #1943 adds the caller-side projection, on the same token tree: `calls`
//! maps every callee name to the fns that call it (comments and string
//! literals cannot fake a token, so a doc comment saying `replay (` is not a
//! call), `qualified_calls` keeps `a::b::name(` spellings with their paths so
//! a same-named helper of another module is not mistaken for the relay, and
//! `uses` records what each `use` statement binds where — the importer side
//! of the cross-file walk. A call this walker cannot attribute (no enclosing
//! `fn`, or inside macro input where expansion invents the caller) is listed
//! under `unattributed_calls`, so the consumer can refuse instead of guess.
//!
//! #1937 adds `mod_decls`: every `mod name;` declaration with the lexical
//! nesting it appears in and the literal `#[path = "…"]` it carries (if any).
//! A file pulled in under several module names — the shared `permission_faults`
//! is `crate::test_permission_faults` to the unit tests and a bare `mod
//! permission_faults;` to the integration crates — is reachable under each of
//! those spellings, so the cross-file walk needs the declarations to know a
//! caller's path can point at that file. A `#[path]` this walker cannot read
//! as a plain string literal (a `cfg_attr` that mentions `path`, say) or a
//! `mod name;` inside macro input leaves the file's declaration set
//! incomplete — reported under `unresolved`, fail closed like `include!`.
//!
//! `scanner_sha256` embeds this file, the shared lexer, both manifests and
//! the lockfile (the #1166 identity discipline): the consumer recomputes the
//! same digest over the source tree and refuses a stale binary instead of
//! quietly scanning with old rules.

use proc_macro2::Delimiter;
use sha2::{Digest, Sha256};
use source_inputs::{SourceToken, lex_source};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;

#[allow(dead_code)]
#[path = "../tests/common/source_inputs.rs"]
mod source_inputs;

/// One `fn name (params` / `fn name <generics` item occurrence, with the
/// inline `mod` nesting the file declares for it (the module path segments
/// nextest reports after the path-derived ones).
#[derive(Debug)]
struct FnItem {
    name: String,
    module: Vec<String>,
    /// Inside a `macro_rules!` body or macro invocation arguments: the item
    /// only exists after expansion, so it cannot prove anything on its own.
    macro_generated: bool,
}

/// One binding of a `use` statement: the name the call sites will use
/// (`site`) and the item path it comes from (`source`, leading `crate`
/// stripped by no one — the consumer expands `crate`/`self`/`super`).
#[derive(Debug)]
struct UseBinding {
    /// Inline `mod` nesting of the `use` statement itself.
    module: Vec<String>,
    site: String,
    source: Vec<String>,
}

/// One `mod name;` declaration, with the literal `#[path = "…"]` it carries
/// (if any) so the consumer can tell which file the module loads.
#[derive(Debug)]
struct ModDecl {
    module: Vec<String>,
    name: String,
    path: Option<String>,
}

#[derive(Default)]
struct FnWalker {
    items: Vec<FnItem>,
    /// Every `macro_rules!` defined in this file, and whether its body can
    /// instantiate `fn $name` items.
    macro_rules: BTreeMap<String, bool>,
    /// Idents appearing in the arguments of each macro invocation. Filtered
    /// against the batch's metavariable-fn macros before it is emitted.
    invocations: BTreeMap<String, BTreeSet<String>>,
    /// Bare `name( … )` calls: callee name → the fns that call it.
    calls: BTreeMap<String, BTreeSet<String>>,
    /// `a::b::name( … )` calls: path spelling → the fns that call it.
    qualified_calls: BTreeMap<String, BTreeSet<String>>,
    /// Callees this walker cannot attribute to a fn (no enclosing item, or
    /// macro input where expansion invents the caller).
    unattributed_calls: BTreeSet<String>,
    uses: Vec<UseBinding>,
    mod_decls: Vec<ModDecl>,
    unresolved: BTreeSet<&'static str>,
}

/// Walk state: the inline module path, whether we are inside text that is
/// macro input rather than compiled items, and the outermost `fn` the calls
/// below this point are printed on behalf of.
#[derive(Default, Clone)]
struct Context {
    module_path: Vec<String>,
    macro_depth: usize,
    inside_invocation: bool,
    outermost_fn: Option<String>,
}

impl FnWalker {
    fn walk(&mut self, tokens: &[SourceToken], context: &Context) {
        let mut index = 0;
        while index < tokens.len() {
            // `#[…]` / `#![…]` carry attributes and doc strings, never fn
            // items; not descending keeps doc text out of the inventory.
            if let Some(after) = attribute_end(tokens, index) {
                index = after;
                continue;
            }
            let SourceToken::Ident(word) = &tokens[index] else {
                if let SourceToken::Group { tokens: inner, .. } = &tokens[index] {
                    self.walk(inner, context);
                }
                index += 1;
                continue;
            };
            // `macro_rules! name { body }` — the body is macro input: its
            // concrete fns still count (conservatively, in this file), and a
            // metavariable name makes the file's name set incomplete.
            if word == "macro_rules"
                && let Some((name, body, after)) = macro_definition(tokens, index)
            {
                let generated = body_has_metavariable_fn(body);
                self.macro_rules.insert(name, generated);
                let mut inner = context.clone();
                inner.macro_depth += 1;
                self.walk(body, &inner);
                index = after;
                continue;
            }
            // A real `include!` token splices another file's items into this
            // one, so this file's set cannot be enumerated. `include_str!` /
            // `include_bytes!` produce string literals, not items.
            if word == "include" && tokens.get(index + 1) == Some(&SourceToken::Punct('!')) {
                self.unresolved.insert("include!");
                index += 3;
                continue;
            }
            if word == "mod" {
                match (tokens.get(index + 1), tokens.get(index + 2)) {
                    (Some(SourceToken::Ident(name)), Some(group @ SourceToken::Group { .. })) => {
                        let SourceToken::Group {
                            delimiter: Delimiter::Brace,
                            tokens: inner,
                        } = group
                        else {
                            // `mod name ( … )` is not a module this walker
                            // can attribute; fail closed rather than guess.
                            self.unresolved.insert("unresolved mod declaration");
                            index += 1;
                            continue;
                        };
                        let mut inner_context = context.clone();
                        inner_context.module_path.push(name.clone());
                        self.walk(inner, &inner_context);
                        index += 3;
                        continue;
                    }
                    (Some(SourceToken::Ident(name)), Some(SourceToken::Punct(';'))) => {
                        // `mod name;` loads a separate file; this file's own
                        // items are unaffected. #1937: the declaration is
                        // recorded with the `#[path]` it carries, because a
                        // caller's `crate::…`/bare-module path can only be
                        // resolved to a file through it. A declaration this
                        // walker cannot pin down is refused, not guessed.
                        let (path, unreadable) = preceding_mod_path(tokens, index);
                        if unreadable {
                            self.unresolved.insert("mod path attribute");
                        }
                        if context.macro_depth > 0 || context.inside_invocation {
                            // `mod name;` inside macro input: expansion decides
                            // whether the module exists at all, so the
                            // declaration set is incomplete either way.
                            self.unresolved.insert("mod declaration in macro input");
                        } else {
                            self.mod_decls.push(ModDecl {
                                module: context.module_path.clone(),
                                name: name.clone(),
                                path,
                            });
                        }
                        index += 3;
                        continue;
                    }
                    _ => {
                        // `mod $name { … }` inside a macro body (or any
                        // other shape the walker cannot attribute).
                        self.unresolved.insert("unresolved mod declaration");
                    }
                }
            }
            if word == "fn" {
                match (tokens.get(index + 1), tokens.get(index + 2)) {
                    (
                        Some(SourceToken::Ident(name)),
                        Some(SourceToken::Group {
                            delimiter: Delimiter::Parenthesis,
                            ..
                        })
                        | Some(SourceToken::Punct('<')),
                    ) => {
                        self.items.push(FnItem {
                            name: name.clone(),
                            module: context.module_path.clone(),
                            macro_generated: context.macro_depth > 0 || context.inside_invocation,
                        });
                        // Walk the body under this fn as the calls' owner.
                        // `fn` items nested in the body (a `mod`, an inner
                        // helper, an `impl Drop` inside a test) already
                        // carried the *file's* nesting before; the only new
                        // state is `outermost_fn`, so a call printed inside a
                        // nested item is attributed to the test that declares
                        // it — the entry point nextest actually reports.
                        if let Some((body, at)) = fn_body_at(tokens, index + 2) {
                            let mut inner = context.clone();
                            inner.outermost_fn =
                                context.outermost_fn.clone().or_else(|| Some(name.clone()));
                            self.walk(body, &inner);
                            index = at + 1;
                        } else {
                            // A declaration without a body (a trait method).
                            index += 2;
                        }
                        continue;
                    }
                    (Some(SourceToken::Punct('$')), _) => {
                        // `fn $name` — the instantiated name is chosen at the
                        // invocation site and cannot be enumerated here.
                        self.unresolved.insert("macro fn name");
                        index += 2;
                        continue;
                    }
                    _ => {}
                }
            }
            // `use … ;` — the importer side of the cross-file walk. Collected
            // wherever it appears (including macro input, where expansion
            // would materialize the binding at the invocation site): a
            // binding that spells the relay's home path is a registration
            // question for the consumer, not something to hide.
            if word == "use"
                && let Some((bindings, after)) = parse_use(tokens, index)
            {
                for mut binding in bindings {
                    binding.module = context.module_path.clone();
                    self.uses.push(binding);
                }
                index = after;
                continue;
            }
            // Any other `name !( … )` is a macro invocation: its arguments
            // are macro input, recorded for the ident join the consumer
            // performs. Expansion happens at this site, so idents bound by a
            // metavariable-fn macro name the fns this file can gain.
            if tokens.get(index + 1) == Some(&SourceToken::Punct('!'))
                && let Some(SourceToken::Group {
                    tokens: arguments, ..
                }) = tokens.get(index + 2)
            {
                let mut idents = BTreeSet::new();
                collect_idents(arguments, &mut idents);
                self.invocations
                    .entry(word.clone())
                    .or_default()
                    .extend(idents);
                let mut inner = context.clone();
                inner.inside_invocation = true;
                self.walk(arguments, &inner);
                index += 3;
                continue;
            }
            // A plain call `name( … )`: not a `fn` definition (that arm
            // consumed the name already, and the token before the name is
            // `fn` for a stray shape), not a method call after `.`, not a
            // macro. `a::b::name( … )` is kept with its path so the consumer
            // can tell the relay from a same-named helper of another module.
            if matches!(
                tokens.get(index + 1),
                Some(SourceToken::Group {
                    delimiter: Delimiter::Parenthesis,
                    ..
                })
            ) {
                let definition =
                    index > 0 && tokens.get(index - 1) == Some(&SourceToken::Ident("fn".into()));
                let method = index > 0 && tokens.get(index - 1) == Some(&SourceToken::Punct('.'));
                if !definition
                    && !method
                    && let Some(spelling) = call_path(tokens, index)
                {
                    match (&context.outermost_fn, spelling.kind) {
                        (Some(owner), PathKind::Bare) => {
                            self.calls
                                .entry(spelling.segments.join("::"))
                                .or_default()
                                .insert(owner.clone());
                        }
                        (Some(owner), PathKind::Qualified) => {
                            self.qualified_calls
                                .entry(spelling.segments.join("::"))
                                .or_default()
                                .insert(owner.clone());
                        }
                        (None, _) => {
                            self.unattributed_calls.insert(spelling.segments.join("::"));
                        }
                    }
                }
            }
            if let SourceToken::Group { tokens: inner, .. } = &tokens[index] {
                self.walk(inner, context);
            }
            index += 1;
        }
    }

    fn document(self, risky_macros: &BTreeMap<String, bool>) -> serde_json::Value {
        let items: Vec<_> = self
            .items
            .iter()
            .map(|item| {
                serde_json::json!({
                    "name": item.name,
                    "mod": item.module,
                    "macro": item.macro_generated,
                })
            })
            .collect();
        let invocations: serde_json::Map<String, serde_json::Value> = self
            .invocations
            .into_iter()
            .filter(|(name, _)| risky_macros.get(name).copied().unwrap_or(false))
            .map(|(name, idents)| {
                (
                    name,
                    serde_json::Value::Array(
                        idents.into_iter().map(serde_json::Value::String).collect(),
                    ),
                )
            })
            .collect();
        let uses: Vec<_> = self
            .uses
            .iter()
            .map(|binding| {
                serde_json::json!({
                    "module": binding.module,
                    "site": binding.site,
                    "source": binding.source,
                })
            })
            .collect();
        let mod_decls: Vec<_> = self
            .mod_decls
            .iter()
            .map(|decl| {
                serde_json::json!({
                    "module": decl.module,
                    "name": decl.name,
                    "path": decl.path,
                })
            })
            .collect();
        let flatten = |table: BTreeMap<String, BTreeSet<String>>| {
            table
                .into_iter()
                .map(|(name, owners)| {
                    (
                        name,
                        serde_json::Value::Array(
                            owners.into_iter().map(serde_json::Value::String).collect(),
                        ),
                    )
                })
                .collect::<serde_json::Map<String, serde_json::Value>>()
        };
        serde_json::json!({
            "fns": items,
            "macro_rules": self.macro_rules,
            "macro_calls": invocations,
            "calls": flatten(self.calls),
            "qualified_calls": flatten(self.qualified_calls),
            "unattributed_calls": self.unattributed_calls.into_iter()
                .collect::<Vec<_>>(),
            "uses": uses,
            "mod_decls": mod_decls,
            "unresolved": self.unresolved.into_iter().collect::<Vec<_>>(),
        })
    }
}

/// How a callee was spelled at the call site.
struct CallSpelling {
    segments: Vec<String>,
    kind: PathKind,
}

enum PathKind {
    Bare,
    Qualified,
}

/// The callee spelling at `index`, walking any `a::b::` prefix backwards.
/// `None` when this is not a name this walker can attribute: a path whose
/// prefix is not a plain ident chain (`<T as X>::name(`).
fn call_path(tokens: &[SourceToken], index: usize) -> Option<CallSpelling> {
    let mut segments = Vec::new();
    let SourceToken::Ident(word) = &tokens[index] else {
        return None;
    };
    segments.push(word.clone());
    let mut position = index;
    let mut qualified = false;
    while position >= 3
        && tokens.get(position - 1) == Some(&SourceToken::Punct(':'))
        && tokens.get(position - 2) == Some(&SourceToken::Punct(':'))
    {
        match tokens.get(position - 3) {
            Some(SourceToken::Ident(segment)) => {
                segments.push(segment.clone());
                qualified = true;
                position -= 3;
            }
            _ => break,
        }
    }
    segments.reverse();
    Some(CallSpelling {
        segments,
        kind: if qualified {
            PathKind::Qualified
        } else {
            PathKind::Bare
        },
    })
}

/// The `{ body }` group of the `fn` whose signature starts at `from`, or
/// `None` for a bodyless declaration. The scan passes over generics, the
/// parameter group, return types and `where` clauses (their brackets are
/// single group tokens, only a brace at this level is a body) and stops at a
/// `;` (trait method) or at the next item keyword (a shape this walker
/// refuses to guess about).
fn fn_body_at(tokens: &[SourceToken], from: usize) -> Option<(&[SourceToken], usize)> {
    let mut scan = from;
    while let Some(token) = tokens.get(scan) {
        match token {
            SourceToken::Group {
                delimiter: Delimiter::Brace,
                tokens: body,
            } => return Some((body.as_slice(), scan)),
            SourceToken::Punct(';') => return None,
            SourceToken::Ident(word) if is_item_keyword(word) => return None,
            _ => scan += 1,
        }
    }
    None
}

fn is_item_keyword(word: &str) -> bool {
    matches!(
        word,
        "fn" | "mod"
            | "use"
            | "impl"
            | "trait"
            | "struct"
            | "enum"
            | "union"
            | "const"
            | "static"
            | "macro_rules"
    )
}

/// Bindings of one `use` statement, plus the first token index after it.
/// `use a::b::c;` binds `c` from `a::b::c`; `use a::b::c as d;` binds `d`
/// from `a::b::c`; `use a::b::*;` binds everything of `a::b` (site `*`);
/// `use a::{b, c as d};` binds each listed name. A shape this parser does
/// not model (a metavariable in the path) yields no bindings rather than a
/// guess — the consumer documents that boundary.
fn parse_use(tokens: &[SourceToken], index: usize) -> Option<(Vec<UseBinding>, usize)> {
    let mut path = Vec::new();
    let mut scan = index + 1;
    loop {
        match tokens.get(scan) {
            Some(SourceToken::Ident(keyword)) if keyword == "as" => {
                // `use path::name as alias;` — the binding's source is the
                // full item path, the site is the alias the calls will use.
                let Some(SourceToken::Ident(alias)) = tokens.get(scan + 1) else {
                    return None;
                };
                let after = scan + 2;
                let semi = tokens.get(after) == Some(&SourceToken::Punct(';'));
                return semi.then_some((
                    vec![UseBinding {
                        module: Vec::new(),
                        site: alias.clone(),
                        source: path,
                    }],
                    after + 1,
                ));
            }
            Some(SourceToken::Ident(segment)) => {
                path.push(segment.clone());
                scan += 1;
            }
            Some(SourceToken::Punct(':'))
                if tokens.get(scan + 1) == Some(&SourceToken::Punct(':')) =>
            {
                scan += 2;
            }
            Some(SourceToken::Group {
                delimiter: Delimiter::Brace,
                tokens: list,
            }) => {
                let bindings = parse_use_list(list, &path, Vec::new());
                let after = scan + 1;
                let semi = tokens.get(after) == Some(&SourceToken::Punct(';'));
                return semi.then_some((bindings, after + 1));
            }
            Some(SourceToken::Punct('*')) => {
                let binding = UseBinding {
                    module: Vec::new(),
                    site: "*".to_string(),
                    source: path.clone(),
                };
                let after = scan + 1;
                let semi = tokens.get(after) == Some(&SourceToken::Punct(';'));
                return semi.then_some((vec![binding], after + 1));
            }
            Some(SourceToken::Punct(';')) => {
                // `use path::name;` — the source keeps the item path, the
                // site is the name the call sites will use.
                let name = path.last().cloned()?;
                return Some((
                    vec![UseBinding {
                        module: Vec::new(),
                        site: name,
                        source: path,
                    }],
                    scan + 1,
                ));
            }
            _ => return None,
        }
    }
}

/// The `use a::{b, c as d, …};` list. `prefix` is the path before the
/// braces, `pending` the path inside nested braces.
fn parse_use_list(
    tokens: &[SourceToken],
    prefix: &[String],
    mut pending: Vec<String>,
) -> Vec<UseBinding> {
    let mut bindings = Vec::new();
    let mut segment = None;
    let mut index = 0;
    while index < tokens.len() {
        match tokens.get(index) {
            Some(SourceToken::Ident(word)) => {
                if word == "as" {
                    if let (Some(SourceToken::Ident(alias)), Some(name)) =
                        (tokens.get(index + 1), segment.take())
                    {
                        bindings.push(UseBinding {
                            module: Vec::new(),
                            site: alias.clone(),
                            source: [prefix, &pending[..], std::slice::from_ref(&name)].concat(),
                        });
                        index += 2;
                    }
                } else {
                    segment = Some(word.clone());
                }
            }
            Some(SourceToken::Punct(',')) => {
                if let Some(name) = segment.take() {
                    bindings.push(UseBinding {
                        module: Vec::new(),
                        site: name.clone(),
                        source: [prefix, &pending[..], std::slice::from_ref(&name)].concat(),
                    });
                }
            }
            Some(SourceToken::Group {
                delimiter: Delimiter::Brace,
                tokens: nested,
            }) => {
                // `use a::{b::{c, d}}` — recurse with the accumulated prefix.
                if let Some(name) = segment.take() {
                    pending.push(name);
                    bindings.extend(parse_use_list(nested, prefix, pending[..].to_vec()));
                    pending.pop();
                } else {
                    bindings.extend(parse_use_list(nested, prefix, pending[..].to_vec()));
                }
            }
            _ => {}
        }
        index += 1;
    }
    if let Some(name) = segment.take() {
        bindings.push(UseBinding {
            module: Vec::new(),
            site: name.clone(),
            source: [prefix, &pending[..], std::slice::from_ref(&name)].concat(),
        });
    }
    bindings
}

/// `#` (optionally `!`) followed by a bracket group is an attribute; the
/// returned index is the first token after it.
fn attribute_end(tokens: &[SourceToken], index: usize) -> Option<usize> {
    if tokens.get(index) != Some(&SourceToken::Punct('#')) {
        return None;
    }
    let attribute = if tokens.get(index + 1) == Some(&SourceToken::Punct('!')) {
        index + 2
    } else {
        index + 1
    };
    match tokens.get(attribute) {
        Some(SourceToken::Group {
            delimiter: Delimiter::Bracket,
            ..
        }) => Some(attribute + 1),
        _ => None,
    }
}

/// The literal `#[path = "…"]` in the attribute run before the item at
/// `index`, and whether that run mentions `path` in any shape this walker
/// cannot read (a `cfg_attr` that can attach one, say). Attributes only
/// precede items, so a bracket group directly above `index` — each preceded by
/// its own `#` — is exactly the run; a group without its `#` is some other
/// expression and stops the scan.
fn preceding_mod_path(tokens: &[SourceToken], index: usize) -> (Option<String>, bool) {
    let mut found = None;
    let mut unreadable = false;
    let mut position = index;
    while let Some(SourceToken::Group {
        delimiter: Delimiter::Bracket,
        tokens: attribute,
    }) = tokens.get(position.wrapping_sub(1))
    {
        let start = if position >= 2
            && tokens.get(position - 2) == Some(&SourceToken::Punct('#'))
            && tokens.get(position - 1) != Some(&SourceToken::Punct('!'))
        {
            position - 2
        } else if position >= 3
            && tokens.get(position - 3) == Some(&SourceToken::Punct('#'))
            && tokens.get(position - 2) == Some(&SourceToken::Punct('!'))
        {
            position - 3
        } else {
            break;
        };
        match &attribute[..] {
            // The plain form: `#[path = "literal"]`. Only an unescaped or raw
            // string literal is accepted — anything else names a path this
            // walker refuses to guess at.
            [
                SourceToken::Ident(word),
                SourceToken::Punct('='),
                SourceToken::Literal(value),
            ] if word == "path" => {
                found = plain_string(value);
                // A path attribute whose value is not a plain string literal
                // must not fall back to the convention (that would silently
                // point the module at the wrong file).
                unreadable = unreadable || found.is_none();
            }
            // Any other mention of `path` in the run could attach one (a
            // `cfg_attr`) and must not pass silently.
            _ if attribute.iter().any(mentions_path) => unreadable = true,
            _ => {}
        }
        position = start;
    }
    (found, unreadable)
}

/// Does this token tree contain the identifier `path`? `r#path` is normalized
/// to `path` by the lexer, so both spellings count.
fn mentions_path(token: &SourceToken) -> bool {
    match token {
        SourceToken::Ident(word) => word == "path",
        SourceToken::Group { tokens, .. } => tokens.iter().any(mentions_path),
        _ => false,
    }
}

/// The text of an unescaped `"…"` or raw `r#"…"#` string literal; `None` for
/// any other literal (escapes, byte strings, numbers).
fn plain_string(literal: &str) -> Option<String> {
    if let Some(rest) = literal.strip_prefix('r') {
        // r#"…"# with any hash count: the body is verbatim, no escapes.
        let hashes = rest.len() - rest.trim_start_matches('#').len();
        let body = rest
            .strip_prefix(&"#".repeat(hashes))?
            .strip_prefix('"')?
            .strip_suffix(&"#".repeat(hashes))?
            .strip_suffix('"')?;
        return Some(body.to_string());
    }
    let body = literal.strip_prefix('"')?.strip_suffix('"')?;
    (!body.contains('\\')).then(|| body.to_string())
}

/// `macro_rules! name <body>` with `{`, `(` or `[` around the body; the name
/// and the body slice, plus the first token index after the definition.
fn macro_definition(
    tokens: &[SourceToken],
    index: usize,
) -> Option<(String, &[SourceToken], usize)> {
    if tokens.get(index + 1) != Some(&SourceToken::Punct('!')) {
        return None;
    }
    let name = match tokens.get(index + 2) {
        Some(SourceToken::Ident(name)) => name.clone(),
        _ => return None,
    };
    match tokens.get(index + 3) {
        Some(SourceToken::Group { tokens: body, .. }) => Some((name, body.as_slice(), index + 4)),
        _ => None,
    }
}

/// Does the macro body anywhere instantiate a `fn $name` item? Nested groups
/// and nested macro definitions are searched too: over-approximating here
/// only makes the consumer more conservative.
fn body_has_metavariable_fn(tokens: &[SourceToken]) -> bool {
    tokens.windows(2).any(|pair| {
        matches!(
            pair,
            [SourceToken::Ident(word), SourceToken::Punct('$')] if word == "fn"
        )
    }) || tokens.iter().any(|token| {
        matches!(token, SourceToken::Group { tokens: inner, .. } if body_has_metavariable_fn(inner))
    })
}

fn collect_idents(tokens: &[SourceToken], idents: &mut BTreeSet<String>) {
    for token in tokens {
        match token {
            SourceToken::Ident(word) => {
                idents.insert(word.clone());
            }
            SourceToken::Group { tokens: inner, .. } => collect_idents(inner, idents),
            _ => {}
        }
    }
}

fn scanner_identity() -> String {
    let sources = [
        include_str!("lex_fn_items.rs"),
        include_str!("../tests/common/source_inputs.rs"),
        include_str!("../Cargo.toml"),
        include_str!("../../Cargo.toml"),
        include_str!("../../Cargo.lock"),
    ];
    let mut hasher = Sha256::new();
    for source in sources {
        hasher.update(source.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn main() {
    let mut input = String::new();
    std::io::stdin()
        .read_to_string(&mut input)
        .expect("#2115 read the file document from stdin");
    let document: serde_json::Value =
        serde_json::from_str(&input).expect("#2115 the stdin document is JSON");
    let files = document
        .get("files")
        .and_then(serde_json::Value::as_object)
        .expect("#2115 the stdin document maps file names to contents");
    let mut parsed = Vec::with_capacity(files.len());
    for (name, content) in files {
        let content = content.as_str().expect("#2115 file content is a string");
        parsed.push((name.clone(), content));
    }
    // Batch join: a macro can be defined in one file and invoked in another,
    // so the risky-macro table is computed over every file before any
    // invocation list is emitted.
    let mut walkers = Vec::with_capacity(parsed.len());
    let mut risky_macros = BTreeMap::new();
    for (_, content) in &parsed {
        let walker = match lex_source(content) {
            Ok(tokens) => {
                let mut walker = FnWalker::default();
                walker.walk(&tokens, &Context::default());
                walker
            }
            Err(_) => FnWalker::default(),
        };
        for (name, generated) in &walker.macro_rules {
            let risky = risky_macros.entry(name.clone()).or_insert(false);
            *risky = *risky || *generated;
        }
        walkers.push(walker);
    }
    let mut inventory = serde_json::Map::new();
    for ((name, content), walker) in parsed.into_iter().zip(walkers) {
        let entry = match lex_source(content) {
            Ok(_) => walker.document(&risky_macros),
            Err(error) => serde_json::json!({
                "error": format!("#2115 the file is not lexable Rust; fail closed: {error}"),
            }),
        };
        inventory.insert(name, entry);
    }
    let document = serde_json::json!({
        "scanner_sha256": scanner_identity(),
        "files": inventory,
    });
    println!("{document}");
}
