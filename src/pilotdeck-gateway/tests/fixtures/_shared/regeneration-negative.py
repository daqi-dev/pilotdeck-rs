#!/usr/bin/env python3
"""#280 exact regeneration controls; compile each mutant, run all, then summarize.

Usage: regeneration-negative.py OUT [--compile-only] [mutation ...]
No fixture/source copy or alternate cargo target is created. Integration-target
execution uses negative_suite's exact-test classifier and the shared source guard.
Run exclusively in this worktree: no other cargo or test process may run here
until restoration finishes. The source lock excludes negative runners only;
the coordinator must serialize ordinary cargo/test jobs in the same worktree.
"""
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (exclusive_write, mutating_sources, run_owned,
                              wait_for_slot)
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
provider = root / 'src/pilotdeck-gateway/tests/fixtures/provider-timing/oracle.json'
registry = root / 'src/pilotdeck-gateway/tests/fixtures/run-registry/oracle.json'
snapshot = root / 'src/pilotdeck-gateway/tests/fixtures/run-registry/normal.after.json'
builtin = root / 'src/pilotdeck-gateway/tests/fixtures/builtin-tools/oracle.json'
grep = root / 'src/pilotdeck-gateway/tests/fixtures/grep-tool/oracle.json'
skills = root / 'src/pilotdeck-gateway/tests/fixtures/skills-tree-files/generate.mts'
prompt = root / 'src/pilotdeck-gateway/tests/fixtures/turn-prompt-hooks/generate.mts'
external = root / 'src/pilotdeck-gateway/tests/fixtures/skills-external/generate.mts'
http = root / 'src/pilotdeck-gateway/tests/fixtures/provider-http-boundaries/generate.mts'
conversion = root / 'src/pilotdeck-gateway/tests/fixtures/web-conversion-lifecycle/generate.mts'
edgeclaw_accessors = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-accessors/generate.mts'
edgeclaw_capture_turn = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-capture-turn/generate.mts'
edgeclaw_dangling_links = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-dangling-links/generate.mts'
edgeclaw_dream = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream/generate.mts'
edgeclaw_dream_run = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream-run/generate.mts'
edgeclaw_dream_runner = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-dream-runner/generate.mts'
edgeclaw_entries = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-entries/generate.mts'
edgeclaw_export = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-export/generate.mts'
edgeclaw_file_memory = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-file-memory/generate.mts'
edgeclaw_heartbeat = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-heartbeat/generate.mts'
edgeclaw_heartbeat_indexer = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-heartbeat-indexer/generate.mts'
edgeclaw_llm = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm/generate.mts'
edgeclaw_llm_config = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-config/generate.mts'
edgeclaw_llm_dream = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-dream/generate.mts'
edgeclaw_llm_envelope = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-envelope/generate.mts'
edgeclaw_llm_fix_r1 = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-fix-r1/generate.mts'
edgeclaw_llm_index = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-index/generate.mts'
edgeclaw_llm_recall = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-recall/generate.mts'
edgeclaw_llm_recall_js_sort = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-llm-recall-js-sort/generate.mts'
edgeclaw_memory_write = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-memory-write/generate.mts'
edgeclaw_messages = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-messages/generate.mts'
edgeclaw_retrieval_helpers = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-retrieval-helpers/generate.mts'
edgeclaw_retriever = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-retriever/generate.mts'
edgeclaw_snapshots = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-snapshots/generate.mts'
edgeclaw_storage = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-storage/generate.mts'
edgeclaw_text = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-text/generate.mts'
edgeclaw_provider = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-provider/generate.mts'
edgeclaw_service_facade = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-service-facade/generate.mts'
edgeclaw_service_orchestration = root / 'src/pilotdeck-gateway/tests/fixtures/edgeclaw-service-orchestration/generate.mts'
cases = [
    ('edgeclaw-service-orchestration-partial-provenance', edgeclaw_service_orchestration,
     'sourceSha256 = sourceClosureSha256(baseline, commit, gitExecutable, sources)',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, gitExecutable, sources)).slice(0, 1))',
     'edgeclaw_service_orchestration_regenerates_identically',
     '#280 edgeclaw-service-orchestration/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-service-facade-partial-provenance', edgeclaw_service_facade,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, sources)',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, sources)).slice(0, 1))',
     'edgeclaw_service_facade_regenerates_identically',
     '#280 edgeclaw-service-facade/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-provider-partial-provenance', edgeclaw_provider,
     'sourceSha256=sourceClosureSha256(baseline,commit,gitExecutable,SOURCES)',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline,commit,gitExecutable,SOURCES)).slice(0, 1))',
     'edgeclaw_provider_regenerates_identically',
     '#280 edgeclaw-provider/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-accessors-partial-provenance', edgeclaw_accessors,
     'sourceSha256=sourceClosureSha256(baseline,commit,git,[source])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline,commit,git,[source])).slice(0, 1))',
     'edgeclaw_accessors_regenerates_identically',
     '#280 edgeclaw-accessors/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-capture-turn-partial-provenance', edgeclaw_capture_turn,
     'sourceSha256 = sourceClosureSha256(baseline, provenance.ref, git, sources)',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, provenance.ref, git, sources)).slice(0, 1))',
     'edgeclaw_capture_turn_regenerates_identically',
     '#280 edgeclaw-capture-turn/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-dangling-links-partial-provenance', edgeclaw_dangling_links,
     'sourceSha256 = sourceClosureSha256(baseline, COMMIT, git, [SOURCE])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, COMMIT, git, [SOURCE])).slice(0, 1))',
     'edgeclaw_dangling_links_regenerates_identically',
     '#280 edgeclaw-dangling-links/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-dream-partial-provenance', edgeclaw_dream,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/review/dream-review.ts`,\n])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/review/dream-review.ts`,\n])).slice(0, 1))',
     'edgeclaw_dream_regenerates_identically',
     '#280 edgeclaw-dream/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-dream-run-partial-provenance', edgeclaw_dream_run,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/review/dream-review.ts`,\n])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/review/dream-review.ts`,\n])).slice(0, 1))',
     'edgeclaw_dream_run_regenerates_identically',
     '#280 edgeclaw-dream-run/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-dream-runner-partial-provenance', edgeclaw_dream_runner,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/review/dream-review.ts`,\n])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/review/dream-review.ts`,\n])).slice(0, 1))',
     'edgeclaw_dream_runner_regenerates_identically',
     '#280 edgeclaw-dream-runner/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-entries-partial-provenance', edgeclaw_entries,
     'sourceSha256=sourceClosureSha256(baseline,commit,git,[source])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline,commit,git,[source])).slice(0, 1))',
     'edgeclaw_entries_regenerates_identically',
     '#280 edgeclaw-entries/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-export-partial-provenance', edgeclaw_export,
     'sourceSha256=sourceClosureSha256(baseline,commit,git,[source])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline,commit,git,[source])).slice(0, 1))',
     'edgeclaw_export_regenerates_identically',
     '#280 edgeclaw-export/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-file-memory-partial-provenance', edgeclaw_file_memory,
     'sourceSha256 = sourceClosureSha256(baseline,commit,git,[source,sqliteSource])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline,commit,git,[source,sqliteSource])).slice(0, 1))',
     'edgeclaw_file_memory_regenerates_identically',
     '#280 edgeclaw-file-memory/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-heartbeat-partial-provenance', edgeclaw_heartbeat,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/pipeline/heartbeat.ts`,\n])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/pipeline/heartbeat.ts`,\n])).slice(0, 1))',
     'edgeclaw_heartbeat_regenerates_identically',
     '#280 edgeclaw-heartbeat/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-heartbeat-indexer-partial-provenance', edgeclaw_heartbeat_indexer,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, sources)',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, sources)).slice(0, 1))',
     'edgeclaw_heartbeat_indexer_regenerates_identically',
     '#280 edgeclaw-heartbeat-indexer/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-llm-partial-provenance', edgeclaw_llm,
     'sourceSha256=sourceClosureSha256(base,commit,git,[ENTRY])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(base,commit,git,[ENTRY])).slice(0, 1))',
     'edgeclaw_llm_regenerates_identically',
     '#280 edgeclaw-llm/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-llm-config-partial-provenance', edgeclaw_llm_config,
     "sourceSha256 = sourceClosureSha256(baseline, commit, git, [\n  'src/context/memory/edgeclaw-memory-core/src/service.ts',\n  'src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts',\n  'src/context/memory/createEdgeClawMemoryProviderFromConfig.ts',\n])",
     "sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [\n  'src/context/memory/edgeclaw-memory-core/src/service.ts',\n  'src/context/memory/edgeclaw-memory-core/src/core/skills/llm-extraction.ts',\n  'src/context/memory/createEdgeClawMemoryProviderFromConfig.ts',\n])).slice(0, 1))",
     'edgeclaw_llm_config_regenerates_identically',
     '#280 edgeclaw-llm-config/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-llm-dream-partial-provenance', edgeclaw_llm_dream,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, [ENTRY, DREAM_ENTRY])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [ENTRY, DREAM_ENTRY])).slice(0, 1))',
     'edgeclaw_llm_dream_regenerates_identically',
     '#280 edgeclaw-llm-dream/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-llm-envelope-partial-provenance', edgeclaw_llm_envelope,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/skills/llm-extraction.ts`,\n])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/skills/llm-extraction.ts`,\n])).slice(0, 1))',
     'edgeclaw_llm_envelope_regenerates_identically',
     '#280 edgeclaw-llm-envelope/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-llm-fix-r1-partial-provenance', edgeclaw_llm_fix_r1,
     "sourceSha256 = sourceClosureSha256(base, commit, git, [ENTRY, 'src/cli/proxy.ts'])",
     "sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(base, commit, git, [ENTRY, 'src/cli/proxy.ts'])).slice(0, 1))",
     'edgeclaw_llm_fix_r1_regenerates_identically',
     '#280 edgeclaw-llm-fix-r1/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-llm-index-partial-provenance', edgeclaw_llm_index,
     'sourceSha256=sourceClosureSha256(base,commit,git,[ENTRY])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(base,commit,git,[ENTRY])).slice(0, 1))',
     'edgeclaw_llm_index_regenerates_identically',
     '#280 edgeclaw-llm-index/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-llm-recall-partial-provenance', edgeclaw_llm_recall,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, [ENTRY])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [ENTRY])).slice(0, 1))',
     'edgeclaw_llm_recall_regenerates_identically',
     '#280 edgeclaw-llm-recall/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-llm-recall-js-sort-partial-provenance', edgeclaw_llm_recall_js_sort,
     'sourceSha256 = sourceClosureSha256(baseline, COMMIT, git, [ENTRY])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, COMMIT, git, [ENTRY])).slice(0, 1))',
     'edgeclaw_llm_recall_js_sort_regenerates_identically',
     '#280 edgeclaw-llm-recall-js-sort/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-memory-write-partial-provenance', edgeclaw_memory_write,
     'const oracle = { sourceCommit: provenance.ref, sourceSha256, missingMessage, writes, afterUnlink, clearSentinel };',
     'const oracle = { sourceCommit: provenance.ref, sourceSha256: Object.fromEntries(Object.entries(sourceSha256).slice(0, 1)), missingMessage, writes, afterUnlink, clearSentinel };',
     'edgeclaw_memory_write_regenerates_identically',
     '#280 edgeclaw-memory-write/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-messages-partial-provenance', edgeclaw_messages,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, [\n  `${root}/message-utils.ts`,\n])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [\n  `${root}/message-utils.ts`,\n])).slice(0, 1))',
     'edgeclaw_messages_regenerates_identically',
     '#280 edgeclaw-messages/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-retrieval-helpers-partial-provenance', edgeclaw_retrieval_helpers,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/retrieval/reasoning-loop.ts`,\n])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/retrieval/reasoning-loop.ts`,\n])).slice(0, 1))',
     'edgeclaw_retrieval_helpers_regenerates_identically',
     '#280 edgeclaw-retrieval-helpers/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-retriever-partial-provenance', edgeclaw_retriever,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, [REASONING, SQLITE, LLM, SERVICE])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [REASONING, SQLITE, LLM, SERVICE])).slice(0, 1))',
     'edgeclaw_retriever_regenerates_identically',
     '#280 edgeclaw-retriever/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-snapshots-partial-provenance', edgeclaw_snapshots,
     'sourceSha256=sourceClosureSha256(baseline,commit,git,[source])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline,commit,git,[source])).slice(0, 1))',
     'edgeclaw_snapshots_regenerates_identically',
     '#280 edgeclaw-snapshots/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-storage-partial-provenance', edgeclaw_storage,
     'sourceSha256 = sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [\n  SQLITE_TS,\n])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, BASELINE_COMMIT, gitExecutable, [\n  SQLITE_TS,\n])).slice(0, 1))',
     'edgeclaw_storage_regenerates_identically',
     '#280 edgeclaw-storage/oracle.json differs from committed bytes outside the exact masks'),
    ('edgeclaw-text-partial-provenance', edgeclaw_text,
     'sourceSha256 = sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/utils/text.ts`,\n  `${root}/core/utils/id.ts`,\n  `${root}/core/general-projects.ts`,\n  `${root}/core/trace-i18n.ts`,\n  `${root}/service.ts`,\n])',
     'sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(baseline, commit, git, [\n  `${root}/core/utils/text.ts`,\n  `${root}/core/utils/id.ts`,\n  `${root}/core/general-projects.ts`,\n  `${root}/core/trace-i18n.ts`,\n  `${root}/service.ts`,\n])).slice(0, 1))',
     'edgeclaw_text_regenerates_identically',
     '#280 edgeclaw-text/oracle.json differs from committed bytes outside the exact masks'),
    ('prompt-partial-provenance', prompt,
     "const sourceSha256=await baselineSourceHashes(root,[...new Set([...Object.keys(old.sourceSha256),...entries.map(p=>'src/'+p)])].sort());",
     "const sourceSha256=Object.fromEntries(Object.entries(await baselineSourceHashes(root,[...new Set([...Object.keys(old.sourceSha256),...entries.map(p=>'src/'+p)])].sort())).slice(0,1));",
     'turn_prompt_hooks_regenerates_identically',
     '#280 turn-prompt-hooks/oracle.json differs from committed bytes outside the exact masks'),
    ('external-partial-provenance', external,
     'const sourceSha256 = await baselineSourceHashes(baseline, sources);',
     'const sourceSha256 = Object.fromEntries(Object.entries(await baselineSourceHashes(baseline, sources)).slice(0, 1));',
     'skills_external_regenerates_identically',
     '#280 skills-external/oracle.json differs from committed bytes outside the exact masks'),
    ('http-partial-provenance', http,
     "const sourceSha256=sourceClosureSha256(base,sourceCommit,gitExecutable,[entry,'src/cli/proxy.ts']);",
     "const sourceSha256=Object.fromEntries(Object.entries(sourceClosureSha256(base,sourceCommit,gitExecutable,[entry,'src/cli/proxy.ts'])).slice(0,1));",
     'provider_http_boundaries_regenerates_identically',
     '#280 provider-http-boundaries/oracle.json differs from committed bytes outside the exact masks'),
    ('conversion-partial-provenance', conversion,
     "const sourceSha256 = sourceClosureSha256(base, sourceCommit, git, ['src/tool/builtin/webFetch.ts']);",
     "const sourceSha256 = Object.fromEntries(Object.entries(sourceClosureSha256(base, sourceCommit, git, ['src/tool/builtin/webFetch.ts'])).slice(0, 1));",
     'web_conversion_lifecycle_regenerates_identically',
     '#280 web-conversion-lifecycle/oracle.json differs from committed bytes outside the exact masks'),
    ('skills-macos-provenance', skills,
     "const oracleMacos = {sourceCommit, sourceSha256, platform: 'darwin', cases: macosCases};",
     "const oracleMacos = {sourceCommit, sourceSha256: {[sources[0]]: sourceSha256[sources[0]]}, platform: 'darwin', cases: macosCases};",
     'skills_tree_files_regenerates_identically',
     '#280 skills-tree-files: companion oracle-macos.json differs from committed bytes'),
    ('grep-output', grep,
     'multi.txt:foo one\\nmulti.txt:foo two\\n\\n[grep pagination] returned=2 total=2 offset=0 limit=250 truncated=false',
     'multi.txt:foo one\\nmulti.txt:foo THREE\\n\\n[grep pagination] returned=2 total=2 offset=0 limit=250 truncated=false',
     'grep_tool_regenerates_identically',
     '#280 grep-tool/oracle.json differs from committed bytes outside the exact masks'),
    ('provider-watchdog-backwards', provider,
     '"kind": "watchdog",\n     "start": 753,\n     "delay": 40',
     '"kind": "watchdog",\n     "start": 0,\n     "delay": 40',
     'provider_timing_regenerates_identically',
     '#280 provider-timing actual deadline order: anthropic/headers/fulfilled/continuation-first'),
    ('provider-continuation-tie', provider,
     '"kind": "parent",\n     "start": 753,\n     "delay": 25',
     '"kind": "parent",\n     "start": 748,\n     "delay": 25',
     'provider_timing_regenerates_identically',
     '#280 provider-timing actual deadline order: anthropic/headers/fulfilled/continuation-first'),
    ('provider-parent-reversed', provider,
     '"kind": "parent",\n     "start": 873,\n     "delay": 15',
     '"kind": "parent",\n     "start": 879,\n     "delay": 15',
     'provider_timing_regenerates_identically',
     '#280 provider-timing actual deadline order: anthropic/headers/fulfilled/parent-first'),
    ('provider-tie-first-shifted', provider,
     '"kind": "parent",\n     "start": 991,\n     "delay": 20',
     '"kind": "parent",\n     "start": 992,\n     "delay": 20',
     'provider_timing_regenerates_identically',
     '#280 provider-timing actual deadline order: anthropic/headers/fulfilled/tie-parent-first'),
    ('provider-tie-last-shifted', provider,
     '"kind": "parent",\n     "start": 1108,\n     "delay": 20',
     '"kind": "parent",\n     "start": 1107,\n     "delay": 20',
     'provider_timing_regenerates_identically',
     '#280 provider-timing actual deadline order: anthropic/headers/fulfilled/tie-parent-last'),
    ('builtin-glob-output', builtin,
     '[glob pagination] returned=2 total=3 truncated=true',
     '[glob pagination] returned=2 total=4 truncated=true',
     'builtin_tools_regenerates_identically',
     '#280 builtin-tools/oracle.json differs from committed bytes outside the exact masks'),
    ('provider-unmasked-byte', provider,
     '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"',
     '"sourceCommit": "dc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"',
     'provider_timing_regenerates_identically',
     '#280 provider-timing/oracle.json differs from committed bytes outside the exact masks'),
    ('registry-unmasked-byte', registry,
     '"sourceCommit": "cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"',
     '"sourceCommit": "dc7c4e653b15c8b2cec7bcf2d15d41d69396ae85"',
     'run_registry_regenerates_identically',
     '#280 run-registry/oracle.json differs from committed bytes outside the exact masks'),
    ('snapshot-unmasked-byte', snapshot, '"schemaVersion":1', '"schemaVersion":2',
     'run_registry_regenerates_identically',
     '#280 run-registry/normal.after.json differs from committed bytes outside the exact masks'),
]


def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('#280 CARGO_TARGET_DIR must be supplied by the caller')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    compile_only = '--compile-only' in sys.argv[2:]
    names = [arg for arg in sys.argv[2:] if arg != '--compile-only']
    selected = [case for case in cases if not names or case[0] in names]
    if names and len(selected) != len(set(names)):
        raise SystemExit('#280 unknown mutation name')
    skipped = []
    if sys.platform != 'darwin':
        if 'skills-macos-provenance' in names:
            raise SystemExit('#280 skills-macos-provenance requires darwin')
        skipped = [dict(mutation=case[0], reason='requires darwin')
                   for case in selected if case[1] == skills]
        for item in skipped:
            print(f"#280 SKIPPED {item['mutation']}: {item['reason']}", flush=True)
        selected = [case for case in selected if case[1] != skills]
    env = os.environ.copy()
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml',
               '-p', 'pilotdeck-gateway', '--test', 'generator_provenance']
    results = []

    def run(label, args):
        slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900)
        exclusive_write(out / (label + '.slots.log'), slot.stdout + slot.stderr)
        result = run_owned(command + args, cwd=root, env=env, timeout=900)
        exclusive_write(out / (label + '.log'), result.stdout + result.stderr)
        return result

    def record(name, test, assertion, status, reason):
        results.append(dict(mutation=name, test=test, assertion=assertion,
                            status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, list(dict.fromkeys(case[1] for case in selected))) as originals:
            for name, path, old, new, test, assertion in selected:
                status, reason = 'INVALID', 'not executed'
                args = [test, '--', '--exact', '--ignored', '--nocapture']
                try:
                    content = originals[path].decode()
                    if content.count(old) != 1:
                        raise RuntimeError(f'#280 stale anchor {name}: count={content.count(old)}')
                    changed = content.replace(old, new, 1)
                    if path.suffix == '.json':
                        json.loads(changed)
                    if not compile_only and not passed(run(name + '-positive', args), test):
                        raise RuntimeError('#280 positive bound test did not pass')
                    path.write_text(changed)
                    if run(name + '-compile', ['--no-run']).returncode:
                        raise RuntimeError('#280 mutant did not compile')
                    if compile_only:
                        status, reason = 'COMPILED', 'mutant compiles; runtime classification not requested'
                    else:
                        status, reason = classify(run(name + '-negative', args),
                                                  Case(name, path, old, new, test, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(originals[path])
                try:
                    restored = run(name + '-restored', ['--no-run'] if compile_only else args)
                    if restored.returncode or (not compile_only and not passed(restored, test)):
                        raise RuntimeError('#280 restored control did not pass')
                except Exception as error:
                    status, reason = 'INVALID', reason + '; ' + str(error)
                record(name, test, assertion, status, reason)
    except Exception as error:
        for row in results:
            row.update(status='INVALID', reason='source guard: ' + str(error))
        for name, _path, _old, _new, test, assertion in selected[len(results):]:
            record(name, test, assertion, 'INVALID', 'source guard: ' + str(error))
    states = ('COMPILED', 'INVALID') if compile_only else ('KILLED', 'SURVIVED', 'INVALID')
    counts = {state: sum(row['status'] == state for row in results) for state in states}
    exclusive_write(out / 'summary.json', json.dumps(dict(controls=results, counts=counts, skipped=skipped), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    expected = 'COMPILED' if compile_only else 'KILLED'
    return int(not results or any(row['status'] != expected for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
