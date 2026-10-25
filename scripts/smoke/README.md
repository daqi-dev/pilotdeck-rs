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
`ZAI_API_KEY` from `/Users/da/ws/pilotdeck-rs/.env.local` at runtime, chooses
one by model prefix (`glm-*` → ZAI, `deepseek-*` → DEEPSEEK, otherwise
Anthropic), and passes it only in the probe child environment. Every artifact
is redacted, then scanned for the loaded key prefixes and generic key
patterns; only hit/miss is printed.

Limits (constants in the script): 6 requests, 128 output tokens per request,
768 total output tokens, 45 s per probe scenario. The probe disables retries
(`maxRetries`/`streamMaxRetries` = 0). Ports 28789/28790/28810-28812 are
checked for occupancy; the probe opens no listener and uses isolated
`HOME`/`TMPDIR`/cwd under the evidence directory.

Outputs: `_tmp/20261025-real-provider-smoke/report.json` and `report.md`, plus
redacted raw stdout/stderr under `run/raw/`.

`probe/Cargo.lock` is a copy of `src/Cargo.lock` at the time of writing, so the
offline build resolves to the same pinned dependency tree (including the
`icu_collator_data` path patch). Refresh it when the product lock changes.
