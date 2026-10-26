"""Single-writer mutations through the shared tree lock; assertion failures only.

``negative_process.mutating_sources`` holds the same-tree flock and verifies the
committed bytes before the first write, so a killed predecessor's residue is a
refusal instead of a silently captured ``original``. Cargo runs through
``run_owned`` (fresh process group, total deadline) and a cargo slot is awaited
before each source write. Evidence goes in a new output folder; every mutant is
restored byte-for-byte and its SHA-256 is retained.

Each mutation names the admission behavior it kills (admission.rs anchors,
InProcessGateway.ts:764-1203 and createLocalGateway.ts:1413-1451 counterparts):
reconnect stale-outcome gate, replacement claim/reservation preflight, compact
and plan command routing and fixed copy, plan rewrite, model-selection fold,
busy mapping, task-budget absent-port copy, compaction timeout reason, the
compaction overflow-delay clamp, and the sdk config absent-port copy.
"""
import hashlib
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
source = root / 'src/pilotdeck-gateway/src/runtime/turn/admission.rs'


def panic_evidence(log):
    """fix-r1 P2: the failing test must be an admission test.

    libtest prints ``thread '<test>' panicked at``; only a panic from a test
    under ``runtime::turn::admission`` counts, so a mutant killed by another
    module's assertion can never satisfy the anchor. fix-r2 P3 (opus r2):
    the prefix must be the module itself or a child, never a sibling module
    that merely shares the name stem (``runtime::turn::admission_helper``).
    """
    for segment in log.split("thread '")[1:]:
        test = segment.split("'", 1)[0]
        if test == 'runtime::turn::admission' or test.startswith('runtime::turn::admission::'):
            return True
    return False


mutations = [
    ('stale-outcome', source, 'if !is(&reconnect, "outcome", "stale_binding") {\n                        return Ok(vec![]);\n                    }', 'if true {\n                        return Ok(vec![]);\n                    }'),
    ('claim-conflict', source, '== SubmitClaim::Conflict', '== SubmitClaim::None'),
    ('conflict-copy', source, '"This session is waiting for its edited replacement turn to be accepted.",', '"changed.",'),
    ('reservation-copy', source, '"This session transcript is currently being updated.",', '"changed.",'),
    ('usage-compact', source, '用法：/compact', 'usage: /compact'),
    ('usage-plan', source, '用法：/plan <任务>', 'usage: /plan <task>'),
    ('plan-rewrite', source, 'set(input, "runMode", "plan".into());', 'set(input, "runMode", "agent".into());'),
    ('normalize-fold', source, 'for key in ["reasoning", "speed"] {', 'for key in ["speed"] {'),
    ('busy-copy', source, '"Compaction is unavailable because this session has an active turn or queued work."',
     '"changed."'),
    ('taskbudget-copy', source, '"Gateway task-budget accounting is unavailable."', '"changed."'),
    ('timeout-reason', source, '"timeout:".encode_utf16().collect()', '"timeout-x:".encode_utf16().collect()'),
    ('compact-prefix', source, 'js_whitespace(trimmed[8])', 'js_whitespace(trimmed[7])'),
    ('plan-prefix', source, 'if !starts_with16(trimmed, "/plan") {', 'if !starts_with16(trimmed, "/planx") {'),
    ('sdk-copy', source, '"sdk_session_config is unavailable."', '"changed."'),
    # fix-r1: the sdkSessionConfig validation window must surface the wired
    # port's refusal at admission time (probe sdk-config-invalid-wired).
    ('sdk-validate-ignored', source, 'Some(port) => port.validate(config),', 'Some(_port) => Ok(()),'),
    # fix-r1: a turn dropped mid-compaction must cancel its timeout timer.
    ('compact-timer-orphan', source, 'if let Some(timer) = self.0.take() {\n            timer.abort();\n        }', 'if let Some(timer) = self.0.take() {\n            let _ = timer;\n        }'),
    # fix-r2 (opus r2 boundary #2): a finite timeoutMs above 2^31-1 must clamp
    # to Node's 1 ms setTimeout behavior, not saturate into a never-firing
    # timer (killed by compact_timeout_clamps_node_overflow_delay).
    ('compact-clamp-removed', source, '.map(pilotdeck_protocol::timer_delay_ms);', '.map(|ms| ms as u64);'),
    # fix-r1: the raw connectionId mismatch gate must treat a differently
    # bound id as stale, not as a validation candidate.
    ('reconnect-mismatch', source, '!matches!(binding.get("connectionId"),', 'matches!(binding.get("connectionId"),'),
]
selected = set(sys.argv[2:])
mutations = [mutation for mutation in mutations
            if not selected or mutation[0] in selected]
assert len({name for name, *_ in mutations}) == len(mutations), 'duplicate mutation name'
assert not selected or len(mutations) == len(selected), 'unknown mutation name'
paths = sorted({path for _name, path, _old, _new in mutations}, key=str)
env = dict(os.environ, CARGO_TARGET_DIR=str(root / '_build/cargo-target'))
command = ['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib','runtime::turn::admission','--','--nocapture']

with mutating_sources(root, paths) as snapshots:
    # All anchors are checked against the locked, HEAD-verified snapshots before
    # the first write: a stale anchor aborts with the tree still pristine.
    for name, path, old, new in mutations:
        text = snapshots[path].decode()
        assert text.count(old) == 1, (name, text.count(old))
        with (out / (name + '.mutation')).open('x') as f:
            f.write(str(path.relative_to(root)) + '\n' + old + '\n=>\n' + new + '\n')
    for name, path, old, new in mutations:
        original = snapshots[path]
        text = original.decode()
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
            (out / (name + '.slots.log')).write_text(slot.stdout + slot.stderr)
            path.write_text(text.replace(old, new))
            result = run_owned(command, cwd=root, env=env, timeout=600)
            log = result.stdout + result.stderr
            (out / (name + '.log')).write_text(log)
            assert (result.returncode == 101 and 'test result: FAILED.' in log
                    and panic_evidence(log)), name
            print(name + ': KILLED (assertion killed)', flush=True)
        finally:
            path.write_bytes(original)
            assert path.read_bytes() == original
            with (out / (name + '.restored-sha256')).open('x') as f:
                f.write(hashlib.sha256(original).hexdigest()+'\n')
# All mutations restored and checked by mutating_sources; compile failures never count.
slot = wait_for_slot(root / 'scripts/cargo-slots.sh')
(out / 'restored.slots.log').write_text(slot.stdout + slot.stderr)
result = run_owned(command, cwd=root, env=env, timeout=600)
log = result.stdout + result.stderr
(out / 'restored.log').write_text(log)
assert result.returncode == 0 and 'test result: ok.' in log, 'restored positive control failed'
print('restored: PASS',flush=True)
