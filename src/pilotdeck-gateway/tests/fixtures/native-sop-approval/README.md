# Actual SOP human approval consumer

Standalone bounded product diagnostic. Run python3 -I producer.py --engine ts|native --baseline <fixed TS> --node <Node22.22.0 ICU78.2> --candidate-manifest <array manifest> --source-manifest <source manifest> --expected-source-sha <SHA256> --gateway <gateway> --sidecar <sidecar> --out <new exclusive directory>.

The caller must serialize shared parity access with /tmp/pdrs-parity.lock. Every endpoint is an owned loopback mock; no real accounts, providers or user home. Raw requests, wire, model inputs, provenance and durable SOP state remain in exclusive output on failure. The producer verifies fixed TS HEAD, exact Node/ICU and pair/source SHA; baseline is an externally guarded read-only resource checkout. This does not replace formal scoring.

Uses real Server and original single-user owner SQLite authority, real trusted session admission through formal identity/credential/agent HTTP requests, active project binding, actual SOP discovery/provider and A7. It does not manufacture admission metadata or inject approval authority.

Checks actual handoff, expected missing-auth and foreign-subject rejection without revision change, authorized public sop_resume, one-shot receipt replay, preserved slot decision, and second actual A7 with successful control tool and completed terminal. Model tool calls require the tool actually advertised in that request; historical tool results do not suppress a later user's valid SOP step. Normal capacity64k is distinct from pre-existing7k budget-negative case; this scenario does not assert compaction.

Scope: mock provider, single-user authority, same-process persisted pause/reply/resume. Fresh-process restart/approval receipt replay not claimed. New final integration head must rerun.
