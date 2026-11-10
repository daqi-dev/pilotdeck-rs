#!/usr/bin/env python3
"""#1343 HEAD-only channel-bluebubbles controls; compile each selected mutant before judging it.

OUT [mutation ...], --compile-only OUT, --check (also --check-anchors).
The shared runner owns source restoration, offline cargo, nonce receipts,
patch evidence, pending registrations and the complete result summary.

The whole `channel/bluebubbles/` tree and this fixture directory are new
files of #1343, and `channel/loader.rs` / `channel/tests.rs` / `channel/mod.rs`
/ `channel/http_client.rs` are its edits, so the matrix cannot run until the
patch has landed in HEAD (`mutating_sources` refuses uncommitted product
files); the script ships with the patch and is executed by the coordinator
afterwards on the landed tree. The prefix is `channel::` (not
`channel::bluebubbles::`) because the reject-table row binds a
`channel::tests` cell; every row is exact, so only its declared cells run.
"""
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_suite import matrix_main

root = Path(__file__).resolve().parents[5]
fixtures = Path(__file__).resolve().parents[1]
source = root / 'src/pilotdeck-gateway/src/channel/bluebubbles'
tests = source / 'tests.rs'
GOLDEN = 'bluebubbles::tests::channel_matches_real_ts'
MUTATIONS = [
    # Dropping the seen-set membership test re-dispatches every duplicate
    # guid; the duplicate-guid case records a second submit + turnEnd.
    ('seen-dedupe-removed', source / 'mod.rs',
     '                !guid.is_empty() && !state.seen_guids.insert(guid.clone())\n',
     '                state.seen_guids.insert(guid.clone());\n                false\n',
     GOLDEN, '#1343 case duplicate-guid-never-re-executes matches the TS oracle'),
    # Inverting the fromMe gate skips every normal row and lets the two
    # self-sent rows through; the first case with a dispatch is the first
    # cell to notice.
    ('fromme-gate-inverted', source / 'mod.rs',
     '        if from_me {\n'
     '            return;\n'
     '        }\n',
     '        if !from_me {\n'
     '            return;\n'
     '        }\n',
     GOLDEN, '#1343 case first-poll-hello-reply matches the TS oracle'),
    # Never advancing `lastTimestamp` keeps the second poll on the start-seeded
    # `after`, so the response-shapes case's `after=1900000300` query never
    # appears in the deduplicated pollQueries projection.
    ('timestamp-advance-dropped', source / 'mod.rs',
     '        if max_ts > state.last_timestamp {\n',
     '        if false {\n',
     GOLDEN, '#1343 case response-shapes-and-timestamp-advance matches the TS oracle'),
    # The `/new` acknowledgement must still post; dropping it costs the
    # new-command case its first send.
    ('new-command-reply-removed', source / 'mod.rs',
     '            Outcome::NewChat => {\n'
     '                self.send_reply(&chat_guid, &JsString::from("已创建新会话。"))\n'
     '                    .await;\n'
     '            }\n',
     '            Outcome::NewChat => {}\n',
     GOLDEN, '#1343 case new-command-session-mapper matches the TS oracle'),
    # `sendReply`'s `running === false` gate is what keeps a turn that
    # finished after stop off the wire (diff 003's second half).
    ('send-reply-running-gate-removed', source / 'mod.rs',
     '        if !self.running.load(std::sync::atomic::Ordering::SeqCst) {\n'
     '            return false;\n'
     '        }\n',
     '        if false {\n'
     '            return false;\n'
     '        }\n',
     'bluebubbles::tests::send_reply_after_stop_is_gated_by_running',
     '#1343 a reply after stop never reaches the wire'),
    # Every poll query carries `limit=<MESSAGE_LIMIT>`; the first case with a
    # query cell compares it byte for byte.
    ('message-limit-changed', source / 'mod.rs',
     'const MESSAGE_LIMIT: u32 = 50;',
     'const MESSAGE_LIMIT: u32 = 51;',
     GOLDEN, '#1343 case first-poll-hello-reply matches the TS oracle'),
    # #2285 ①: `sendReply`'s `AbortSignal.timeout(120_000)` is the semantic
    # sibling of MESSAGE_LIMIT; moving the deadline off the TS literal must
    # fail the constant pin.
    ('send-timeout-changed', source / 'mod.rs',
     'const SEND_TIMEOUT: Duration = Duration::from_millis(120_000);',
     'const SEND_TIMEOUT: Duration = Duration::from_millis(120_001);',
     'bluebubbles::tests::send_timeout_is_the_ts_abort_deadline',
     '#2285 the reply POST keeps the TS AbortSignal.timeout(120_000) deadline'),
    # #2285 ②: the poll driver holds the runtime only weakly, so a channel
    # dropped without stop ends it; re-arming a strong reference (the TS
    # leak shape) keeps the dropped channel polling forever.
    ('driver-weak-reverted', source / 'mod.rs',
     '        let driver = tokio::spawn(async move {\n'
     '            let mut timer = tokio::time::interval(POLL_MS);\n',
     '        let driver = tokio::spawn(async move {\n'
     '            let _hold = weak.upgrade();\n'
     '            let mut timer = tokio::time::interval(POLL_MS);\n',
     'bluebubbles::tests::poll_driver_exits_when_runtime_is_dropped',
     '#2285 dropping a started channel without stop ends the poll driver '
     '(the server keeps seeing polls after the drop)'),
    # The poll GET's only bound is the undici default 300 s deadline
    # (measured 301,257 ms on Node v22.22.0); unwiring that deadline must
    # fail the silent-server behavioral pin.
    ('unbounded-timeout-dropped',
     root / 'src/pilotdeck-gateway/src/channel/http_client.rs',
     '        match tokio::time::timeout(\n'
     '            UNDICI_DEFAULT_TIMEOUT,\n',
     '        match tokio::time::timeout(\n'
     '            std::time::Duration::from_secs(100000),\n',
     'http_client::tests::request_unbounded_settles_a_silent_server_at_the_undici_default',
     '#1343 a silent server settles the no-signal poll GET at the undici default'),
    # #2285 r4 ④: a deadline drifting into the (300 s, 400 s) blind window
    # still settles the outcome assertion; only the elapsed pin kills it.
    ('unbounded-timeout-drift-350s',
     root / 'src/pilotdeck-gateway/src/channel/http_client.rs',
     '        match tokio::time::timeout(\n'
     '            UNDICI_DEFAULT_TIMEOUT,\n',
     '        match tokio::time::timeout(\n'
     '            std::time::Duration::from_secs(350),\n',
     'http_client::tests::request_unbounded_settles_a_silent_server_at_the_undici_default',
     '#1343 the settle deadline is the undici default, not a later bound'),
    # #2285 r4 ④: the folded constant pin — moving the measured undici
    # default off 300 s dies on the value pin before any exchange runs.
    ('unbounded-constant-changed',
     root / 'src/pilotdeck-gateway/src/channel/http_client.rs',
     'pub(crate) const UNDICI_DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);',
     'pub(crate) const UNDICI_DEFAULT_TIMEOUT: Duration = Duration::from_secs(301);',
     'http_client::tests::request_unbounded_settles_a_silent_server_at_the_undici_default',
     '#1343 the no-signal poll GET stays bounded at the undici default'),
    # #2285 r4 ⑤: an exchange that never writes any bytes must fail the
    # wall-clock watchdog namedly instead of hanging the paused-clock test
    # with the keeper spinning on one core.
    ('request-stalls-before-bytes',
     root / 'src/pilotdeck-gateway/src/channel/http_client.rs',
     '        let exchange = async {\n',
     '        let exchange = async {\n'
     '            std::future::pending::<()>().await;\n',
     'http_client::tests::request_unbounded_settles_a_silent_server_at_the_undici_default',
     '#1343 server never saw the request'),
    # #2285 r4 ⑤: a request that ends before the server saw any bytes must
    # fail the is_finished race namedly (the watchdog's other branch).
    ('request-dies-before-receipt',
     root / 'src/pilotdeck-gateway/src/channel/http_client.rs',
     '        let request = builder\n'
     '            .body(Full::new(Bytes::from(body.to_owned())))\n'
     '            .map_err(|error| format!("TypeError: {error}"))?;\n',
     '        let request = builder\n'
     '            .body(Full::new(Bytes::from(body.to_owned())))\n'
     '            .map_err(|error| format!("TypeError: {error}"))?;\n'
     '        return Ok(HttpResponse { status: 200, status_text: "", body: Bytes::new() });\n',
     'http_client::tests::request_unbounded_settles_a_silent_server_at_the_undici_default',
     '#1343 the request task ends only after the server saw the request'),
    # #2285 r2 ④: a stray stamped with a foreign epoch — the exact leak the
    # old `arm()` clear used to hide — must fail the case that observes it,
    # not be silently dropped by the next arm.
    ('strays-foreign-epoch', tests,
     '                                let index = record.polls;\n'
     '                                record.polls += 1;\n',
     '                                let index = record.polls;\n'
     '                                record.polls += 1;\n'
     '                                mock.strays.lock().expect("#1343 stray mutex")\n'
     '                                    .push((0, format!("{method} {path}")));\n',
     GOLDEN, '#2285 every stray lands inside the case that armed it (case first-poll-hello-reply)'),
    # #2285 fix-r1 boundary #1: re-inserting the r1 `arm()` clear must die —
    # a late stray that really walked the disarmed server path must survive
    # the next arm, not be silently dropped.
    ('arm-clear-reinserted', tests,
     '    fn arm(&self, case: &Value, record: Arc<Mutex<Record>>) -> u64 {\n'
     '        let epoch = self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;\n',
     '    fn arm(&self, case: &Value, record: Arc<Mutex<Record>>) -> u64 {\n'
     '        let epoch = self.epoch.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;\n'
     '        self.strays.lock().expect("#1343 stray mutex").clear();\n',
     'bluebubbles::tests::late_stray_survives_the_next_arm',
     '#2285 a late stray survives the next arm charged to its own epoch '
     '(the next arm must not clear the late stray)'),
    # #2285 fix-r1 boundary #1: a late request really injected at the replay
    # tail, after disarm — charged through `mock.stray()` on the real
    # socket path — must fail the case that armed it, not vanish.
    ('late-stray-real-path', tests,
     '    drained(&record).await;\n'
     '    mock.disarm();\n',
     '    drained(&record).await;\n'
     '    mock.disarm();\n'
     '    let late_client = HttpClient::new();\n'
     '    let _ = late_client\n'
     '        .request_unbounded("GET", &format!("http://127.0.0.1:{MOCK_PORT}/api/v1/message?late=1"), None, "")\n'
     '        .await;\n',
     GOLDEN, '#1343 case missing-config records no request outside an armed case'),
    # #2285 fix-r1 sibling #2: inlining the per-tick poll moves the panic
    # into the driver task itself; the tick after the poisoned row never
    # arrives, so the third-poll wait fires the cell.
    ('poll-task-inlined-into-driver', source / 'mod.rs',
     '                        let Some(runtime) = weak.upgrade() else { break };\n'
     '                        tokio::spawn(async move {\n'
     '                            runtime.poll_messages().await;\n'
     '                        });\n',
     '                        let Some(runtime) = weak.upgrade() else { break };\n'
     '                        runtime.poll_messages().await;\n',
     'bluebubbles::tests::poll_task_panic_does_not_stop_the_driver',
     '#2285 a panicking poll task does not stop the poll driver'),
    # #2285 fix-r1 boundary #2: back to the r1 single-silent-window
    # termination — a starved first window ends the drain before the
    # in-window arrival can extend it.
    ('drained-single-quiet', tests,
     '            if quiet >= 2 {\n'
     '                break;\n'
     '            }\n',
     '            if quiet >= 1 {\n'
     '                break;\n'
     '            }\n',
     'bluebubbles::tests::drained_holds_through_a_single_silent_window',
     '#2285 the disarm drain holds through a single silent window '
     '(two consecutive silent windows end the drain at exactly 200 ms)'),
    # #2285 r2 review P2a-2: dropping the `quiet = 0` reset turns "two
    # consecutive silent windows" into two cumulative ones — the 50 → 100
    # (movement) → 150 ms trace ends the drain at 150 ms and the
    # exact-trace pin (SETTLE * 4) fires.
    ('drained-quiet-not-reset', tests,
     '        } else {\n'
     '            quiet = 0;\n'
     '            last = now;\n'
     '        }\n',
     '        } else {\n'
     '            last = now;\n'
     '        }\n',
     'bluebubbles::tests::drained_holds_through_a_single_silent_window',
     '#2285 the disarm drain holds through a single silent window '
     '(two consecutive silent windows end the drain at exactly 200 ms)'),
    # #2285 r2 review P2a-1: removing the turn-index poisoning lets the
    # dispatch complete normally, so the gateway projection is no longer
    # empty — the submit-count ruler (judged after the third poll, when the
    # dispatch has certainly run) must go red; the old immediately-after-
    # polls>=2 assertion could pass on a dispatch that had not executed yet.
    ('poison-turn-index-removed', tests,
     '    // Poison the turn-index mutex before any dispatch can use it.\n'
     '    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {\n'
     '        let _guard = gateway.next.lock().expect("#2285 turn index");\n'
     '        panic!("#2285 poison the turn index");\n'
     '    }));\n',
     '',
     'bluebubbles::tests::poll_task_panic_does_not_stop_the_driver',
     '#2285 a panicking poll task does not stop the poll driver '
     '(the poisoned dispatch panicked before recording)'),
    # A8.3-CHANNEL-BLUEBUBBLES-001-R2: the implemented channel must not be in
    # the unmigrated rejection table; re-inserting the key refuses startup
    # for the subset the loader test keeps loadable.
    ('reject-table-reinserts-bluebubbles',
     root / 'src/pilotdeck-gateway/src/channel/loader.rs',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &[];',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &["bluebubbles"];',
     'tests::unmigrated_subset_channels_stay_loadable',
     '#1386 subset channel must not be rejected'),
    # Dropping a case from the parsed oracle must trip the generator's
    # literal 16, never silently pass with fewer cells.
    ('oracle-shrunk-list', tests,
     '    let oracle = oracle();\n    let mock = Mock::new();\n',
     '    let mut oracle = oracle();\n'
     '    let _ = oracle["cases"].as_array_mut().expect("#1343 mutant cases").pop();\n'
     '    oracle["expectedCaseCount"] = json!(15);\n'
     '    let mock = Mock::new();\n',
     GOLDEN, "#1343 oracle holds the generator's 16 cases"),
    ('oracle-wrong-declaration', fixtures / 'channel-bluebubbles/oracle.json',
     '"expectedCaseCount": 16,',
     '"expectedCaseCount": 15,',
     GOLDEN, '#1343 oracle case count matches its declaration'),
    # An empty cases array must be rejected, never skipped.
    ('oracle-empty-cases', fixtures / 'channel-bluebubbles/oracle.json',
     '  "cases": [\n',
     '  "cases": [],\n  "disabledCases": [\n',
     GOLDEN, '#1343 oracle cases non-empty'),
    # #2433: silencing the mount's serverUrl type guard lets a non-string
    # `extra.serverUrl` construct (String()-coerced), so the loader records
    # no console.error line and the channel enters the startup array — the
    # TS constructor's `url.replace` throws instead and the key is skipped.
    ('server-url-guard-silenced', source / 'mod.rs',
     '            if let Some(value) = options\n'
     '                .get("serverUrl")\n'
     '                .filter(|value| !sms::nullish(value))\n',
     '            if let Some(value) = options\n'
     '                .get("serverUrl")\n'
     '                .filter(|_| false)\n',
     'bluebubbles::tests::loader_records_server_url_type_errors_and_continues',
     '#2433 the loader records the serverUrl TypeError and skips the key (42)'),
]


def main():
    guarded = list(source.glob('*.rs'))
    guarded.extend(path for path in (fixtures / 'channel-bluebubbles').rglob('*') if path.is_file())
    guarded.append(root / 'src/pilotdeck-gateway/src/channel/loader.rs')
    guarded.append(root / 'src/pilotdeck-gateway/src/channel/tests.rs')
    guarded.append(root / 'src/pilotdeck-gateway/src/channel/http_client.rs')
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    return matrix_main(root, __file__, 'channel::', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())
