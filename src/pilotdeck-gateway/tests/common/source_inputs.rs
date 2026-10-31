//! Shared proc-macro2 lexer and source-edge parser for #449 and #1166.
use proc_macro2::{Delimiter, Ident, TokenStream, TokenTree};
use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

#[derive(Debug)]
pub(crate) struct SourceEdge {
    pub path: PathBuf,
    pub module_dir: PathBuf,
    pub rust_source: bool,
}

// File names lib.rs/main.rs imply crate ownership only at Cargo roots.
// A default `mod lib;` or `mod main;` still owns its stem directory.
pub(crate) fn source_module_dir(file: &Path) -> PathBuf {
    let parent = file.parent().expect("#1166 source has a parent");
    if matches!(
        file.file_name().and_then(|name| name.to_str()),
        Some("mod.rs")
    ) {
        parent.to_path_buf()
    } else {
        parent.join(file.file_stem().expect("#1166 source has a stem"))
    }
}

pub(crate) fn workspace_target(root: &Path, candidate: &Path) -> PathBuf {
    assert!(
        !candidate.ancestors().any(Path::is_symlink),
        "#1166 Rust input path contains a symlink: {}",
        candidate.display()
    );
    let path = candidate.canonicalize().unwrap_or_else(|error| {
        panic!(
            "#1166 input cannot be resolved: {} ({error})",
            candidate.display()
        )
    });
    assert!(
        path.starts_with(root) && path.is_file(),
        "#1166 input is outside repository or not a file: {}",
        path.display()
    );
    path
}

pub(crate) fn workspace_source_edges(
    root: &Path,
    file: &Path,
    module_dirs: Vec<PathBuf>,
) -> Vec<SourceEdge> {
    let source = read_vendored_source("workspace", file);
    let tokens = lex_source(&source).unwrap_or_else(|error| {
        panic!(
            "#1166 source is not lexable Rust; fail closed: {} ({error})",
            file.display()
        )
    });
    let mut scanner = EdgeScanner {
        package: "workspace",
        directory: root,
        file,
        targets: Vec::new(),
        macro_depth: 0,
        workspace: true,
    };
    scanner.walk(&tokens, true, module_dirs);
    scanner.targets
}

fn names_include(tokens: &[SourceToken]) -> bool {
    tokens.iter().any(|token| match token {
        SourceToken::Ident(word) => {
            matches!(word.as_str(), "include" | "include_str" | "include_bytes")
        }
        SourceToken::Group { tokens, .. } => names_include(tokens),
        _ => false,
    })
}

fn names_indirect_include(tokens: &[SourceToken]) -> bool {
    tokens.iter().enumerate().any(|(index, token)| match token {
        SourceToken::Ident(word) => {
            matches!(word.as_str(), "include" | "include_str" | "include_bytes")
                && tokens.get(index + 1) != Some(&SourceToken::Punct('!'))
        }
        SourceToken::Group { tokens, .. } => names_indirect_include(tokens),
        _ => false,
    })
}

// One lexer for every vendored source decision. `proc_macro2` owns comments,
// raw strings (including `br"..."` / `cr"..."`), literals and delimiter
// matching; this file only projects and inspects the token tree. `Group`
// nesting is the depth that decides whether a `#[path]` is at file top level.
#[derive(Debug, PartialEq)]
pub(crate) enum SourceToken {
    Ident(String),
    Literal(String),
    Punct(char),
    Group {
        delimiter: Delimiter,
        tokens: Vec<SourceToken>,
    },
}

pub(crate) fn lex_source(source: &str) -> Result<Vec<SourceToken>, proc_macro2::LexError> {
    let stream = TokenStream::from_str(source)?;
    Ok(lex_stream(stream))
}

fn lex_stream(stream: TokenStream) -> Vec<SourceToken> {
    stream
        .into_iter()
        .map(|token| match token {
            TokenTree::Ident(ident) => SourceToken::Ident(ident_text(&ident)),
            TokenTree::Literal(literal) => SourceToken::Literal(literal.to_string()),
            TokenTree::Punct(punct) => SourceToken::Punct(punct.as_char()),
            TokenTree::Group(group) => SourceToken::Group {
                delimiter: group.delimiter(),
                tokens: lex_stream(group.stream()),
            },
        })
        .collect()
}

// `r#path` and `r#include` name the same item as their plain spelling. Normalize
// at the lexer boundary so every identifier check has one shape.
fn ident_text(ident: &Ident) -> String {
    let text = ident.to_string();
    text.strip_prefix("r#").unwrap_or(&text).to_owned()
}

pub(crate) fn delimiter_chars(delimiter: Delimiter) -> Option<(char, char)> {
    match delimiter {
        Delimiter::Parenthesis => Some(('(', ')')),
        Delimiter::Brace => Some(('{', '}')),
        Delimiter::Bracket => Some(('[', ']')),
        Delimiter::None => None,
    }
}

// Only a plain string without escapes or a raw string is a static path. Bytes,
// C strings, escaped strings and suffixed literals are rejected: a new literal
// shape must extend this scanner instead of silently skipping an edge.
pub(crate) fn plain_string_value(literal: &str) -> Option<String> {
    if literal.starts_with('"') {
        let value = literal.strip_prefix('"')?.strip_suffix('"')?;
        return (!value.contains('\\') && !value.contains('"')).then(|| value.to_owned());
    }
    let rest = literal.strip_prefix('r')?;
    let hashes = rest.bytes().take_while(|byte| *byte == b'#').count();
    let body = rest[hashes..].strip_prefix('"')?;
    let closing = format!("\"{}", "#".repeat(hashes));
    body.strip_suffix(&closing).map(str::to_owned)
}

pub(crate) fn assert_vendored_source_target(
    package: &str,
    directory: &Path,
    source: &Path,
    kind: &str,
    value: &str,
) -> PathBuf {
    let target = source.parent().unwrap().join(value);
    let file_type = fs::symlink_metadata(&target)
        .unwrap_or_else(|error| {
            panic!(
                "vendored package {package} {kind} target cannot be resolved: {} ({error})",
                target.display()
            )
        })
        .file_type();
    // Check the link itself before canonicalizing: a symlink whose destination
    // is inside the package is still an unreviewed second path into the file.
    assert!(
        !file_type.is_symlink(),
        "vendored package {package} {kind} target is a symlink: {}",
        target.display()
    );
    let target = target.canonicalize().unwrap_or_else(|error| {
        panic!(
            "vendored package {package} {kind} target cannot be resolved: {} ({error})",
            target.display()
        )
    });
    assert!(
        target.starts_with(directory),
        "vendored package {package} {kind} target escapes its package directory: {} -> {}",
        source.display(),
        target.display()
    );
    assert!(
        target.is_file(),
        "vendored package {package} {kind} target is not a file: {}",
        target.display()
    );
    target
}

// Fail closed on non-literal paths and macro-generated edges: parsing an
// arbitrary macro expansion is out of scope, so a new literal shape must
// extend this scanner instead of silently skipping an edge. `#[path]` is only
// auditable at the file top level: inside any `Group` (a `macro_rules!` body,
// an inline `mod name { ... }`, the `mod $i { ... }` shape a macro expands to,
// a function body, or macro invocation arguments) rustc derives the base
// directory from the expansion context. This scanner would instead audit
// `<file dir>/<literal>` and could pass a decoy while rustc compiles a
// different file, so the shape is rejected.
pub(crate) fn vendored_source_edges(package: &str, directory: &Path, file: &Path) -> Vec<PathBuf> {
    let source = read_vendored_source(package, file);
    let tokens = lex_source(&source).unwrap_or_else(|error| {
        panic!(
            "vendored package {package} source is not lexable Rust; fail closed: {} ({error})",
            file.display()
        )
    });
    let mut scanner = EdgeScanner {
        package,
        directory,
        file,
        targets: Vec::new(),
        macro_depth: 0,
        workspace: false,
    };
    scanner.walk(&tokens, true, vec![source_module_dir(file)]);
    scanner.targets.into_iter().map(|edge| edge.path).collect()
}

// The `.rs` branch and the candidate branch both read bytes first so an
// invalid-UTF-8 source is reported as this guard's own fail-closed panic
// instead of an `unwrap` of `io::ErrorKind::InvalidData`.
fn read_vendored_source(package: &str, file: &Path) -> String {
    let bytes = fs::read(file).unwrap_or_else(|error| {
        panic!(
            "vendored package {package} source cannot be read: {} ({error})",
            file.display()
        )
    });
    String::from_utf8(bytes).unwrap_or_else(|error| {
        panic!(
            "vendored package {package} source is not valid UTF-8; fail closed: {} ({error})",
            file.display()
        )
    })
}

// Policy table (both modes share proc-macro2 and reject escaped literals):
//                     vendored #449             workspace #1166
// includes            include! only             include!/include_str!/include_bytes!
// trailing comma      reject                    accept one
// default modules     collected by caller       resolve recursively
// path base/bounds    vendored package rules     repository, no symlink
// conditional path    scan conservatively       reject unresolved context
// alias/indirection   vendored boundary policy  reject include aliases
// A workspace seed may be a custom Cargo crate root or an ordinary module.
// Scan the union of both ownership contexts, then use exact contexts on edges.
// Extra inputs are conservative; no existing decoy can hide the actual input.
struct EdgeScanner<'a> {
    package: &'a str,
    directory: &'a Path,
    file: &'a Path,
    targets: Vec<SourceEdge>,
    macro_depth: usize,
    workspace: bool,
}

impl EdgeScanner<'_> {
    fn walk(&mut self, tokens: &[SourceToken], top_level: bool, module_dirs: Vec<PathBuf>) {
        let mut index = 0;
        let mut explicit_module = false;
        while index < tokens.len() {
            // `#[...]` and `#![...]` are attribute syntax, not ordinary
            // nesting. `cfg_attr(...)` argument groups stay part of the
            // attribute and are walked by `attribute_paths`.
            if let Some((attribute, attribute_tokens)) = attribute_bracket_group(tokens, index) {
                explicit_module |= attribute_names_path(attribute_tokens);
                self.attribute_paths(attribute_tokens, top_level);
                if self.workspace {
                    self.walk(attribute_tokens, false, module_dirs.clone());
                }
                index = attribute + 1;
                continue;
            }
            if matches!(&tokens[index], SourceToken::Ident(word) if word == "include" || (self.workspace && matches!(word.as_str(), "include_str" | "include_bytes")))
                && tokens.get(index + 1) == Some(&SourceToken::Punct('!'))
                && let Some(SourceToken::Group {
                    tokens: arguments, ..
                }) = tokens.get(index + 2)
            {
                assert!(
                    self.macro_depth == 0,
                    "vendored package {} include! appears inside a macro_rules! definition body; the expansion site may be a different file, so the base directory cannot be determined statically; fail closed: {}",
                    self.package,
                    self.file.display()
                );
                let SourceToken::Ident(kind) = &tokens[index] else {
                    unreachable!()
                };
                self.include(arguments, kind);
                index += 3;
                continue;
            }
            if matches!(&tokens[index], SourceToken::Ident(word) if word == "macro_rules")
                && let Some(body) = macro_body(tokens, index)
            {
                if let SourceToken::Group {
                    tokens: body_tokens,
                    ..
                } = &tokens[body]
                {
                    self.macro_depth += 1;
                    self.walk(body_tokens, false, module_dirs.clone());
                    self.macro_depth -= 1;
                }
                index = body + 1;
                continue;
            }
            if self.workspace && matches!(&tokens[index], SourceToken::Ident(word) if word == "use")
            {
                let end = tokens[index..]
                    .iter()
                    .position(|token| *token == SourceToken::Punct(';'))
                    .map_or(tokens.len(), |offset| index + offset);
                assert!(
                    !names_include(&tokens[index..end]),
                    "#1166 aliased include cannot be resolved statically"
                );
            }
            if self.workspace && matches!(&tokens[index], SourceToken::Ident(word) if word == "mod")
            {
                match (tokens.get(index + 1), tokens.get(index + 2)) {
                    (Some(SourceToken::Ident(name)), Some(SourceToken::Punct(';'))) => {
                        assert!(
                            self.macro_depth == 0,
                            "#1166 macro module cannot be resolved statically"
                        );
                        if !explicit_module {
                            let mut resolved = Vec::new();
                            for directory in &module_dirs {
                                let candidates = [
                                    directory.join(format!("{name}.rs")),
                                    directory.join(name).join("mod.rs"),
                                ];
                                let existing: Vec<_> = candidates
                                    .into_iter()
                                    .filter(|path| path.is_file())
                                    .collect();
                                assert!(
                                    existing.len() <= 1,
                                    "#1166 default Rust module is ambiguous: {} / {name}",
                                    directory.display()
                                );
                                resolved.extend(existing);
                            }
                            assert!(
                                !resolved.is_empty(),
                                "#1166 default Rust module must resolve to a file: {name}"
                            );
                            resolved.sort();
                            resolved.dedup();
                            for candidate in resolved {
                                let path = workspace_target(self.directory, &candidate);
                                self.targets.push(SourceEdge {
                                    module_dir: source_module_dir(&path),
                                    path,
                                    rust_source: true,
                                });
                            }
                        }
                        explicit_module = false;
                        index += 3;
                        continue;
                    }
                    (
                        Some(SourceToken::Ident(name)),
                        Some(SourceToken::Group {
                            delimiter: Delimiter::Brace,
                            tokens: inner,
                        }),
                    ) => {
                        self.walk(
                            inner,
                            false,
                            module_dirs.iter().map(|dir| dir.join(name)).collect(),
                        );
                        explicit_module = false;
                        index += 3;
                        continue;
                    }
                    (Some(SourceToken::Punct('$')), Some(SourceToken::Ident(_)))
                        if self.macro_depth > 0 =>
                    {
                        if let Some(SourceToken::Group {
                            delimiter: Delimiter::Brace,
                            tokens: inner,
                        }) = tokens.get(index + 3)
                        {
                            // An inline generated module adds no file edge itself.
                            // Its body is still scanned at macro depth: any source
                            // edge whose expansion directory is unknown is rejected.
                            self.walk(inner, false, module_dirs.clone());
                            explicit_module = false;
                            index += 4;
                            continue;
                        }
                        panic!("#1166 macro module cannot be resolved statically");
                    }
                    _ => panic!(
                        "#1166 unresolved Rust module declaration: {}",
                        self.file.display()
                    ),
                }
            }
            if self.workspace
                && let SourceToken::Ident(word) = &tokens[index]
            {
                assert!(
                    !explicit_module || word == "pub",
                    "#1166 path attribute must bind a module"
                );
            }
            // Any other group is a module body, function body, or macro
            // argument list: its inner `#[path]` is not at file top level.
            if let SourceToken::Group { tokens: inner, .. } = &tokens[index] {
                if self.workspace && index > 0 && tokens[index - 1] == SourceToken::Punct('!') {
                    assert!(
                        !names_indirect_include(inner),
                        "#1166 indirect include macro cannot be resolved statically: {}",
                        self.file.display()
                    );
                }
                self.walk(inner, false, module_dirs.clone());
            }
            index += 1;
        }
    }

    // `#[path = "..."]`, `#[r#path = "..."]` and the nested
    // `cfg_attr(..., path|r#path = "...")` form. doc comments arrive as
    // `#[doc = "..."]`; their text is a literal, never an edge.
    fn attribute_paths(&mut self, tokens: &[SourceToken], top_level: bool) {
        if is_doc_attribute(tokens) {
            return;
        }
        if self.workspace && attribute_names_path(tokens) {
            assert!(
                matches!(tokens.first(), Some(SourceToken::Ident(name)) if name == "path"),
                "#1166 conditional path attribute cannot be resolved statically"
            );
        }
        attribute_path_keys(tokens, &mut |tokens, index| {
            assert!(
                top_level,
                "vendored package {} #[path] must appear at the file top level; a non-top-level #[path] (macro body, `mod $i {{ ... }}`, inline module, function body, or macro argument) has a base directory determined by the expansion context, so this scanner rejects it: {}",
                self.package,
                self.file.display()
            );
            let literal = match tokens.get(index + 2) {
                Some(SourceToken::Literal(literal)) => literal,
                _ => self.reject_literal("#[path]"),
            };
            let value =
                plain_string_value(literal).unwrap_or_else(|| self.reject_literal("#[path]"));
            self.push_target("#[path]", &value, true);
        });
    }

    fn include(&mut self, arguments: &[SourceToken], kind: &str) {
        let arguments = if self.workspace {
            arguments
                .strip_suffix(&[SourceToken::Punct(',')])
                .unwrap_or(arguments)
        } else {
            arguments
        };
        let [SourceToken::Literal(literal)] = arguments else {
            self.reject_literal("include!");
        };
        let value = plain_string_value(literal).unwrap_or_else(|| self.reject_literal("include!"));
        self.push_target("include!", &value, kind == "include");
    }

    fn push_target(&mut self, kind: &str, value: &str, rust_source: bool) {
        let path = if self.workspace {
            assert!(!value.is_empty(), "#1166 empty Rust input path");
            workspace_target(
                self.directory,
                &self.file.parent().expect("#1166 source parent").join(value),
            )
        } else {
            assert_vendored_source_target(self.package, self.directory, self.file, kind, value)
        };
        let module_dir = path.parent().expect("#1166 target parent").to_path_buf();
        self.targets.push(SourceEdge {
            path,
            module_dir,
            rust_source,
        });
    }

    fn reject_literal(&self, kind: &str) -> ! {
        panic!(
            "vendored package {} {kind} must be a plain string literal: {}",
            self.package,
            self.file.display()
        )
    }
}

// `macro_rules! name <body>` uses `{`, `(` or `[` for the body. Return the
// group index; anything after the optional name that is not a group is not a
// definition this scanner can attribute.
fn macro_body(tokens: &[SourceToken], index: usize) -> Option<usize> {
    if tokens.get(index + 1) != Some(&SourceToken::Punct('!')) {
        return None;
    }
    let mut cursor = index + 2;
    if matches!(tokens.get(cursor), Some(SourceToken::Ident(_))) {
        cursor += 1;
    }
    match tokens.get(cursor) {
        Some(SourceToken::Group { .. }) => Some(cursor),
        _ => None,
    }
}

// Scan every file that can name a source edge, not only `.rs`: `#[path]` and
// `include!` compile targets with any extension or none. A non-UTF-8 file is a
// binary payload (e.g. the ICU collator blobs), which rustc cannot compile as
// Rust source and which therefore cannot introduce an edge; skip it. A
// non-`.rs` file that proc-macro2 cannot lex is ordinary text only when both
// the whitespace-stripped raw text and the whitespace-stripped comment-free
// text contain no edge marker; otherwise fail closed.
// The marker test must stay syntax-shaped: bare `path` / `include` substrings
// appear in ordinary files (the real vendored `Cargo.toml` `path = ...`,
// `LICENSE` "include", `README.md`, `encoder.json`, `vocab.bpe`), and rejecting
// those would make the real positive control fail. `include!` / `#[path` /
// `#![path` / `cfg_attr(...path=...)` are the only spellings that can name an
// edge.
pub(crate) fn vendored_source_mentions_edge(package: &str, path: &Path) -> bool {
    let bytes = fs::read(path).unwrap_or_else(|error| {
        panic!(
            "vendored package {package} source cannot be read: {} ({error})",
            path.display()
        )
    });
    let Ok(text) = String::from_utf8(bytes) else {
        return false;
    };
    match lex_source(&text) {
        Ok(tokens) => lexemes_name_edge(&tokens),
        Err(_) => {
            // Test two views of the same text in union. A marker obfuscated by
            // comments (`include/*c*/!(`) only appears once comments are gone;
            // a marker after a `//` that belongs to a string (`"http://..."`)
            // is deleted by the naive comment strip and only appears in the
            // whitespace-compacted raw text. Either view matching is enough:
            // this fallback only decides whether the file must be a candidate,
            // so over-approximating is the safe direction.
            let raw: String = text.chars().filter(|ch| !ch.is_whitespace()).collect();
            let uncommented: String = strip_comments(&text)
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect();
            assert!(
                !text_names_edge(&raw) && !text_names_edge(&uncommented),
                "vendored package {package} non-.rs file cannot be lexed as Rust but contains an edge marker (`#[path]` / `#![path]` / `include!` / `cfg_attr(...path=...)`, comment-obfuscated or not): {}",
                path.display()
            );
            false
        }
    }
}

// The text fallback for an unlexable non-`.rs` file. A marker split by
// whitespace is still caught after compaction. `cfg_attr` only counts together
// with a `path =` / `r#path =` argument, so `Cargo.toml`-style `path = "..."`
// alone does not reject a plain text file. Both outer (`#[path]`) and inner
// (`#![path]`) attributes count, matching the token-side `!` handling.
fn text_names_edge(compact: &str) -> bool {
    compact.contains("#[path")
        || compact.contains("#[r#path")
        || compact.contains("#![path")
        || compact.contains("#![r#path")
        || compact.contains("include!")
        || compact.contains("r#include!")
        || (compact.contains("cfg_attr")
            && (compact.contains("path=") || compact.contains("r#path=")))
}

// Naive `/* ... */` / `//` stripping for text the lexer rejected. There is no
// trustworthy string/char/raw-string context after a lexer failure, so a `//`
// inside `"http://..."` truncates the rest of that line; the caller therefore
// tests this view together with the raw text. Unterminated block comments drop
// the tail, which is another over-approximation in the same safe direction.
fn strip_comments(text: &str) -> String {
    let mut stripped = String::with_capacity(text.len());
    let mut rest = text;
    loop {
        let block = rest.find("/*");
        let line = rest.find("//");
        if let Some(block) = block
            && line.is_none_or(|line| block < line)
        {
            stripped.push_str(&rest[..block]);
            match rest[block + 2..].find("*/") {
                Some(end) => rest = &rest[block + 2 + end + 2..],
                None => break,
            }
        } else if let Some(line) = line {
            stripped.push_str(&rest[..line]);
            rest = rest[line..]
                .split_once('\n')
                .map_or("", |(_comment, tail)| tail);
        } else {
            stripped.push_str(rest);
            break;
        }
    }
    stripped
}

// `#[...]` / `#![...]` at `index` -> the bracket group index and its tokens.
// Shared by the edge scanner and the candidate-marker test so both agree on
// which shapes are attributes; `#` not followed by a bracket group is ordinary
// punctuation and is not an attribute.
fn attribute_bracket_group(
    tokens: &[SourceToken],
    index: usize,
) -> Option<(usize, &[SourceToken])> {
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
            tokens,
        }) => Some((attribute, tokens)),
        _ => None,
    }
}

// `#[doc = "..."]` (from `///` / `//!`) carries a string literal, never an edge.
fn is_doc_attribute(tokens: &[SourceToken]) -> bool {
    matches!(tokens.first(), Some(SourceToken::Ident(name)) if name == "doc")
}

// Walk the attribute body, nested `cfg_attr(...)` groups included, and call
// `visit` for every `path` / `r#path` key followed by `=`. Shared by the edge
// scanner and the candidate-marker test so a new attribute shape is handled by
// both at once; a one-sided change would collect a file without scanning it,
// or scan a file that was never collected as a candidate.
fn attribute_path_keys<'a>(
    tokens: &'a [SourceToken],
    visit: &mut impl FnMut(&'a [SourceToken], usize),
) {
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token, SourceToken::Ident(word) if word == "path")
            && tokens.get(index + 1) == Some(&SourceToken::Punct('='))
        {
            visit(tokens, index);
        }
        if let SourceToken::Group { tokens: inner, .. } = token {
            attribute_path_keys(inner, visit);
        }
    }
}

// Candidate test over the same token tree: `#` followed by a bracket group
// whose tokens contain a `path` attribute key, or `include` followed by `!`.
fn lexemes_name_edge(tokens: &[SourceToken]) -> bool {
    let mut index = 0;
    while index < tokens.len() {
        if matches!(&tokens[index], SourceToken::Ident(word) if word == "include")
            && tokens.get(index + 1) == Some(&SourceToken::Punct('!'))
        {
            return true;
        }
        if let Some((_, attribute_tokens)) = attribute_bracket_group(tokens, index)
            && attribute_names_path(attribute_tokens)
        {
            return true;
        }
        if let SourceToken::Group { tokens: inner, .. } = &tokens[index]
            && lexemes_name_edge(inner)
        {
            return true;
        }
        index += 1;
    }
    false
}

fn attribute_names_path(tokens: &[SourceToken]) -> bool {
    if is_doc_attribute(tokens) {
        return false;
    }
    let mut found = false;
    attribute_path_keys(tokens, &mut |_, _| found = true);
    found
}
