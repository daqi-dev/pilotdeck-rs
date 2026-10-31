#!/usr/bin/env python3
"""#132 surrogate-collation-matrix originals; account for every selected mutant.

--check-anchors validates anchors against the working tree. Otherwise provide a
new output directory and optional mutation names; CARGO_TARGET_DIR and
PDRS_BASELINE must be supplied by the caller. Commit first: the source guard is
never bypassed and refuses sources that are not in HEAD, so the matrix runs
post-commit on the host. Run exclusively in this worktree: no concurrent cargo
or test process may run until restoration finishes.

#132a fix-r1: the goldens are ICU-sensitive. A host whose node reports an ICU
build other than the recorded 78.2 (homebrew node@22 baseline, decision card
2026-10-31) gets the whole suite BLOCKED — a dedicated status that is never
counted as INVALID — and the process exits 3 (0=all killed, 1=control failure
or refusal, 2=unverified restore, 3=environment blocked).
"""
import json
import os
from pathlib import Path
import sys

sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import (Case, classify, invalidate_completed, passed,
                            publish_summary)

root = Path(__file__).resolve().parents[5]
FIXTURE = Path(__file__).resolve().parent
GENERATOR = FIXTURE / 'generate.mts'
ORACLE = FIXTURE / 'oracle.json'
PROVENANCE = root / 'src/pilotdeck-gateway/tests/generator_provenance.rs'
GUARD = 'surrogate_collation_matrix_regenerates_identically'
STRUCTURE = 'surrogate_collation_matrix_covers_locale_and_key_classes'
DIFFERS = ('#280 surrogate-collation-matrix/oracle.json differs from committed '
           'bytes outside the exact masks')
GENERATOR_FAILED = '#280 surrogate-collation-matrix: generator failed'
ICU_PIN = '78.2'
BLOCKED_MARKER = '#132a BLOCKED'
BLOCKED_EXIT = 3


class BlockedEnvironment(BaseException):
    """The ICU baseline gate tripped: the environment, not the controls, is wrong.

    Deliberately not an ``Exception``: the per-mutation handler must not fold it
    into an INVALID row — the suite-level outcome is BLOCKED (#132a fix-r1).
    """


def icu_blockage():
    """Return the BLOCKED reason when node's ICU build is not the recorded one."""
    probe = run_owned(['node', '-p', 'process.versions.icu'], timeout=60)
    if probe.returncode != 0:
        # A missing node surfaces through the ordinary cargo failure path.
        return None
    version = probe.stdout.strip()
    if version != ICU_PIN:
        return (f'{BLOCKED_MARKER}: surrogate-collation-matrix requires ICU '
                f'{ICU_PIN} (homebrew node@22 baseline, decision card '
                f'2026-10-31); this host reports {version}')
    return None


def blocked_line(log):
    hit = next((line for line in log.splitlines() if BLOCKED_MARKER in line), None)
    return None if hit is None else hit.strip()


def publish_blocked(out, results):
    """BLOCKED twin of ``publish_summary``; ``pending.counts`` has no such state."""
    target = Path(out) / 'summary.json'
    temporary = target.with_name(target.name + '.tmp')
    temporary.write_text(json.dumps({
        'controls': results,
        'counts': {'BLOCKED': len(results)},
    }, indent=2) + '\n')
    temporary.replace(target)
# The en_US row header through the second item of the first case's response:
# the surrogate frames are byte-identical across locales, so only the row
# header makes the first case's bytes unique in the oracle.
EN_ROW_PREFIX = '"resolvedLocale": "en-US",\n      "node": "v22.22.0",\n      "icu": "78.2",\n      "cases": [\n        {\n          "id": "catalog-lone-high",\n          "method": "model_catalog_list",\n          "keys": [\n            "\\ud800",\n            "\\ud801",\n            "\\ud807",\n            "\\udbff"\n          ],\n          "yaml": "schemaVersion: 1\\nagent: {model: \\"parity/\\\\ud800\\"}\\nmodel:\\n  providers:\\n    parity:\\n      protocol: openai\\n      url: https://example.invalid/v1\\n      apiKey: mock-only\\n      models:\\n        \\"\\\\ud800\\": {}\\n        \\"\\\\ud801\\": {}\\n        \\"\\\\ud807\\": {}\\n        \\"\\\\udbff\\": {}\\n",\n          "request": "{\\"type\\":\\"request\\",\\"id\\":\\"catalog-lone-high\\",\\"method\\":\\"model_catalog_list\\",\\"params\\":{}}",\n          "response": "{\\"type\\":\\"response\\",\\"id\\":\\"catalog-lone-high\\",\\"ok\\":true,\\"result\\":{\\"items\\":[{\\"id\\":\\"parity/\\\\ud800\\",\\"provider\\":\\"parity\\",\\"model\\":\\"\\\\ud800\\",\\"displayName\\":\\"\\\\ud800\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/\\\\ud801\\",\\"provider\\":\\"parity\\",\\"model\\":\\"\\\\ud801\\",\\"displayName\\":\\"\\\\ud801\\",\\"available\\":true,\\"capabilities\\":{}}'
EN_ROW_SWAP = '"resolvedLocale": "en-US",\n      "node": "v22.22.0",\n      "icu": "78.2",\n      "cases": [\n        {\n          "id": "catalog-lone-high",\n          "method": "model_catalog_list",\n          "keys": [\n            "\\ud800",\n            "\\ud801",\n            "\\ud807",\n            "\\udbff"\n          ],\n          "yaml": "schemaVersion: 1\\nagent: {model: \\"parity/\\\\ud800\\"}\\nmodel:\\n  providers:\\n    parity:\\n      protocol: openai\\n      url: https://example.invalid/v1\\n      apiKey: mock-only\\n      models:\\n        \\"\\\\ud800\\": {}\\n        \\"\\\\ud801\\": {}\\n        \\"\\\\ud807\\": {}\\n        \\"\\\\udbff\\": {}\\n",\n          "request": "{\\"type\\":\\"request\\",\\"id\\":\\"catalog-lone-high\\",\\"method\\":\\"model_catalog_list\\",\\"params\\":{}}",\n          "response": "{\\"type\\":\\"response\\",\\"id\\":\\"catalog-lone-high\\",\\"ok\\":true,\\"result\\":{\\"items\\":[{\\"id\\":\\"parity/\\\\ud801\\",\\"provider\\":\\"parity\\",\\"model\\":\\"\\\\ud801\\",\\"displayName\\":\\"\\\\ud801\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/\\\\ud800\\",\\"provider\\":\\"parity\\",\\"model\\":\\"\\\\ud800\\",\\"displayName\\":\\"\\\\ud800\\",\\"available\\":true,\\"capabilities\\":{}}'
EN_ROW_DUPLICATE = '"resolvedLocale": "en-US",\n      "node": "v22.22.0",\n      "icu": "78.2",\n      "cases": [\n        {\n          "id": "catalog-lone-high",\n          "method": "model_catalog_list",\n          "keys": [\n            "\\ud800",\n            "\\ud801",\n            "\\ud807",\n            "\\udbff"\n          ],\n          "yaml": "schemaVersion: 1\\nagent: {model: \\"parity/\\\\ud800\\"}\\nmodel:\\n  providers:\\n    parity:\\n      protocol: openai\\n      url: https://example.invalid/v1\\n      apiKey: mock-only\\n      models:\\n        \\"\\\\ud800\\": {}\\n        \\"\\\\ud801\\": {}\\n        \\"\\\\ud807\\": {}\\n        \\"\\\\udbff\\": {}\\n",\n          "request": "{\\"type\\":\\"request\\",\\"id\\":\\"catalog-lone-high\\",\\"method\\":\\"model_catalog_list\\",\\"params\\":{}}",\n          "response": "{\\"type\\":\\"response\\",\\"id\\":\\"catalog-lone-high\\",\\"ok\\":true,\\"result\\":{\\"items\\":[{\\"id\\":\\"parity/\\\\ud800\\",\\"provider\\":\\"parity\\",\\"model\\":\\"\\\\ud800\\",\\"displayName\\":\\"\\\\ud800\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/\\\\ud800\\",\\"provider\\":\\"parity\\",\\"model\\":\\"\\\\ud800\\",\\"displayName\\":\\"\\\\ud800\\",\\"available\\":true,\\"capabilities\\":{}}'
SV_CONTROL_SPAN = '{\\"id\\":\\"parity/Z\\",\\"provider\\":\\"parity\\",\\"model\\":\\"Z\\",\\"displayName\\":\\"Z\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/ä\\",\\"provider\\":\\"parity\\",\\"model\\":\\"ä\\",\\"displayName\\":\\"ä\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/中文\\",\\"provider\\":\\"parity\\",\\"model\\":\\"中文\\",\\"displayName\\":\\"中文\\",\\"available\\":true,\\"capabilities\\":{}}'
SV_CONTROL_FLATTENED = '{\\"id\\":\\"parity/ä\\",\\"provider\\":\\"parity\\",\\"model\\":\\"ä\\",\\"displayName\\":\\"ä\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/Z\\",\\"provider\\":\\"parity\\",\\"model\\":\\"Z\\",\\"displayName\\":\\"Z\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/中文\\",\\"provider\\":\\"parity\\",\\"model\\":\\"中文\\",\\"displayName\\":\\"中文\\",\\"available\\":true,\\"capabilities\\":{}}'
SV_CONTROL_REVERSED = '{\\"id\\":\\"parity/中文\\",\\"provider\\":\\"parity\\",\\"model\\":\\"中文\\",\\"displayName\\":\\"中文\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/ä\\",\\"provider\\":\\"parity\\",\\"model\\":\\"ä\\",\\"displayName\\":\\"ä\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/Z\\",\\"provider\\":\\"parity\\",\\"model\\":\\"Z\\",\\"displayName\\":\\"Z\\",\\"available\\":true,\\"capabilities\\":{}}'
ZH_MIXED_SPAN = '{\\"id\\":\\"parity/ä\\",\\"provider\\":\\"parity\\",\\"model\\":\\"ä\\",\\"displayName\\":\\"ä\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/\\\\ud800\\",\\"provider\\":\\"parity\\",\\"model\\":\\"\\\\ud800\\",\\"displayName\\":\\"\\\\ud800\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/\\\\ud801\\",\\"provider\\":\\"parity\\",\\"model\\":\\"\\\\ud801\\",\\"displayName\\":\\"\\\\ud801\\",\\"available\\":true,\\"capabilities\\":{}}'
ZH_MIXED_SWAPPED = '{\\"id\\":\\"parity/ä\\",\\"provider\\":\\"parity\\",\\"model\\":\\"ä\\",\\"displayName\\":\\"ä\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/\\\\ud801\\",\\"provider\\":\\"parity\\",\\"model\\":\\"\\\\ud801\\",\\"displayName\\":\\"\\\\ud801\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/\\\\ud800\\",\\"provider\\":\\"parity\\",\\"model\\":\\"\\\\ud800\\",\\"displayName\\":\\"\\\\ud800\\",\\"available\\":true,\\"capabilities\\":{}}'
JA_MIXED_SPAN = '{\\"id\\":\\"parity/ä\\",\\"provider\\":\\"parity\\",\\"model\\":\\"ä\\",\\"displayName\\":\\"ä\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/中\\",\\"provider\\":\\"parity\\",\\"model\\":\\"中\\",\\"displayName\\":\\"中\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/𠀀\\",\\"provider\\":\\"parity\\",\\"model\\":\\"𠀀\\",\\"displayName\\":\\"𠀀\\",\\"available\\":true,\\"capabilities\\":{}}'
JA_MIXED_FLATTENED = '{\\"id\\":\\"parity/ä\\",\\"provider\\":\\"parity\\",\\"model\\":\\"ä\\",\\"displayName\\":\\"ä\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/𠀀\\",\\"provider\\":\\"parity\\",\\"model\\":\\"𠀀\\",\\"displayName\\":\\"𠀀\\",\\"available\\":true,\\"capabilities\\":{}},{\\"id\\":\\"parity/中\\",\\"provider\\":\\"parity\\",\\"model\\":\\"中\\",\\"displayName\\":\\"中\\",\\"available\\":true,\\"capabilities\\":{}}'
mutations = [
    ('locale-dropped', GENERATOR,
     "const locales = ['en_US.UTF-8', 'zh_CN.UTF-8', 'de_DE.UTF-8', 'sv_SE.UTF-8', 'ja_JP.UTF-8'];",
     "const locales = ['en_US.UTF-8', 'zh_CN.UTF-8', 'de_DE.UTF-8', 'sv_SE.UTF-8'];",
     GUARD, DIFFERS),
    ('partial-provenance', GENERATOR,
     'const sourceSha256 = await baselineSourceHashes(baseline, sources);',
     'const sourceSha256 = Object.fromEntries(Object.entries(await baselineSourceHashes(baseline, sources)).slice(0, 1));',
     GUARD, DIFFERS),
    ('key-class-mutated', GENERATOR,
     "['lone-high', ['\\ud800', '\\ud801', '\\ud807', '\\udbff']],", "['lone-high', ['\\ud800', '\\ud801', '\\ud808', '\\udbff']],",
     GUARD, DIFFERS),
    ('resolved-pin-loosened', GENERATOR,
     "['sv_SE.UTF-8', 'sv-SE'],", "['sv_SE.UTF-8', 'sv'],",
     GUARD, GENERATOR_FAILED),
    # #132a fix-r1: the ICU baseline gate itself is a check, so it gets its own
    # paired mutants — loosening either pin must fail the guard, not pass
    # silently on whatever ICU build happens to be installed.
    ('generator-icu-pin-loosened', GENERATOR,
     "assert.equal(process.versions.icu, '78.2',", "assert.equal(process.versions.icu, '77.1',",
     GUARD, GENERATOR_FAILED),
    ('guard-icu-pin-loosened', PROVENANCE,
     '        "78.2",\n        "#132a BLOCKED: surrogate-collation-matrix requires ICU 78.2 (homebrew node@22 baseline)"',
     '        "77.1",\n        "#132a BLOCKED: surrogate-collation-matrix requires ICU 78.2 (homebrew node@22 baseline)"',
     GUARD, '#132a BLOCKED: surrogate-collation-matrix requires ICU 78.2 (homebrew node@22 baseline)'),
    ('oracle-node-version', ORACLE,
     '"node": "v22.22.0",\n  "icu": "78.2",\n  "sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",', '"node": "v22.21.0",\n  "icu": "78.2",\n  "sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",',
     STRUCTURE, '#132 pinned Node runtime'),
    ('oracle-child-icu', ORACLE,
     '"resolvedLocale": "en-US",\n      "node": "v22.22.0",\n      "icu": "78.2",', '"resolvedLocale": "en-US",\n      "node": "v22.22.0",\n      "icu": "78.1",',
     STRUCTURE, '#132 child ICU version'),
    ('oracle-resolved-locale', ORACLE,
     '"resolvedLocale": "en-US",', '"resolvedLocale": "en-US-Latn",',
     STRUCTURE, '#132 the child locale must resolve as pinned'),
    ('oracle-scratch-leak', ORACLE,
     '"resolvedLocale": "en-US",', '"resolvedLocale": "en-US-pdrs-surrogate-matrix",',
     STRUCTURE, '#132 oracle must not leak the generator scratch root'),
    ('oracle-commit-mutated', ORACLE,
     '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",',
     '"sourceCommit": "dc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",',
     STRUCTURE, '#1109 oracle baseline commit mismatch'),
    ('oracle-lone-order', ORACLE,
     EN_ROW_PREFIX, EN_ROW_SWAP,
     STRUCTURE, '#132 lone surrogate catalog order is the recorded code-point order; a U+FFFD fold is caught by the session winner'),
    ('oracle-permutation', ORACLE,
     EN_ROW_PREFIX, EN_ROW_DUPLICATE,
     STRUCTURE, '#132 catalog-lone-high order must be a permutation of the keys'),
    ('oracle-winner-agreement', ORACLE,
     SV_CONTROL_SPAN, SV_CONTROL_REVERSED,
     STRUCTURE, 'winner must agree with the catalog comparator'),
    ('oracle-mixed-tail', ORACLE,
     ZH_MIXED_SPAN, ZH_MIXED_SWAPPED,
     STRUCTURE, 'assigned-then-surrogate-then-FFFD tail must hold in'),
    # #132a fix-r1 (review opus P2): oracle-side regressions of the locale
    # divergence, not just test-side expectation constants. Flattening sv_SE's
    # control row to the en_US order (a transposition that keeps 中文 last, so
    # the per-case winner gate stays green) is what the en_US-vs-sv_SE order
    # divergence must catch; same for ja_JP's mixed-uniform 中/𠀀 inversion.
    ('oracle-sv-control-flattened', ORACLE,
     SV_CONTROL_SPAN, SV_CONTROL_FLATTENED,
     STRUCTURE, '#132 en_US vs sv_SE control order must differ'),
    ('oracle-ja-mixed-flattened', ORACLE,
     JA_MIXED_SPAN, JA_MIXED_FLATTENED,
     STRUCTURE, '#132 ja_JP vs en_US mixed order must differ (中 before 𠀀)'),
    ('test-locale-count', PROVENANCE,
     'locales.len(),\n        "#132 exactly five recorded locales"', 'locales.len() + 1,\n        "#132 exactly five recorded locales"',
     STRUCTURE, '#132 exactly five recorded locales'),
    ('test-case-count', PROVENANCE,
     'key_classes.len() * 2,', 'key_classes.len() * 2 + 1,',
     STRUCTURE, '#132 both gateway methods per key class'),
    ('test-keys-class', PROVENANCE,
     '("band-boundary", &[r"\\udbff", r"\\udc00", "\\u{FFFD}"]),', '("band-boundary", &[r"\\udc00", r"\\udbff", "\\u{FFFD}"]),',
     STRUCTURE, 'keys must match the designed key class'),
    ('test-env-mapping', PROVENANCE,
     '"/fixture/home",\n                "#132 catalog-{name} env root mapping"', '"/fixture/home-mutated",\n                "#132 catalog-{name} env root mapping"',
     STRUCTURE, '#132 catalog-lone-high env root mapping'),
    ('test-session-env', PROVENANCE,
     '"/fixture/home",\n                "#132 session-{name} env root mapping"', '"/fixture/home-mutated",\n                "#132 session-{name} env root mapping"',
     STRUCTURE, '#132 session-lone-high env root mapping'),
    ('test-home-mapping', PROVENANCE,
     '"/fixture/home",\n                "#132 session-{name} home root mapping"', '"/fixture/home-mutated",\n                "#132 session-{name} home root mapping"',
     STRUCTURE, '#132 session-lone-high home root mapping'),
    ('test-entries-reversed', PROVENANCE,
     'keys[keys.len() - 1 - i],', 'keys[i],',
     STRUCTURE, 'entries must keep the reversed input order'),
    ('test-entry-tag', PROVENANCE,
     'format!("w{i}"),', 'format!("w{}", i + 1),',
     STRUCTURE, 'entry tag'),
    ('test-winners-count', PROVENANCE,
     'winners.len(),\n                2,', 'winners.len(),\n                3,',
     STRUCTURE, 'saved and effective must both record the winner'),
    ('test-winners-agree', PROVENANCE,
     'winners[0], winners[1],', 'winners[0], winners[1].clone() + "x",',
     STRUCTURE, 'saved and effective must agree'),
    ('test-band-boundary', PROVENANCE,
     'vec![r"\\udbff", r"\\udc00", "\\u{FFFD}"],', 'vec![r"\\udc00", r"\\udbff", "\\u{FFFD}"],',
     STRUCTURE, '#132 the surrogate band must sort below U+FFFD'),
    ('test-embedded-fold', PROVENANCE,
     'vec!["ab", r"a\\ud800b", r"a\\udc00b", "a\\u{FFFD}b"],', 'vec!["ab", r"a\\udc00b", r"a\\ud800b", "a\\u{FFFD}b"],',
     STRUCTURE, '#132 embedded lone surrogates stay distinct and below U+FFFD in the recorded order; a fold tie is caught by the session winner'),
    ('test-locale-divergence', PROVENANCE,
     'control_orders[0], control_orders[1],', 'control_orders[0], control_orders[2],',
     STRUCTURE, '#132 en_US vs zh_CN control order must differ'),
    ('test-control-winner', PROVENANCE,
     'control_winners[0], control_winners[1],', 'control_winners[0], control_winners[2],',
     STRUCTURE, '#132 en_US vs zh_CN control winner must differ'),
    # #132a fix-r1 (review DeepSeek P2): the four key classes that only had a
    # permutation check now pin the exact catalog order. These four classes are
    # byte-identical across all five locale rows (unlike lone-high, the first
    # case after the row header), so an oracle-side mutation has no unique
    # anchor; the test-side expectation vectors keep the asserts alive while
    # oracle-lone-order / oracle-permutation / oracle-mixed-tail cover the
    # shared oracle-bytes -> catalog_order -> assertion path.
    ('test-fffd-vs-pair', PROVENANCE,
     'vec!["😀", "\\u{10FFFF}", "\\u{FFFD}"],', 'vec!["\\u{10FFFF}", "😀", "\\u{FFFD}"],',
     STRUCTURE, '#132 the replacement character must sort after astral code points'),
    ('test-valid-pair', PROVENANCE,
     'vec!["😀", "😁", "𠀀", "\\u{10FFFF}"],', 'vec!["😀", "𠀀", "😁", "\\u{10FFFF}"],',
     STRUCTURE, '#132 valid surrogate pairs must order by code point'),
    ('test-cjk-embedded', PROVENANCE,
     'vec!["中", r"中\\ud800", r"中\\udc00", "中\\u{FFFD}"],', 'vec!["中", r"中\\udc00", r"中\\ud800", "中\\u{FFFD}"],',
     STRUCTURE, '#132 cjk-embedded-fold catalog order is the recorded stable order; a U+FFFD fold is caught by the session winner'),
    ('test-pair-interpretation', PROVENANCE,
     'vec![r"𐀀", r"\\ud800\\ud801", r"\\udc00\\ud800"],', 'vec![r"\\ud800\\ud801", r"𐀀", r"\\udc00\\ud800"],',
     STRUCTURE, '#132 a surrogate pair must order by code point, not by UTF-16 code unit'),
    # #132a fix-r1 (review opus P2): the de_DE / ja_JP pins. The de row is
    # byte-identical to en_US and ja_JP outside the row headers, so no oracle
    # anchor can single a row out; the index constants keep the asserts live.
    ('test-de-control-pin', PROVENANCE,
     'control_orders[2], control_orders[0],', 'control_orders[2], control_orders[1],',
     STRUCTURE, '#132 de_DE control order must match en_US; its arrival rests on resolvedLocale alone'),
    ('test-ja-control-pin', PROVENANCE,
     'control_orders[4], control_orders[0],', 'control_orders[4], control_orders[1],',
     STRUCTURE, '#132 ja_JP control order must match en_US (ICU 78.2)'),
    ('test-de-mixed-pin', PROVENANCE,
     'mixed_orders[2], mixed_orders[0],', 'mixed_orders[2], mixed_orders[1],',
     STRUCTURE, '#132 de_DE mixed order must match en_US (ICU 78.2)'),
]


def main():
    if sys.argv[1:] == ['--check-anchors']:
        for name, path, old, new, _test, _marker in mutations:
            if path.read_text().count(old) != 1 or old == new:
                raise SystemExit(f'{name}: expired or invalid anchor')
        print(f'{len(mutations)} unique anchors checked')
        return 0
    args = [arg for arg in sys.argv[1:] if arg != '--compile-only']
    compile_only = len(args) != len(sys.argv) - 1
    if not args:
        raise SystemExit('usage: negative_controls.py --check-anchors |'
                         ' [--compile-only] NEW_OUTPUT_DIR [mutation ...]')
    out = Path(args[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    selected = [row for row in mutations if not args[1:] or row[0] in args[1:]]
    if args[1:] and len(selected) != len(args[1:]):
        raise SystemExit('#132 unknown mutation name')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR') or not env.get('PDRS_BASELINE'):
        raise SystemExit('#132 caller must supply CARGO_TARGET_DIR and PDRS_BASELINE')
    # #132a fix-r1: gate the ICU baseline before any cargo run, so a mismatched
    # host (ICU 77.1 source-builds vs the recorded 78.2) reports the whole suite
    # BLOCKED instead of four GUARD mutations degrading into
    # "positive control did not pass" INVALIDs (macmini 2026-10-31).
    if not compile_only:
        blocked = icu_blockage()
        if blocked is not None:
            results = [dict(mutation=name, test=test, assertion=marker,
                            status='BLOCKED', reason=blocked, compiled=False)
                       for name, _path, _old, _new, test, marker in selected]
            publish_blocked(out, results)
            for row in results:
                print(f"{row['mutation']}: BLOCKED: {row['reason']}", flush=True)
            print(f'SUMMARY: BLOCKED={len(results)}', flush=True)
            return BLOCKED_EXIT
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--test', 'generator_provenance']
    results = []

    def run(label, args):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        # No -q: cargo forwards it to libtest, which drops the named per-test
        # lines the classifier needs.
        result = run_owned(command + args, cwd=root, env=env, timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    def record(name, test, assertion, status, reason):
        results.append(dict(mutation=name, test=test, assertion=assertion,
                            status=status, reason=reason, compiled=False))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        fixture_root = FIXTURE.parent
        guarded = [GENERATOR, ORACLE, PROVENANCE, Path(__file__).resolve(),
                   *(fixture_root / name for name in (
                       'negative_process.py', 'negative_suite.py',
                       'negative_panic.py', 'pending_reanchor.py'))]
        with mutating_sources(root, guarded) as originals:
            for name, path, old, new, test, marker in selected:
                row = dict(mutation=name, test=test, assertion=marker,
                           status='INVALID', reason='not run', compiled=False)
                args = [test, '--', '--exact']
                if test == GUARD:
                    args.append('--ignored')
                try:
                    text = originals[path].decode()
                    if text.count(old) != 1 or old == new:
                        raise ValueError('expired or invalid anchor')
                    changed = text.replace(old, new, 1)
                    if path.suffix == '.json':
                        json.loads(changed)
                    if not compile_only:
                        positive = run(name + '-positive', args)
                        if not passed(positive, test):
                            # The guard's own ICU gate: an environment mismatch
                            # must not be recorded as a control failure.
                            blocked = blocked_line(positive.stdout + positive.stderr)
                            if blocked is not None:
                                raise BlockedEnvironment(blocked)
                            raise RuntimeError('positive control did not pass')
                    path.write_text(changed)
                    if run(name + '-compile', ['--no-run']).returncode != 0:
                        raise RuntimeError('mutant did not compile')
                    row['compiled'] = True
                    if compile_only:
                        row.update(status='COMPILED',
                                   reason='mutant compiles; runtime classification not requested')
                    else:
                        status, reason = classify(run(name + '-negative', args),
                                                  Case(name, path, old, new, test, marker))
                        row.update(status=status, reason=reason)
                except Exception as error:
                    row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
                finally:
                    path.write_bytes(originals[path])
                try:
                    restored = run(name + '-restored', ['--no-run'] if compile_only else args)
                    if restored.returncode or (not compile_only and not passed(restored, test)):
                        raise RuntimeError('restored control did not pass')
                except Exception as error:
                    row.update(status='INVALID', reason=row['reason'] + f'; restored: {error}')
                record(row['mutation'], row['test'], row['assertion'],
                       row['status'], row['reason'])
    except BlockedEnvironment as error:
        # The environment gate, not a control, tripped mid-run: every row is
        # BLOCKED, never INVALID, and the restored bytes stay verified by the
        # guard's own exit re-check (#132a fix-r1).
        reason = str(error)
        for row in results:
            row.update(status='BLOCKED', reason=reason)
        for name, _path, _old, _new, test, marker in selected[len(results):]:
            record(name, test, marker, 'BLOCKED', reason)
        publish_blocked(out, results)
        print(f'SUMMARY: BLOCKED={len(results)}', flush=True)
        return BLOCKED_EXIT
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate_completed(results, reason)
        for name, _path, _old, _new, test, marker in selected[len(results):]:
            record(name, test, marker, 'INVALID', reason)
    publish_summary(out, results)
    states = ('COMPILED', 'INVALID') if compile_only else ('KILLED', 'SURVIVED', 'INVALID')
    counts = {state: sum(row['status'] == state for row in results) for state in states}
    print('SUMMARY: ' + ' '.join(f'{state}={counts[state]}' for state in states), flush=True)
    expected = 'COMPILED' if compile_only else 'KILLED'
    return int(not results or any(row['status'] != expected for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
