"""Review-round-4 mutation evidence, using a disposable crate copy only.

Run from the worktree root; the real source and recorded oracles stay unchanged.
All mutants must compile and fail the same document differential assertion.
"""
import json
import os
from pathlib import Path
import re
import shutil
import subprocess

root = Path(__file__).resolve().parents[3]
evidence = root / "_tmp/review-yaml"
scratch = evidence / "mutations"
scratch.mkdir(parents=True, exist_ok=True)
crate = scratch / "pilotdeck-yaml"
if crate.exists():
    shutil.rmtree(crate)
shutil.copytree(root / "src/pilotdeck-yaml", crate)
workspace = (root / "src/Cargo.toml").read_text()
workspace = re.sub(r"members = \[.*?\]", 'members = ["pilotdeck-yaml"]', workspace, flags=re.S)
workspace = workspace.replace('path = "pilotdeck-protocol"', f'path = "{root}/src/pilotdeck-protocol"')
# #132: [patch.crates-io] paths (vendored icu_collator) are relative to src/; the
# scratch workspace does not copy vendor/ or data/, so a relative patch path would
# dangle and cargo refuses to start (every mutant would fail for the wrong reason).
patch = re.search(r"^\[patch\.crates-io\]\n(.*?)(?=^\[|\Z)", workspace, flags=re.M | re.S)
assert patch or not re.search(r"^\s*\[patch\b", workspace, flags=re.M), "#132 unrecognised [patch] table header"
if patch:
    body = patch.group(1)
    rows = [row for row in body.splitlines() if row.split("#", 1)[0].strip()]
    assert all(re.fullmatch(r'\s*[A-Za-z0-9_-]+\s*=\s*\{\s*path\s*=\s*"[^"/][^"]*"\s*\}\s*', row) for row in rows), rows
    absolute = re.sub(r'path\s*=\s*"([^"]+)"', lambda m: f'path = "{root}/src/{m.group(1)}"', body)
    workspace = workspace[:patch.start(1)] + absolute + workspace[patch.end(1):]
(scratch / "Cargo.toml").write_text(workspace)
shutil.copy(root / "src/Cargo.lock", scratch / "Cargo.lock")
scalar = "src/compose/resolve_flow_scalar.rs"
mutations = []
for name, code in [("a", 97), ("b", 98), ("e", 101), ("f", 102), ("r", 114), ("v", 118)]:
    original = {97: 7, 98: 8, 101: 27, 102: 12, 114: 13, 118: 11}[code]
    mutations.append((f"escape-{name}", scalar, f"Some({code}) => Some({original})", f"Some({code}) => Some(0)"))
mutations += [
    ("escape-t", scalar, "Some(116 | 9) => Some(9)", "Some(116 | 9) => Some(0)"),
    ("escape-literal-tab", scalar, "Some(116 | 9) => Some(9)", "Some(116) => Some(9)"),
    ("escape-slash", scalar, "32 | 34 | 47 | 92", "32 | 34 | 92"),
    ("unicode-upper-reject", scalar, "*c <= 0x10ffff", "*c < 0x10ffff"),
    ("unicode-over-accept", scalar, "*c <= 0x10ffff", "*c <= 0x110000"),
    ("block-key-1024", "src/compose/resolve_block_map.rs", "found.offset > kp.start + 1024", "found.offset >= kp.start + 1024"),
    ("block-key-1025", "src/compose/resolve_block_map.rs", "found.offset > kp.start + 1024", "found.offset > kp.start + 1025"),
    ("flow-seq-key-1024", "src/compose/resolve_flow_collection.rs", "found.offset > props.start + 1024", "found.offset >= props.start + 1024"),
    ("flow-seq-key-1025", "src/compose/resolve_flow_collection.rs", "found.offset > props.start + 1024", "found.offset > props.start + 1025"),
]
env = {**os.environ, "CARGO_HOME": str(root / "_build/cargo-home"), "CARGO_TARGET_DIR": str(root / "_build/mutation-target")}
command = ["cargo", "test", "--manifest-path", str(scratch / "Cargo.toml"), "-p", "pilotdeck-yaml", "--offline", "--test", "parse_differential", "review_round4_branches_match_node"]
results = []
for name, file, before, after in mutations:
    path = crate / file
    clean = path.read_text()
    assert clean.count(before) == 1, (name, before)
    try:
        path.write_text(clean.replace(before, after))
        run = subprocess.run(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, check=False)
        (evidence / f"mutation-{name}-round4.log").write_text(run.stdout)
        detected = run.returncode == 101 and "test result: FAILED" in run.stdout and "round4-" in run.stdout
        results.append({"mutation": name, "source": file, "before": before, "after": after,
                        "exitCode": run.returncode, "detectedByRound4Fixture": detected})
        print(name, "FAIL detected" if detected else "NOT DETECTED", flush=True)
        assert detected, name
    finally:
        path.write_text(clean)
    (evidence / "mutations-round4.json").write_text(json.dumps(results, indent=2) + "\n")
run = subprocess.run(command, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, check=False)
(evidence / "mutation-restored-round4.log").write_text(run.stdout)
assert run.returncode == 0, "restored crate failed"
print("restored crate: PASS", flush=True)
