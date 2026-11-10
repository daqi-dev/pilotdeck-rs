# Agent model field constructor oracle

New unfrozen product-side evidence for the model-input review. Generate using exact Node22.22.0/ICU78.2 with baseline fixed at cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85:

node --import BASELINE/node_modules/tsx/dist/loader.mjs generate.mts BASELINE FRESH_ORACLE

The generator byte-checks the actual fixed TS agent.ts, builtinSubagentTypes.ts, subagentContinuation.ts and executeCode.ts against git show before invoking their real constructors. It does not parse expected descriptions out of source text and does not reimplement description generation.

Twelve agent cases cover default, custom/empty fallback presets, custom open depth, at/last/zero cap, negative clamping and fractional boundaries. Default/custom descriptions are actually compared at the same depth. Corresponding Rust test inputs use the production subagentDefinitions key; tests call AgentTool::for_config and also confirm the custom registry replaces built-ins, retaining the inherited fixed-TS description behavior rather than rewriting it.

Two continuation cases invoke createSubagentContinuationTool and createSendMessageTool. Tests call the production ContinuationTool constructor for both. Comparisons cover actual model fields (name, description, inputSchema), aliases/kind/capability/result-size metadata and read-only/concurrency/open-world behavior. rawSerializableDefinition retains additional source metadata, including outputSchema; this is not a claim that all non-model output contracts are equivalent.

executeDescriptions captures actual createExecuteCodeTool output for absent sandbox and configured modes. policySelections additionally runs the real executeCode execution path with configured workspace-write while the host resolvePolicy returns either read-only or workspace-write. SandboxPort.prepare records the resolved policy and deliberately rejects before running a child. This demonstrates configured description and per-invocation policy are separate inputs. It is not a filesystem sandbox enforcement test, not proof of Native explicit-authorization behavior and not a reason to add an unrequested caveat to fixed model text.

The evidence supports independent arbitration of the cited P1/P2 source claims. It does not self-approve a severity downgrade, accepted difference or formal parity completion. No production strings, old oracle, frozen scorer or execute_code shared source are changed. Rust tests are integrated and executed by the root owner.

The constructor matrix also compares all eight Rust-representable `execute_code` descriptions: no sandbox plus three explicit modes, each with search enabled and disabled. Two `mode-omitted` cases are TS-only because native `SandboxMode` requires a concrete mode. `policySelections` remains runtime arbitration evidence for TS policy resolution, not a claim of a corresponding Rust host-injection test. All generators use the shared baseline guard and record the import closure hashes.
