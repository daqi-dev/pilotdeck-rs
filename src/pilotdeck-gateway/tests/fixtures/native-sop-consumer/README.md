# Native SOP configured consumer
Unfrozen product diagnostic, derived from the retained r49 producer.
Use run.py with --engine ts/native --baseline PATH --node EXACT_NODE --candidate-manifest PATH --source-manifest PATH --expected-source-sha SHA --out FRESH_PATH and, for native, --gateway PATH --sidecar PATH.
Both sides keep the original SOP definitions, contextMode=new_session, lifecycle HTTP provider, PreCompact/PostCompact hooks, five user messages, maxContextTokens=7000 and mocked usage=11000.
The model only emits submit_step_result when that name is actually advertised.
This is explicitly one successful SOP turn followed by four expected context-budget failures. The first turn must finish completed and successfully submit the terminal SOP step. Each later turn must finish prompt_too_long with agent_context_recovery_failed, and none may call an unadvertised tool. Compaction notifications, summarizer requests and both hook kinds remain mandatory.
This is not five successful turns, not a formal accepted difference and not a substitute for larger-capacity multi-turn success coverage.
The caller passes immutable source and candidate manifests. Node22.22.0/ICU78.2, fixed TS SHA, binary hashes, isolated home/workspaces, bounded driver/stop and the shared diagnostic lock are checked.

## Independent normal-capacity scenario
--scenario normal-capacity uses the same SOP nodes, contextMode, hooks, five inputs and advertised-tool mock, with a separate effective64k context window that can hold the full normal tool/system prompt. All five turns must finish completed without error or failed tool calls; the first must successfully submit the terminal SOP step.
The default budget-negative scenario remains7k with11k mocked usage and its unchanged mandatory compact/summarizer/hook checks. Do not reinterpret its four expected budget failures as success. Normal capacity is a distinct positive branch: optional observed hooks are retained and session-checked; it does not claim emergency compaction or approval recovery coverage.
