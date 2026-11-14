#!/usr/bin/env python3
"""Temporary #2558 guard mutations; require the bound guard to fail, then restore.

Run with no other writers/builds in this worktree. Evidence goes in a new
output folder; original source bytes are verified by ``mutating_sources``
(HEAD baseline — this file must be committed before it can run). Each mutant
is recorded as KILLED / SURVIVED / INVALID and the script exits non-zero
unless every selected mutant is KILLED.

Bound guards (all in ``src/pilotdeck-gateway/tests/console_panic_safety.rs``):

* ``error_console_writes_stay_non_panicking`` — the listed-file scan
* ``shared_writer_call_sites_stay_listed`` — the derived listing rule
* ``production_tree_stays_eprintln_free`` — fix-r3's whole-tree production
  scan plus its listed-file anchor
* ``assert_no_eprintln_kills_a_real_eprintln`` — the detector self-test
  (fix-r4: the widened match set; fix-r5: ``eprint``, the fourth macro)
* ``test_family_files_stay_declaration_gated`` — fix-r8's declaration pin,
  made path-resolving in fix-r9 (the anchor set the production walk's
  name exemption shares)
* ``assert_no_eprintln_skips_only_cfg_test_items`` — the skip-boundary
  fixture self-test (fix-r9: the keyword-follower whitelist)
* ``anchor_scanner_resolves_rustc_module_semantics`` — fix-r10/fix-r11's
  module-resolution fixture (the rustc-pinned scanner semantics every
  earlier anchor decision is judged against)

Use: CARGO_TARGET_DIR=... python3 -I -B .../negative_controls.py <new output
directory> [mutation name ...] (no names: every mutant).
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_failed, panic_evidence
from negative_selection import (baseline_passed, claim_dir, missing_target,
                                 refuse, refuse_unusable_target, registered,
                                 select, verdict_exit)
from negative_process import (CARGO_SLOTS_WAIT_SECONDS, mutating_sources,
                               run_owned, wait_for_slot, write_source_atomic)

ROOT = Path(__file__).resolve().parents[5]
TLS = 'src/pilotdeck-gateway/src/runtime/model/http/tls.rs'
GUARD = 'src/pilotdeck-gateway/tests/console_panic_safety.rs'
ENCODING = 'src/pilotdeck-gateway/src/runtime/model/http/encoding.rs'
MAIN = 'src/pilotdeck-gateway/src/main.rs'
# fix-r12 (MM sibling#3): the shared lexer the fixture's renamed `r#path`
# literal observes (tests/common/source_inputs.rs).
SHARED_LEXER = 'src/pilotdeck-gateway/tests/common/source_inputs.rs'

LIST_SCAN = 'error_console_writes_stay_non_panicking'
LISTING_RULE = 'shared_writer_call_sites_stay_listed'
TREE_SCAN = 'production_tree_stays_eprintln_free'
DETECTOR_SELF_TEST = 'assert_no_eprintln_kills_a_real_eprintln'
DECLARATION_PIN = 'test_family_files_stay_declaration_gated'
SKIP_TEST = 'assert_no_eprintln_skips_only_cfg_test_items'
SCANNER_TEST = 'anchor_scanner_resolves_rustc_module_semantics'

# Each mutant is applied to the original, never cumulatively. #2558 fix-r3
# review P1: the tls-warn-line-reverted / tls-guard-unlisted pair pins the
# listed-file scan and the listing rule over the new tls.rs entry; the tree
# mutants pin the whole-tree scan, its listed-file anchor, and both
# directions of the test-support exclusion. fix-r4 review P1-1: the
# stdout-banner-reverted / guard-stdout-macros-dropped pair pins the
# stdout face of the same scans — the entry's banner lines reverting to
# bare `println!` must turn the listed-file scan red, and the detector's
# widened match set reverting to `eprintln` alone must turn the self-test
# red. fix-r5 review P1: guard-eprint-macro-dropped pins the fourth macro
# alone — dropping just `eprint` while keeping the fix-r4 three must fail
# the self-test's fourth loop iteration, so the set cannot silently
# regress to r4's shape either.
# fix-r11 (B2, MM sibling#1 ≡ DS sibling#2): every fix-r10/fix-r11
# scanner decision gets its paired mutant, the fix-r9 pairing rule —
# each row degrades one decision back to the shape it replaced and the
# fixture's own assertion names the regressed decision in its panic
# payload. The atomic-publish decision (write_source_atomic) is rig I/O,
# not scanner semantics: it has no mutant here and is pinned by the
# restore red check inside this runner (the sha256 compare that fails
# the run on a dirty tree).
# fix-r12 (B2 pairing continues): fix-r12's scanner decisions get their
# paired mutants too — the A1 arm's literal join, the E12b plainness
# rule (now the re-check inside the shared-walker visit), the shared
# lexer's `r#` normalization, and the `..`-literal mount normalization.
# The shared-lexer row mutates the shared helper itself
# (tests/common/source_inputs.rs), bound to the fixture that observes
# it through the de-tautologized `r#path` literal.
# fix-r13 (the r12 review's one double-hit finding, sibling#1 ==
# boundary#1): the `..` normalization's inline-body call site gets its
# own fixture and its own paired mutant, the same one-decision-one-
# mutant rule.
mutations = [
    ('tls-warn-line-reverted', TLS,
     '            // #2558: the load_pem warn line\'s non-panicking shape.\n'
     '            crate::native_process::channels::console_log_line(\n'
     '                &mut std::io::stderr(),\n'
     '                &format!(\n'
     '                    "Warning: could not read CA directory {}: {error}",\n'
     '                    dir.display()\n'
     '                ),\n'
     '            );',
     '            eprintln!(\n'
     '                "Warning: could not read CA directory {}: {error}",\n'
     '                dir.display()\n'
     '            );'),
    ('tls-guard-unlisted', GUARD,
     '    "src/lib.rs",\n    "src/runtime/model/http/tls.rs",\n'
     '    "src/host/tests/resume.rs",',
     '    "src/lib.rs",\n    "src/main.rs",\n'
     '    "src/host/tests/resume.rs",'),
    ('stdout-banner-reverted', MAIN,
     '    // #2558: best-effort console lines, like TS\'s `console.log` — a raw\n'
     '    // `println!` here panics when the write fails (std behavior), and these\n'
     '    // banners run on the startup path inside the main task: a closed stdout\n'
     '    // would kill the listener right after the bind, before the accept loop\n'
     '    // answers a connection. The stdout twin of the metadata line below (the\n'
     '    // stderr half of the same fix, `refresh.rs`\'s `log` default shape).\n'
     '    pilotdeck_gateway::native_process::console_log_line(\n'
     '        &mut std::io::stdout(),\n'
     '        &format!("PilotDeck server listening: http://{address}"),\n'
     '    );\n'
     '    pilotdeck_gateway::native_process::console_log_line(\n'
     '        &mut std::io::stdout(),\n'
     '        &format!("WebSocket: ws://{address}/ws"),\n'
     '    );',
     '    println!("PilotDeck server listening: http://{address}");\n'
     '    println!("WebSocket: ws://{address}/ws");'),
    ('guard-stdout-macros-dropped', GUARD,
     '            SourceToken::Ident(word)\n'
     '                if word == "eprintln"\n'
     '                    || word == "println"\n'
     '                    || word == "print"\n'
     '                    || word == "eprint" =>\n'
     '            {',
     '            SourceToken::Ident(word) if word == "eprintln" => {'),
    ('guard-eprint-macro-dropped', GUARD,
     '            SourceToken::Ident(word)\n'
     '                if word == "eprintln"\n'
     '                    || word == "println"\n'
     '                    || word == "print"\n'
     '                    || word == "eprint" =>\n'
     '            {',
     '            SourceToken::Ident(word)\n'
     '                if word == "eprintln" || word == "println" || word == "print" =>\n'
     '            {'),
    ('unlisted-file-eprintln', ENCODING,
     'pub(crate) fn decode_response(',
     'fn negative_control_eprintln_probe() {\n'
     '    eprintln!("#2558 negative control: an unlisted production eprintln");\n'
     '}\n\n'
     'pub(crate) fn decode_response('),
    ('tree-walk-no-descend', GUARD,
     '                stack.push(path);',
     '                drop(path);'),
    ('exclusion-widened-to-everything', GUARD,
     '    name == "tests.rs" || name.ends_with("_tests.rs") || name.starts_with("test_")',
     '    true'),
    ('exclusion-narrowed-test-family', GUARD,
     '    name == "tests.rs" || name.ends_with("_tests.rs") || name.starts_with("test_")',
     '    name == "tests.rs" || name.ends_with("_tests.rs")'),

    ('anchor-gates-basename-degraded', GUARD,
     '    /// name equality — the fix-r9 rule.\n'
     '    fn gates(&self, file: &Path) -> bool {\n'
     '        self.mounted_files.contains(file)\n'
     '    }\n',
     '    fn gates(&self, file: &Path) -> bool {\n'
     '        self.mounted_files\n'
     '            .iter()\n'
     '            .any(|mounted| mounted.file_name() == file.file_name())\n'
     '    }\n'),
    ('anchor-mod-dir-replaced', GUARD,
     '            PathAttribute::NotPath => [\n'
     '                dir.join(format!("{name}.rs")),\n'
     '                dir.join(name).join("mod.rs"),\n'
     '            ]\n',
     '            PathAttribute::NotPath => [\n'
     '                base.join(format!("{name}.rs")),\n'
     '                base.join(name).join("mod.rs"),\n'
     '            ]\n'),
    ('anchor-path-literal-file-dir-dropped', GUARD,
     '            PathAttribute::Literal(literal) => '
     'Some(normalize_mounted(&base.join(literal))),\n',
     '            PathAttribute::Literal(literal) => '
     'Some(normalize_mounted(&dir.join(literal))),\n'),
    ('follower-whitelist-reverted', GUARD,
     '                    return match tokens.get(index + 1) {\n'
     '                        Some(SourceToken::Literal(_)) => true,\n'
     '                        Some(SourceToken::Ident(name)) => !matches!(\n'
     '                            name.as_str(),\n'
     '                            "if" | "else"\n'
     '                                | "while"\n'
     '                                | "for"\n'
     '                                | "loop"\n'
     '                                | "match"\n'
     '                                | "break"\n'
     '                                | "continue"\n'
     '                                | "return"\n'
     '                                | "in"\n'
     '                                | "ref"\n'
     '                                | "mut"\n'
     '                                | "as"\n'
     '                                | "where"\n'
     '                                | "move"\n'
     '                                | "dyn"\n'
     '                                | "self"\n'
     '                                | "Self"\n'
     '                                | "true"\n'
     '                                | "false"\n'
     '                                | "await"\n'
     '                        ),\n'
     '                        Some(SourceToken::Punct(\'<\' | \'!\' | \'\\\'\')) => true,\n'
     '                        _ => false,\n'
     '                    };\n',
     '                    if matches!(\n'
     '                        tokens.get(index + 1),\n'
     '                        Some(SourceToken::Punct(\':\' | \'=\' | \',\' | \'>\'))\n'
     '                    ) {\n'
     '                        return false;\n'
     '                    }\n'
     '                    return true;\n'),

    # fix-r11 (B2): the fix-r10/fix-r11 scanner decisions, one paired
    # mutant each, all bound to the fixture that pins the rustc
    # module-resolution semantics.
    # fix-r10 boundary#1: un-attributed `mod x;` declarations must open a
    # declaration walk (recognition), and the anchor fixpoint must let an
    # anchored name-exempted file contribute its own mounts.
    ('plain-mod-declaration-blind', GUARD,
     '            SourceToken::Ident(word) => word == "pub" || word == "mod",\n',
     '            SourceToken::Ident(_) => false,\n'),
    ('anchor-fixpoint-dropped', GUARD,
     '            if exempted && anchored.contains(file) {\n',
     '            if false && exempted && anchored.contains(file) {\n'),
    # fix-r10 boundary#2: the inline body is visited exactly once, through
    # its own declaration's body directory (a module context, not opaque).
    ('inline-body-opaque', GUARD,
     '                        collect_mod_declarations(\n'
     '                            body,\n'
     '                            file,\n'
     '                            body_dir,\n'
     '                            &body_base,\n',
     '                        collect_mod_declarations(\n'
     '                            body,\n'
     '                            file,\n'
     '                            body_dir,\n'
     '                            &PathBase::Opaque,\n'),
    # fix-r10 boundary#3: a `#[path]` literal inside an inline `mod` body
    # joins that inline module's directory, not the declaring file's.
    ('inline-path-base-parent-kept', GUARD,
     '                            (_, PathBase::Module(_)) => {\n'
     '                                let nested = dir.join(name);\n'
     '                                (nested.clone(), '
     'PathBase::Module(nested))\n'
     '                            }\n',
     '                            (_, PathBase::Module(_)) => {\n'
     '                                let nested = dir.join(name);\n'
     '                                (nested.clone(), '
     'PathBase::Module(dir.to_path_buf()))\n'
     '                            }\n'),
    # fix-r10 sibling#1: a non-module context mounts nothing — the
    # expansion-context `#[path]` guess the #1180-audited shared scanner
    # rejects (fail-closed).
    ('opaque-context-mounts', GUARD,
     '        PathBase::Opaque => None,\n',
     '        PathBase::Opaque => match path_attr {\n'
     '            PathAttribute::Literal(literal) => Some(dir.join(literal)),\n'
     '            _ => None,\n'
     '        },\n'),
    # fix-r10 simplify#1: an unreadable `path` key mounts nothing — no
    # fail-open collapse into the default-candidate branch.
    ('unreadable-path-default-mounts', GUARD,
     '            PathAttribute::Unreadable => None,\n',
     '            PathAttribute::Unreadable => [\n'
     '                dir.join(format!("{name}.rs")),\n'
     '                dir.join(name).join("mod.rs"),\n'
     '            ]\n'
     '            .into_iter()\n'
     '            .find(|candidate| candidate.is_file()),\n'),
    # fix-r10 sibling#2: a name-exempted file seeds nothing until it is
    # itself anchored — the seeds stay gated mounts of non-exempted files.
    ('exempted-files-seed', GUARD,
     '        .filter(|(file, _)| {\n'
     '            !file\n'
     '                .file_name()\n'
     '                .and_then(|name| name.to_str())\n'
     '                .is_some_and(is_test_support_source)\n'
     '        })\n',
     '        .filter(|_| true)\n'),
    # fix-r11 B1 (MM boundary#1): the compilation closure — only the
    # crate roots and what reachable declarations mount are harvested;
    # an unreachable file's gated declaration is a phantom seed.
    ('unreachable-seeds-again', GUARD,
     '        .filter(|(file, _)| {\n'
     '            *file == manifest.join("src/lib.rs") '
     '|| *file == manifest.join("src/main.rs")\n'
     '        })\n',
     '        .filter(|_| true)\n'),
    # fix-r11 B3 (DS boundary#1, rustc E13): a direct `#[path]` whose
    # value is not one plain string literal is malformed — nothing loads,
    # never the default candidates.
    ('direct-path-multitoken-notpath', GUARD,
     '                _ => PathAttribute::Unreadable,\n',
     '                _ => PathAttribute::NotPath,\n'),
    # fix-r11 B4 (DS boundary#2, rustc E10/E10b): a `#[path]`-loaded
    # file's own children resolve from the loaded file's directory, not
    # the stem directory a default-mounted file would use.
    ('path-mounted-child-stem-dir', GUARD,
     '            let child_dir = if mounts.path_mounted.contains(target) {\n',
     '            let child_dir = if false && '
     'mounts.path_mounted.contains(target) {\n'),
    # fix-r10 simplify#1's merge half: two readable `#[path]` literals on
    # one declaration merge to unreadable — last-wins is a guess.
    ('duplicate-path-last-wins', GUARD,
     '            (PathAttribute::Literal(_), PathAttribute::Literal(_)) '
     '=> PathAttribute::Unreadable,\n',
     '            (PathAttribute::Literal(_), other) => other,\n'),
    # fix-r12 (A1, three-lineage DS sibling#1 ≡ MM sibling#1 ≡ DS
    # boundary#2): the `#[path]`-mounted inline body's directory is the
    # literal's join — degrading it to the plain-inline `dir.join(name)`
    # ignores the literal, so the body's gated declaration misses its
    # file.
    ('inline-path-literal-ignored', GUARD,
     '                                let mounted = '
     'normalize_mounted(&base.join(literal));\n',
     '                                let mounted = dir.join(name);\n'),
    # fix-r12 (E12b, MM sibling#2 ≡ DS same point): the plainness rule
    # itself — relaxing it counts any value as a key, so the non-literal
    # `cfg_attr`-carried path becomes unreadable and the default
    # candidates the off-branch rustc loads never mount.
    ('conditional-key-value-any', GUARD,
     '        if matches!(\n'
     '            key_tokens.get(index + 2),\n'
     '            Some(SourceToken::Literal(value))\n'
     '                if source_inputs::plain_string_value(value).is_some()\n'
     '        ) {\n'
     '            found = true;\n'
     '        }\n',
     '        if key_tokens.get(index + 2).is_some() {\n'
     '            found = true;\n'
     '        }\n'),
    # fix-r12 (MM sibling#3): the shared lexer's `r#` normalization —
    # keeping the raw spelling leaves the attribute NotPath, and the
    # renamed `r#path` literal (no same-named default candidate) mounts
    # nothing.
    ('shared-lexer-rpath-kept', SHARED_LEXER,
     '    let text = ident.to_string();\n'
     '    text.strip_prefix("r#").unwrap_or(&text).to_owned()\n',
     '    ident.to_string()\n'),
    # fix-r12 (MM boundary#1, rustc E10b): the `..`-component
    # normalization — pushing `..` through keeps the text join, so the
    # src-walk table lookup misses the file rustc compiles and the
    # subtree drops out of the harvest.
    ('dot-dot-mount-text-joined', GUARD,
     '            std::path::Component::ParentDir => {\n'
     '                normalized.pop();\n'
     '            }\n',
     '            std::path::Component::ParentDir => {\n'
     '                normalized.push("..");\n'
     '            }\n'),
    # fix-r13 (r12 review sibling#1 == boundary#1, one finding hit by
    # both roles): the same normalization's INLINE-body call site — the
    # row above degrades `normalize_mounted` itself (both call sites);
    # this one reverts only the inline join to the text form, which at
    # fix-r12 left every fixture green (the file-form `dot` fixture
    # covers the other call site, and the inline `inl` literal is
    # dot-free). The `inl2` fixture's distinct target observes it.
    ('inline-dot-normalize-dropped', GUARD,
     '                                let mounted = '
     'normalize_mounted(&base.join(literal));\n',
     '                                let mounted = base.join(literal);\n'),
]
# #1666: static denominator. `len(mutations)` is not a denominator: deleting a
# row shortens the run silently, and an emptied table prints `0/0 killed`.
EXPECTED_NAMES = (
    'tls-warn-line-reverted', 'tls-guard-unlisted', 'stdout-banner-reverted',
    'guard-stdout-macros-dropped', 'guard-eprint-macro-dropped',
    'unlisted-file-eprintln',
    'tree-walk-no-descend', 'exclusion-widened-to-everything',
    'exclusion-narrowed-test-family',
    'anchor-gates-basename-degraded', 'anchor-mod-dir-replaced',
    'anchor-path-literal-file-dir-dropped', 'follower-whitelist-reverted',
    'plain-mod-declaration-blind', 'anchor-fixpoint-dropped',
    'inline-body-opaque', 'inline-path-base-parent-kept',
    'opaque-context-mounts', 'unreadable-path-default-mounts',
    'exempted-files-seed', 'unreachable-seeds-again',
    'direct-path-multitoken-notpath', 'path-mounted-child-stem-dir',
    'duplicate-path-last-wins',
    'inline-path-literal-ignored', 'conditional-key-value-any',
    'shared-lexer-rpath-kept', 'dot-dot-mount-text-joined',
    'inline-dot-normalize-dropped',
)
# The bound guard and the assertion-message fragment that must appear in
# that guard's own panic payload (negative_panic.panic_evidence scope).
CHECKS = {
    'tls-warn-line-reverted': (
        LIST_SCAN,
        'src/runtime/model/http/tls.rs still writes an error console line'),
    'tls-guard-unlisted': (
        LISTING_RULE,
        'names the shared writer but is not in'),
    'stdout-banner-reverted': (
        LIST_SCAN,
        'src/main.rs still writes an error console line'),
    'guard-stdout-macros-dropped': (
        DETECTOR_SELF_TEST,
        '#2558 the detector must panic on a real println! site'),
    'guard-eprint-macro-dropped': (
        DETECTOR_SELF_TEST,
        '#2558 the detector must panic on a real eprint! site'),
    'unlisted-file-eprintln': (
        TREE_SCAN,
        'src/runtime/model/http/encoding.rs still writes an error console line'),
    'tree-walk-no-descend': (
        TREE_SCAN,
        'the production walk skipped the #2558 source '
        'src/runtime/memory/maintenance.rs'),
    # fix-r9: the name exemption is anchor-aware, so widening the
    # predicate to `true` no longer drops src/main.rs (unmounted files
    # stay in the walk). fix-r10 (sibling#2): the widened predicate now
    # empties the anchor seeds outright — name-exempted files no longer
    # contribute gated mounts, and under `true` every file counts as
    # name-exempted, so nothing is anchored and every file stays in the
    # walk; the guard goes red scanning the test-support spellings' own
    # diagnostics. The marker pins the first such file in the walk's
    # deterministic order (the same one the narrowed mutant reaches from
    # the other side: the exemption is what keeps it out of the walk,
    # and only a mounted one at that).
    'exclusion-widened-to-everything': (
        TREE_SCAN,
        'src/test_process.rs still writes an error console line'),
    'exclusion-narrowed-test-family': (
        TREE_SCAN,
        'src/test_process.rs still writes an error console line'),
    # fix-r9 (DS sibling#3 + DS boundary#2 P2b): the pin's path-resolved
    # anchor set and the follower whitelist must be killable — each new
    # mutant degrades one fix-r9 decision back to the shape it replaced.
    'anchor-gates-basename-degraded': (
        DECLARATION_PIN,
        'the anchor has degraded to name comparison'),
    'anchor-mod-dir-replaced': (
        DECLARATION_PIN,
        'escapes the production walk by its name alone'),
    'anchor-path-literal-file-dir-dropped': (
        DECLARATION_PIN,
        'escapes the production walk by its name alone'),
    'follower-whitelist-reverted': (
        SKIP_TEST,
        '#2558 a production site must still fail the scan'),
    # fix-r11 (B2): the fix-r10/fix-r11 scanner decisions, paired mutants
    # bound to the fixture. Each marker is the fragment of the fixture's
    # own first-failing assertion that names the regressed decision; the
    # fixture asserts the pinning shapes in a fixed order, so the marker
    # is the earliest assertion the degraded code can reach.
    'plain-mod-declaration-blind': (
        SCANNER_TEST,
        'fix-r10 boundary#1 regressed'),
    'anchor-fixpoint-dropped': (
        SCANNER_TEST,
        'fix-r10 boundary#1 regressed'),
    'inline-body-opaque': (
        SCANNER_TEST,
        'fix-r10 boundary#2 regressed'),
    'inline-path-base-parent-kept': (
        SCANNER_TEST,
        'fix-r10 boundary#3 regressed'),
    'opaque-context-mounts': (
        SCANNER_TEST,
        'non-module-context `#[path]` guess'),
    'unreadable-path-default-mounts': (
        SCANNER_TEST,
        'fix-r10 simplify#1 regressed'),
    'exempted-files-seed': (
        SCANNER_TEST,
        'seeds nothing until it is itself anchored'),
    'unreachable-seeds-again': (
        SCANNER_TEST,
        'the unreachable `orphan.rs`'),
    'direct-path-multitoken-notpath': (
        SCANNER_TEST,
        'rustc rejects the value (E13)'),
    'path-mounted-child-stem-dir': (
        SCANNER_TEST,
        'own directory (rustc E10)'),
    'duplicate-path-last-wins': (
        SCANNER_TEST,
        'carries `#[path]` twice'),
    # fix-r12: the paired mutants above, each marker the fragment of the
    # fixture's own first-failing assertion that names the regressed
    # decision.
    'inline-path-literal-ignored': (
        SCANNER_TEST,
        'fix-r11 A1 regressed'),
    'conditional-key-value-any': (
        SCANNER_TEST,
        'fix-r11 B3 regressed'),
    'shared-lexer-rpath-kept': (
        SCANNER_TEST,
        'rustc E11'),
    'dot-dot-mount-text-joined': (
        SCANNER_TEST,
        'fix-r12 boundary#1 regressed'),
    'inline-dot-normalize-dropped': (
        SCANNER_TEST,
        'fix-r13 sibling#1/boundary#1 regressed'),
}
selected = sys.argv[2:]
# #1666: ordinary raises, so `python3 -O` cannot strip these guards.
try:
    registered(mutations, EXPECTED_NAMES)
    # `mutations` stays the frozen literal so `anchor_selfcheck.py` can read it
    # statically; the requested subset is a separate name.
    run_rows = select(mutations, selected, always=())
except ValueError as error:
    refuse(str(error))
env = os.environ.copy()
if missing_target(env):  # #1666: absent, empty or blank, one shared predicate
    refuse('CARGO_TARGET_DIR must be supplied by the caller')
refuse_unusable_target()  # #1666 fix-r9: dot / root / src are unusable too
out = claim_dir(sys.argv[1])
summary = []
failures = []
# fix-r10 (review concurrency#1): publish and restore every mutated
# source through `write_source_atomic` — one rename, never a truncating
# in-place write, the same publisher `mutating_sources`'s own exit
# restore uses. A `path.write_text` truncates first, so a SIGKILL
# mid-write-back could leave an empty or half-written guard source: the
# next run's HEAD baseline preflight refuses it (fail-closed, no false
# pass), but every same-tree cargo build breaks before that. The mode
# is snapshotted from the source, so the new inode of the temp file
# does not become a mode-only `git diff` under `core.filemode=true`.
modes = {ROOT / row[1]: (ROOT / row[1]).stat().st_mode & 0o7777
         for row in run_rows}


def run(full):
    result = run_owned(['cargo', 'test', '--manifest-path', 'src/Cargo.toml',
                        '-p', 'pilotdeck-gateway', '--test',
                        'console_panic_safety', full, '--', '--exact'],
                       cwd=ROOT, env=env, timeout=900)
    return result.returncode, result.stdout + result.stderr


with mutating_sources(ROOT, sorted({ROOT / row[1] for row in run_rows})) as snapshots:
    def source_path(relative):
        return ROOT / relative

    # #819: an unmutated baseline for every named target must be green first.
    baseline_ok = set()
    for test in dict.fromkeys(CHECKS[name][0] for name, *_ in run_rows):
        # #1908: the shared wait replaces the inline 300 s-sample /
        # 1800 s-deadline loop; the budget is the shared constant.
        slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh',
                              timeout=CARGO_SLOTS_WAIT_SECONDS)
        (out / f'slots-baseline-{test}.log').write_text(slots.stdout + slots.stderr)
        code, log = run(test)
        (out / f'baseline-{test}.log').write_text(log)
        ok = code == 0 and baseline_passed(log, test)
        print(f'baseline {test}: {"PASS" if ok else "INVALID"}', flush=True)
        if ok:
            baseline_ok.add(test)
        else:
            failures.append('baseline-' + test)

    for name, relative, old, new in run_rows:
        path = source_path(relative)
        original = snapshots[path]
        before = hashlib.sha256(original).hexdigest()
        test, message = CHECKS[name]
        try:
            occurrences = original.decode().count(old)
            if occurrences != 1:
                raise ValueError(
                    f'{name}: anchor occurs {occurrences} times (expected 1)')
            changed = original.decode().replace(old, new, 1)
            (out / f'{name}.patch').write_text(''.join(difflib.unified_diff(
                original.decode().splitlines(True),
                changed.splitlines(True),
                fromfile=relative, tofile=relative)))
            # #1908: same shared-wait migration as the baseline loop above.
            slots = wait_for_slot(ROOT / 'scripts/cargo-slots.sh',
                                 timeout=CARGO_SLOTS_WAIT_SECONDS)
            (out / f'{name}.slots.log').write_text(slots.stdout + slots.stderr)
            write_source_atomic(ROOT, path, changed.encode(), mode=modes[path])
            code, log = run(test)
            (out / f'{name}.log').write_text(log)
            evidence = panic_evidence(log, test)
            marker_ok = message in evidence
            killed = (test in baseline_ok and code == 101
                      and named_test_failed(log, test)
                      and marker_ok
                      and 'could not compile' not in log)
            failure = next((line.strip() for line in log.splitlines()
                            if 'panicked at' in line or 'assertion' in line), '')
            status = 'KILLED' if killed else (
                'SURVIVED' if code == 0 else 'INVALID')
            summary.append(dict(name=name, test=test, status=status,
                                returncode=code, failure=failure,
                                sourceSha256=before))
            print(f'{name}: {status} rc={code} {failure}', flush=True)
            if status != 'KILLED':
                failures.append(name)
        except Exception as error:
            summary.append(dict(name=name, test=test, status='INVALID',
                                reason=f'{type(error).__name__}: {error}',
                                sourceSha256=before))
            failures.append(name)
        finally:
            write_source_atomic(ROOT, path, original, mode=modes[path])
            restored = hashlib.sha256(path.read_bytes()).hexdigest()
            if restored != before:
                raise RuntimeError(
                    f'{name}: restore failed ({restored} != {before}); the tree is dirty')
            (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
counts = {state: sum(row['status'] == state for row in summary)
          for state in ('KILLED', 'SURVIVED', 'INVALID')}
print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()), flush=True)
if failures:
    print(f'not killed: {", ".join(failures)}', file=sys.stderr)
killed_rows = sum(1 for row in summary if row['status'] == 'KILLED')
print(f'{killed_rows}/{len(run_rows)} killed; every source restored byte-for-byte', flush=True)
# #1666: a zero-row run is never success (`0/0 killed` used to exit 0).
sys.exit(verdict_exit(summary, key='status'))
