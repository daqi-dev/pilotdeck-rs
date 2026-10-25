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
  cap as argv and echoes it back; the harness also records the summed usage.
  The probe records the last `usage` event in the stream callback, before the
  200-event report cap, so a late usage event is not lost. If no usage
  arrives, the scenario is marked `usage: missing`, the id is listed in
  `limits.usageMissing`, and the token caps are reported as **UNVERIFIED** for
  it; missing usage is never counted as 0 tokens. If the probe does not echo
  `maxOutputTokens`, the id is listed in `limits.echoMissing`. **Either one
  makes the run exit 3 (BLOCKED/partial), never 0.** Exit 0 therefore means
  every cap was actually checked, not merely that every scenario PASSed.
- Request budget is checked before the next scenario *and* after accounting:
  a probe that retries and reports more than one request for a scenario, or
  whose known+unknown total exceeds `MAX_REQUESTS`, records a
  `limits.violations` entry and the scenario FAILs (exit 1). The pre-check
  alone could not see retries the probe already performed.
- 45 s per probe scenario (inner, passed to the probe); 60 s harness hard cap.
  On the hard cap the whole probe process group is killed and the scenario is
  classified `environment-or-provider (timeout)`, never product/protocol.
  Ctrl-C (KeyboardInterrupt/SystemExit) also kills the whole process group
  before re-raising; the detached probe/cargo children do not receive the
  terminal SIGINT on their own. SIGINT is blocked across `Popen` itself so the
  fork/exec-to-handle-assignment window cannot lose the child: the pending
  signal is delivered after the handle exists, inside the cleanup `try`.
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
`mkdir(.smoke.lock)` acquisition; `owner.json` is written immediately after
through a directory file descriptor opened at mkdir time, so a concurrent
reclaim that moves the directory cannot make this run write its owner into
another lock. A lock directory without a readable `owner.json` is treated as
held (a run may be between mkdir and the owner write), never as stale.
Reclaim verifies the owner pid *and* the directory identity (device+inode)
before and after renaming it aside, immediately reserves the freed path, and
refuses if either changed. `release()` removes the directory only when both
`owner.json` still names this process and the inode is the one this process
created. A concurrent second run refuses with exit 3 instead of deleting the
first run's records.

A lock whose owner pid looks alive is still reclaimed once it is older than
the two-hour age cap. That is the escape hatch for pid reuse and for
`kill(pid, 0)` raising `PermissionError` (another uid); one bounded run is
well under 15 minutes, so a two-hour-old lock cannot be healthy. If a run is
intentionally suspended longer, use a different `--evidence-dir`.

Manual recovery after a crash or a refused reclaim (exit 3, message names
`.smoke.lock` / `.smoke.lock.stale-*`):

```sh
# Confirm no smoke run is active first (the run is bounded; check the pids in
# owner.json and `ps`).
rm -rf .smoke.lock .smoke.lock.stale-*   # only inside the evidence directory
# or simply use a fresh --evidence-dir
```

Exit codes:

| code | meaning |
| --- | --- |
| 0 | all model-runtime scenarios PASS, every token cap actually verified (usage observed and `maxOutputTokens` echoed), and the secret self-check verified |
| 1 | at least one scenario FAIL, including a request-budget violation |
| 2 | secret self-check failed (leak, generic hit, 0 keys, or a partial artifact scan) |
| 3 | BLOCKED/partial: no model-runtime scenario actually ran (missing key, full cargo slots, build failure/timeout), a model-runtime scenario did not PASS (e.g. PASS + BLOCKED after the request budget ran out), usage or `maxOutputTokens` echo is missing (caps UNVERIFIED), or another run holds the evidence lock |

`BLOCKED` is not a pass. With zero requests sent the script exits 3 even if the
only reason is a missing key. Exit 0 requires **every** model-runtime scenario
to be PASS **and** every token cap verified; a PASS + BLOCKED mix, missing
usage, or a missing `maxOutputTokens` echo is partial and exits 3.
`report.json` carries `capsVerified` and `overallPass` (exit 0) as explicit
machine-readable signals; `finalPass` is only the secret self-check, so do not
read it as the run verdict.

`stream-cancel` normally aborts before the provider's final `usage` event, so
that scenario reports `usage: missing` and the run exits 3 even though the
cancel assertion PASSed. That is the intended honest outcome of "unverified is
not a pass", not a harness failure; a run that must be exit 0 has to verify the
cap, which this scenario structurally cannot.

`--self-check-only` with an unreadable or empty env file exits 2. It does not
take the evidence lock: a concurrent normal run may remove a stale-lock
directory or rewrite a report between listing and reading. The scanner walks
with `os.walk(onerror=...)`, skips one unreadable/missing directory at a time,
counts vanished files as `filesSkipped` and directories as `walkErrors`, and
keeps scanning the rest. A partial scan (`scanTruncated`) is **not** a pass:
`self_check_ok` returns false and the run exits 2 instead of reporting
"未命中" for bytes that were never read.

Probe unit tests (the crate is a separate workspace, so the product
`cargo test --all` / clippy / fmt jobs do not see it):

```sh
sh scripts/cargo-slots.sh
cargo test --offline --locked --manifest-path scripts/smoke/probe/Cargo.toml
```

`python3 -I scripts/test-smoke-real-provider.py` runs the same command after
`scripts/cargo-slots.sh` as `test_probe_rust_tests()`, through
`run_bounded()` with a 900 s timeout so a hung test kills the whole
`cargo`/rustc/test process group rather than leaving an orphan writer in the
shared target directory. CI runs that Python suite, so the thirteen probe
tests are executed there. A missing `cargo` or a full slot set fails the
suite instead of silently skipping it.

`probe/src/main.rs:error_value()` copies the canonical `status` (when numeric)
alongside `code`/`retryable`. The Python consumer classifies `status >= 500`
as `environment-or-provider` even when an upstream non-canonical `type`
(`api_error`) won the code, and a probe unit test asserts that field is
present end-to-end in the emitted JSON shape.

`probe/Cargo.lock` is tracked (`git ls-files scripts/smoke/probe/Cargo.lock`).
It pins the same versions as `src/Cargo.lock` but is resolved for the probe
graph: it carries the `real-provider-probe` entry and drops workspace-only
members such as `pilotdeck-cli`. It is not a byte-for-byte copy. Refresh it
when the product lock changes.
