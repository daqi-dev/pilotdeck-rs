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
    # A8.3-CHANNEL-BLUEBUBBLES-001-R2: the implemented channel must not be in
    # the unmigrated rejection table; re-inserting the key refuses startup
    # for the subset the loader test keeps loadable.
    ('reject-table-reinserts-bluebubbles',
     root / 'src/pilotdeck-gateway/src/channel/loader.rs',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &[\n'
     '    "homeassistant",\n',
     'pub const UNMIGRATED_CHANNEL_KEYS: &[&str] = &[\n'
     '    "bluebubbles",\n'
     '    "homeassistant",\n',
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
]


def main():
    guarded = list(source.glob('*.rs'))
    guarded.extend(path for path in (fixtures / 'channel-bluebubbles').rglob('*') if path.is_file())
    guarded.append(root / 'src/pilotdeck-gateway/src/channel/loader.rs')
    guarded.append(root / 'src/pilotdeck-gateway/src/channel/tests.rs')
    args = ['--check'] if sys.argv[1:] == ['--check-anchors'] else sys.argv[1:]
    return matrix_main(root, __file__, 'channel::', MUTATIONS,
                       passed_count=1, child_count=0, paths=guarded,
                       exact=True, args=args)


if __name__ == '__main__':
    raise SystemExit(main())
