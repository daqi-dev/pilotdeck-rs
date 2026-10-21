//! Keep the human-approved #386 exception confined to its original module.
use regex::Regex;
use std::{collections::BTreeMap, fs, path::Path};

// Replace trivia/literals with spaces rather than deleting them: comments must
// not join two identifiers, and lifetimes must not hide the following code.
fn code_only(source: &str) -> String {
    let bytes = source.as_bytes();
    let mut code = bytes.to_vec();
    let character =
        Regex::new(r"^'(?:\\(?:u\{[0-9a-fA-F_]+\}|x[0-9a-fA-F]{2}|[^\r\n])|[^'\\\r\n])'").unwrap();
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
    String::from_utf8(code).unwrap()
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

fn scan_crate(root: &Path) -> BTreeMap<std::path::PathBuf, [usize; 2]> {
    let mut counts = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                if entry.file_name() != "target" {
                    pending.push(path);
                }
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                counts.insert(
                    path,
                    boundary_counts(&fs::read_to_string(entry.path()).unwrap()),
                );
            }
        }
    }
    counts
}

#[test]
fn unsafe_exception_stays_in_process_spawn() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let counts = scan_crate(root);
    let approved = root.join("src/process_spawn.rs");
    assert!(counts.contains_key(&approved), "approved module must exist");
    for (path, counts) in counts {
        assert_eq!(
            counts,
            if path == approved { [1, 1] } else { [0, 0] },
            "{}: counts [unsafe_code, unsafe] changed; review the unsafe boundary",
            path.display()
        );
    }
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
fn scans_all_crate_directories_except_target() {
    struct Scratch(std::path::PathBuf);
    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }
    let scratch =
        Scratch(Path::new("/tmp").join(format!("pdrs-boundary-{}", uuid::Uuid::new_v4())));
    let files = [
        "src/lib.rs",
        "tests/new.rs",
        "tests/common/path_module.rs",
        "benches/probe.rs",
        "examples/probe.rs",
        "build.rs",
        "other/path_module.rs",
    ];
    for file in files
        .into_iter()
        .chain(["target/ignored.rs", "nested/target/ignored.rs"])
    {
        let path = scratch.0.join(file);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, "#[warn(unsafe_code)] unsafe /* gap */ fn f() {}").unwrap();
    }
    let counts = scan_crate(&scratch.0);
    assert_eq!(counts.len(), files.len());
    for file in files {
        assert_eq!(counts[&scratch.0.join(file)], [1, 1], "{file}");
    }
}
