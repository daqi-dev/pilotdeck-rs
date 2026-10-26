#!/usr/bin/env python3
"""#755 negative controls: restore each silent cwd fallback and prove the
matching positive test turns red. Every mutation is in-place under
`mutating_sources`; cargo runs through `run_owned`; a missing
`CARGO_TARGET_DIR` is an error (the caller owns the build directory)."""
import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[5]
if len(sys.argv) != 2:
    sys.exit("usage: negative_controls.py <evidence-dir>")
OUT = Path(sys.argv[1]).resolve()
OUT.mkdir(parents=True, exist_ok=False)

if not os.environ.get("CARGO_TARGET_DIR"):
    sys.exit("CARGO_TARGET_DIR must be set by the caller")

sys.path.insert(0, str(ROOT / "src/pilotdeck-gateway/tests/fixtures"))
from negative_process import mutating_sources, run_owned, wait_for_slot  # noqa: E402

SOURCE = ROOT / "src/pilotdeck-gateway/src"
METADATA = SOURCE / "metadata.rs"
CATALOG = SOURCE / "model_catalog.rs"
TRANSCRIPT = SOURCE / "storage/transcript_adapter.rs"
STATS = SOURCE / "runtime/model/stats.rs"
SESSION_MODEL = SOURCE / "session_model.rs"

MUTATIONS = [
    (
        "from-env-cwd",
        METADATA,
        "        let cwd = process_cwd().map_err(|error| process_cwd_error(&error))?;",
        "        let cwd = match std::env::current_dir() {\n"
        "            Ok(cwd) => cwd,\n"
        "            Err(_) => return Ok(None),\n"
        "        };",
        ["metadata::tests::deleted_process_cwd_fails_from_env"],
    ),
    (
        "resolve-process-path-cwd",
        METADATA,
        "        process_cwd()\n"
        "            .map_err(|error| process_cwd_error(&error))?\n"
        "            .join(cwd)",
        "        std::env::current_dir()\n"
        "            .unwrap_or_else(|_| PathBuf::from(\"/\"))\n"
        "            .join(cwd)",
        ["metadata::tests::deleted_process_cwd_only_affects_relative_resolve_inputs"],
    ),
    (
        "native-metadata-cwd",
        CATALOG,
        "        crate::metadata::process_cwd().map_err(|error| cwd_error(&error))?",
        "        crate::metadata::process_cwd().unwrap_or_default()",
        ["model_catalog::tests::deleted_process_cwd_only_affects_relative_pilot_home"],
    ),
    (
        "transcript-cwd",
        TRANSCRIPT,
        "    crate::metadata::process_cwd()\n"
        "        .map(|cwd| cwd.join(base))\n"
        "        .map_err(|error| AdapterError::Io(crate::metadata::process_cwd_error(&error)))",
        "    Ok(std::env::current_dir().unwrap_or_default().join(base))",
        ["storage::transcript_adapter_tests::deleted_process_cwd_fails_relative_transcript_bases"],
    ),
    (
        "session-model-relative-key",
        SESSION_MODEL,
        "    if metadata.cwd.as_os_str().is_empty() && project_units.first() != Some(&(b'/' as u16)) {\n"
        "        return Err(error(\"ENOENT\", \"ENOENT: no such file or directory, uv_cwd\"));\n"
        "    }\n",
        "",
        ["session_model::tests::empty_native_cwd_rejects_a_relative_project_key"],
    ),
    (
        "stats-cwd",
        STATS,
        "    let cwd = crate::metadata::process_cwd()\n"
        "        .map_err(|error| crate::metadata::process_cwd_error(&error))?;",
        "    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from(\"/\"));",
        ["runtime::model::stats::tests::deleted_process_cwd_only_affects_relative_pilot_home"],
    ),
]

wait_for_slot(ROOT / "scripts/cargo-slots.sh")
env = os.environ.copy()
env.setdefault("CARGO_TARGET_DIR", os.environ["CARGO_TARGET_DIR"])
summary = []
with mutating_sources(ROOT, sorted({path for _, path, _, _, _ in MUTATIONS})) as snapshots:
    for name, path, old, new, tests in MUTATIONS:
        original = snapshots[path]
        content = original.decode()
        assert content.count(old) == 1, (name, content.count(old))
        path.write_text(content.replace(old, new, 1))
        try:
            result = run_owned(
                [
                    "cargo",
                    "test",
                    "--manifest-path",
                    "src/Cargo.toml",
                    "-p",
                    "pilotdeck-gateway",
                    "--offline",
                    "--lib",
                    *tests,
                ],
                cwd=ROOT,
                env=env,
                timeout=900,
            )
            log = result.stdout + result.stderr
            (OUT / f"{name}.log").write_text(log)
            target = tests[-1]
            killed = (
                result.returncode == 101
                and "FAILED" in log
                and target in log
            )
            summary.append(
                {
                    "name": name,
                    "killed": killed,
                    "returncode": result.returncode,
                    "test": target,
                }
            )
            print(
                f"{name}: {'KILLED' if killed else 'NOT KILLED'} rc={result.returncode}",
                flush=True,
            )
            assert killed, f"{name}: mutant survived or the test did not run"
        finally:
            path.write_bytes(original)
            assert path.read_bytes() == original
        (OUT / "summary.json").write_text(
            json.dumps(summary, ensure_ascii=False, indent=2) + "\n"
        )
print(f"{len(summary)}/{len(MUTATIONS)} killed; every source restored byte-for-byte")
