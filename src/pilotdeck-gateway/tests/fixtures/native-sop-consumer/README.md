# Native SOP configured consumer
Unfrozen product diagnostic, derived from the retained r49 producer.
Use run.py with --engine ts/native --baseline PATH --node EXACT_NODE --candidate-manifest PATH --source-manifest PATH --expected-source-sha SHA --out FRESH_PATH and, for native, --gateway PATH --sidecar PATH.
Both sides keep the original SOP definitions, contextMode=new_session, lifecycle HTTP provider, PreCompact/PostCompact hooks, five user messages, maxContextTokens=7000 and mocked usage=11000.
The model only emits submit_step_result when that name is actually advertised.
This is explicitly one successful SOP turn followed by four expected context-budget failures. The first turn must finish completed and successfully submit the terminal SOP step. Each later turn must finish prompt_too_long with agent_context_recovery_failed, and none may call an unadvertised tool. Compaction notifications, summarizer requests and both hook kinds remain mandatory.
This is not five successful turns, not a formal accepted difference and not a substitute for larger-capacity multi-turn success coverage.
The caller passes immutable source and candidate manifests. Node22.22.0/ICU78.2, fixed TS SHA, binary hashes, isolated home/workspaces, bounded driver/stop and the shared diagnostic lock are checked.
