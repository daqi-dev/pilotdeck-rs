#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-messages.

Every case compiles and must fail a runtime oracle assertion; the original
bytes are restored in `finally`. `mutating_sources` demands that the gateway
product tree match the baseline, so an uncommitted authoring tree is refused;
the coordinator runs this once after committing (item 11 of the task).

A surviving mutant does not abort the run: every case is executed, then the
script prints KILLED/SURVIVED and exits 1 when anything survived. This is what
let the host run stop at `extract-depth` and never reach the other cases.

For an uncommitted authoring tree, `NEGATIVE_BASELINE=$(git stash create)` can
name a dangling commit of the current worktree so the same script can prove
1-2 samples without committing. The default remains HEAD.
"""
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
messages = root / 'src/pilotdeck-gateway/src/runtime/memory/edgeclaw/messages.rs'
out = Path(sys.argv[1]).resolve()
target = os.environ.get('CARGO_TARGET_DIR')
assert target, 'negative.py requires CARGO_TARGET_DIR from the caller'
baseline = os.environ.get('NEGATIVE_BASELINE', 'HEAD')
out.mkdir(parents=True, exist_ok=True)
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
]
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
killed, survived = [], []
with mutating_sources(root, sorted({path for _, path, _, _, _ in cases}),
                      baseline=baseline) as originals:
    for name, path, before, after, test in cases:
        reason = None
        try:
            source = originals[path].decode()
            assert source.count(before) == 1, (name, source.count(before))
            slots = wait_for_slot(root / 'scripts/cargo-slots.sh')
            path.write_text(source.replace(before, after))
            try:
                cmd = ['cargo', 'test', '--manifest-path', str(root / 'src/Cargo.toml'),
                       '-p', 'pilotdeck-gateway', '--test', 'edgeclaw_messages', test, '--', '--exact']
                run = run_owned(cmd, cwd=root, env=env, timeout=300)
                log = run.stdout + run.stderr
                (out / f'{name}.log').write_text(slots.stdout + slots.stderr + log)
                failed = 'test result: FAILED.' in log or 'panicked at' in log
                if not (run.returncode != 0 and failed and 'could not compile' not in log):
                    reason = 'did not fail a runtime assertion'
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
