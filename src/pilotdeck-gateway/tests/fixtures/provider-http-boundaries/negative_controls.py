#!/usr/bin/env python3
"""#411 / #360 / #860. Run after committing; OUT [NAME ...].

Every mutant must compile, fail the bound assertion, and pass after restoration.
All selected outcomes are reported. --check-anchors is read-only. Full suite is
for the coordinator host; use at most 1-2 samples in a worker session.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot
from negative_suite import Case, classify, passed

root = Path(__file__).resolve().parents[5]
source = root / 'src/pilotdeck-gateway/src/runtime/model/http.rs'
errors = root / 'src/pilotdeck-gateway/src/runtime/model/http/errors.rs'
generator = root / 'src/pilotdeck-gateway/tests/fixtures/provider-http-boundaries/generate.mts'
cases = [
    ('no-proxy-bom', source, "character == ',' || crate::metadata::js_space(character)", "character == ',' || (crate::metadata::js_space(character) && character != '\\u{feff}')", 'runtime::model::http::boundary_proxy_tests::no_proxy_js_whitespace_over_tcp', '#860'),
    ('no-proxy-nel', source, "character == ',' || crate::metadata::js_space(character)", "character == ',' || crate::metadata::js_space(character) || character == '\\u{0085}'", 'runtime::model::http::boundary_proxy_tests::no_proxy_js_whitespace_over_tcp', '#860'),
    ('idle-zero', source, 'options.stream_timeout_ms.filter(|ms| *ms > 0.0)', 'options.stream_timeout_ms.filter(|ms| *ms >= 0.0)', 'runtime::model::http::boundary_proxy_tests::zero_idle_option_uses_provider_or_default', '#360'),
    ('idle-default', source, '_ => DEFAULT_STREAM_IDLE_TIMEOUT_MS,', '_ => DEFAULT_STREAM_IDLE_TIMEOUT_MS / 10.0,', 'runtime::model::http::boundary_proxy_tests::zero_idle_option_uses_provider_or_default', '#360'),
    ('tie-body-first', source, 'let body_first = body_idle < remaining;', 'let body_first = body_idle <= duration(idle_ms);', 'timer_contract_matches_node_outside_near_tie', '#411'),
    ('shorter-body-ignored', source, 'let body_first = body_idle < remaining;', 'let body_first = false;', 'timer_contract_matches_node_outside_near_tie', '#411'),
    ('repeat-equality', source, 'if self.repeated >= self.limit {', 'if self.repeated > self.limit {', 'repeated_chunk_boundaries_match_node', '#360'),
    ('repeat-disabled', source, 'if self.repeated >= self.limit {', 'if self.repeated >= self.limit + 100.0 {', 'repeated_chunk_boundaries_match_node', '#360'),
    ('repeat-short-utf16', source, 't.equals_str("text_delta") && text.len_utf16() > 2', 't.equals_str("text_delta") && text.len_utf16() >= 2', 'repeated_chunk_boundaries_match_node', '#360'),
    ('repeat-reset', source, 'self.last = None;', '// mutant: keep last text', 'repeated_chunk_boundaries_match_node', '#360'),
    ('duration-equality', source, 'Some(max) if now - self.started > max =>', 'Some(max) if now - self.started >= max =>', 'duration_boundaries_match_node', '#360'),
    ('duration-overrun', source, 'Some(max) if now - self.started > max =>', 'Some(max) if now - self.started > max + 1.0 =>', 'duration_boundaries_match_node', '#360'),
    ('tls-future', source, '=> "CERT_NOT_YET_VALID",', '=> "UNABLE_TO_VERIFY_LEAF_SIGNATURE",', 'certificate_causes_match_node_over_tls', '#360'),
    ('tls-purpose', source, '=> "INVALID_PURPOSE",', '=> "UNABLE_TO_VERIFY_LEAF_SIGNATURE",', 'certificate_causes_match_node_over_tls', '#360'),
    ('tls-signature', source, '"CERT_SIGNATURE_FAILURE"', '"UNABLE_TO_VERIFY_LEAF_SIGNATURE"', 'certificate_causes_match_node_over_tls', '#360'),
    ('tls-expired', source, '=> "CERT_HAS_EXPIRED",', '=> "UNABLE_TO_VERIFY_LEAF_SIGNATURE",', 'certificate_causes_match_node_over_tls', '#360'),
    ('tls-name', source, '=> "ERR_TLS_CERT_ALTNAME_INVALID",', '=> "UNABLE_TO_VERIFY_LEAF_SIGNATURE",', 'certificate_causes_match_node_over_tls', '#360'),
    ('token-range', errors, 'capture(range_max_tokens, l, 1)', 'capture(range_max_tokens, "", 1)', 'token_limit_branches_match_node', '#360'),
    ('token-available-underscore', errors, 'capture(available_underscore, l, 1)', 'capture(available_underscore, "", 1)', 'token_limit_branches_match_node', '#360'),
    ('token-available-space', errors, 'capture(available_space, l, 1)', 'capture(available_space, "", 1)', 'token_limit_branches_match_node', '#360'),
    ('token-greater-than', errors, 'capture(greater_than_context, l, 1)', 'capture(greater_than_context, "", 1)', 'token_limit_branches_match_node', '#360'),
    ('token-exceeds', errors, 'capture(exceeds_context, l, 1)', 'capture(exceeds_context, "", 1)', 'token_limit_branches_match_node', '#360'),
    ('token-at-most', errors, 'capture(at_most, l, 1)', 'capture(at_most, "", 1)', 'token_limit_branches_match_node', '#360'),
    ('token-completion-at-most', errors, 'capture(completion_at_most, l, 1)', 'capture(completion_at_most, "", 1)', 'token_limit_branches_match_node', '#360'),
    ('token-requested', errors, 'capture(requested_output, l, 1).is_some()', 'capture(requested_output, l, 1).is_none()', 'token_limit_branches_match_node', '#360'),
    ('token-prompt', errors, 'capture(prompt_contains, l, 1)', 'capture(prompt_contains, "", 1)', 'token_limit_branches_match_node', '#360'),
    ('token-input', errors, 'capture(input_tokens, l, 1)', 'capture(input_tokens, "", 1)', 'token_limit_branches_match_node', '#360'),
    ('token-output-portion', errors, 'output_portion().captures(l)', 'output_portion().captures("")', 'token_limit_branches_match_node', '#360'),
    ('token-context-limit', errors, '        ctx_limit,\n', '        ctx_maximum,\n', 'token_limit_branches_match_node', '#360'),
    ('token-context-model', errors, '        ctx_max_model_len,\n', '        ctx_maximum,\n', 'token_limit_branches_match_node', '#360'),
    ('token-context-window', errors, '        ctx_window,\n', '        ctx_maximum,\n', 'token_limit_branches_match_node', '#360'),
    ('token-context-length', errors, '        ctx_length,\n', '        ctx_maximum,\n', 'token_limit_branches_match_node', '#360'),
    ('token-context-chinese', errors, '        ctx_chinese,\n', '        ctx_maximum,\n', 'token_limit_branches_match_node', '#360'),
    ('token-zero-context-suppression', errors, 'if l.contains("max_tokens")\n        && (l.contains("available_tokens") || l.contains("range of max_tokens"))', 'if false && l.contains("max_tokens")\n        && (l.contains("available_tokens") || l.contains("range of max_tokens"))', 'token_limit_branches_match_node', '#360'),
    ('generator-wire-snapshot', generator, 'requests:structuredClone(seen),resolvedIdle:', 'requests:seen,resolvedIdle:', 'provider_http_boundaries_regenerate_identically', '#360'),
    ('body-anthropic-omitted', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("anthropic") && field(&final_body, "max_tokens").as_number() == Some(32.0) { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-anthropic-full', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("anthropic") && field(&final_body, "max_tokens").as_number() == Some(17.0) && is(field(field(&final_body, "tool_choice"), "type"), "auto") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-anthropic-none', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("anthropic") && field(&final_body, "max_tokens").as_number() == Some(17.0) && is(field(field(&final_body, "tool_choice"), "type"), "none") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-anthropic-required', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("anthropic") && field(&final_body, "max_tokens").as_number() == Some(17.0) && is(field(field(&final_body, "tool_choice"), "type"), "any") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-anthropic-empty', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("anthropic") && field(&final_body, "max_tokens").as_number() == Some(0.0) { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-openai-omitted', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("openai") && field(&final_body, "max_tokens").as_number() == Some(32.0) { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-openai-full', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("openai") && field(&final_body, "max_tokens").as_number() == Some(17.0) && is(field(&final_body, "tool_choice"), "auto") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-openai-none', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("openai") && field(&final_body, "max_tokens").as_number() == Some(17.0) && is(field(&final_body, "tool_choice"), "none") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-openai-required', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("openai") && field(&final_body, "max_tokens").as_number() == Some(17.0) && is(field(&final_body, "tool_choice"), "required") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-openai-empty', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("openai") && field(&final_body, "max_tokens").as_number() == Some(0.0) { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-openai-responses-omitted', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("openai-responses") && field(&final_body, "max_output_tokens").as_number() == Some(32.0) { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-openai-responses-full', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("openai-responses") && field(&final_body, "max_output_tokens").as_number() == Some(17.0) && is(field(&final_body, "tool_choice"), "auto") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-openai-responses-none', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("openai-responses") && field(&final_body, "max_output_tokens").as_number() == Some(17.0) && is(field(&final_body, "tool_choice"), "none") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-openai-responses-required', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("openai-responses") && field(&final_body, "max_output_tokens").as_number() == Some(17.0) && is(field(&final_body, "tool_choice"), "required") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-openai-responses-empty', source, 'let final_body = providers::final_body(body, provider);', 'let mut final_body = providers::final_body(body, provider);\n    if protocol(provider).equals_str("openai-responses") && field(&final_body, "max_output_tokens").as_number() == Some(0.0) { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-google-omitted', source, 'let final_body = providers::google::final_body(body, provider);', 'let mut final_body = providers::google::final_body(body, provider);\n    if protocol(provider).equals_str("google") && field(field(&final_body, "generationConfig"), "maxOutputTokens").as_number() == Some(32.0) { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-google-full', source, 'let final_body = providers::google::final_body(body, provider);', 'let mut final_body = providers::google::final_body(body, provider);\n    if protocol(provider).equals_str("google") && field(field(&final_body, "generationConfig"), "maxOutputTokens").as_number() == Some(17.0) && is(field(field(field(&final_body, "toolConfig"), "functionCallingConfig"), "mode"), "AUTO") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-google-none', source, 'let final_body = providers::google::final_body(body, provider);', 'let mut final_body = providers::google::final_body(body, provider);\n    if protocol(provider).equals_str("google") && field(field(&final_body, "generationConfig"), "maxOutputTokens").as_number() == Some(17.0) && is(field(field(field(&final_body, "toolConfig"), "functionCallingConfig"), "mode"), "NONE") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-google-required', source, 'let final_body = providers::google::final_body(body, provider);', 'let mut final_body = providers::google::final_body(body, provider);\n    if protocol(provider).equals_str("google") && field(field(&final_body, "generationConfig"), "maxOutputTokens").as_number() == Some(17.0) && is(field(field(field(&final_body, "toolConfig"), "functionCallingConfig"), "mode"), "ANY") { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('body-google-empty', source, 'let final_body = providers::google::final_body(body, provider);', 'let mut final_body = providers::google::final_body(body, provider);\n    if protocol(provider).equals_str("google") && field(field(&final_body, "generationConfig"), "maxOutputTokens").as_number() == Some(0.0) { final_body = JsValue::Null; }', 'request_body_branches_match_node', '#360'),
    ('token-context-maximum', errors, 'limit_pattern!(ctx_maximum, r"maximum context length(?: is|:)?\\s*([0-9]+)");', 'limit_pattern!(ctx_maximum, r"maximum context length(?: is|:)?\\s*([0-9])");', 'token_limit_branches_match_node', '#360'),
    ('token-estimate-clamp', errors, 'Some((context - prompt).max(1.0))', 'Some((context - prompt).max(0.0))', 'token_limit_branches_match_node', '#360'),
    ('token-portion-clamp', errors, 'available_output: Some((context - used).max(1.0)),', 'available_output: Some((context - used).max(0.0)),', 'token_limit_branches_match_node', '#360'),
    ('token-output-priority', errors, 'if max_output.is_some() || available_output.is_some() {', 'if (max_output.is_some() || available_output.is_some()) && !l.contains("limit 100 tokens") {', 'token_limit_branches_match_node', '#360'),
]


def main():
    if sys.argv[1:] == ['--check-anchors']:
        failures = []
        for name, path, old, _, _, _ in cases:
            count = path.read_text().count(old)
            if count != 1:
                failures.append(f'{name}: expected 1 anchor, got {count}')
        print('\n'.join(failures) if failures else f'{len(cases)} anchors OK')
        return int(bool(failures))
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('out', type=Path)
    parser.add_argument('mutations', nargs='*')
    args = parser.parse_args()
    env = os.environ.copy()
    if not env.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be provided by the caller')
    unknown = set(args.mutations) - {row[0] for row in cases}
    if unknown:
        parser.error(f'unknown mutations: {sorted(unknown)}')
    selected = [row for row in cases if not args.mutations or row[0] in args.mutations]
    args.out.mkdir(parents=True, exist_ok=False)
    results = []

    def run(label, test, compile_only=False):
        command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway']
        command += ['--lib'] if test.startswith('runtime::') else ['--test', 'provider_http_boundaries']
        try:
            slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=300)
            (args.out / f'{label}.slots.log').write_text(slot.stdout + slot.stderr)
            suffix = ['--no-run'] if compile_only else [test, '--', '--exact', '--include-ignored']
            result = run_owned(command + suffix, cwd=root, env=env, timeout=600)
            (args.out / f'{label}.log').write_text(result.stdout + result.stderr)
            return result
        except Exception as error:
            raw = ''
            if isinstance(error, subprocess.TimeoutExpired):
                for item in (error.stdout, error.stderr):
                    raw += item.decode(errors='replace') if isinstance(item, bytes) else item or ''
            with (args.out / f'{label}.log').open('a') as log:
                log.write(raw + f'\n{type(error).__name__}: {error}\n')
            raise

    def record(name, test, assertion, status, reason):
        results.append(dict(mutation=name, test=test, assertion=assertion, status=status, reason=reason))
        print(f'{name}: {status}: {reason}', flush=True)

    try:
        with mutating_sources(root, sorted({row[1] for row in selected})) as snapshots:
            for name, path, old, new, test, assertion in selected:
                status, reason = 'INVALID', 'not run'
                try:
                    original = snapshots[path].decode()
                    if original.count(old) != 1:
                        raise ValueError(f'{name}: expected one source anchor')
                    if not passed(run(name + '-positive', test), test):
                        reason = 'bound positive test did not pass'
                    else:
                        path.write_text(original.replace(old, new, 1))
                        if run(name + '-compile', test, compile_only=True).returncode != 0:
                            reason = 'mutant failed to compile'
                        else:
                            status, reason = classify(run(name + '-negative', test), Case(name, path, old, new, test, assertion))
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    path.write_bytes(snapshots[path])
                try:
                    if not passed(run(name + '-restored', test), test):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = 'INVALID', reason + f'; restored: {error}'
                record(name, test, assertion, status, reason)
    except Exception as error:
        reason = f'source guard: {error}'
        for row in results:
            row.update(status='INVALID', reason=row['reason'] + '; ' + reason)
        for name, _, _, _, test, assertion in selected[len(results):]:
            record(name, test, assertion, 'INVALID', reason)
    counts = {state: sum(row['status'] == state for row in results) for state in ('KILLED', 'SURVIVED', 'INVALID')}
    (args.out / 'summary.json').write_text(json.dumps(dict(controls=results, counts=counts), indent=2) + '\n')
    print(json.dumps(counts), flush=True)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
