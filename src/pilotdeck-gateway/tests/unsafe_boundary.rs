//! Keep the human-approved #386 exception confined to its original module.
use regex::Regex;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

// Replace trivia/literals with spaces rather than deleting them: comments must
// not join two identifiers, and lifetimes must not hide the following code.
fn code_and_strings(source: &str) -> (String, BTreeMap<usize, usize>) {
    let bytes = source.as_bytes();
    let mut code = bytes.to_vec();
    let character =
        Regex::new(r"^'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|[^\r\n])|[^'\\\r\n])'").unwrap();
    let mut strings = BTreeMap::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i..].starts_with(b"//") {
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i] == b'r'
            && let Some(end) = raw_string_end(bytes, i)
        {
            i = end;
            strings.insert(start, i);
        } else if bytes[i] == b'"' {
            i += 1;
            while i < bytes.len() {
                let byte = bytes[i];
                i += 1;
                if byte == b'\\' {
                    i = (i + 1).min(bytes.len());
                } else if byte == b'"' {
                    break;
                }
            }
            strings.insert(start, i);
        } else if bytes[i] == b'\''
            && let Some(literal) = character.find(&source[i..])
        {
            i += literal.end();
        } else {
            // Advance over a whole UTF-8 character before slicing source again.
            i += source[i..].chars().next().unwrap().len_utf8();
            continue;
        }
        code[start..i].fill(b' ');
    }
    (String::from_utf8(code).unwrap(), strings)
}

fn code_only(source: &str) -> String {
    code_and_strings(source).0
}

// Also handles byte/C raw strings: the leading b/c is an unrelated identifier
// once the complete r###"..."### portion has been blanked out.
fn raw_string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut quote = start + 1;
    while bytes.get(quote) == Some(&b'#') {
        quote += 1;
    }
    if bytes.get(quote) != Some(&b'"') {
        return None;
    }
    let hashes = quote - start - 1;
    let mut end = quote + 1;
    while end < bytes.len() {
        if bytes[end] == b'"'
            && bytes.get(end + 1..end + 1 + hashes) == Some(&bytes[start + 1..quote])
        {
            return Some(end + 1 + hashes);
        }
        end += 1;
    }
    Some(bytes.len())
}

fn boundary_counts(source: &str) -> [usize; 2] {
    let code = code_only(source);
    // Count every occurrence, including cfg_attr, nested lint lists and macro
    // input. Counting the keyword itself covers blocks, fn/impl/trait/extern
    // and unsafe attributes regardless of intervening whitespace/comments.
    [r"\bunsafe_code\b", r"\bunsafe\b"]
        .map(|pattern| Regex::new(pattern).unwrap().find_iter(&code).count())
}

// Only the crate-root build target is excluded; nested target directories
// can contain normal Rust modules. Every text file is checked regardless of
// its name or extension. Keep
// unreadable entries visible, including binary data and symlinks (which could
// otherwise hide source outside the crate). Binary fixtures are pinned below.
#[derive(Default)]
struct CrateScan {
    sources: BTreeMap<PathBuf, String>,
    unreadable: Vec<PathBuf>,
}

fn scan_crate(root: &Path) -> CrateScan {
    let mut scan = CrateScan::default();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_name() == ".git" {
                continue;
            }
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                if path != root.join("target") {
                    pending.push(path);
                }
            } else if kind.is_file() {
                match fs::read_to_string(&path) {
                    Ok(source) => {
                        scan.sources.insert(path, source);
                    }
                    Err(_) => scan.unreadable.push(path),
                }
            } else {
                scan.unreadable.push(path);
            }
        }
    }
    scan
}

// Resolve only literal paths. Computed paths (concat!/env!/macro parameters)
// and escaped literals fail closed, rather than guessing what rustc will load.
fn referenced_paths(source: &str) -> Vec<String> {
    let (code, strings) = code_and_strings(source);
    let mut starts = Vec::new();
    let includes = Regex::new(r"\binclude(?:_str|_bytes)?\s*!\s*[({\[]").unwrap();
    // Reject renamed/re-exported builtins rather than losing their paths.
    let imports = Regex::new(r"\buse\b[^;]*\binclude(?:_str|_bytes)?\b").unwrap();
    assert!(
        !imports.is_match(&code),
        "include macros must be invoked directly, without aliases"
    );
    starts.extend(includes.find_iter(&code).map(|m| (m.end(), true)));
    let attributes = Regex::new(r"#\s*!?\s*\[").unwrap();
    let path_key = Regex::new(r"\bpath\s*=").unwrap();
    for attribute in attributes.find_iter(&code) {
        let mut depth = 1;
        let mut end = attribute.end();
        for byte in code[attribute.end()..].bytes() {
            end += 1;
            match byte {
                b'[' => depth += 1,
                b']' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                break;
            }
        }
        for key in path_key.find_iter(&code[attribute.end()..end]) {
            starts.push((attribute.end() + key.end(), false));
        }
    }
    starts
        .into_iter()
        .map(|(start, is_include)| {
            let (&literal_start, &literal_end) = strings
                .range(start..)
                .next()
                .expect("source reference must use a literal path");
            assert!(
                code[start..literal_start].trim().is_empty(),
                "source reference must use a literal path"
            );
            let tail = code[literal_end..].trim_start();
            let tail = tail.strip_prefix(',').unwrap_or(tail).trim_start();
            assert!(
                if is_include {
                    tail.starts_with([')', ']', '}'])
                } else {
                    tail.starts_with([']', ')'])
                },
                "source reference must contain only a literal path"
            );
            let closing = code.len() - tail.len();
            assert!(
                strings
                    .range(literal_end..)
                    .next()
                    .is_none_or(|(&next, _)| next >= closing),
                "source reference must contain only one literal path"
            );
            let literal = &source[literal_start..literal_end];
            let value = if literal.starts_with('r') {
                let quote = literal.find('"').unwrap();
                &literal[quote + 1..literal.len() - quote]
            } else {
                &literal[1..literal.len() - 1]
            };
            assert!(
                !value.contains('\\'),
                "escaped source paths require explicit review"
            );
            value.to_owned()
        })
        .collect()
}

fn assert_references_scanned(root: &Path, scan: &CrateScan) {
    let root = root.canonicalize().unwrap();
    for (path, source) in &scan.sources {
        for reference in referenced_paths(source) {
            let target = path
                .parent()
                .unwrap()
                .join(&reference)
                .canonicalize()
                .unwrap_or_else(|error| {
                    panic!(
                        "{}: unscanned reference {reference:?}: {error}",
                        path.display()
                    )
                });
            assert!(
                target.starts_with(&root) && scan.sources.contains_key(&target),
                "{}: reference {reference:?} resolves outside crate or to an unscanned file: {}",
                path.display(),
                target.display()
            );
        }
    }
}

#[test]
fn unsafe_exception_stays_in_process_spawn() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .canonicalize()
        .unwrap();
    let scan = scan_crate(&root);
    // These two existing malformed-UTF-8 fixtures cannot be Rust sources.
    // Pin their bytes so the whitelist cannot conceal a changed payload.
    use sha2::{Digest, Sha256};
    let binary_fixtures = [
        (
            "tests/fixtures/operation-ledger/invalid-utf8.jsonl",
            "dbd351c8be0dbdd20ff4b5867b01ef368f59f40a846dac5b68e70554d21b9b60",
        ),
        (
            "tests/fixtures/run-registry/quirk-invalid-utf8.json",
            "25ce634041412dc81a98a0dc4e4097882d10c69f4f20efdba02bdc57a7cba3dd",
        ),
    ];
    let mut unreadable = scan.unreadable.clone();
    unreadable.sort();
    assert_eq!(
        unreadable,
        binary_fixtures.map(|(file, _)| root.join(file)),
        "unreadable crate files differ from the binary fixture whitelist"
    );
    for (file, hash) in binary_fixtures {
        assert_eq!(
            format!("{:x}", Sha256::digest(fs::read(root.join(file)).unwrap())),
            hash,
            "binary fixture whitelist bytes changed: {file}"
        );
    }
    assert_references_scanned(&root, &scan);
    let approved = root.join("src/process_spawn.rs");
    assert!(
        scan.sources.contains_key(&approved),
        "approved module must exist"
    );
    // Existing prose/TypeScript uses the keyword in backticks/template
    // literals, which Rust's lexer treats as code. Pin each whole payload;
    // these allowances cannot be transferred to executable Rust content.
    let text_fixtures = [
        (
            "tests/fixtures/bash-tool/README.md",
            "0df8b3920914bf0df60c5725c2df2c8ac23321238d8b928389b0de2418483a4f",
            [0, 1],
        ),
        (
            "tests/fixtures/compaction/generate.mts",
            "9ba6ea79166d8882c1290c63e9c965492bdbb6a030fbcc60f630a760dcd85d2a",
            [0, 1],
        ),
        (
            "tests/fixtures/recorder/generate.mts",
            "58d3ac0756397415509461ad613b6674fac39392b1daa65b0953364a604b6462",
            [0, 1],
        ),
        (
            "tests/fixtures/sdk-session/generate.mts",
            "a72cdcbbfa38f71e7a202d86ccd7daf101229b5dded0c9f9ee41dc7ed4edb4af",
            [0, 1],
        ),
        (
            "tests/fixtures/snapshot-projection/generate.mts",
            "2ea22f95eb951fdd7595dd5473853edc0267ebb083306a05ace6d0bf9e6c82d2",
            [0, 13],
        ),
        (
            "tests/fixtures/transcript-adapter/generate.mts",
            "873e684346bee0cd967188472993e87724076b8b21eb71a0cf0006d1101f21fb",
            [0, 1],
        ),
        (
            "tests/fixtures/upload/generate.mts",
            "61f0170f2491270eb91132be01d40f2bc56420b5ce3cd99c8204519e1da76b92",
            [0, 1],
        ),
        (
            "tests/fixtures/workspace-snapshot/generate.mts",
            "7f57bb0169d5f7238e962d36ecf4be9f5bc189378166cd85fdf7dff8974c1b28",
            [0, 2],
        ),
    ];
    let mut violations = Vec::new();
    for (path, source) in scan.sources {
        // Cargo has one deny declaration and one explanatory comment token.
        let expected = if path == approved {
            [1, 1]
        } else if path == root.join("Cargo.toml") {
            assert!(source.lines().any(|line| line == "unsafe_code = \"deny\""));
            [1, 1]
        } else if let Some((_, hash, counts)) = text_fixtures
            .iter()
            .find(|(file, _, _)| path == root.join(file))
        {
            assert_eq!(
                format!("{:x}", Sha256::digest(source.as_bytes())),
                *hash,
                "text fixture allowance bytes changed: {}",
                path.display()
            );
            *counts
        } else {
            [0, 0]
        };
        let counts = boundary_counts(&source);
        if counts != expected {
            violations.push(format!(
                "{}: counts [unsafe_code, unsafe] changed: {counts:?} != {expected:?}",
                path.display()
            ));
        }
    }
    assert!(violations.is_empty(), "{}", violations.join("\n"));
}

#[test]
fn counts_all_lint_levels_and_keyword_forms() {
    for source in [
        "#[expect(unsafe_code)] pub fn zz_pid() -> i32 { unsafe /* */ { libc::getpid() } }",
        "#[warn(unsafe_code)] unsafe fn f() {}",
        "#[deny(unsafe_code)] unsafe impl Trait for Type {}",
        "#[forbid(unsafe_code)] unsafe trait Trait {}",
        "#[cfg_attr(unix, allow(other(nested), rust::unsafe_code))] unsafe extern \"C\" {}",
        "#![allow(dead_code, unsafe_code)] #[unsafe(no_mangle)] fn f() {}",
        "macro_input!(unsafe_code); unsafe // line comment\n /* outer /* inner */ */ {}",
        "fn f<'a>(x: &'a str) { unsafe {} } #[warn(unsafe_code)]",
    ] {
        assert_eq!(boundary_counts(source), [1, 1], "{source}");
    }
    assert_eq!(boundary_counts("un/* separator */safe {}"), [0, 0]);
    assert_eq!(boundary_counts("unsafe_code_more unsafe_more"), [0, 0]);
}

#[test]
fn ignores_comments_and_all_string_and_character_forms() {
    let source = r####"
        // unsafe { #[allow(unsafe_code)]
        /* unsafe fn /* unsafe trait */ unsafe impl unsafe_code */
        let normal = "unsafe { unsafe_code \" still a string";
        let bytes = b"unsafe { unsafe_code";
        let c = c"unsafe { unsafe_code";
        let raw = r"unsafe { unsafe_code";
        let hashes = r###"unsafe { "## unsafe_code /*"###;
        let raw_bytes = br##"unsafe { unsafe_code"##;
        let raw_c = cr#"unsafe { unsafe_code"#;
        let chars = ('"', '\'', '\\', '\u{1f980}', '\x75', '界', b'"');
    "####;
    assert_eq!(boundary_counts(source), [0, 0]);
    assert_eq!(
        boundary_counts(&format!("{source} #[warn(unsafe_code)] unsafe {{}}")),
        [1, 1]
    );
    // Quotes in character literals must not start a string that hides code.
    assert_eq!(boundary_counts("let c = '\"'; unsafe {}"), [0, 1]);
}

#[test]
fn scans_all_files_except_target_and_git() {
    let scratch = Scratch::new();
    let files = [
        "src/lib.rs",
        "tests/new.rs",
        "tests/common/path_module.rs",
        "benches/probe.rs",
        "examples/probe.rs",
        "build.rs",
        "other/path_module.rs",
        "src/evil.RS",
        "src/evil.rs.txt",
        "extensionless",
        "src/target/mod.rs",
        "nested/target/probe.rs",
    ];
    for file in files.into_iter().chain([
        "target/ignored.rs",
        "nested/.git/ignored.rs",
        ".git/ignored.rs",
    ]) {
        let path = scratch.0.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#[warn(unsafe_code)] unsafe /* gap */ fn f() {}").unwrap();
    }
    let scan = scan_crate(&scratch.0);
    assert!(scan.unreadable.is_empty());
    assert_eq!(scan.sources.len(), files.len());
    for file in files {
        assert_eq!(
            boundary_counts(&scan.sources[&scratch.0.join(file)]),
            [1, 1],
            "{file}"
        );
    }
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = Path::new("/tmp").join(format!("pdrs-boundary-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn resolves_path_attributes_and_all_include_macros() {
    let source = r####"
        #[path /* comment */ = r#"evil.RS"#] mod evil;
        #[cfg_attr(unix, path = "nested/module.rs")] mod module;
        include /* comment */ ! { "evil.rs.txt" }
        include_str!["fixture.json",];
        include_bytes!(r##"fixture.data"##);
        // #[path = "ignored.rs"] include!("ignored.rs");
        const TEXT: &str = "include!(\"ignored.rs\")";
    "####;
    assert_eq!(
        referenced_paths(source),
        [
            "evil.rs.txt",
            "fixture.json",
            "fixture.data",
            "evil.RS",
            "nested/module.rs"
        ]
    );
    for source in [
        "use std::include as load; load!(\"outside.rs\");",
        "include!(concat!(\"outside\", \".rs\"));",
        "include!(env!(\"SOURCE\"));",
        "#[path = concat!(\"outside\", \".rs\")] mod evil;",
        "include!(\"a\", \"b\");",
        r#"#[path = "esc\x61ped.rs"] mod evil;"#,
    ] {
        assert!(
            std::panic::catch_unwind(|| referenced_paths(source)).is_err(),
            "{source}"
        );
    }
}

#[test]
fn rejects_external_missing_and_excluded_references() {
    let scratch = Scratch::new();
    let root = scratch.0.join("crate");
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(root.join("src/evil.RS"), "fn f() {}").unwrap();
    fs::write(scratch.0.join("x.rs"), "fn f() {}").unwrap();
    let main = root.join("src/lib.rs");
    fs::write(
        &main,
        "#[path = \"evil.RS\"] mod evil; include_str!(\"evil.RS\");",
    )
    .unwrap();
    assert_references_scanned(&root, &scan_crate(&root));
    for (directory, reference) in [("target", "../target/x.rs"), (".git", "../.git/x.rs")] {
        fs::create_dir(root.join(directory)).unwrap();
        fs::write(root.join(directory).join("x.rs"), "fn f() {}").unwrap();
        fs::write(&main, format!("include!({reference:?});")).unwrap();
        assert!(
            std::panic::catch_unwind(|| assert_references_scanned(&root, &scan_crate(&root)))
                .is_err()
        );
    }
    for source in [
        "#[path = \"../../x.rs\"] mod evil;",
        "include_str!(\"missing.txt\");",
        "include_bytes!(\"../../x.rs\");",
    ] {
        fs::write(&main, source).unwrap();
        assert!(
            std::panic::catch_unwind(|| assert_references_scanned(&root, &scan_crate(&root)))
                .is_err(),
            "{source}"
        );
    }
}

#[test]
fn unreadable_files_are_reported() {
    let scratch = Scratch::new();
    let invalid = scratch.0.join("binary.data");
    fs::write(&invalid, [0xff]).unwrap();
    let scan = scan_crate(&scratch.0);
    assert_eq!(scan.unreadable, [invalid]);
    assert!(scan.sources.is_empty());
    #[cfg(unix)]
    {
        let link = scratch.0.join("external.RS");
        std::os::unix::fs::symlink("/outside-crate.rs", &link).unwrap();
        assert!(scan_crate(&scratch.0).unreadable.contains(&link));
    }
}
