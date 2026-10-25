# Real-provider smoke (`scripts/smoke/`)

Two layers, deliberately separated:

1. **Model runtime (runnable now).** `real_provider.py` builds and runs
   `probe/`, a standalone crate that calls the gateway's public
   `complete_model` / `stream_model` HTTP runtime against llm-center
   (Anthropic Messages protocol). It covers a text round-trip, a mid-stream
   cancel, and one forced tool call.
2. **Gateway process (#586/#589, NOT_YET).** The script emits `NOT_YET` rows
   with the exact missing wiring. It never turns those into passes. The
   gateway binary on `origin/main` is a WS/HTTP proxy only, and the CLI
   `run`/`server` entries are `Failure::Todo`.

```sh
cd <worktree>
python3 -I scripts/smoke/real_provider.py
```

Key handling: the script reads only `ANTHROPIC_API_KEY`, `DEEPSEEK_API_KEY`,
`ZAI_API_KEY` from `.env.local` at runtime. It searches this checkout first,
then the main checkout (the parent of `git rev-parse --git-common-dir`), the
same search path as `scripts/run-review-roles.py`. Value parsing follows
`scripts/review-deepseek.py:parse_env_value` for well-formed values: a quoted
value ends at its closing quote and an unquoted value drops a trailing
` # …` comment. (`scripts/run-review-roles.py` still has the older
`split(" #")` parser; changing that review tool is out of scope here and is
tracked as a follow-up.) An unbalanced quote makes the key unusable here (the
run is BLOCKED) instead of aborting the whole process. The chosen key is
passed only in the probe child environment; every artifact is redacted, then
scanned for the loaded key prefixes and generic key patterns. Only hit/miss is
printed. An empty key set is **not** a pass: the self-check prints
`0 keys loaded, cannot verify` and returns non-zero.

Limits (constants in `real_provider.py`, echoed by the probe in
`PROBE_RESULT`):

- 3 requests (one per scenario; `MAX_REQUESTS == len(SCENARIOS)`).
- 128 output tokens per request; 384 total. The probe receives the per-request
  cap as argv and reports the usage it observed; the harness compares the
  echoed value and the summed usage against the constants. The probe records
  the last `usage` event in the stream callback, before the 200-event report
  cap, so a late usage event is not lost. If no usage arrives, the scenario is
  marked `usage: missing`, the id is listed in `limits.usageMissing`, and the
  token caps are reported as **UNVERIFIED** for it; missing usage is never
  counted as 0 tokens.
- 45 s per probe scenario (inner, passed to the probe); 60 s harness hard cap.
  On the hard cap the whole probe process group is killed and the scenario is
  classified `environment-or-provider (timeout)`, never product/protocol.
  Ctrl-C (KeyboardInterrupt/SystemExit) also kills the whole process group
  before re-raising; the detached probe/cargo children do not receive the
  terminal SIGINT on their own.
- `cargo build` 600 s and `cargo-slots.sh` 30 s caps; both kill the process
  group and produce a BLOCKED row plus a report instead of a traceback.
- The probe disables retries with the keys the product actually reads
  (`requestMaxRetries`/`streamMaxRetries` = 0), and sets the model to
  `thinking.state = "disabled"` so the 128-token text scenario does not lose
  its body to provider-side reasoning.

Ports 28789/28790/28810-28812 are checked for occupancy; the probe opens no
listener and uses isolated `HOME`/`TMPDIR`/cwd under the run directory.

Outputs: `_tmp/<YYYYMMDD>-real-provider-smoke/` holds the latest
`report.json`/`report.md` plus one exclusive `run-<UTC>-<pid>/` directory per
invocation. The run directory is authoritative and never overwritten: it keeps
`report.json`/`report.md`, `limits.json`, `probe-build.log`, and redacted raw
stdout/stderr under `raw/`. The evidence root is guarded by an atomic
`mkdir(.smoke.lock)` acquisition; `owner.json` is written immediately after.
A lock directory without a readable `owner.json` is treated as held (a run may
be between mkdir and the owner write), never as stale. A lock is reclaimed
only when its owner pid is dead; the reclaim renames the directory aside,
re-reads the moved owner, and refuses if the owner changed. `release()` removes
the directory only when `owner.json` still names this process. A concurrent
second run refuses with exit 3 instead of deleting the first run's records.

Exit codes:

| code | meaning |
| --- | --- |
| 0 | all model-runtime scenarios PASS and the secret self-check verified |
| 1 | at least one scenario FAIL |
| 2 | secret self-check failed (leak, generic hit, or 0 keys in `--self-check-only`) |
| 3 | BLOCKED/partial: no model-runtime scenario actually ran (missing key, full cargo slots, build failure/timeout), a model-runtime scenario did not PASS (e.g. PASS + BLOCKED after the request budget ran out), or another run holds the evidence lock |

`BLOCKED` is not a pass. With zero requests sent the script exits 3 even if the
only reason is a missing key. Exit 0 requires **every** model-runtime scenario
to be PASS; a PASS + BLOCKED mix is partial and exits 3. `--self-check-only`
with an unreadable or empty env file exits 2. It does not take the evidence
lock: a concurrent normal run may delete a stale-lock directory or rewrite a
report between listing and reading, so the scanner skips vanished files
(recorded as `filesSkipped`) instead of raising a traceback.

Probe unit tests (the crate is a separate workspace, so the product
`cargo test --all` / clippy / fmt jobs do not see it):

```sh
sh scripts/cargo-slots.sh
cargo test --offline --locked --manifest-path scripts/smoke/probe/Cargo.toml
```

`python3 -I scripts/test-smoke-real-provider.py` runs the same command after
`scripts/cargo-slots.sh` as `test_probe_rust_tests()`; CI runs that Python
suite, so the eight (now eleven) probe tests are executed there. A missing
`cargo` or a full slot set fails the suite instead of silently skipping it.

`probe/Cargo.lock` is tracked (`git ls-files scripts/smoke/probe/Cargo.lock`).
It pins the same versions as `src/Cargo.lock` but is resolved for the probe
graph: it carries the `real-provider-probe` entry and drops workspace-only
members such as `pilotdeck-cli`. It is not a byte-for-byte copy. Refresh it
when the product lock changes.
