//! Keep the human-approved #386 exception confined to its original module.
use regex::Regex;
use std::{fs, path::Path};

#[test]
fn unsafe_exception_stays_in_process_spawn() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let patterns = [
        r"\ballow\s*\([^)]*\bunsafe_code\b[^)]*\)",
        r"\bunsafe\s*\{",
        r"\bunsafe\s+fn\b",
        r"\bunsafe\s+impl\b",
    ]
    .map(|pattern| Regex::new(pattern).unwrap());
    let mut pending = vec![root.clone()];
    let mut found_exception = false;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                let source = fs::read_to_string(&path).unwrap();
                let counts = patterns
                    .each_ref()
                    .map(|pattern| pattern.find_iter(&source).count());
                let approved = path == root.join("process_spawn.rs");
                found_exception |= approved;
                assert_eq!(
                    counts,
                    if approved { [1, 1, 0, 0] } else { [0; 4] },
                    "{}: counts [allow, block, fn, impl] changed; review the unsafe boundary",
                    path.display()
                );
            }
        }
    }
    assert!(found_exception, "approved module must exist");
}
