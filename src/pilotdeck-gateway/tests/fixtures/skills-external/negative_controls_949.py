#!/usr/bin/env python3
"""#949 negative controls for the five #929 tests and the credentialEnv success path.

Uses the #946 nonce-receipt matrix (``negative_suite.matrix_main``): every
mutant is bound to one exact assertion identity, the whole matrix runs to
completion, and any non-KILLED row exits non-zero. The legacy
``negative_controls.py`` still owns the earlier #929 controls.

The source tree must equal ``HEAD``; commit the patch before running:

  CARGO_TARGET_DIR=/path/to/target python3 \
    src/pilotdeck-gateway/tests/fixtures/skills-external/negative_controls_949.py \
    _tmp/<date>-issue-949/negative-controls
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import matrix_main

ROOT = Path(__file__).resolve().parents[5]
EXTERNAL = ROOT / 'src/pilotdeck-gateway/src/skills/external.rs'
BINDING = ROOT / 'src/pilotdeck-gateway/src/skills/binding.rs'
PREFIX = 'skills::external_tests::'

MUTATIONS = [
    # 1. external_manifest_is_loaded_once_per_port: break the shared flight so
    # two concurrent callers each own a manifest GET.
    ('manifest-flight-not-shared', EXTERNAL,
     '''        if let Some(flight) = &state.flight {
            return ManifestStep::Wait(flight.clone());
        }''',
     '''        if let Some(flight) = &state.flight {
            let _ = flight;
        }''',
     'external_manifest_is_loaded_once_per_port',
     '#949 manifest cache: external-manifest-is-loaded-once-per-port'),
    # 2. snapshot_projection_applies_the_optional_chain: only the null branch
    # changes, so the earlier missing-modules assertion still passes.
    ('snapshot-null-chain-returns-null', BINDING,
     '''    if matches!(modules, JsValue::Undefined | JsValue::Null) {
        return Ok(JsValue::Undefined);
    }''',
     '''    if matches!(modules, JsValue::Null) {
        return Ok(JsValue::Null);
    }''',
     'snapshot_projection_applies_the_optional_chain',
     '#949 snapshot chain: snapshot-projection-applies-the-optional-chain'),
    # 3. facade_replays_the_ts_list_success_case: bypass the external port.
    ('facade-external-routed-to-native', BINDING,
     '''            SkillsBinding::External => {
                let binding = ExternalSkillBinding::from_value(&binding)?;
                ExternalSkillsPort::new(binding, self.external.clone())
                    .call(method, input)
                    .await
            }''',
     '            SkillsBinding::External => self.native.call(method, input),',
     'facade_replays_the_ts_list_success_case',
     '#949 facade replay: facade-replays-the-ts-list-success-case'),
    # 4. hyper_transport_reaches_a_real_module_host: the production transport
    # succeeds but drops the response body, so the protocol parse fails.
    ('hyper-transport-drops-body', EXTERNAL,
     '                Ok(Ok(body)) => body,',
     '''                Ok(Ok(body)) => {
                    let _ = body;
                    Vec::new()
                }''',
     'hyper_transport_reaches_a_real_module_host',
     '#949 real transport: hyper-transport-reaches-a-real-module-host'),
    # 5. panicked_owner_before_waiter_notification_is_recovered: keep the
    # cancellation path working and only skip the terminal state on unwind.
    ('panic-window-skips-abandoned', EXTERNAL,
     '        self.flight.settle(ManifestOutcome::Abandoned);',
     '''        if !std::thread::panicking() {
            self.flight.settle(ManifestOutcome::Abandoned);
        }''',
     'panicked_owner_before_waiter_notification_is_recovered',
     '#949 panic window: panicked-owner-before-waiter-notification-is-recovered'),
    # 6. credentialEnv success path: the port must send the Bearer header.
    ('credential-not-sent', EXTERNAL,
     '''        match credential {
            Some(value) => Ok(Some(format!("Bearer {value}"))),''',
     '''        match credential {
            Some(value) => {
                let _ = value;
                Ok(None)
            }''',
     'external_credential_env_value_is_sent_as_bearer',
     '#949 credential success: external-credential-env-value-is-sent-as-bearer'),
]


def main():
    return matrix_main(ROOT, __file__, PREFIX, MUTATIONS,
                       passed_count=1, child_count=0,
                       paths=[EXTERNAL, BINDING], exact=True)


if __name__ == '__main__':
    raise SystemExit(main())
