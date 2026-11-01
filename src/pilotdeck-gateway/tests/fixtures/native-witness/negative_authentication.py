#!/usr/bin/env python3
"""#1140 authentication mutations; memory rehearsal or guarded post-commit run."""
import sys
if not sys.flags.isolated:
    raise SystemExit('run with python3 -I')
sys.dont_write_bytecode = True
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import re
import unittest
from unittest.mock import patch

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

native = root / 'scripts/gateway-parity/native_evidence.py'
session = root / 'scripts/gateway-parity/session_domain.py'
witness = root / 'scripts/native-witness/witness.py'
collector = root / 'scripts/native-witness/collect.py'
e2e = root / 'src/pilotdeck-gateway/tests/fixtures/native-session-e2e/test_e2e.py'
gateway = root / 'src/pilotdeck-gateway/tests/fixtures/native-session-e2e/mock_gateway.py'
common = root / 'scripts/gateway-parity/common.py'
comparison = root / 'scripts/gateway-parity/compare.py'
controls_script = root / 'scripts/gateway-parity/controls/run_all.py'
session_controls = root / 'scripts/gateway-parity/controls/session_domain.py'
cases = [
    ('omit-provider-output-turn_completed', native, "'turn_completed'", "'not_a_model_output'", 'test_provider_requirement_derived_from_expectations'),
    ('omit-provider-output-assistant_text_delta', native, "'assistant_text_delta'", "'not_a_model_output'", 'test_provider_requirement_derived_from_expectations'),
    ('omit-provider-output-assistant_thinking_delta', native, "'assistant_thinking_delta'", "'not_a_model_output'", 'test_provider_requirement_derived_from_expectations'),
    ('omit-provider-output-assistant_block', native, "'assistant_block'", "'not_a_model_output'", 'test_provider_requirement_derived_from_expectations'),
    ('omit-provider-output-model_stream_event', native, "'model_stream_event'", "'not_a_model_output'", 'test_provider_requirement_derived_from_expectations'),
    ('omit-provider-output-tool_calls_detected', native, "'tool_calls_detected'", "'not_a_model_output'", 'test_provider_requirement_derived_from_expectations'),
    ('omit-provider-output-tool_call_started', native, "'tool_call_started'", "'not_a_model_output'", 'test_provider_requirement_derived_from_expectations'),
    ('omit-provider-output-tool_call_completed', native, "'tool_call_completed'", "'not_a_model_output'", 'test_provider_requirement_derived_from_expectations'),
    ('require-provider-network-call', session, "not requests or provider['endpoint'] in targets", "provider['endpoint'] in targets", 'test_no_provider_traffic_is_scored'),
    ('require-collector-model-traffic', collector, '    w.native.provider_cases(requests, cases)', "    w.need(bool(requests), 'session capture saw no provider traffic')\n    w.native.provider_cases(requests, cases)", 'test_no_provider_traffic_is_scored'),
    ('require-all-provider-cases', session, 'required = native_evidence.required_provider_cases(document)', 'required = set(expected)', 'test_provider_optional_case_missing'),
    ('ignore-provider-expectations', native, "for frame in row['rust']['frames']", 'for frame in []', 'test_provider_requirement_derived_from_expectations'),
    ('constant-provider-requirements', native, "return {row['case'] for row in document['cases']\n            if any(frame.partition(':')[0] in output_types for frame in row['rust']['frames'])}", "return {cell + '/' + order + '/' + phase for cell in ('no-registry-fast', 'registry-fast') for order in ('unknown-first', 'known-final-first') for phase in ('model', 'tool')}", 'test_provider_requirement_derived_from_expectations'),
    ('ignore-provider-model-output', native, "frame.partition(':')[0] in output_types", "frame.partition(':')[0] == 'model_request_started'", 'test_provider_requirement_derived_from_expectations'),
    ('premature-manifest-complete', gateway, 'self.served_cases == set(self.expectations)', 'True', 'test_gateway_manifest_completion_matrix'),
    ('omit-manifest-incomplete-error', gateway, 'if not self.completed.is_set():', 'if False:', 'test_gateway_manifest_completion_matrix'),
    ('short-gateway-idle-timeout', gateway, 'connection.settimeout(180)', 'connection.settimeout(5)', 'test_gateway_manifest_completion_matrix'),
    ('leak-registry-value-error', native, "raise Incomplete('#1140 registry JSON malformed') from error", 'raise', 'test_registry_malformed_is_incomplete'),
    ('drop-observer-binding-shape', session, "    require(all(isinstance(binding, dict) for binding in bindings),\n            'BINDING', 'observer hello binding malformed')\n", '    pass\n', 'test_observer_binding_shape_require'),
    ('jsonl-drop-internal-blanks', native, "return lines[:-1] if lines[-1] == '' else lines", 'return [line for line in lines if line.strip()]', 'test_jsonl_shared_line_boundaries'),
    ('jsonl-shared-splitlines', native, "lines = text.split('\\n')", 'lines = text.splitlines()', 'test_jsonl_unicode_separators'),
    ('jsonl-frames-splitlines', comparison, 'jsonl_lines(raw)', 'raw.splitlines()', 'test_jsonl_validate_frames_and_upstream'),
    ('jsonl-upstream-splitlines', comparison, 'jsonl_lines(raw_upstream)', 'raw_upstream.splitlines()', 'test_jsonl_validate_frames_and_upstream'),
    ('jsonl-disk-splitlines', comparison, 'jsonl_lines(text)', 'text.splitlines()', 'test_jsonl_compare_disk_and_controls'),
    ('jsonl-index-splitlines', comparison, 'jsonl_lines(item.read_text())', 'item.read_text().splitlines()', 'test_jsonl_compare_disk_and_controls'),
    ('jsonl-control-rows-splitlines', controls_script, "jsonl_lines((folder / f'scenarios/{name}/frames.jsonl').read_text())", "(folder / f'scenarios/{name}/frames.jsonl').read_text().splitlines()", 'test_jsonl_compare_disk_and_controls'),
    ('jsonl-fingerprint-splitlines', controls_script, 'jsonl_lines(path.read_text())', 'path.read_text().splitlines()', 'test_jsonl_compare_disk_and_controls'),
    ('omit-capture-receipt-audit', native, "for name, key in receipt_artifacts('capture').items():", 'for name, key in []:', 'test_receipt_schema_is_shared'),
    ('omit-session-receipt-audit', session, "for name, key in native_evidence.receipt_artifacts('session-domain').items():", 'for name, key in []:', 'test_receipt_schema_is_shared'),
    ('copy-control-receipt-schema', session_controls, 'NATIVE_RECEIPT_KEYS = session_domain_module.native_evidence.NATIVE_RECEIPT_KEYS', 'NATIVE_RECEIPT_KEYS = dict(session_domain_module.native_evidence.NATIVE_RECEIPT_KEYS)', 'test_receipt_schema_is_shared'),
    ('omit-capture-receipt-production', witness, "for name, key in native.receipt_artifacts('capture').items()", 'for name, key in []', 'test_receipt_producers_follow_schema'),
    ('omit-session-receipt-production', collector, "for name, key in w.native.receipt_artifacts('session-domain').items()", 'for name, key in []', 'test_receipt_producers_follow_schema'),
    ('unknown-failed-exec', native, "need(tid in owners and tid not in ended, 'failed exec from untracked/exited task')", "need(True, 'failed exec from untracked/exited task')", 'test_unknown_task_failed_exec'),
    ('ended-failed-exec', native, "need(tid in owners and tid not in ended, 'failed exec from untracked/exited task')", "need(True, 'failed exec from untracked/exited task')", 'test_ended_task_failed_exec'),
    ('allow-pid-reuse', native, "need(child not in owners, 'PID reuse is unsupported')", "need(True, 'PID reuse is unsupported')", 'test_pid_reuse_rejected'),
    ('allow-nonleader-exec', native, "need(tid in owners and owners[tid] == tid, 'exec lacks fork lineage or is nonleader thread')", "need(True, 'exec lacks fork lineage or is nonleader thread')", 'test_nonleader_exec_rejected'),
    ('skip-successful-execveat', native, "            if result.startswith('-1 '):\n                need(tid in owners", "            if result.startswith('-1 ') or name == 'execveat':\n                need(tid in owners", 'test_successful_execveat_rejected'),
    ('allow-fragment', native, "wire.get('fin') is True", 'True', 'test_ws_fragment_rejected'),
    ('allow-rsv', native, "wire.get('rsv') == 0", 'True', 'test_ws_rsv_rejected'),
    ('allow-binary', native, "wire.get('opcode') == 1", 'True', 'test_ws_binary_rejected'),
    ('allow-server-mask', native, "wire.get('masked') is (direction == 'send')", 'True', 'test_ws_server_mask_rejected'),
    ('allow-client-unmasked', native, "wire.get('masked') is (direction == 'send')", 'True', 'test_ws_client_unmasked_rejected'),
    ('allow-direction', native, "direction in ('send', 'receive')", 'True', 'test_ws_unknown_direction_rejected'),
    ('omit-tracer-ownership', witness, '            ownership.add(owned)', '            pass', 'test_tracer_registered_before_exercise'),
    ('retain-reaped-tracer', witness, '                ownership.discard(owned)', '                pass', 'test_confirmed_tracer_provider_ownership_matrix'),
    ('drop-pending-tracer', witness, "                cleanup_errors.append('wait: ' + repr(cleanup_error))", "                ownership.discard(owned)\n                cleanup_errors.append('wait: ' + repr(cleanup_error))", 'test_unconfirmed_tracer_retains_lease_matrix'),
    ('mask-business-error', witness, "            error.add_note('#1140 failure publication: ' + repr(publish_error))", '            raise', 'test_failure_publication_preserves_original_error'),
    ('mask-provider-error', collector, "            original_error.add_note('#1140 provider cleanup: ' + repr(cleanup_error))", '            raise', 'test_provider_cleanup_preserves_business_error'),
    ('mask-provider-publication', collector, "                original_error.add_note('#1140 provider failure publication: ' + repr(publish_error))", '                raise', 'test_provider_publication_preserves_business_error'),
    ('skip-binary-bytes', native, "need(actual == binary['sha256'], 'executable bytes changed since prelaunch')", "need(True, 'executable bytes changed since prelaunch')", 'test_same_path_wrong_binary_rejected'),
    ('accept-synthetic', session, "require(launch.get('scope') != 'SYNTHETIC_CONTROL_ONLY'\n            and evidence.get('scope') != 'SYNTHETIC_CONTROL_ONLY', 'NATIVE_CAPTURE',", "require(True, 'NATIVE_CAPTURE',", 'test_formal_rejects_synthetic'),
    ('accept-failure-receipt', native, "need(not (folder / 'failure.json').exists(), 'capture carries a failure record')", "need(True, 'capture carries a failure record')", 'test_failure_receipt'),
    ('omit-formal-replay', session, 'evidence = native_evidence.checked_evidence(raw, launch, evidence)', 'evidence = evidence', 'test_omitted_short_descendant'),
    ('trust-formal-network', native, "network_equal = evidence['networkWitness'] == replay['networkWitness']", 'network_equal = True', 'test_omitted_network'),
    ('promote-incomplete', native, "checked['execWitness']['complete'] = evidence['execWitness']['complete'] is True and exec_equal", "checked['execWitness']['complete'] = True", 'test_failure_flags_not_promoted'),
    ('drop-provider-coverage', session, 'if case in required and case not in covered:', 'if False:', 'test_provider_case_missing'),
    ('loose-provider-marker', native, "markers = set(re.findall(r'\\[native-witness:([^\\]]+)\\]', '\\n'.join(texts)))\n        need(markers == {case}, 'provider request must have one case owner')\n        need(case in cases and f'[native-witness:{case}] controlled wait' in texts", "markers = set(re.findall(r'\\[native-witness:([^\\]]+)\\]', request['raw']))\n        need(markers == {case}, 'provider request must have one case owner')\n        need(case in cases and f'[native-witness:{case}] controlled wait' in request['raw']", 'test_provider_marker_wrong_field'),
    ('title-covers-case', native, "        if body.get('stream') is not True:\n            continue  # Title requests cannot supply a model case witness.", '        if False:\n            continue', 'test_provider_title_not_model'),
    ('trust-case-observation', native, "need(observation == original, 'case observation disagrees with retained originals')", "need(True, 'case observation disagrees with retained originals')", 'test_cross_case_frames'),
    ('ignore-registry-project', native, "(record.get('projectKey'), record.get('sessionKey'), record.get('runId')) == (project, session, run)", "(record.get('sessionKey'), record.get('runId')) == (session, run)", 'test_cross_project_registry'),
    ('allow-duplicate-run', native, "need(len(records) <= 1, 'duplicate full run key')", "need(True, 'duplicate full run key')", 'test_duplicate_registry'),
    ('ignore-project-marker', native, "source['projectMarkerText'].strip() == project", 'True', 'test_cross_project_transcript'),
    ('ignore-actual-marker', native, "need(path in paths, 'transcript marker is not the actual case transcript')", "need(True, 'transcript marker is not the actual case transcript')", 'test_decoy_marker'),
    ('allow-failed-marker', native, "if marker is None or result.startswith('-1 ') or owners[tid] != root:", "if marker is None or owners[tid] != root:\n                continue\n            result = '5' if result.startswith('-1 ') else result\n            if False:", 'test_failed_marker'),
    ('skip-frozen-before-deadline', session, "if report['frozen']['exitCode'] != 0:\n                        raise ApparatusVerificationError('FROZEN: deadline requires successful frozen check')", "if False:\n                        raise ApparatusVerificationError('FROZEN: deadline requires successful frozen check')", 'test_deadline_frozen_failure'),
    ('retain-invalid-cells', session, "report.pop('cells', None)  # No consumable partial score after failed final verification.", 'None', 'test_final_frozen_failure'),
    ('skip-empty-trace-replay', session, 'evidence = native_evidence.checked_evidence(raw, launch, evidence)', 'evidence = evidence', 'test_empty_trace'),
    ('skip-case-chain', session, 'paths = native_evidence.session_chain(contract, rows, observer, provider, driver, sources, expected)', "paths = [row['identity']['transcriptPath'] for row in rows]", 'test_hello_only'),
    ('allow-profile-roles', native, "Counter(contract.get('processRoles', [])) == Counter(['host'] + ['sidecar'] * len(cases))\n         and set(contract.get('binaries', {})) == {'host', 'sidecar'}", 'True', 'test_profile_helper'),
    ('allow-profile-endpoint', native, "contract.get('externalEndpoints') == [endpoint]", 'True', 'test_profile_endpoint'),
    ('allow-profile-connections', native, "contract.get('connections') == 1", 'True', 'test_profile_connections'),
    ('misattribute-observer-event', native, "request_id = frame.get('id')", "request_id = next(reversed(submitted)) if row['direction'] == 'receive' and submitted else frame.get('id')", 'test_observer_wrong_request'),
    ('ignore-submitted-identity', native, "all(identity[k] == params.get(k) for k in ('projectKey', 'sessionKey', 'runId'))", 'True', 'test_synchronized_old_identity'),
    ('ignore-driver-identity', native, "need(identity == anchor, 'driver and case identity disagree')\n        params = submitted[identity['requestId']]", "identity = anchor\n        params = submitted[identity['requestId']]", 'test_old_case_identity'),
    ('allow-inapplicable-records', session, "if registry['applicable'] is False and (registry.get('events') or registry.get('record')):", 'if False:', 'test_inapplicable_records'),
    ('accept-formal-fragment', native, "wire.get('fin') is True", 'True', 'test_formal_ws_profile'),
    ('allow-balanced-decoy', native, "need(path in paths, 'transcript marker is not the actual case transcript')", "need(True, 'transcript marker is not the actual case transcript')", 'test_balanced_decoy'),
    ('trust-missing-binary', native, "raise BinaryUnavailable('#1140 BINARY_UNAVAILABLE: restore original absolute path ' + binary['path']) from error", 'continue', 'test_original_binary_path_required'),
    ('skip-wait-after-kill-failure', witness, 'tracer.wait(timeout=5)', 'None if cleanup_errors else tracer.wait(timeout=5)', 'test_kill_error_still_waits'),
    ('ignore-frozen-manifest', session, "if report['frozen']['exitCode'] != 0:\n                        raise ApparatusVerificationError('FROZEN: deadline requires successful frozen check')", "if False:\n                        raise ApparatusVerificationError('FROZEN: deadline requires successful frozen check')", 'test_unregistered_manifest_blocks_deadline'),
    ('ignore-frozen-expectation', session, "if report['frozen']['exitCode'] != 0:\n                        raise ApparatusVerificationError('FROZEN: deadline requires successful frozen check')", "if False:\n                        raise ApparatusVerificationError('FROZEN: deadline requires successful frozen check')", 'test_unregistered_expectation_blocks_deadline'),
    ('allow-provider-multi-owner', native, "need(markers == {case}, 'provider request must have one case owner')", "need(True, 'provider request must have one case owner')", 'test_provider_multiple_case_owners'),
    ('allow-null-run', native, "all(isinstance(identity.get(k), str) and bool(identity[k])\n                 for k in ('requestId', 'projectKey', 'sessionKey', 'runId', 'transcriptPath'))", 'True', 'test_null_run_identity'),
    ('ignore-event-envelope', native, "frame.get('type') == 'event'", 'True', 'test_final_envelope'),
    ('ignore-event-sequence', native, "frame['seq'] == len(received[request_id])", 'True', 'test_event_sequence'),
    ('binary-unavailable-not-blocked', session, '    except native_evidence.BinaryUnavailable as error:\n        raise ApparatusVerificationError(str(error)) from error', "    except native_evidence.BinaryUnavailable as error:\n        raise EvidenceError('NATIVE_CAPTURE', str(error)) from error", 'test_missing_binary_is_blocked'),
    ('retain-successful-tracer', witness, '            result = tracer.wait(timeout=max(0.001, deadline - time.monotonic()))\n            ownership.discard(owned)', '            result = tracer.wait(timeout=max(0.001, deadline - time.monotonic()))\n            pass', 'test_successful_capture_releases_tracer'),
    ('allow-scm-rights', native, "and 'msg_controllen=0' in args", 'and True', 'test_scm_rights_rejected'),
    ('unicode-splitlines', native, "jsonl_lines(source['transcriptText'])", "source['transcriptText'].splitlines()", 'test_jsonl_unicode_separators'),
    ('leak-jsonl-value-error', native, "raise Incomplete('#1140 transcript JSONL malformed') from error", 'raise', 'test_jsonl_malformed_is_incomplete'),
    ('omit-a2-hashes', session, 'return {**apparatus_hashes(), **{p.relative_to(ROOT).as_posix(): digest(p) for p in paths}}', 'return {p.relative_to(ROOT).as_posix(): digest(p) for p in paths}', 'test_hashes_cover_a2'),
    ('stale-provider-pending', collector, "                    failure['cleanupPending'] = sorted(common.LIVE_GROUPS)\n                    temporary", "                    failure['cleanupPending'] = failure['cleanupPending']\n                    temporary", 'test_incomplete_provider_cleanup_refreshes_receipt'),
    ('provider-probe-no-reuse', e2e, '            provider_guard.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)', '            pass', 'test_consecutive_collections_reuse_port_pair'),
    ('leak-failed-provider-guard', e2e, '            provider_guard.close()\n            gateway_guard.close()', '            gateway_guard.close()', 'test_failed_reservation_closes_both_sockets'),
    ('leak-active-gateway', gateway, 'owned_sockets = (self.listener, self.connection)', 'owned_sockets = (self.listener,)' , 'test_gateway_cancel_unconnected_and_partial_frame'),
    ('skip-exception-provider-cleanup', collector, '        try:\n            stop_provider()\n        except BaseException as cleanup_error:', '        try:\n            pass\n        except BaseException as cleanup_error:', 'test_collection_exception_still_cleans_up'),
    ('swallow-gateway-close-failure', e2e, 'if not gateway.close(30):', 'if not gateway.close(30) and False:', 'test_failed_gateway_close_is_asserted'),
]
messages = {
    'omit-provider-output-turn_completed': '#1140 r10 provider requirement follows expectation frame changes',
    'omit-provider-output-assistant_text_delta': '#1140 r10 provider requirement follows expectation frame changes',
    'omit-provider-output-assistant_thinking_delta': '#1140 r10 provider requirement follows expectation frame changes',
    'omit-provider-output-assistant_block': '#1140 r10 provider requirement follows expectation frame changes',
    'omit-provider-output-model_stream_event': '#1140 r10 provider requirement follows expectation frame changes',
    'omit-provider-output-tool_calls_detected': '#1140 r10 provider requirement follows expectation frame changes',
    'omit-provider-output-tool_call_started': '#1140 r10 provider requirement follows expectation frame changes',
    'omit-provider-output-tool_call_completed': '#1140 r10 provider requirement follows expectation frame changes',
    'require-provider-network-call': '#1140 r10 zero model traffic reaches per-case scoring',
    'require-collector-model-traffic': '#1140 r10 zero model traffic reaches per-case scoring',
    'require-all-provider-cases': '#1140 r10 non-model expectations do not require provider requests',
    'ignore-provider-expectations': '#1140 r10 provider requirement follows expectation frame changes',
    'constant-provider-requirements': '#1140 r10 provider requirement follows expectation frame changes',
    'ignore-provider-model-output': '#1140 r10 provider requirement follows expectation frame changes',
    'premature-manifest-complete': '#1140 r10 gateway success requires the whole manifest',
    'omit-manifest-incomplete-error': '#1140 r10 incomplete manifest records an error',
    'short-gateway-idle-timeout': '#1140 r10 gateway idle read retains 180 second allowance',
    'leak-registry-value-error': '#1140 r10 malformed registry is controlled Incomplete',
    'drop-observer-binding-shape': '#1140 r10 observer binding shape directly rejected',
    'jsonl-drop-internal-blanks': '#1140 r10 JSONL retains internal blanks and only drops terminal LF',
    'jsonl-shared-splitlines': '#1140 r9 JSONL splits only LF and retains Unicode text',
    'jsonl-frames-splitlines': '#1140 r10 recorder and upstream JSONL preserve Unicode text',
    'jsonl-upstream-splitlines': '#1140 r10 recorder and upstream JSONL preserve Unicode text',
    'jsonl-disk-splitlines': '#1140 r10 compare and controls preserve JSONL Unicode text',
    'jsonl-index-splitlines': '#1140 r10 compare and controls preserve JSONL Unicode text',
    'jsonl-control-rows-splitlines': '#1140 r10 compare and controls preserve JSONL Unicode text',
    'jsonl-fingerprint-splitlines': '#1140 r10 compare and controls preserve JSONL Unicode text',
    'omit-capture-receipt-audit': '#1140 r10 receipt producers and consumers use one schema',
    'omit-session-receipt-audit': '#1140 r10 receipt producers and consumers use one schema',
    'copy-control-receipt-schema': '#1140 r10 receipt producers and consumers use one schema',
    'omit-capture-receipt-production': '#1140 r10 capture and session producers follow shared receipt schema',
    'omit-session-receipt-production': '#1140 r10 capture and session producers follow shared receipt schema',
    'swallow-gateway-close-failure': '#1140 r9 failed gateway cleanup cannot be swallowed by classmethod',
    'provider-probe-no-reuse': '#1140 r9 consecutive collections release the same port pair',
    'leak-failed-provider-guard': '#1140 r9 failed reservation closes both guards',
    'leak-active-gateway': '#1140 r9 gateway cancellation stops listener and active socket',
    'skip-exception-provider-cleanup': '#1140 r9 collection failure leaves no owned socket or thread',
    'unicode-splitlines': '#1140 r9 JSONL splits only LF and retains Unicode text',
    'leak-jsonl-value-error': '#1140 r9 malformed JSONL is controlled Incomplete',
    'omit-a2-hashes': '#1140 r9 E1 hashes include canonical A2 dependencies',
    'stale-provider-pending': '#1140 r9 provider cleanup finishes before lease release and refreshes pending',
    'unknown-failed-exec': '#1140 r8 unknown task failed exec rejected',
    'ended-failed-exec': '#1140 r8 ended task failed exec rejected',
    'allow-pid-reuse': '#1140 r8 PID reuse rejected',
    'allow-nonleader-exec': '#1140 r8 nonleader successful exec rejected',
    'skip-successful-execveat': '#1140 r8 successful execveat rejected',
    'allow-fragment': '#1140 r8 fragmented hello profile rejected',
    'allow-rsv': '#1140 r8 RSV hello profile rejected',
    'allow-binary': '#1140 r8 binary frame profile rejected',
    'allow-server-mask': '#1140 r8 masked server hello profile rejected',
    'allow-client-unmasked': '#1140 r8 unmasked client frame profile rejected',
    'allow-direction': '#1140 r8 unknown direction profile rejected',
    'omit-tracer-ownership': '#1140 r8 tracer registered before exercise',
    'retain-reaped-tracer': '#1140 r8 confirmed tracer removes only its ownership',
    'drop-pending-tracer': '#1140 r8 unconfirmed tracer retains ownership',
    'mask-business-error': '#1140 r8 failure publication preserves original error',
    'mask-provider-error': '#1140 r8 provider cleanup preserves original business error',
    'mask-provider-publication': '#1140 r8 provider publication preserves original business error',
    'skip-binary-bytes': '#1140 r8 same path changed executable bytes rejected',
    'accept-synthetic': '#1140 r8 formal rejects synthetic',
    'accept-failure-receipt': '#1140 r8 failure plus receipt rejected',
    'omit-formal-replay': '#1140 r8 formal replay catches omitted descendant',
    'trust-formal-network': '#1140 r8 formal replay catches omitted network',
    'promote-incomplete': '#1140 r8 incomplete flags never promoted',
    'drop-provider-coverage': '#1140 r10 missing required provider request is a per-case FAIL',
    'loose-provider-marker': '#1140 r8 provider marker must be user message',
    'title-covers-case': '#1140 r8 title cannot cover model case',
    'trust-case-observation': '#1140 r8 swapped frames disagree with observer',
    'ignore-registry-project': '#1140 r8 registry uses full project session run key',
    'allow-duplicate-run': '#1140 r8 duplicate full run key rejected',
    'ignore-project-marker': '#1140 r8 transcript must belong to requested project',
    'ignore-actual-marker': '#1140 r8 decoy marker cannot attribute sidecar',
    'allow-failed-marker': '#1140 r8 failed open cannot attribute sidecar',
    'skip-frozen-before-deadline': '#1140 r8 frozen failure blocks before scoring',
    'retain-invalid-cells': '#1140 r8 final frozen failure revokes cells',
    'skip-empty-trace-replay': '#1140 r8 empty raw trace rejected',
    'skip-case-chain': '#1140 r8 hello alone cannot cover cases',
    'allow-profile-roles': '#1140 r8 contract cannot expand frozen roles',
    'allow-profile-endpoint': '#1140 r8 contract cannot expand provider endpoints',
    'allow-profile-connections': '#1140 r8 frozen profile fixes connection count',
    'misattribute-observer-event': '#1140 r8 observer event requires active request',
    'ignore-submitted-identity': '#1140 r8 driver case cannot drift from submitted run',
    'ignore-driver-identity': '#1140 r8 stale case identity rejected',
    'allow-inapplicable-records': '#1140 r8 inapplicable shape rejects record',
    'accept-formal-fragment': '#1140 r8 formal loader rejects consistent fragmented hello',
    'allow-balanced-decoy': '#1140 r8 balanced allocation plus decoy rejected',
    'trust-missing-binary': '#1140 r8 original binary path required despite relocated copy',
    'skip-wait-after-kill-failure': '#1140 r8 kill failure still waits for tracer',
    'ignore-frozen-manifest': '#1140 r8 unregistered fixtures/session-calibration.json blocks deadline',
    'ignore-frozen-expectation': '#1140 r8 unregistered fixtures/session-expectations.json blocks deadline',
    'allow-provider-multi-owner': '#1140 r8 provider body cannot belong to multiple cases',
    'allow-null-run': '#1140 r8 null run is not an execution identity',
    'ignore-event-envelope': '#1140 r8 final must have event envelope',
    'ignore-event-sequence': '#1140 r8 event sequence must be contiguous',
    'binary-unavailable-not-blocked': '#1140 r8 absent binary is controlled BLOCKED prerequisite',
    'retain-successful-tracer': '#1140 r8 successful capture releases tracer ownership',
    'allow-scm-rights': '#1140 r8 SCM_RIGHTS egress rejected',
}


def bound(test):
    # Test files have disjoint method names; resolve the full name before run.
    for filename, classname in (('test_authentication.py', 'AuthenticationTests'), ('test_chain.py', 'ChainTests'),
                                ('../native-session-e2e/test_e2e.py', 'LifecycleTests')):
        path = (Path(__file__).parent / filename).resolve()
        if '    def ' + test + '(' in path.read_text():
            return path, classname + '.' + test
    raise ValueError('unknown bound test ' + test)


def preflight():
    errors = []
    for name, path, old, new, test in cases:
        source = path.read_text()
        try:
            if source.count(old) != 1:
                raise ValueError('anchor count ' + str(source.count(old)))
            compile(source.replace(old, new, 1), str(path), 'exec')
            bound(test)
        except Exception as error:
            errors.append(name + ': ' + str(error))
    print(json.dumps({'compiled': len(cases) - len(errors), 'errors': errors}))
    return not errors


def memory_run(name):
    row = next(row for row in cases if row[0] == name)
    _, path, old, new, test = row
    changed = path.read_bytes().replace(old.encode(), new.encode(), 1)
    compile(changed, str(path), 'exec')
    original_read = Path.read_bytes
    original_code = importlib.machinery.SourceFileLoader.get_code
    def read(p):
        return changed if p.resolve() == path else original_read(p)
    def code(loader, fullname):
        return compile(changed, str(path), 'exec') if Path(loader.path).resolve() == path else original_code(loader, fullname)
    fixture, full_name = bound(test)
    with patch.object(Path, 'read_bytes', read), patch.object(importlib.machinery.SourceFileLoader, 'get_code', code):
        spec = importlib.util.spec_from_file_location('bound_tests', fixture)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        suite = unittest.defaultTestLoader.loadTestsFromName(full_name, module)
        result = unittest.TextTestRunner(verbosity=2).run(suite)
    return 0 if result.wasSuccessful() else 1


def classify(result, test, marker):
    log = result.stdout + result.stderr
    if result.returncode == 0 and re.search(r'^OK$', log, re.M):
        return 'SURVIVED'
    # Only the named test's own assertion is a kill; unrelated errors/timeouts
    # and setup failures stay INVALID. unittest subtests may yield many rows.
    if (result.returncode == 1 and re.search(r'^FAIL: ' + re.escape(test) + r' \(', log, re.M)
            and re.search(r'^AssertionError:[^\n]*' + re.escape(marker), log, re.M)
            and 'ERROR:' not in log and 'Ran 1 test in ' in log):
        return 'KILLED'
    return 'INVALID'


def main():
    if sys.argv[1:2] == ['--memory-run']:
        return memory_run(sys.argv[2])
    if not preflight():
        return 2
    if sys.argv[1:] == ['--check-anchors']:
        return 0
    arguments = sys.argv[1:]
    memory = '--memory' in arguments
    arguments = [a for a in arguments if a != '--memory']
    if len(arguments) not in (1, 3) or (len(arguments) == 3 and arguments[1] != '--case'):
        raise SystemExit('usage: negative_authentication.py NEW_OUTPUT [--memory] [--case NAME] | --check-anchors')
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by caller')
    selected = cases if len(arguments) == 1 else [row for row in cases if row[0] == arguments[2]]
    if not selected:
        raise SystemExit('unknown mutation')
    out = Path(arguments[0]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    results = []
    def run(label, command):
        value = run_owned(command, cwd=root, env=os.environ.copy(), timeout=60)
        (out / (label + '.log')).write_text(value.stdout + value.stderr)
        return value
    def one(row, restore=False):
        name, path, old, new, test = row
        status, reason = 'INVALID', 'not run'
        fixture, full = bound(test)
        command = [sys.executable, '-I', '-B', str(fixture), full]
        try:
            changed = path.read_text().replace(old, new, 1)
            compile(changed, str(path), 'exec')
            positive = run(name + '-positive', command)
            if positive.returncode != 0 or 'Ran 1 test in ' not in positive.stderr:
                raise ValueError('bound positive failed')
            if memory:
                result = run(name + '-mutant', [sys.executable, '-I', '-B', __file__, '--memory-run', name])
            else:
                path.write_text(changed)
                result = run(name + '-mutant', command)
            status = classify(result, test, messages[name])
            reason = 'compiled; ' + full + '; ' + messages[name]
        except Exception as error:
            reason = repr(error)
        finally:
            if restore:
                path.write_bytes(originals[path])
        try:
            restored = run(name + '-restored', command)
            if restored.returncode != 0:
                status, reason = 'INVALID', 'restored bound test failed'
        except Exception as error:
            status, reason = 'INVALID', 'restored run failed: ' + repr(error)
        results.append({'name': name, 'status': status, 'reason': reason})
    try:
        if memory:
            for row in selected:
                one(row)
        else:
            paths = [native, session, witness, collector, common, comparison, controls_script, session_controls, e2e, gateway, Path(__file__),
                     Path(__file__).with_name('test_authentication.py'), Path(__file__).with_name('test_chain.py')]
            with mutating_sources(root, paths) as originals:
                for row in selected:
                    one(row, True)
    except Exception as error:
        results = [{'name': row[0], 'status': 'INVALID', 'reason': 'guard: ' + repr(error)} for row in selected]
    summary = {'mode': 'memory-only' if memory else 'guarded-source', 'results': results,
               'counts': {key: sum(row['status'] == key for row in results) for key in ('KILLED', 'SURVIVED', 'INVALID')}}
    temporary = out / 'summary.json.tmp'
    temporary.write_text(json.dumps(summary, indent=2) + '\n')
    temporary.replace(out / 'summary.json')
    print(json.dumps(summary, indent=2))
    return 0 if len(results) == len(selected) and all(row['status'] == 'KILLED' for row in results) else 1


if __name__ == '__main__':
    raise SystemExit(main())
