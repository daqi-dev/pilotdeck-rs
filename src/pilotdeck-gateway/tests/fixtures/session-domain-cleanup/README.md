# session-domain-cleanup

#164 controls for the ps-free process-group cleanup probe in
`scripts/gateway-parity/session_domain.py`.

## Run in this session (does not mutate source)

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/check_cleanup.py \
  --output _tmp/20261026-issue-164/negative/behavior --mode all
```

`clean` is the empty-group positive control; `leak` leaves a `sleep 60`
grandchild after the launcher exits; `ignore` leaves a grandchild that ignores
SIGTERM to exercise the bounded SIGKILL escalation; `blocked` patches
`killpg` to `PermissionError` and pins `BLOCKED`/non-PASS; `runtime-negative`
replaces only the probe result with `VERIFIED` at runtime and shows the leaky
fixture would then look accepted, while the real probe still reports
`GROUP_NOT_EMPTY`; `status` pins `VERIFIED -> PASS` and
`GROUP_NOT_EMPTY/BLOCKED/NOT_VERIFIED -> PASS_WITH_CLEANUP_UNVERIFIED`.
Every process mode checks the live process group and the grandchild PID after
the run.

## Run after commit (source mutation controls)

`negative_controls.py` uses `negative_process.mutating_sources`, so it refuses
to run until the #164 change is committed and the named source is byte-identical
to HEAD. It mutates `session_domain.py` one anchor at a time, requires the
behavior check to fail, restores the bytes, and finally requires the restored
source to pass. A stale anchor exits 2 (`ANCHOR_EXPIRED`), never `SURVIVED`.
No cargo is launched, so `CARGO_TARGET_DIR` is not used by this fixture.

```sh
python3 -I src/pilotdeck-gateway/tests/fixtures/session-domain-cleanup/negative_controls.py \
  --output _tmp/20261026-issue-164/negative/source
```

`pgrep-before.txt` / `pgrep-after.txt` are collected around the mutations using
the unique `session-domain-cleanup/launcher.py` pattern; the fixture never
matches or signals another user's `sleep`.
