#!/usr/bin/env python3
"""#2622 transcript-archive mutations; commit first, never bypass HEAD.

NEW_OUTPUT [mutation ...]. --check validates anchors without changing sources.
Every mutant must compile, kill its exact test, and pass after restoration.
The executor runs every selected mutant before publishing the one summary.
"""
from pathlib import Path
import difflib
import os
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, write_source_atomic
from negative_selection import refuse
from negative_suite import Case, classify, passed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
ARCHIVE = ROOT / 'src/pilotdeck-gateway/src/native_process/methods/transcript_archive.rs'
CAPABILITIES = ROOT / 'src/pilotdeck-gateway/src/native_process/capabilities.rs'
STORAGE = ROOT / 'src/pilotdeck-gateway/src/storage/transcript.rs'
ADAPTER = ROOT / 'src/pilotdeck-gateway/src/storage/transcript_adapter.rs'
ORACLE = Path(__file__).resolve().parent / 'oracle.json'
EXPORT_ROWS = 'native_process::methods::transcript_archive::tests::export_rows_match_ts'
RESTORE_ROWS = 'native_process::methods::transcript_archive::tests::restore_rows_match_ts'
BUSY = 'native_process::methods::transcript_archive::tests::busy_and_loaded_session_rows_match_ts'
CHAT_TREE = 'native_process::methods::transcript_archive::tests::chat_tree_holds_only_final_transcripts'
POSITION = 'native_process::methods::transcript_archive::tests::session_transcript_archive_sits_between_its_ts_neighbors'
R5 = 'native_process::methods::transcript_archive::tests::restore_refuses_while_the_key_is_already_restoring'
R11 = 'native_process::methods::transcript_archive::tests::restore_exists_check_waits_for_the_transcript_fence'
R13 = 'native_process::methods::transcript_archive::tests::restore_refuses_while_a_session_holds_the_writer_lease'
mutations = [
    # --- export delegate -----------------------------------------------------
    # The busy guard ahead of every storage read (guard ORDER: busy-1 has a
    # transcript, so dropping the guard turns the busy error into an ok export).
    ('export-busy-guard-omitted', ARCHIVE,
     '        if self.owner.has_active_turn(&key) {\n            return Err(gateway_error(\n                "SESSION_BUSY",\n                "Cannot export a transcript while its session has an active turn.",\n            ));\n        }',
     '        if false { }', BUSY, '#2622 export.busy'),
    # `transcript_missing` → SESSION_NOT_FOUND.
    ('export-missing-defaulted', ARCHIVE,
     '.any(|diagnostic| diagnostic.code == "transcript_missing")',
     '.any(|diagnostic| diagnostic.code == "transcript_never")',
     EXPORT_ROWS, '#2622 export.missing'),
    # error-severity diagnostics → SESSION_TRANSCRIPT_INVALID.
    ('export-invalid-severity-ignored', ARCHIVE,
     '.any(|diagnostic| diagnostic.severity == "error")',
     '.any(|diagnostic| diagnostic.severity == "never")',
     EXPORT_ROWS, '#2622 export.invalid'),
    # The synthetic-message filter in toPortableSessionArchive.
    ('export-synthetic-filter-omitted', ARCHIVE,
     'if field(field(message, "metadata"), "synthetic") == &JsValue::Bool(true) {',
     'if false {', EXPORT_ROWS, '#2622 export.rich'),
    # validate's message-count arm is what answers zero-message exports.
    ('export-message-count-omitted', ARCHIVE,
     'if messages.is_empty() || messages.len() > MAX_PORTABLE_SESSION_ARCHIVE_MESSAGES {',
     'if messages.len() > MAX_PORTABLE_SESSION_ARCHIVE_MESSAGES {',
     EXPORT_ROWS, '#2622 export.empty-text'),
    # The 512-unit title ceiling drops (never truncates) over-long titles.
    ('export-title-ceiling-dropped', ARCHIVE,
     'if !text.is_empty() && text.len_utf16() <= MAX_PORTABLE_SESSION_ARCHIVE_TITLE_LENGTH {',
     'if !text.is_empty() {', EXPORT_ROWS, '#2622 export.long-title'),
    # The pretty render's V8 string-length budget: without it a wide deep
    # payload renders the full multi-hundred-MB layout instead of taking the
    # TS helper's catch fallback.
    ('export-pretty-budget-omitted', ARCHIVE,
     'const MAX_PRETTY_OUTPUT_BYTES: usize = (1 << 29) - 24;',
     'const MAX_PRETTY_OUTPUT_BYTES: usize = (1 << 40);',
     'native_process::methods::transcript_archive::tests::wide_deep_tool_input_falls_back_past_the_pretty_output_bound',
     '#2622 the fallback keeps this inside the shared bound'),
    # --- restore delegate ----------------------------------------------------
    # `hasActiveTurn || hasSession` ahead of the restore conflict.
    ('restore-busy-guard-omitted', ARCHIVE,
     'if self.owner.has_active_turn(&key) || self.owner.current_handle(&key).is_some() {',
     'if false {', BUSY, '#2622 restore.busy'),
    # `restoringSessionKeys.has(key)`, read before any storage work.
    ('restore-set-membership-ignored', ARCHIVE,
     '        if self\n            .restoring_session_keys\n            .lock()\n            .unwrap_or_else(|error| error.into_inner())\n            .contains(&key)\n        {',
     '        if false {', R5,
     '#2622 R5 an in-flight restore refuses with the conflict, not the busy error'),
    # The exists check inside the fence.
    ('restore-exists-check-omitted', ARCHIVE,
     '            if exists {',
     '            if false {', RESTORE_ROWS, '#2622 restore.exists'),
    # `if (archive.title)` — the session_metadata entry a titled restore writes.
    ('restore-title-metadata-omitted', ARCHIVE,
     '    if truthy(title) {',
     '    if false {', RESTORE_ROWS, '#2622 restore.written-bytes'),
    # `importedMessages: archive.messages.length`.
    ('restore-imported-count-zeroed', ARCHIVE,
     'JsValue::Number(imported_messages as f64)',
     'JsValue::Number(0.0)', RESTORE_ROWS, '#2622 restore.fresh'),
    # R12: the set's add/remove pair rides inside the fenced blocking task, so
    # a failed write cannot strand the key.
    ('restore-set-not-cleaned', ARCHIVE,
     '            restoring\n                .lock()\n                .unwrap_or_else(|error| error.into_inner())\n                .remove(&key_for_write);',
     '            let _ = &restoring;', R13,
     '#2622 the failed restore removed its key'),
    # R13: the lease refusal answers the busy domain error, not the rename
    # family's writer-busy code.
    ('restore-lease-busy-code', ARCHIVE,
     '    let Some(_lease) = transcript_locks::cold_write_lease(&storage.transcript_path) else {\n        return Err(gateway_error(\n            "SESSION_BUSY",\n            "Cannot restore over an active Gateway session.",\n        ));\n    };',
     '    let Some(_lease) = transcript_locks::cold_write_lease(&storage.transcript_path) else {\n        return Err(gateway_error(\n            "SESSION_WRITER_BUSY",\n            "Cannot restore over an active Gateway session.",\n        ));\n    };',
     R13, '#2622 R13 the lease refusal answers the busy domain error'),
    # R11: the exists check runs under the transcript fence; rekeying the
    # acquire lets the queued restore race ahead of the landing transcript.
    ('restore-fence-not-taken', ARCHIVE,
     'let write_fence = transcript_locks::acquire(&fence_path).await?;',
     'let write_fence = transcript_locks::acquire(&fence_path.with_extension("unfenced")).await?;',
     R11, '#2622 R11 the exists check ran under the fence'),
    # The store-side replace is the restore's visibility commit (the ledger
    # writer's own rename never runs for a cold restore); landing under a
    # staging name leaves the chat tree without the final transcript.
    ('restore-rename-to-staging-name', ADAPTER,
     'fs::rename(&cleanup.0, path).map_err(|e| path_error(e, "rename", path))?;',
     'fs::rename(&cleanup.0, path.with_extension("jsonl2")).map_err(|e| path_error(e, "rename", path))?;',
     CHAT_TREE, '#2622 chat.files holds exactly the final transcripts'),
    # --- capability row ------------------------------------------------------
    ('capability-archive-name-unwired', CAPABILITIES,
     'name: "session_transcript_archive",',
     'name: "unwired_session_transcript_archive",',
     POSITION, '#2622 the archive name is advertised'),
]

# #1538: the table is its own denominator — pin the row count and the roster
# (order and repeats included) so a dropped, renamed or duplicated mutation is
# refused before anything runs or compiles.
EXPECTED_TOTAL = 17
EXPECTED_NAMES = (
    'export-busy-guard-omitted',
    'export-missing-defaulted',
    'export-invalid-severity-ignored',
    'export-synthetic-filter-omitted',
    'export-message-count-omitted',
    'export-title-ceiling-dropped',
    'export-pretty-budget-omitted',
    'restore-busy-guard-omitted',
    'restore-set-membership-ignored',
    'restore-exists-check-omitted',
    'restore-title-metadata-omitted',
    'restore-imported-count-zeroed',
    'restore-set-not-cleaned',
    'restore-lease-busy-code',
    'restore-fence-not-taken',
    'restore-rename-to-staging-name',
    'capability-archive-name-unwired',
)
if len(mutations) != EXPECTED_TOTAL:
    raise SystemExit(f'#1538 fixed mutation denominator: expected {EXPECTED_TOTAL}, '
                     f'found {len(mutations)}')
if tuple(row[0] for row in mutations) != EXPECTED_NAMES:
    raise SystemExit('#1538 registered mutation roster must equal the frozen list: '
                     f'expected={list(EXPECTED_NAMES)}, '
                     f'found={[row[0] for row in mutations]}')


def mutate(case, text):
    if text.count(case.old) != 1 or case.old == case.new:
        raise ValueError(f'#2622 stale anchor: {case.mutation}: {text.count(case.old)}')
    return text.replace(case.old, case.new, 1)


def main():
    selected_rows = [Case(*row) for row in mutations]
    if sys.argv[1:] == ['--check']:
        for case in selected_rows:
            mutate(case, case.path.read_text())
        print(f'#2622 {len(selected_rows)} mutation anchors OK')
        return 0
    if len(sys.argv) < 2:
        raise SystemExit('usage: negative_controls.py NEW_OUTPUT [mutation ...]')
    env = dict(os.environ)
    if not env.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR must be supplied by the caller')
    selected = sys.argv[2:]
    # A repeated name is a caller-side typo (`a a` for `a b`), not a subset;
    # folding it into a set turns a mistyped selection into a green run over a
    # smaller scope, the same false green as an unknown name (#1734).
    repeated = sorted({name for name in selected if selected.count(name) > 1})
    if repeated:
        refuse('repeated mutation name: ' + ', '.join(repeated))
    if set(selected) - {case.mutation for case in selected_rows}:
        raise SystemExit('unknown mutation name')
    if selected:
        selected_rows = [case for case in selected_rows if case.mutation in selected]
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only', LANG='en_US.UTF-8')
    env.pop('LC_ALL', None)
    env.pop('LC_MESSAGES', None)
    command = ['cargo', 'test', '--offline', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib']
    results = []

    def run(label, args):
        slot = wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900)
        (out / (label + '.slots.log')).write_text(slot.stdout + slot.stderr)
        result = run_owned(command + args, cwd=ROOT, env=env, timeout=900)
        (out / (label + '.log')).write_text(result.stdout + result.stderr)
        return result

    for case in selected_rows:
        row = dict(mutation=case.mutation, test=case.test, assertion=case.assertion,
                   status='INVALID', reason='', compiled=False)
        args = [case.test, '--', '--exact', '--include-ignored']
        try:
            # Protect the executable fixture and judge, as well as product bytes.
            with mutating_sources(ROOT, [case.path, Path(__file__).resolve(), ORACLE,
                    ORACLE.with_name('generate.mts'), ORACLE.with_name('README.md'),
                    CAPABILITIES, STORAGE, ADAPTER,
                    ROOT / 'src/pilotdeck-gateway/src/native_process/management_tests.rs']) as originals:
                original = originals[case.path].decode()
                changed = mutate(case, original)
                if not passed(run(case.mutation + '-positive', args), case.test):
                    raise RuntimeError('positive control failed')
                (out / (case.mutation + '.patch')).write_text(''.join(difflib.unified_diff(
                    original.splitlines(True), changed.splitlines(True), fromfile=str(case.path), tofile=str(case.path))))
                write_source_atomic(ROOT, case.path, changed.encode())
                compiled = run(case.mutation + '-compile', ['--no-run'])
                row['compiled'] = compiled.returncode == 0
                if not row['compiled']:
                    raise RuntimeError('mutant did not compile')
                row['status'], row['reason'] = classify(run(case.mutation + '-negative', args), case)
                # Restore and re-run inside the source guard: the restored control
                # compiles the tree, so it must never race another runner that
                # takes the same .negative-source.lock (#1440).
                # #1908: one atomic rename; a truncating write_bytes here
                # has the same SIGKILL half-write window as the mutation
                # write itself.
                write_source_atomic(ROOT, case.path, originals[case.path])
                if not passed(run(case.mutation + '-restored', args), case.test):
                    raise RuntimeError('restored positive control failed')
        except Exception as error:
            row.update(status='INVALID', reason=f'{type(error).__name__}: {error}')
        results.append(row)
        print(f"{case.mutation}: {row['status']}: {row['reason']}", flush=True)
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{key}={value}' for key, value in counts.items()),
          flush=True)
    publish_summary(out, results)
    # A run that killed nothing proved nothing; an INVALID row is a control
    # whose own harness failed, never a pass (#818 `verdict_exit` shape).
    return int(not results
               or not counts['KILLED']
               or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
