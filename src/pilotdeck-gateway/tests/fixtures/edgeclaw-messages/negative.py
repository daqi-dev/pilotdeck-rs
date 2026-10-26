#!/usr/bin/env python3
"""Reversible product mutations for edgeclaw-messages.

Every case compiles and must fail a runtime oracle assertion; the original
bytes are restored in `finally`. New files are not in HEAD before the commit,
so `mutating_sources` refuses to run this script in the authoring tree: the
coordinator runs it once after committing (item 11 of the task).
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
out.mkdir(parents=True, exist_ok=True)
cases = [
    # 1. Depth bound: the 5th nested content object stops extracting.
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
    # 9. Timestamps need a 3-digit year, so the date branch never matches.
    ('timestamp-year-width', messages,
     r'r"(?i)^$S*\[[^\]\n]*(?:[0-9]{4}-',
     r'r"(?i)^$S*\[[^\]\n]*(?:[0-9]{3}-', 'user_strip_matches_ts'),
    # 10. last_turn drops the last user message and keeps only later turns.
    ('last-turn-slice', messages,
     'return all.split_off(last_user);',
     'return all.split_off((last_user + 1).min(all.len()));',
     'normalize_matches_ts'),
    # 11. Greedy thinking removal eats the content between two pairs.
    ('thinking-greedy-pair', messages,
     'r"(?is)<think>.*?</think>"', 'r"(?is)<think>.*</think>"',
     'assistant_matches_ts'),
]
env = dict(os.environ)
env['CARGO_TARGET_DIR'] = target
with mutating_sources(root, sorted({path for _, path, _, _, _ in cases})) as originals:
    try:
        for name, path, before, after, test in cases:
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
                assert run.returncode != 0 and failed and 'could not compile' not in log, \
                    name + ' did not fail a runtime assertion'
                lines = [line for line in log.splitlines()
                         if 'failed:' in line or 'panicked at' in line or 'test result:' in line]
                print(name + ': ' + ' | '.join(lines), flush=True)
            finally:
                path.write_bytes(originals[path])
    finally:
        for path, data in originals.items():
            path.write_bytes(data)
            assert path.read_bytes() == data, 'restoration failed: ' + str(path)
