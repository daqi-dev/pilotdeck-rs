#!/usr/bin/env python3
"""#1327 HEAD-only channel controls; compile each selected mutant before judging it.

OUT [mutation ...], --compile-only OUT, --check (also --check-anchors).
The shared runner owns source restoration, offline cargo, nonce receipts,
patch evidence, pending registrations and the complete result summary.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import matrix_main

root = Path(__file__).resolve().parents[5]
fixtures = Path(__file__).resolve().parents[1]
source = root / 'src/pilotdeck-gateway/src/channel'
tests = source / 'tests.rs'
MUTATIONS = [
    ('config-enabled-gate', source / 'loader.rs',
     'if !js_truthy(cfg.get("enabled")) {',
     'if true {',
     'loader_matches_real_ts', '#1327 loader oracle'),
    ('config-order', source / 'loader.rs',
     '    "telegram",\n    "discord",',
     '    "discord",\n    "telegram",',
     'loader_matches_real_ts', '#1327 loader oracle'),
    ('nullish-password', source / 'loader.rs',
     'if nullish(&token) {',
     'if false {',
     'loader_matches_real_ts', '#1327 loader oracle'),
    ('load-error-format', source / 'loader.rs',
     'error.to_js_string()',
     'error.message()',
     'loader_matches_real_ts', '#1327 loader oracle'),
    ('missing-module-continue', source / 'loader.rs',
     '                continue;\n            }\n        };',
     '                break;\n            }\n        };',
     'registry_missing_and_replacement', '#1327 registry continue and replace'),
    ('disable-skipped', source / 'server.rs',
     'if managed.insert(key.clone()) && !desired.contains(&key) {',
     'if managed.insert(key.clone()) && false && !desired.contains(&key) {',
     'lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('managed-dedup', source / 'server.rs',
     'if managed.insert(key.clone()) && !desired.contains(&key) {',
     'if (managed.insert(key.clone()) || true) && !desired.contains(&key) {',
     'lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('close-admission', source / 'server.rs',
     '            state.closing = true;',
     '            state.closing = false;',
     'lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('late-start-stop-reason', source / 'server.rs',
     'begin_stop(handle, "server-shutdown").await?;',
     'begin_stop(handle, "hot-reload").await?;',
     'lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('rollback-skipped', source / 'server.rs',
     'if let Some(previous) = previous {',
     'if let Some(previous) = previous.filter(|_| false) {',
     'lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('stop-restoration', source / 'server.rs',
     'if !state.running.iter().any(|item| item.key == key) {',
     'if false && !state.running.iter().any(|item| item.key == key) {',
     'lifecycle_matches_real_ts', '#1327 lifecycle oracle'),
    ('close-errors-dropped', source / 'server.rs',
     'let result = if errors.is_empty() {',
     'let result = if true {',
     'adapter_panics_do_not_strand_lifecycle', '#1327 panic stop rejection'),
    ('report-order-bypassed', source / 'server.rs',
     'if reports.dispatching {',
     'if false {',
     'cross_thread_status_order', '#1327 cross-thread status order'),
    ('status-callback-skipped', source / 'server.rs',
     'if let Some(report) = &self.deps.report_status {',
     'if let Some(report) = &None::<StatusReporter> {',
     'status_callback_can_reenter_close', '#1327 reentrant callback initiated close'),

    ('option-field-order', source / 'loader.rs',
     '            insert(&mut built, "token", field(cfg, "token"));\n            insert(&mut built, "webhookUrl", field(cfg, "webhookUrl"));',
     '            insert(&mut built, "webhookUrl", field(cfg, "webhookUrl"));\n            insert(&mut built, "token", field(cfg, "token"));',
     'loader_matches_real_ts', '#1327 loader oracle'),
    ('error-after-loop', source / 'loader.rs',
     '    let mut channels = Vec::new();\n    for key in CHANNEL_LOADER_KEYS {\n        let Some(cfg) = adapters.get(key) else {\n            continue;\n        };\n        if !js_truthy(cfg.get("enabled")) {\n            continue;\n        }\n        let constructor = match mounts.is_registered(key).then(|| mounts.creator(key)) {\n            Some(constructor) => constructor,\n            None => {\n                console_error(format!(\n                    "[adapters] Failed to load channel \\"{key}\\": Error: channel module not registered: {key}"\n                ));\n                continue;\n            }\n        };\n        let built = build_options(key, cfg, options);\n        match constructor(&built) {\n            Ok(channel) => channels.push(channel),\n            Err(error) => console_error(format!(\n                "[adapters] Failed to load channel \\"{key}\\": {}",\n                error.to_js_string()\n            )),\n        }\n    }\n    channels',
     '    let mut channels = Vec::new();\n    let mut delayed_errors = Vec::new();\n    for key in CHANNEL_LOADER_KEYS {\n        let Some(cfg) = adapters.get(key) else {\n            continue;\n        };\n        if !js_truthy(cfg.get("enabled")) {\n            continue;\n        }\n        let constructor = match mounts.is_registered(key).then(|| mounts.creator(key)) {\n            Some(constructor) => constructor,\n            None => {\n                delayed_errors.push(format!(\n                    "[adapters] Failed to load channel \\"{key}\\": Error: channel module not registered: {key}"\n                ));\n                continue;\n            }\n        };\n        let built = build_options(key, cfg, options);\n        match constructor(&built) {\n            Ok(channel) => channels.push(channel),\n            Err(error) => delayed_errors.push(format!(\n                "[adapters] Failed to load channel \\"{key}\\": {}",\n                error.to_js_string()\n            )),\n        }\n    }\n    for error in delayed_errors { console_error(error); }\n    channels',
     'loader_matches_real_ts', '#1327 loader oracle'),
    ('replace-snapshot-gap', source / 'server.rs',
     '        #[cfg(test)]\n        inner.transition("replace-checked", key);\n        state\n            .running',
     '        drop(state);\n        #[cfg(test)]\n        inner.transition("replace-checked", key);\n        let state = inner.state.lock().expect("channel state");\n        state\n            .running',
     'replace_close_keeps_previous_snapshot', '#1327 replace snapshot atomic'),
    ('close-skips-stop-prefix', source / 'server.rs',
     '            wait_stop_prefixes(&inner).await;',
     '            if false { wait_stop_prefixes(&inner).await; }',
     'disable_close_seals_ingress_before_host', '#1327 disable close waits stop prefix'),
    ('late-stop-error-swallowed', source / 'server.rs',
     'begin_stop(handle, "server-shutdown").await?;',
     'let _ = begin_stop(handle, "server-shutdown").await;',
     'late_start_stop_failure_matches_real_ts', '#1327 late stop failure oracle'),
    ('lifecycle-empty-cases', fixtures / 'channel-lifecycle/oracle.json',
     '  "cases": [\n',
     '  "cases": [],\n  "disabledCases": [\n',
     'lifecycle_matches_real_ts', '#1327 lifecycle oracle cases must be non-empty'),
    ('lifecycle-missing-case', tests,
     '    let cases = lifecycle_cases(&oracle);',
     '    let mut oracle = oracle;\n    let _ = oracle["cases"].as_array_mut().expect("#1327 mutant cases").pop();\n    let cases = lifecycle_cases(&oracle);',
     'lifecycle_matches_real_ts', '#1327 lifecycle oracle case count'),
    ('lifecycle-wrong-declaration', fixtures / 'channel-lifecycle/oracle.json',
     '  "expectedCaseCount": 19,',
     '  "expectedCaseCount": 18,',
     'lifecycle_matches_real_ts', '#1327 lifecycle generator declaration'),
    ('loader-empty-cases', fixtures / 'channel-loader/oracle.json',
     '  "cases": [\n',
     '  "cases": [],\n  "disabledCases": [\n',
     'loader_matches_real_ts', '#1327 loader oracle cases must be non-empty'),
    ('loader-missing-case', tests,
     '    let cases = loader_cases(&oracle);',
     '    let mut oracle = oracle;\n    let _ = oracle["cases"].as_array_mut().expect("#1327 mutant cases").pop();\n    let cases = loader_cases(&oracle);',
     'loader_matches_real_ts', '#1327 loader oracle case count'),
    ('loader-wrong-declaration', fixtures / 'channel-loader/oracle.json',
     '  "expectedCaseCount": 8,',
     '  "expectedCaseCount": 7,',
     'loader_matches_real_ts', '#1327 loader generator declaration'),
]


def main():
    guarded = list(source.glob('*.rs'))
    for name in ('channel-loader', 'channel-lifecycle'):
        guarded.extend(path for path in (fixtures / name).rglob('*') if path.is_file())
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    return matrix_main(root, __file__, 'channel::tests::', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())
