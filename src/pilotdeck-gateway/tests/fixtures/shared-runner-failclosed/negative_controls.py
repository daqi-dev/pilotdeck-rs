#!/usr/bin/env python3
# anchor-selfcheck: unsupported #1076 shared-runner mutation executor; anchors are checked below
"""#1076 paired mutations for the shared negative-runner cancellation fix.

The two delivered Python executors keep their summary in memory until the
source guard restored and re-verified every guarded file.  The pre-fix shapes
published each row directly and caught only ``Exception``; a SIGINT or a raw
``SystemExit(256)`` in the guard's restore window therefore left an all-KILLED
``summary.json`` behind with an unnormalized exit.

#1076 fix-r1 adds the second half of the contract: the widened handler must
not turn a guard *refusal to run* (HEAD/baseline mismatch, busy lock, expired
anchor) into rc=2.  Refusals keep the historical rc=1 that callers already
assert; only a cancellation or an unverified restore is rc=2.  The four rows
added below pin both refusal phases (before and after the guard opened) for
both executors.

#1076 fix-r2 closes the unwinding hole: a body refusal followed by a *different*
exception from the guard's ``__exit__`` restore/re-verify is void -> rc=2, and
a cancellation raised inside the body (not only at guard exit) is rc=2 too.
The rows below pin the exception-identity check, the ``not
isinstance(error, Exception)`` cancellation branch, both phase transitions and
both halves of the ``('restore', 'exit')`` tuple for each executor.

#3043 fix-r5 swaps the local loose judges for the family's canonical
four-condition set (``gateway-entry-guard/negative_controls.py``, #991/#1060;
the same swap fix-r4 made for close-order-reader-failclosed) and moves the
publish/restore writes onto ``write_source_atomic``.  A skip-shaped green log
is not a pass, and the marker echoed anywhere but the bound test's own
AssertionError block is not a kill; the bound test now runs as the script it
lives in so the canonical ``__main__`` description layouts match.

Each mutant is syntax-compiled before its positive control runs.  A stale
anchor aborts the whole selected run as INVALID (never SURVIVED).  All selected
rows finish before the summary is published; any non-KILLED row is a nonzero
exit.  This script mutates tracked files, so it must run post-commit
(``mutating_sources`` refuses a working-tree baseline mismatch).  No cargo is
launched; the bound unittest imports the mutated Python module in a child
process.  ``RUNNER`` itself is not mutated, so an untracked pre-commit copy is
not part of the guarded set.
"""
import argparse
import json
import os
from pathlib import Path
import re
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import (mutating_sources, new_output_directory,
                              run_owned, write_source_atomic)

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SUITE = FIXTURES / 'negative_suite.py'
SQLITE = FIXTURES / '_shared/negative_sqlite_header.py'

# (name, source, before, after, bound unittest, exact #1076 assertion message)
MUTANT_ROWS = [
    (
        'runner-baseexception-narrowed',
        SUITE,
        '\n    except BaseException as error:',
        '\n    except Exception as error:',
        'test_negative_suite.CancellationTests.test_guard_sigint_voids_last_kill_and_exits_2',
        '#1076 a SIGINT in the guard exit must return 2',
    ),
    (
        'runner-publish-before-verify',
        SUITE,
        # #1413 fix-r3 renamed the console stem to ``stem(case)``; the anchor
        # has to follow the source it mutates or this control goes INVALID.
        "        print(f'{stem(case)}: {status}: {reason}', flush=True)",
        "        print(f'{stem(case)}: {status}: {reason}', flush=True)\n"
        "        publish_summary(out, results)",
        'test_negative_suite.CancellationTests.'
        'test_guard_systemexit_256_voids_last_kill_and_exits_2',
        '#1076 no summary may exist while the guard is unverified',
    ),
    (
        # #2064: re-anchored onto `_publish_json` -- the #1579/#1694 refactor
        # moved the summary publish there, and this row's old `publish_summary`
        # body anchor had been stale since (count=0 at HEAD d86d1dd38).
        'runner-nonatomic-publish',
        SUITE,
        "        if overwrite:\n"
        "            temporary.replace(path)",
        "        if overwrite:\n"
        "            path.write_text(json.dumps(payload, indent=2) + '\\n')",
        'test_negative_suite.CancellationTests.test_publish_summary_publishes_atomically',
        '#1076 an interrupted publish must preserve the previous summary',
    ),
    (
        'sqlite-baseexception-narrowed',
        SQLITE,
        '\n    except BaseException as error:',
        '\n    except Exception as error:',
        'test_negative_suite.SqliteHeaderCancellationTests.'
        'test_sqlite_guard_sigint_voids_last_kill_and_exits_2',
        '#1076 a sqlite SIGINT in the guard exit must return 2',
    ),
    (
        'sqlite-publish-in-loop',
        SQLITE,
        "                print(f'{name}: {status}: {reason}', flush=True)",
        "                print(f'{name}: {status}: {reason}', flush=True)\n"
        "                write_summary(output, results)",
        'test_negative_suite.SqliteHeaderCancellationTests.'
        'test_sqlite_guard_systemexit_256_voids_last_kill_and_exits_2',
        '#1076 no summary may exist while the sqlite guard is unverified',
    ),
    (
        'sqlite-nonatomic-publish',
        SQLITE,
        "    temporary.write_text(json.dumps({'controls': results, 'counts': counts}, "
        "indent=2) + '\\n')\n"
        "    temporary.replace(target)",
        "    target.write_text(json.dumps({'controls': results, 'counts': counts}, "
        "indent=2) + '\\n')",
        'test_negative_suite.SqliteHeaderCancellationTests.'
        'test_write_summary_publishes_atomically',
        '#1076 an interrupted publish must preserve the previous summary',
    ),
    (
        # fix-r1: the refusal branch must stay rc=1; a reviewer found that the
        # first delivery returned 2 here and broke a real caller assertion.
        'runner-refusal-exit-two',
        SUITE,
        "            return 2\n        return 1",
        "            return 2\n        return 2",
        'test_negative_suite.SuiteTests.test_guard_refusal_accounts_for_every_case',
        '#1076 a HEAD guard refusal must keep exit code 1',
    ),
    (
        'sqlite-refusal-exit-two',
        SQLITE,
        "            return 2\n        return 1",
        "            return 2\n        return 2",
        'test_negative_suite.SqliteHeaderExecutorTests.test_guard_refusal_keeps_exit_one',
        '#1076 a sqlite HEAD guard refusal must keep exit code 1',
    ),
    (
        # In-window refusals (expired anchor / malformed registration) are
        # refusals too: rc=1, never the restore/cancel rc=2.
        'runner-body-refusal-exit-two',
        SUITE,
        "        if (phase in ('restore', 'exit') or not isinstance(error, Exception)",
        "        if (phase in ('restore', 'exit', 'body') or not isinstance(error, Exception)",
        'test_negative_suite.SuiteTests.test_in_window_refusal_keeps_exit_one',
        '#1076 an in-window refusal must keep exit code 1',
    ),
    (
        'sqlite-body-refusal-exit-two',
        SQLITE,
        "        if (phase in ('restore', 'exit') or not isinstance(error, Exception)",
        "        if (phase in ('restore', 'exit', 'body') or not isinstance(error, Exception)",
        'test_negative_suite.SqliteHeaderExecutorTests.test_stale_anchor_refusal_keeps_exit_one',
        '#1076 a sqlite stale-anchor refusal must keep exit code 1',
    ),
    (
        # fix-r2 / Codex P1: the body already raised and the guard's own
        # restore/re-verify raises a *different* object while unwinding. The
        # identity check must keep that void -> rc=2 even though phase is 'body'.
        'runner-body-error-overwritten-exit-one',
        SUITE,
        "                or (body_error is not None and error is not body_error)):",
        "                or (False)):",
        'test_negative_suite.SuiteTests.'
        'test_body_refusal_with_guard_restore_failure_exits_two',
        '#1076 a body refusal plus an unverified guard exit must return 2',
    ),
    (
        'sqlite-body-error-overwritten-exit-one',
        SQLITE,
        "                or (body_error is not None and error is not body_error)):",
        "                or (False)):",
        'test_negative_suite.SqliteHeaderExecutorTests.'
        'test_stale_anchor_with_guard_restore_failure_exits_two',
        '#1076 a sqlite stale anchor plus an unverified guard exit must return 2',
    ),
    (
        # fix-r2: cancellation inside the body must stay rc=2 on its own, not
        # only while the guard is exiting.
        'runner-body-cancellation-exit-one',
        SUITE,
        "        if (phase in ('restore', 'exit') or not isinstance(error, Exception)",
        "        if (phase in ('restore', 'exit') or False",
        'test_negative_suite.SuiteTests.test_body_cancellation_keeps_exit_two',
        '#1076 a body cancellation with a clean guard exit must return 2',
    ),
    (
        'sqlite-body-cancellation-exit-one',
        SQLITE,
        "        if (phase in ('restore', 'exit') or not isinstance(error, Exception)",
        "        if (phase in ('restore', 'exit') or False",
        'test_negative_suite.SqliteHeaderExecutorTests.test_body_cancellation_keeps_exit_two',
        '#1076 a sqlite body cancellation must return 2',
    ),
    (
        # fix-r2: the per-row restore phase is what makes an unverified byte
        # restore rc=2; folding it back into 'body' must be killed.
        'runner-restore-phase-body',
        SUITE,
        "                        phase = 'restore'",
        "                        phase = 'body'",
        'test_negative_suite.SuiteTests.test_byte_restore_failure_stops_unsafe_mutation',
        '#1076 an unverified byte restore must exit 2',
    ),
    (
        'sqlite-restore-phase-body',
        SQLITE,
        "                        phase = 'restore'",
        "                        phase = 'body'",
        'test_negative_suite.SqliteHeaderExecutorTests.test_row_restore_failure_returns_two',
        '#1076 an unverified sqlite row restore must return 2',
    ),
    (
        # Both halves of the phase tuple are load-bearing: dropping 'exit'
        # hides a guard-exit restore failure, dropping 'restore' hides a
        # per-row restore failure.
        'runner-exit-branch-one',
        SUITE,
        "phase in ('restore', 'exit')",
        "phase in ('restore',)",
        'test_negative_suite.SuiteTests.test_guard_exit_invalidates_previous_kills',
        '#1076 guard restoration failure must exit 2',
    ),
    (
        'sqlite-exit-branch-one',
        SQLITE,
        "phase in ('restore', 'exit')",
        "phase in ('restore',)",
        'test_negative_suite.SqliteHeaderExecutorTests.'
        'test_guard_restore_failure_invalidates_all_completed_kills',
        '#1076 a failed guard exit must return exit code 2',
    ),
    (
        'runner-restore-branch-one',
        SUITE,
        "phase in ('restore', 'exit')",
        "phase in ('exit',)",
        'test_negative_suite.SuiteTests.test_byte_restore_failure_stops_unsafe_mutation',
        '#1076 an unverified byte restore must exit 2',
    ),
    (
        'sqlite-restore-branch-one',
        SQLITE,
        "phase in ('restore', 'exit')",
        "phase in ('exit',)",
        'test_negative_suite.SqliteHeaderExecutorTests.test_row_restore_failure_returns_two',
        '#1076 an unverified sqlite row restore must return 2',
    ),
    # #1890: the shared `--audit` binding helper replaced six drifted copies.
    # Each rejection branch is load-bearing, so each gets a mutant that
    # re-opens exactly the hole it closes.
    (
        'audit-binding-empty-short-allowed',
        SUITE,
        "        if not short:\n"
        "            raise ValueError('bound test has an empty short name: ' + test)",
        "        if False:\n"
        "            raise ValueError('bound test has an empty short name: ' + test)",
        'test_negative_suite.AuditBindingTests.test_empty_short_name_is_refused',
        '#1890 an empty short name must be refused',
    ),
    (
        'audit-binding-extra-level-allowed',
        SUITE,
        "        if '::' in short:\n"
        "            raise ValueError('bound test has an extra module level: ' + test)",
        "        if False:\n"
        "            raise ValueError('bound test has an extra module level: ' + test)",
        'test_negative_suite.AuditBindingTests.test_extra_module_level_is_refused',
        '#1890 an extra module level must be refused',
    ),
    (
        'audit-binding-unlisted-module-allowed',
        SUITE,
        "    raise ValueError('bound test module not audited: ' + test)",
        "    return modules[0][1], test",
        'test_negative_suite.AuditBindingTests.test_unaudited_module_is_refused',
        '#1890 an unaudited module must be refused',
    ),
    # #1890: the companion rule is "the declaration really exists". Loosening it
    # to always-true is the same failure the six drifted copies had.
    (
        'audit-binding-missing-declaration-allowed',
        SUITE,
        "    compact = ''.join(_rust_code_only(source).split())",
        "    return True\n    compact = ''.join(_rust_code_only(source).split())",
        'test_negative_suite.AuditBindingTests.test_declares_test_needs_the_exact_name',
        '#1890 a missing declaration must be refused',
    ),
    # #1890 r8: the table-row form needs a left boundary, or a longer name that
    # merely ends in the short name (`xcase_a =>`) satisfies the binding.
    (
        'audit-binding-suffix-name-allowed',
        SUITE,
        "    if re.search(r'(?<!\\w)' + escaped + '=>', compact):",
        "    if short + '=>' in compact:",
        'test_negative_suite.AuditBindingTests.test_declares_test_needs_the_exact_name',
        '#1890 a longer name ending in the short name must not count',
    ),
    (
        # #2064 r10: the fn form's boundary is now a single left-lookahead in
        # `_FN_LEFT`; removing it re-opens the embedded-fn hole (the old bare
        # `\w$`-style acceptance) for `run_fncase_a(`.
        'audit-binding-embedded-fn-allowed',
        SUITE,
        "_FN_LEFT = r'(?<!\\w)(?:(?:' + '|'.join(_FN_QUALIFIERS) + r'))*'",
        "_FN_LEFT = r'(?:(?:' + '|'.join(_FN_QUALIFIERS) + r'))*'",
        'test_negative_suite.AuditBindingTests.test_declares_test_needs_the_exact_name',
        '#1890 a longer identifier must not satisfy the fn form',
    ),
    (
        'audit-binding-longer-macro-allowed',
        SUITE,
        "    return bool(macro) and re.search(\n"
        "        r'(?<!\\w)' + re.escape(macro) + r'\\(' + escaped + ',', compact) is not None",
        "    return bool(macro) and macro + '(' + escaped + ',' in compact",
        'test_negative_suite.AuditBindingTests.test_declares_test_needs_the_exact_name',
        '#1890 a longer macro name must not satisfy the table form',
    ),
    # #2064: the three audit-binding residuals plus the r10 left-boundary
    # fix; each mutant re-opens exactly the hole its bound test pins shut.
    (
        # #2064-1: declares_test's own entry refused nothing, so a caller
        # that bypasses bind_module_file could bind an empty short name.
        'audit-binding-declares-empty-short-allowed',
        SUITE,
        "    if not short:\n"
        "        raise ValueError('declares_test needs a short name')",
        "    if not short and macro:\n"
        "        raise ValueError('declares_test needs a short name')",
        'test_negative_suite.AuditBindingTests.test_declares_test_empty_short_is_refused',
        '#2064 declares_test must refuse an empty short name',
    ),
    (
        # #2064-2a: deleting the qualifier whitelist strands every
        # `async fn` / `pub fn` binding (a false stale, not a pass).
        'audit-binding-qualifiers-dropped',
        SUITE,
        "_FN_LEFT = r'(?<!\\w)(?:(?:' + '|'.join(_FN_QUALIFIERS) + r'))*'",
        "_FN_LEFT = r'(?<!\\w)'",
        'test_negative_suite.AuditBindingTests.test_declares_test_accepts_every_existing_form',
        '#2064 an async fn declaration must satisfy the fn form',
    ),
    (
        # #2064-2b: the old head.endswith() accepted any head ending in a
        # qualifier word, so `runasyncfncase_a(`/`xpubfncase_a(` passed as a
        # qualified declaration.
        'audit-binding-qualifier-suffix-allowed',
        SUITE,
        "_FN_LEFT = r'(?<!\\w)(?:(?:' + '|'.join(_FN_QUALIFIERS) + r'))*'",
        "_FN_LEFT = r'(?<!\\w)(?:\\w*(?:' + '|'.join(_FN_QUALIFIERS) + r'))*'",
        'test_negative_suite.AuditBindingTests.test_declares_test_needs_the_exact_name',
        '#2064 a qualifier glued onto a longer identifier must not satisfy the fn form',
    ),
    (
        # #2064 r10: comments and literals are blanked before matching; each
        # stripping branch that stops working re-opens its carrier.
        'audit-binding-line-comment-kept',
        SUITE,
        "        if source.startswith('//', i):",
        "        if source.startswith('//', i) and False:",
        'test_negative_suite.AuditBindingTests.test_declares_test_ignores_comment_and_string_carriers',
        '#2064 a line comment must not satisfy the fn form',
    ),
    (
        'audit-binding-block-comment-kept',
        SUITE,
        "        if source.startswith('/*', i):\n"
        "            depth, i = 1, i + 2",
        "        if source.startswith('/*', i) and False:\n"
        "            depth, i = 1, i + 2",
        'test_negative_suite.AuditBindingTests.test_declares_test_ignores_comment_and_string_carriers',
        '#2064 a block comment must not satisfy the fn form',
    ),
    (
        'audit-binding-string-literal-kept',
        SUITE,
        "_STRING_START = re.compile(r'(?:cr|c|br|rb|r|b)?(#*)\"')",
        "_STRING_START = re.compile(r'(?:cr|c|br|rb|r|b)?(#*)\"\\Z')",
        'test_negative_suite.AuditBindingTests.test_declares_test_ignores_comment_and_string_carriers',
        '#2064 a string literal must not satisfy the fn form',
    ),
    (
        # The char-literal branch: a `'"'` that no longer scans as one char
        # re-opens its quote as a string opener and un-strips later code.
        'audit-binding-char-literal-kept',
        SUITE,
        "        if source[i] == \"'\" or (source[i] == 'b' and _token_start(source, i)):",
        "        if False and (source[i] == 'b' and _token_start(source, i)):",
        'test_negative_suite.AuditBindingTests.test_declares_test_ignores_comment_and_string_carriers',
        '#2064 a quote char literal must not open a string',
    ),
    (
        # #449 / #1166: the fail-closed scan rule itself -- an unterminated
        # literal that no longer raises audits whatever it guessed.
        'audit-binding-unterminated-string-accepted',
        SUITE,
        "                while True:\n"
        "                    if i >= length:\n"
        "                        raise ValueError('unterminated Rust string')",
        "                while True:\n"
        "                    if i >= length:\n"
        "                        break",
        'test_negative_suite.AuditBindingTests.test_declares_test_unterminated_literal_is_refused',
        '#2064 an unterminated literal must fail closed',
    ),
    (
        # #2064 r11: the C-string prefixes. Dropping ``cr|c`` re-opens the
        # raw C string hole: the inner quote closes a bogus plain string and
        # the carried fn text returns to the code view.
        'audit-binding-c-string-prefix-allowed',
        SUITE,
        "_STRING_START = re.compile(r'(?:cr|c|br|rb|r|b)?(#*)\"')",
        "_STRING_START = re.compile(r'(?:br|rb|r|b)?(#*)\"')",
        'test_negative_suite.AuditBindingTests.test_declares_test_ignores_comment_and_string_carriers',
        '#2064 a raw C string literal must not satisfy the fn form',
    ),
    (
        # #2064 r11: the escape char bodies. Reverting to the pattern that
        # cannot close '\u{…}' / '\xNN' leaks their quotes into the code view.
        'audit-binding-char-escape-allowed',
        SUITE,
        '_CHAR_START = re.compile(r"b?\'(?:\\\\u\\{[0-9A-Fa-f_]+\\}|\\\\x[0-9A-Fa-f]{2}|\\\\.|[^\\\\\'])\'")',
        '_CHAR_START = re.compile(r"b?\'(?:\\\\.|[^\\\\\'])\'")',
        'test_negative_suite.AuditBindingTests.test_rust_code_only_blanks_escape_char_literals',
        '#2064 an escaped char literal must be blanked whole',
    ),
    (
        # #2064 r4: the fail-closed refusal of a raw hash delimiter with no
        # r-bearing prefix. Disabling the refusal re-opens the c#"…"# carrier:
        # the inner quote closes a bogus plain string and the carried fn text
        # returns to the code view (here it dies as unterminated instead, so
        # the bound kill is the message check after the assertRaises).
        'audit-binding-c-string-hash-prefix-accepted',
        SUITE,
        "            if hashes and 'r' not in start.group(0):",
        "            if False and 'r' not in start.group(0):",
        'test_negative_suite.AuditBindingTests.test_declares_test_ignores_comment_and_string_carriers',
        '#2064 a hash delimiter without an r prefix must fail closed',
    ),
    (
        # #2064 r4: the gate that lets a c-prefixed literal open a string.
        # Dropping ``c`` from the gate set re-opens the raw C string hole via
        # the cr carrier (bound there; the c# carrier below would die as an
        # unterminated string and blur the first failure).
        'audit-binding-c-string-gate-allowed',
        SUITE,
        "        if source[i] == '\"' or (source[i] in 'rbc' and _token_start(source, i)):",
        "        if source[i] == '\"' or (source[i] in 'rb' and _token_start(source, i)):",
        'test_negative_suite.AuditBindingTests.test_declares_test_ignores_comment_and_string_carriers',
        '#2064 a raw C string literal must not satisfy the fn form',
    ),
]


def apply_mutation(name, original, before, after):
    """Return the mutant bytes, or fail closed on a stale/no-op anchor."""
    count = original.count(before)
    if count != 1:
        raise ValueError(f'#1076 {name} anchor expired: count={count}, expected=1')
    if before == after:
        raise ValueError(f'#1076 {name} is a no-op mutation')
    mutant = original.replace(before, after, 1)
    compile(mutant, f'{name}.py', 'exec')
    return mutant


def bound_test(test):
    # #3043 fix-r5: the canonical judges' ``descriptions()`` accepts only the
    # script-run id shape ('Class.method'); the rows keep the historical
    # fully-qualified form for the summary, so the module prefix is stripped
    # exactly here -- and a row that escapes the suite script is refused
    # instead of silently producing a judge layout nothing matches.
    prefix = 'test_negative_suite.'
    if not test.startswith(prefix):
        raise ValueError(f'#1076 bound test outside the suite script: {test}')
    return test[len(prefix):]


def run_test(test, env):
    # #3043 fix-r5: the bound test runs as the script it lives in
    # (``python -B test_negative_suite.py Class.method -v``) instead of
    # ``-m unittest test_negative_suite.Class.method``, so the canonical
    # ``__main__`` description layout below is the one produced. cwd stays on
    # FIXTURES so the script-dir import of ``negative_suite`` keeps resolving
    # to the (possibly mutated) module. No ``-I``: isolated mode drops the
    # script directory from ``sys.path`` and would break that import.
    return run_owned([sys.executable, '-B',
                      str(FIXTURES / 'test_negative_suite.py'),
                      bound_test(test), '-v'],
                     cwd=FIXTURES, env=env, timeout=180)


# #3043 fix-r5: the judge set below is copied verbatim from the family's
# canonical entry ``gateway-entry-guard/negative_controls.py`` (#991/#1060),
# the same swap fix-r4 (a4) made for close-order-reader-failclosed. The local
# drift it replaces accepted any rc-0 log containing "Ran 1 test" and "\nOK"
# -- a skip-shaped green log passes it -- and called any log with the marker,
# "FAIL:" and "AssertionError" a KILL, so the marker echoed in stdout over an
# unrelated failure block was judged KILLED. Only the bound test's own
# AssertionError may report a kill, in either unittest layout.
def single_test(log):
    return len(re.findall(r'^Ran 1 test in [0-9.]+s$', log, re.MULTILINE)) == 1


def descriptions(test):
    owner, method = test.rsplit('.', 1)
    # Accept both observed unittest layouts, retaining the exact module,
    # class and method identity in either layout (#1060).
    return (f'{method} (__main__.{test})', f'{method} (__main__.{owner})')


def passed(result, test):
    log = result.stdout + result.stderr
    return (result.returncode == 0
            and any(description + ' ... ok' in log.splitlines()
                    for description in descriptions(test))
            and single_test(log) and log.rstrip().endswith('\nOK')
            and 'skipped' not in log)


def classify(result, test, message):
    log = result.stdout + result.stderr
    if passed(result, test):
        return 'SURVIVED', 'bound test passed the mutant'
    if (result.returncode != 1 or not single_test(log)
            or not re.search(r'^FAILED \(failures=[1-9][0-9]*\)\s*\Z', log, re.MULTILINE)
            or 'ERROR:' in log):
        return 'INVALID', 'missing normal single-test assertion failure'
    # subTest descriptions follow either layout. Only this test's own
    # AssertionError may supply the marker.
    names = '|'.join(re.escape(name) for name in descriptions(test))
    header = re.compile(
        rf'FAIL: (?:{names})(?: \(.*\))?')
    separator = '\n' + '-' * 70 + '\n'
    for block in log.split('\n' + '=' * 70 + '\n')[1:]:
        title, found, body = block.partition(separator)
        if not found or not header.fullmatch(title):
            continue
        body = body.split(separator, 1)[0]
        assertion = re.search(r'^AssertionError: (.*)', body, re.MULTILINE | re.DOTALL)
        if assertion and message in assertion.group(1):
            return 'KILLED', message
    return 'INVALID', 'missing bound assertion (anchor, compile or wrong failure)'


def publish(output, results):
    counts = {state: sum(row['status'] == state for row in results)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    target = output / 'summary.json'
    temporary = target.with_name(target.name + '.tmp')
    temporary.write_text(json.dumps({'controls': results, 'counts': counts},
                                    indent=2) + '\n')
    temporary.replace(target)
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()),
          flush=True)


def invalidate(results, reason):
    for row in results:
        row.update(status='INVALID', reason=row['reason'] + '; ' + reason)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path)
    parser.add_argument('--only', action='append')
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    known = {row[0] for row in MUTANT_ROWS}
    if args.only and (set(args.only) - known or len(args.only) != len(set(args.only))):
        print('unknown or duplicate --only name', file=sys.stderr)
        return 2
    selected = [row for row in MUTANT_ROWS if not args.only or row[0] in args.only]

    if args.check:
        for name, path, before, after, _test, _message in selected:
            apply_mutation(name, path.read_text(), before, after)
        print(f'{len(selected)} anchors: ok')
        return 0
    if args.output is None:
        print('--output is required unless --check is used', file=sys.stderr)
        return 2

    output = new_output_directory(args.output)
    env = dict(os.environ)
    env['PYTHONDONTWRITEBYTECODE'] = '1'
    # No cargo subprocess is launched: the bound unittest is a Python child.
    results = []
    try:
        with mutating_sources(ROOT, [SUITE, SQLITE]) as originals:
            texts = {path: originals[path].decode() for path in originals}
            # Preflight every selected anchor before any positive/mutant run:
            # an expired anchor aborts as INVALID, never SURVIVED.
            for name, path, before, after, _test, _message in selected:
                apply_mutation(name, texts[path], before, after)
            for name, path, before, after, test, message in selected:
                bound = bound_test(test)
                status, reason = 'INVALID', 'not executed'
                try:
                    if not passed(run_test(test, env), bound):
                        reason = 'positive control did not pass'
                    else:
                        # fix-r5 (a5-shaped): publish goes through
                        # write_source_atomic (temp file + one rename),
                        # never a truncating in-place write -- a SIGKILL
                        # mid-write leaves the old or the new bytes, not half
                        # a file (#1903; the #3088 fix-r6 same-family shape).
                        write_source_atomic(
                            ROOT, path,
                            texts[path].replace(before, after, 1).encode('utf-8'))
                        status, reason = classify(run_test(test, env), bound, message)
                except Exception as error:
                    status, reason = 'INVALID', f'{type(error).__name__}: {error}'
                finally:
                    write_source_atomic(ROOT, path, originals[path])
                    if path.read_bytes() != originals[path]:
                        raise RuntimeError(f'#1076 byte restoration failed: {path}')
                try:
                    if not passed(run_test(test, env), bound):
                        status, reason = 'INVALID', reason + '; restored bound test did not pass'
                except Exception as error:
                    status, reason = (
                        'INVALID', reason + f'; restored: {type(error).__name__}: {error}')
                results.append({'mutation': name, 'test': test,
                                'status': status, 'reason': reason})
                print(f'{name}: {status}: {reason}', flush=True)
    except BaseException as error:
        reason = f'source guard: {type(error).__name__}: {error}'
        invalidate(results, reason)
        for name, _path, _before, _after, test, _message in selected[len(results):]:
            results.append({'mutation': name, 'test': test,
                            'status': 'INVALID', 'reason': reason})
        publish(output, results)
        if isinstance(error, SystemExit):
            raise SystemExit(2) from error
        return 2
    publish(output, results)
    return int(not results or any(row['status'] != 'KILLED' for row in results))


if __name__ == '__main__':
    raise SystemExit(main())
