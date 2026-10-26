#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-messages.

Before any mutation the script runs the unmutated suite once and requires
exit code 0: a panic caused by the environment (for example a bad
``PDRS_BASELINE``) must not be mistaken for a killed mutant. A killed mutant
must then fail on an assertion, not on any panic:

* ``assertion `left == right` failed`` from an ``assert_eq!`` replay, or
* an assertion message beginning with one of the oracle's ``{group}/`` anchors
  (the replay helper formats its message that way).

``could not compile`` is never a kill. Every case compiles and the original
bytes are restored in ``finally``. ``mutating_sources`` demands that the gateway
product tree match the baseline, so an uncommitted authoring tree is refused;
the coordinator runs this once after committing (item 11 of the task).

Every anchor is checked against the baseline bytes before the unmutated suite
runs: a stale anchor is a harness defect, not a surviving mutant, so it prints
``STALE ANCHOR`` and exits 2 (same rule as #772 fix-r2). A surviving mutant
does not abort the run: every case is executed, then the script prints
KILLED/SURVIVED and exits 1 when anything survived. For an
uncommitted authoring tree, ``NEGATIVE_BASELINE=$(git stash create)`` can name a
dangling commit of the current worktree so the same script can prove 1-2
samples without committing. The default remains HEAD. ``NEGATIVE_ONLY=a,b``
runs just those cases (the unmutated baseline still runs first); it is for
proving a sample without spending the full mutation matrix.
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
fixtures = Path(__file__).resolve().parent
messages = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/messages.rs'
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)

oracle_groups = {case['group'] for case in
                 json.loads((fixtures / 'oracle.json').read_text())['cases']}
assertion_anchors = sorted(oracle_groups | {'mentions-scale'})

mention_scan = '''    let value = text.units();
    let mut index = skip_js_space(value, 0);
    while value.get(index) == Some(&u16::from(b'@')) {
        let mut end = index + 1;
        while end < value.len() && is_mention_unit(value[end]) {
            end += 1;
        }
        if end == index + 1 {
            break;
        }
        index = skip_js_space(value, end);
    }
    JsString::from_utf16(value[index..].to_vec())'''

mention_loop = '''    fn trim_start_js(value: &JsString) -> JsString {
        let start = value
            .units()
            .iter()
            .position(|unit| !js_is_space_unit(*unit))
            .unwrap_or(value.len_utf16());
        JsString::from_utf16(value.units()[start..].to_vec())
    }
    fn mention_pattern() -> &'static Regex {
        static RE: OnceLock<Regex> = OnceLock::new();
        RE.get_or_init(|| rx(r"^@[A-Za-z0-9_.-]+$S*"))
    }
    let mut cleaned = trim_start_js(text);
    loop {
        let encoded = encode(&cleaned);
        let next = mention_pattern().replace(&encoded, "").to_string();
        if next == encoded {
            return cleaned;
        }
        cleaned = trim_start_js(&decode(&next));
    }'''

cases = [
    # 1. Depth bound: a content object at depth 5 stops extracting.
    ('extract-depth', messages, 'const MAX_DEPTH: usize = 5;',
     'const MAX_DEPTH: usize = 4;', 'extraction_matches_ts'),
    # 2. A line with 3 plugin markers becomes noise.
    ('plugin-line-threshold', messages,
     '|| count_plugin_state_markers(&normalized) >= 4)',
     '|| count_plugin_state_markers(&normalized) >= 3)', 'plugin_matches_ts'),
    # 3. A paragraph containing "session status" plus 2 markers stops being noise.
    ('plugin-session-threshold', messages,
     'if paragraph_session_pattern().is_match(&encode(&normalized)) && markers >= 2 {',
     'if paragraph_session_pattern().is_match(&encode(&normalized)) && markers >= 3 {',
     'plugin_matches_ts'),
    # 4. Leading whitespace survives sender-hint normalization.
    ('sender-hint-leading-space', messages,
     'if in_space && !out.is_empty() {', 'if in_space {', 'sender_matches_ts'),
    # 5. The lookahead no longer stops at the current-message marker.
    ('quoted-lookahead', messages,
     'let current = "[Current message - respond to this]";',
     'let current = "[Current message - respond here]";', 'user_strip_matches_ts'),
    # 6. A media line ends at the first `]` instead of the last one before \n.
    ('media-last-bracket', messages,
     '.rposition(|unit| *unit == u16::from(b\']\'))',
     '.position(|unit| *unit == u16::from(b\']\'))', 'user_strip_matches_ts'),
    # 7. toolCall no longer counts as tool content.
    ('tool-call-kind', messages, 'kind.equals_str("toolCall")',
     'kind.equals_str("tool-call")', 'tools_match_ts'),
    # 8. `! poll` no longer takes the whitespace/bang branch.
    ('bang-whitespace', messages,
     'if after > 1 && after < units.len() {',
     'if after > 2 && after < units.len() {', 'commands_match_ts'),
    # 9. Timestamps need a 5-digit year, so the 4-digit date branch never matches.
    ('timestamp-year-width', messages,
     r'r"^$S*\[[^\]\n]*(?:[0-9]{4}-',
     r'r"^$S*\[[^\]\n]*(?:[0-9]{5}-', 'user_strip_matches_ts'),
    # 10. last_turn drops the last user message and keeps only later turns.
    ('last-turn-slice', messages,
     'return all.split_off(last_user);',
     'return all.split_off((last_user + 1).min(all.len()));',
     'normalize_matches_ts'),
    # 11. Greedy thinking removal eats the content between two pairs.
    ('thinking-greedy-pair', messages,
     r'r"(?s)(?i-u:<think>).*?(?i-u:</think>)"',
     r'r"(?s)(?i-u:<think>).*(?i-u:</think>)"', 'assistant_matches_ts'),
    # 12. Non-ASCII units regain Rust Unicode folding onto ASCII (JS /s/i is
    #     false for LONG S and /i/ is false for DOTLESS I).
    ('case-fold-canonicalize', messages,
     'if !(0x80..=0xFFFF).contains(&point) {',
     'if !(0x0..=0xFFFF).contains(&point) {', 'case_fold_matches_ts'),
    # 13. One regex reverts to Rust Unicode folding (KELVIN K matches `/k/i`
    #     here, but the TS non-Unicode /i leaves it unmatched).
    ('case-fold-regex', messages,
     r'r"(?i-u:</?think)[^>]*>"',
     r'r"(?i:</?think)[^>]*>"', 'case_fold_matches_ts'),
    # 14. ci_eq becomes case-sensitive, so uppercase envelope markers stop.
    ('case-fold-ci-eq', messages,
     'canonicalize_case(left) == canonicalize_case(right)',
     'left == right', 'case_fold_matches_ts'),
    # 15. The current-message segment takes the first marker instead of the
    #     last one (TS `lastIndexOf`, message-utils.ts:327-332).
    ('extract-current-first', messages,
     '.rposition(|part| part == marker)',
     '.position(|part| part == marker)', 'case_fold_matches_ts'),
    # 16. The single-pass mention strip reverts to one whole-string
    #     encode/replace/decode per mention. `leading_mentions_scale_is_linear`
    #     must hit its watchdog instead of finishing.
    ('mentions-quadratic', messages, mention_scan, mention_loop,
     'leading_mentions_scale_is_linear'),
    # 17. One whitelist item disappears, so the oracle case for it (and only
    #     it) comes back as `undefined`.
    ('slash-whitelist-missing', messages, '    "allowlist",\n', '',
     'commands_match_ts'),
    # 18. The single scan stops treating `-` as a mention unit (the TS
    #     `[\w.-]` class includes it), so `@a-b.c` only loses `@a`.
    ('mentions-unit-class', messages,
     '|| matches!(unit, 0x005F | 0x002E | 0x002D)',
     '|| matches!(unit, 0x005F | 0x002E)', 'user_strip_matches_ts'),
    # 19. The scan stops consuming the whitespace after a mention, so a
    #     leading separator survives.
    ('mentions-no-trim', messages,
     'index = skip_js_space(value, end);', 'index = end;',
     'user_strip_matches_ts'),
]
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
only = {name for name in os.environ.get('NEGATIVE_ONLY', '').split(',') if name}
unknown = only - {case[0] for case in cases}
assert not unknown, f'unknown NEGATIVE_ONLY cases: {sorted(unknown)}'


def assertion_failure(log):
    if 'could not compile' in log:
        return False
    if 'assertion `left == right` failed' in log:
        return True
    return any(f'{anchor}/' in log for anchor in assertion_anchors)


killed, survived = [], []
with mutating_sources(root, sorted({path for _, path, _, _, _ in cases}),
                      baseline=baseline) as originals:
    # A product line that this list did not follow used to be reported as
    # SURVIVED (`AssertionError: (name, 0)`), hiding a broken control behind
    # the same label as a real surviving mutant. Refuse before compiling.
    sources = {path: data.decode() for path, data in originals.items()}
    stale = []
    for name, path, before, _after, _test in cases:
        count = sources[path].count(before)
        if count != 1:
            stale.append((name, path.relative_to(root).as_posix(), count))
    if stale:
        report = '\n'.join(
            f'STALE ANCHOR {name}: {count} matches in {relative}'
            for name, relative, count in stale) + '\n'
        (out / 'stale-anchors.log').write_text(report)
        print(report, end='', flush=True)
        print('STALE ANCHORS: negative.py is out of sync with messages.rs; refusing to run',
              flush=True)
        sys.exit(2)
    base_cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_messages']
    slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
    run = run_owned(base_cmd, cwd=root, env=env, timeout=600)
    (out / 'baseline.log').write_text(slots.stdout + slots.stderr + run.stdout + run.stderr)
    if run.returncode != 0:
        print('BASELINE FAILED: unmutated suite must pass before mutations', flush=True)
        sys.exit(1)

    for name, path, before, after, test in cases:
        if only and name not in only:
            continue
        reason = None
        try:
            source = originals[path].decode()
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            path.write_text(source.replace(before, after))
            try:
                cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_messages', test, '--', '--exact']
                run = run_owned(cmd, cwd=root, env=env, timeout=300)
                log = run.stdout + run.stderr
                (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
                if not (run.returncode != 0 and assertion_failure(log)):
                    reason = 'did not fail a narrowed assertion'
            finally:
                path.write_bytes(originals[path])
        except Exception as error:
            reason = f'{type(error).__name__}: {error}'
        if reason is None:
            killed.append(name)
            print(f'KILLED   {name}: {test}', flush=True)
        else:
            survived.append((name, reason))
            print(f'SURVIVED {name}: {reason}', flush=True)
    for path, data in originals.items():
        path.write_bytes(data)
        assert path.read_bytes() == data, 'restoration failed: ' + str(path)
print(f'KILLED   {len(killed)}: ' + ', '.join(killed))
print(f'SURVIVED {len(survived)}: ' + ', '.join(name for name, _ in survived))
if survived:
    sys.exit(1)
