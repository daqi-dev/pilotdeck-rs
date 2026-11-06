"""Toolchain starts are mocked; watchdog controls run only this Python interpreter."""
import ast
import hashlib
import importlib.util
import os
from collections import Counter
from pathlib import Path
import re
import stat
import subprocess
import tempfile
import tomllib
import types
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('metadata_io_driver', Path(__file__).with_name('check_metadata_io.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class EnvironmentControls(unittest.TestCase):
    nextest_command = ('cargo nextest run --workspace --no-fail-fast --profile ci '
                       '--config-file ../.config/nextest.toml')
    doctest_command = 'cargo test --workspace --doc'
    negative_controls_command = ('python3 -I src/pilotdeck-gateway/tests/fixtures/'
                                 'session-assembly-1316/negative_controls.py')
    anchor_selfcheck_command = ('python3 -I src/pilotdeck-gateway/tests/fixtures/'
                                'anchor_selfcheck.py --anchors-only')
    self_check_command = negative_controls_command + ' --self-check'
    # #1692: the self-check is itself a guard, so CI also runs its meta controls
    # (each deletes one guard from a copy of the fixture; every case must go red).
    self_check_controls_command = negative_controls_command + ' --self-check-controls'
    # #1525: the workflow may carry exactly one step-level env block, and the ci
    # profile exactly one override. Both are inputs to the tests (a permission
    # switch; replaying the output of tests that can skip), never to the
    # scheduler — the drift guards below pin their full text, not just a prefix.
    permission_faults_env = ['PDRS_REQUIRE_PERMISSION_FAULTS: "1"']
    nextest_success_output_filter = (
        'test(=case_insensitive_cwd_spelling_writes_and_edits) | '
        'test(=nfd_directory_with_nfc_cwd_writes_and_edits) | '
        'test(=permission_denied_is_not_reported_as_outside) | '
        'test(=grep_outputs_match_real_ts) | '
        'test(=clear_failing_inside_delete_entries_rearms_indexed_rows) | '
        'test(=dump_request_write_failure_semantics) | '
        'test(=committed_marker_with_unreadable_manifest_republishes) | '
        'test(=scenarios_match_ts_outcomes_and_trees) | '
        'test(=real_ts_tool_budget_and_apply_tool_results_replay) | '
        'test(=skills::tests::unreadable_scan_child_matches_real_ts_before_denial_and_after_restore) | '
        'test(=skills::tests::real_ts_scenarios_match_outcomes_and_file_trees) | '
        'test(=runtime::tools::builtin::grep::tests::files_sort_keeps_submillisecond_mtime_precision) | '
        'test(=project_files::tests::node_round4_unreadable_huge_file_reports_open_error_first) | '
        'test(=project_files::tests::special_files_do_not_block_and_unreadable_files_still_list) | '
        'test(=project_files::tests::native_read_errors_match_node_ws_responses) | '
        'test(=dispatch::tests::list_projects_preserves_real_node_scandir_error_suppression) | '
        'test(=runtime::cron::tests::migration_blocked_dir_tasks_eacces_matches_ts) | '
        'test(=runtime::cron::tests::migration_non_utf8_home_keeps_one_physical_key)')

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name).resolve()
        (self.root / 'rust-toolchain.toml').write_text('[toolchain]\nchannel="1.99"\n')
        self.home = self.root / 'original-home'
        (self.home / '.cargo').mkdir(parents=True)
        self.toolchain = self.home / '.rustup/toolchains/1.99-mock-host'
        (self.toolchain / 'bin').mkdir(parents=True)
        for name in ('cargo', 'rustc', 'rustdoc', 'cargo-clippy', 'clippy-driver', 'rustfmt'):
            path = self.toolchain / 'bin' / name
            path.write_text('mock only; never execute\n')
            path.chmod(0o755)
        lib = self.toolchain / 'lib/rustlib'
        (lib / 'mock-host/lib').mkdir(parents=True)
        (lib / 'components').write_text('\n'.join(name+'-mock-host' for name in
            ('cargo', 'rustc', 'rust-std', 'clippy-preview', 'rustfmt-preview')))
        self.original = dict(HOME=str(self.home), PATH='/original/bin', RUSTC_WRAPPER='untrusted-wrapper',
                             RUSTC_WORKSPACE_WRAPPER='untrusted-workspace-wrapper',
                             CARGO_BUILD_RUSTC_WRAPPER='untrusted-build-wrapper',
                             CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='untrusted-build-workspace-wrapper',
                             RUSTUP_TOOLCHAIN='missing-inherited-toolchain')

    def test_node_home_never_flows_into_cargo_and_executables_are_explicit(self):
        with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('no process allowed')) as process:
            preflight = driver.toolchain_preflight(self.toolchain, self.original, self.root)
            cargo, node = driver.environments(self.original, preflight, self.root/'scratch', self.root/'target')
            self.assertEqual(cargo['HOME'], str(self.home))
            self.assertEqual(node['HOME'], str(self.root/'scratch/isolation/home'))
            self.assertNotEqual(cargo['HOME'], node['HOME'])
            self.assertEqual(cargo['CARGO_HOME'], str(self.home/'.cargo'))
            self.assertEqual(cargo['RUSTUP_HOME'], str(self.home/'.rustup'))
            self.assertEqual(cargo['RUSTUP_AUTO_INSTALL'], '0')
            self.assertEqual(cargo['RUSTC'], str(self.toolchain/'bin/rustc'))
            self.assertEqual(preflight['binaries']['cargo']['path'], str(self.toolchain/'bin/cargo'))
            for key in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
                        'CARGO_BUILD_RUSTC_WRAPPER', 'CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER'):
                self.assertEqual(cargo[key], '')
            self.assertNotIn('RUSTUP_TOOLCHAIN', cargo)
            process.assert_not_called()

    def test_original_home_spelling_is_preserved_even_when_it_is_a_symlink(self):
        alias = self.root/'home-alias'
        alias.symlink_to(self.home, target_is_directory=True)
        original = dict(self.original, HOME=str(alias))
        with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('no process allowed')) as process:
            preflight = driver.toolchain_preflight(self.toolchain, original, self.root)
            cargo, _ = driver.environments(original, preflight, self.root/'scratch', self.root/'target')
            self.assertEqual(cargo['HOME'], str(alias))
            process.assert_not_called()

    def test_missing_installation_is_rejected_before_any_proxy(self):
        with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('proxy started')) as process:
            with self.assertRaises((FileNotFoundError, RuntimeError)):
                driver.toolchain_preflight(self.toolchain/'missing', self.original, self.root)
            process.assert_not_called()

    def test_incident_or_redirected_installation_is_rejected_without_starting_it(self):
        with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('proxy started')) as process:
            outside = self.root / 'scratch/downloaded'
            outside.mkdir(parents=True)
            with self.assertRaises(RuntimeError):
                driver.toolchain_preflight(outside, self.original, self.root)
            cargo = self.toolchain/'bin/cargo'
            cargo.unlink()
            cargo.symlink_to(self.root/'nonoriginal-proxy')
            with self.assertRaises(RuntimeError):
                driver.toolchain_preflight(self.toolchain, self.original, self.root)
            process.assert_not_called()

    def test_missing_component_fails_before_any_proxy(self):
        with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('proxy started')) as process:
            (self.toolchain/'lib/rustlib/components').write_text('cargo-mock-host\n')
            with self.assertRaises(RuntimeError):
                driver.toolchain_preflight(self.toolchain, self.original, self.root)
            process.assert_not_called()

    def test_remaining_preflight_failures_never_start_a_process(self):
        changes = ('relative-home', 'missing-cargo-home', 'wrong-channel', 'missing-std', 'not-executable')
        for change in changes:
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temp:
                # Each branch gets an independent mock installation.
                case = EnvironmentControls('test_missing_component_fails_before_any_proxy')
                case.setUp()
                self.addCleanup(case.doCleanups)
                original = dict(case.original)
                if change == 'relative-home': original['HOME'] = 'relative'
                if change == 'missing-cargo-home': original['CARGO_HOME'] = str(Path(temp)/'absent')
                if change == 'wrong-channel': (case.root/'rust-toolchain.toml').write_text('[toolchain]\nchannel="1.98"\n')
                if change == 'missing-std': (case.toolchain/'lib/rustlib/mock-host/lib').rmdir()
                if change == 'not-executable': (case.toolchain/'bin/cargo').chmod(0o644)
                with patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('no process allowed')) as process:
                    with self.assertRaises((OSError, RuntimeError)):
                        driver.toolchain_preflight(case.toolchain, original, case.root)
                    process.assert_not_called()

    def test_environment_ignores_rustflags_cargo_and_node_injection(self):
        original = dict(self.original, RUSTFLAGS='--bad-flag', CARGO_ENCODED_RUSTFLAGS='bad',
                        CARGO_BUILD_RUSTFLAGS='bad', CARGO_INCREMENTAL='bad', CARGO_HOME_POISON='bad',
                        LD_PRELOAD='spy', DYLD_INSERT_LIBRARIES='spy', NODE_OPTIONS='--require spy', NODE_PATH='spy')
        pf = driver.toolchain_preflight(self.toolchain, original, self.root)
        cargo, node = driver.environments(original, pf, self.root/'scratch', self.root/'target', '/fixed/node', '/fixed/git')
        for key in ('RUSTFLAGS', 'CARGO_ENCODED_RUSTFLAGS', 'CARGO_BUILD_RUSTFLAGS', 'CARGO_INCREMENTAL',
                    'CARGO_HOME_POISON', 'LD_PRELOAD', 'DYLD_INSERT_LIBRARIES', 'NODE_OPTIONS', 'NODE_PATH'):
            self.assertNotIn(key, cargo)
            self.assertNotIn(key, node)
        self.assertIn('/fixed', node['PATH'])
        self.assertNotIn('CARGO_HOME', node)
        self.assertNotIn('RUSTUP_HOME', node)

    def test_source_inventory_is_exactly_the_fifteen_required_files(self):
        expected = {
            'src/pilotdeck-gateway/src/metadata.rs', 'src/pilotdeck-gateway/src/metadata/tests.rs',
            'src/pilotdeck-gateway/src/pairing/tests.rs', 'src/pilotdeck-gateway/tests/record_metadata_io.mts',
            'src/pilotdeck-gateway/tests/check_metadata_io.py', 'src/pilotdeck-gateway/tests/test_metadata_io_driver.py',
            'src/pilotdeck-gateway/tests/baseline_source_guard.mjs', 'src/pilotdeck-gateway/tests/test_baseline_source_guard.mjs',
            'src/pilotdeck-gateway/tests/fixtures/metadata/describe-io-errors.json',
            '.github/workflows/ci.yml', 'rust-toolchain.toml', 'src/Cargo.toml', 'src/Cargo.lock',
            'src/pilotdeck-gateway/Cargo.toml',
            'src/pilotdeck-gateway/tests/test_metadata_observer.mjs',
        }
        self.assertEqual(len(driver.SOURCE_FILES), 15)
        self.assertEqual(set(driver.SOURCE_FILES), expected)

    def ci_jobs(self, issue='#904'):
        text = (Path(__file__).resolve().parents[3]/'.github/workflows/ci.yml').read_text()
        self.assertEqual(text.count('\njobs:\n'), 1, f'{issue} one jobs mapping')
        jobs = {}
        name = None
        for line in text.split('\njobs:\n', 1)[1].splitlines(keepends=True):
            if not line.strip() or line.lstrip().startswith('#'):
                if name is not None:
                    jobs[name] += line
                continue
            header = re.fullmatch(r'  ([\w-]+):\s*(?:#.*)?\n?', line)
            if header:
                name = header[1]
                self.assertNotIn(name, jobs, f'{issue} unique job names')
                jobs[name] = ''
            else:
                self.assertTrue(name is not None and line.startswith('    '),
                                f'{issue} unsupported job declaration')
                jobs[name] += line
        # New jobs must explicitly register their Node setup and merge gating here:
        # scope runs Python/git only and gates the four heavy jobs (#1199);
        # metadata-observer/rust/a51-coverage need their own Node and check gates;
        # negative-controls is Python/cargo only (#1632) and is gated like a heavy job;
        # check only aggregates; gateway-schema-baseline has Node and its own required check.
        self.assertEqual(set(jobs), {'scope', 'metadata-observer', 'check', 'rust', 'a51-coverage',
                                    'negative-controls', 'gateway-schema-baseline'},
                         f'{issue} exact CI job inventory')
        return jobs

    def ci_steps(self, name, body, issue='#904'):
        self.assertEqual(len(re.findall(r'^    steps:$', body, re.M)), 1,
                         f'{issue} {name} has one steps sequence')
        sequence = body.split('    steps:\n', 1)[1]
        items = list(re.finditer(r'^      - +(.*)$', sequence, re.M))
        self.assertTrue(items, '#919 steps must be an explicit block sequence')
        self.assertTrue(all(not line.strip() or line.lstrip().startswith('#')
                            for line in sequence[:items[0].start()].splitlines()),
                        '#919 unsupported step declaration')
        steps = []
        for index, item in enumerate(items):
            end = items[index + 1].start() if index + 1 < len(items) else len(sequence)
            # YAML permits any positive number of spaces after "-". Canonicalize
            # that item's key column so every sibling guard sees the same fields.
            extra = item.start(1) - item.start() - 8
            lines = [item[1]]
            for line in sequence[item.end():end].removeprefix('\n').splitlines():
                if not line.strip() or line.lstrip().startswith('#'):
                    lines.append(line)
                    continue
                self.assertTrue(line.startswith(' ' * (8 + extra)),
                                '#919 unsupported step indentation')
                lines.append(line[extra:])
            step = '\n'.join(lines) + '\n'
            self.ci_step_fields(step, issue)  # Reject every unparsed key, not just run lookalikes.
            canonical = []
            for line in step.splitlines():
                if line.strip() and not line.lstrip().startswith('#') and (
                        not line.startswith(' ') or len(line) - len(line.lstrip(' ')) == 8):
                    key, value = line.split(':', 1)
                    line = key.strip().strip("\"'") + ':' + value
                    if canonical:
                        line = '        ' + line
                canonical.append(line)
            steps.append('\n'.join(canonical) + '\n')
        return steps

    def ci_step_fields(self, step, issue='#904'):
        fields = {}
        lines = step.splitlines()
        i = 0
        allowed = {'name', 'id', 'if', 'uses', 'run', 'env', 'with', 'working-directory',
                   'shell', 'continue-on-error', 'timeout-minutes'}
        while i < len(lines):
            line = lines[i]
            i += 1
            if not line.strip() or line.lstrip().startswith('#'):
                continue
            key = re.fullmatch(r"""(?:        |)([a-z][a-z-]*|'[a-z][a-z-]*'|"[a-z][a-z-]*") *:(.*)""", line)
            self.assertIsNotNone(key, '#919 unsupported step key syntax')
            name = key[1].strip("\"'")
            self.assertIn(name, allowed, '#919 unsupported step key')
            self.assertNotIn(name, fields, f'{issue} step has at most one run field' if name == 'run'
                             else '#919 duplicate step key')
            block = []
            while i < len(lines):
                line = lines[i]
                if line.strip() and len(line) - len(line.lstrip(' ')) <= 8:
                    break
                if line.strip():
                    block.append(line.strip())
                i += 1
            fields[name] = (key[2].strip(), block)
        return fields

    def ci_run_commands(self, step, issue='#904'):
        run = self.ci_step_fields(step, issue).get('run')
        if run is None:
            return []
        value, block = run
        if re.fullmatch(r'[|>][0-9]*[+-]?\s*(#.*)?', value):
            self.assertTrue(block, f'{issue} run block must not be empty')
            return block
        self.assertTrue(value and value[0] not in '|>[]{}&*!%@#'
                        and not value.startswith('${{')
                        and not (value.startswith('"') and '\\' in value),
                        f'{issue} unsupported run scalar')
        self.assertFalse(block, f'{issue} single-line run cannot have a continuation')
        return [value]

    def test_required_check_gates_observer_failure_instead_of_skipping(self):
        jobs = self.ci_jobs()
        heavy = ('metadata-observer', 'rust', 'a51-coverage', 'negative-controls')
        for name in jobs:
            self.assertIn(name, jobs, f'#904 required job {name} exists')
            self.assertNotRegex(jobs[name], r'''(?m)^\s*(?:continue-on-error|'continue-on-error'|"continue-on-error")\s*:''',
                                f'#904 {name} cannot ignore failure')
        check = jobs['check']
        self.assertEqual(re.findall(r'^    needs: (.*)$', check, re.M),
                         ['[scope, ' + ', '.join(heavy) + ']'], '#904 exact required needs')
        self.assertEqual(re.findall(r'^    if: (.*)$', check, re.M),
                         ['${{ always() }}'], '#904 check always runs')
        steps = self.ci_steps('check', check)
        self.assertEqual(len(steps), 5, '#904 exactly five rejection steps')

        def gate_steps(condition):
            return [step for step in steps
                    if re.findall(r'^        if: (.*)$', step, re.M) == [condition]]

        # #1199：scope 自身失败（含取消/跳过）必须拒绝合并——它是唯一允许放宽 #904 的来源。
        scope_gates = gate_steps("${{ always() && needs.scope.result != 'success' }}")
        self.assertEqual(len(scope_gates), 1, '#904 scope failure must reject the merge')
        self.assertEqual(re.findall(r'^        run: (.*)$', scope_gates[0], re.M),
                         ['exit 1'], '#904 scope rejection exits one')
        for job in heavy:
            condition = ('${{ always() && needs.' + job + ".result != 'success' && !(needs." + job
                         + ".result == 'skipped' && needs.scope.outputs.docs_only == 'true') }}")
            gates = gate_steps(condition)
            self.assertEqual(len(gates), 1,
                             f'#904 {job} rejects every non-success beyond the docs-only skip')
            self.assertEqual(re.findall(r'^        run: (.*)$', gates[0], re.M),
                             ['exit 1'], f'#904 {job} rejection exits one')
        # #1199：三个重 job 的跳过只认 scope 的输出；needs / if 是精确字符串，不许加别的条件。
        for job in heavy:
            prefix = jobs[job].split('    steps:\n', 1)[0]
            gating = re.findall(r"""(?m)^    ['"]?(if|needs|continue-on-error)['"]?\s*:\s*(.*)$""",
                                prefix)
            self.assertEqual(gating, [('needs', '[scope]'),
                                      ('if', "${{ needs.scope.outputs.docs_only != 'true' }}")],
                             f'#1199 {job} skips only on the scope classifier')
        # #1199：scope job 自身无条件运行，否则整条放宽路径失去来源。
        scope_prefix = jobs['scope'].split('    steps:\n', 1)[0]
        self.assertEqual(re.findall(r"""(?m)^    ['"]?(?:if|needs|continue-on-error)['"]?\s*:""",
                                   scope_prefix), [], '#1199 scope job runs unconditionally')
        observer_steps = self.ci_steps('metadata-observer', jobs['metadata-observer'])
        self.assertTrue(any(self.ci_run_commands(step)
                            == ['node src/pilotdeck-gateway/tests/test_metadata_observer.mjs']
                            for step in observer_steps), '#904 observer generation control runs')

    def test_ci_rejects_docs_only_gate_drift(self):
        workflow = self.ci_workflow_fixture()
        jobs = self.ci_jobs()
        heavy = ('metadata-observer', 'rust', 'a51-coverage', 'negative-controls')
        cases = []

        def change(label, old, new, message):
            self.assertEqual(workflow.count(old), 1, f'#1199 unique gate mutation anchor: {label}')
            cases.append((label, workflow.replace(old, new, 1), message))

        def change_job(label, owner, old, new, message):
            self.assertEqual(jobs[owner].count(old), 1, f'#1199 unique job gate anchor: {label}')
            cases.append((label, workflow.replace(
                jobs[owner], jobs[owner].replace(old, new, 1), 1), message))

        # #1199 规格 4：去掉 docs_only 条件后 skipped 被接受 → 必须失败。
        for job in heavy:
            strict = '${{ always() && needs.' + job + ".result != 'success' }}"
            relaxed = ('${{ always() && needs.' + job + ".result != 'success' && !(needs." + job
                       + ".result == 'skipped' && needs.scope.outputs.docs_only == 'true') }}")
            change(f'{job}-carve-out-dropped', relaxed, strict,
                   f'#904 {job} rejects every non-success beyond the docs-only skip')
            change(f'{job}-any-skip-accepted', relaxed,
                   '${{ always() && needs.' + job + ".result != 'success' && !(needs."
                   + job + ".result == 'skipped') }}",
                   f'#904 {job} rejects every non-success beyond the docs-only skip')
        # #1199 规格 4：scope 失败时 check 仍通过 → 必须失败。
        scope_step = ('      - name: Require scope success\n'
                      "        if: ${{ always() && needs.scope.result != 'success' }}\n"
                      '        working-directory: .\n'
                      '        run: exit 1\n')
        change('scope-gate-removed', scope_step, '', '#904 exactly five rejection steps')
        change('scope-gate-relaxed', "needs.scope.result != 'success'",
               "needs.scope.result == 'failure'", '#904 scope failure must reject the merge')
        change('check-needs-drops-scope',
               'needs: [scope, metadata-observer, rust, a51-coverage, negative-controls]',
               'needs: [metadata-observer, rust, a51-coverage, negative-controls]',
               '#904 exact required needs')
        # #1199 规格 4：重 job 加 continue-on-error → 必须失败。
        for owner in heavy:
            change_job(f'{owner}-continue-on-error', owner, '    steps:\n',
                       '    continue-on-error: true\n    steps:\n',
                       f'#904 {owner} cannot ignore failure')
        # needs / if 是精确字符串：去掉、反转、放宽、换依赖都必须被拒。
        docs_only_if = "    if: ${{ needs.scope.outputs.docs_only != 'true' }}\n"
        for owner in heavy:
            change_job(f'{owner}-needs-dropped', owner, '    needs: [scope]\n', '',
                       f'#1199 {owner} skips only on the scope classifier')
            change_job(f'{owner}-if-dropped', owner, docs_only_if, '',
                       f'#1199 {owner} skips only on the scope classifier')
            change_job(f'{owner}-if-inverted', owner, docs_only_if,
                       "    if: ${{ needs.scope.outputs.docs_only == 'true' }}\n",
                       f'#1199 {owner} skips only on the scope classifier')
            change_job(f'{owner}-if-broadened', owner, docs_only_if, '    if: ${{ true }}\n',
                       f'#1199 {owner} skips only on the scope classifier')
            change_job(f'{owner}-needs-broadened', owner, '    needs: [scope]\n',
                       '    needs: [scope, gateway-schema-baseline]\n',
                       f'#1199 {owner} skips only on the scope classifier')
        # scope job 自身必须无条件，否则放宽路径失去唯一来源。
        change('scope-job-conditional', '  scope:\n', '  scope:\n    if: false\n',
               '#1199 scope job runs unconditionally')
        change('scope-job-continue-on-error', '  scope:\n', '  scope:\n    continue-on-error: true\n',
               '#904 scope cannot ignore failure')
        self.check_ci_mutations(cases,
                                'test_required_check_gates_observer_failure_instead_of_skipping', '#1199')

    def test_scope_job_runs_its_own_self_test_then_classifier(self):
        jobs = self.ci_jobs()
        steps = self.ci_steps('scope', jobs['scope'])
        checkout_lines = [line.strip() for line in steps[0].splitlines()
                          if line.strip() and not line.lstrip().startswith('#')]
        # 浅克隆拿不到 base..head 两点 diff 的历史，分类器会退化为 fail-closed 的全量。
        self.assertEqual(checkout_lines, ['uses: actions/checkout@v4', 'with:', 'fetch-depth: 0'],
                         '#1199 scope checkout keeps full history')
        self.assertEqual([self.ci_run_commands(step) for step in steps[1:]],
                         [['python3 -I scripts/test-ci-scope.py'], ['python3 -I scripts/ci_scope.py']],
                         '#1199 scope runs the self-test then the classifier')
        classify = self.ci_step_fields(steps[2])
        self.assertEqual(classify.get('id'), ('classify', []),
                         '#1199 classifier step is the docs_only source')
        self.assertEqual(classify.get('env'),
                         ('', ['EVENT_NAME: ${{ github.event_name }}',
                               'PR_BASE: ${{ github.event.pull_request.base.sha }}',
                               'PR_HEAD: ${{ github.event.pull_request.head.sha }}',
                               'MG_BASE: ${{ github.event.merge_group.base_sha }}',
                               'MG_HEAD: ${{ github.event.merge_group.head_sha }}']),
                         '#1199 classifier receives the exact event ranges')
        for step in steps:
            fields = self.ci_step_fields(step)
            for key in ('if', 'continue-on-error'):
                self.assertNotIn(key, fields, '#1199 scope steps stay unconditional')
        self.assertEqual(re.findall(r'^    outputs:\n((?:      [^\n]*\n)+)', jobs['scope'], re.M),
                         ['      docs_only: ${{ steps.classify.outputs.docs_only }}\n'],
                         '#1199 scope exports exactly docs_only')

    def test_ci_rejects_scope_job_drift(self):
        workflow = self.ci_workflow_fixture()
        jobs = self.ci_jobs()
        cases = []

        def change(label, old, new, message):
            self.assertEqual(jobs['scope'].count(old), 1, f'#1199 unique scope mutation anchor: {label}')
            cases.append((label, workflow.replace(
                jobs['scope'], jobs['scope'].replace(old, new, 1), 1), message))

        # 浅克隆 / 过滤克隆拿不到 base..head 历史，分类器会静默退化成 fail-closed 全量。
        change('shallow-checkout', 'fetch-depth: 0', 'fetch-depth: 1',
               '#1199 scope checkout keeps full history')
        change('checkout-filter', 'fetch-depth: 0', 'fetch-depth: 0\n          filter: blob:none',
               '#1199 scope checkout keeps full history')
        # 自测步与分类步都不许跳过或忽略失败。
        selftest = ('      - name: 分类器自测（正负例与内存内变异负控）\n'
                    '        run: python3 -I scripts/test-ci-scope.py\n')
        change('selftest-removed', selftest, '', '#1199 scope runs the self-test then the classifier')
        change('selftest-conditional', selftest, selftest + '        if: false\n',
               '#1199 scope steps stay unconditional')
        change('selftest-continue-on-error', selftest, selftest + '        continue-on-error: true\n',
               '#1199 scope steps stay unconditional')
        change('classifier-command-changed', 'run: python3 -I scripts/ci_scope.py', 'run: echo true',
               '#1199 scope runs the self-test then the classifier')
        change('classifier-id-dropped', '        id: classify\n', '',
               '#1199 classifier step is the docs_only source')
        change('classifier-env-dropped-mg-base',
               '          MG_BASE: ${{ github.event.merge_group.base_sha }}\n', '',
               '#1199 classifier receives the exact event ranges')
        change('outputs-dropped',
               '    outputs:\n      docs_only: ${{ steps.classify.outputs.docs_only }}\n', '',
               '#1199 scope exports exactly docs_only')
        self.check_ci_mutations(cases, 'test_scope_job_runs_its_own_self_test_then_classifier', '#1199')

    def test_ci_scope_whitelist_is_exactly_the_single_board_path(self):
        path = Path(__file__).resolve().parents[3] / 'scripts/ci_scope.py'
        source = path.read_bytes().decode()
        self.assertEqual(re.findall(r'^DOCS_ONLY_PATH = .*$', source, re.M),
                         ["DOCS_ONLY_PATH = 'state/board.md'"],
                         '#1199 docs-only whitelist is exactly the single board path')
        # 行为层的完整正负例由 scripts/test-ci-scope.py 在 scope job 里跑；这里 exec 读到的
        # 源码做行为抽查，防「文本钉住了、模块本身坏了」——用 exec 而非 importlib 是为了让
        # read_bytes 补丁下的变异源码也能流进行为层（fix-r2 的守卫变异因此可杀）。
        module = types.ModuleType('ci_scope_guard')
        module.__dict__['__name__'] = 'ci_scope_guard'
        exec(compile(source, str(path), 'exec'), module.__dict__)  # noqa: S102 —— 只进隔离命名空间
        self.assertTrue(module.paths_are_docs_only(['state/board.md']),
                       '#1199 board-only path classifies true')
        self.assertFalse(module.paths_are_docs_only(['state/board.md', 'docs/a.md']),
                         '#1199 board plus any other file is not docs-only')
        # #1199 fix-r2（DeepSeek P2-2）：以 board.md 为前缀但不等于它的路径必须判 False——
        # 全等退化成 startswith 时这些样例全部漏判（变异负控见 test_ci_rejects_ci_scope_whitelist_drift）。
        self.assertFalse(module.paths_are_docs_only(['state/board.md.bak']),
                         '#1199 board.md.bak prefix path is not docs-only')
        self.assertFalse(module.paths_are_docs_only(['state/board.mdx']),
                         '#1199 board.mdx prefix path is not docs-only')
        self.assertFalse(module.paths_are_docs_only(['state/board.md', 'state/board.md/x.py']),
                         '#1199 path under board.md is not docs-only')
        self.assertFalse(module.paths_are_docs_only(['state/decisions-20261031-x.md']),
                         '#1199 ruler-input state file is not docs-only')
        self.assertFalse(module.paths_are_docs_only([]), '#1199 empty path list is not docs-only')

    def test_ci_rejects_ci_scope_whitelist_drift(self):
        source = (Path(__file__).resolve().parents[3] / 'scripts/ci_scope.py').read_bytes()
        line = b"DOCS_ONLY_PATH = 'state/board.md'\n"
        self.assertEqual(source.count(line), 1, '#1199 unique whitelist mutation anchor')
        # #1199 fix-r1（收窄）：白名单退回目录前缀 / 加宽成元组 / 值改成目录 → 必须失败
        # （行为层由 scripts/test-ci-scope.py 的同名变异再加一道）。
        cases = [(name, source.replace(line, changed, 1),
                  '#1199 docs-only whitelist is exactly the single board path')
                 for name, changed in (
                ('whitelist-returns-to-prefixes',
                 b"DOCS_PREFIXES = ('state/', '.42cog/', 'docs/')\n"),
                ('whitelist-widens-to-tuple',
                 b"DOCS_ONLY_PATH = ('state/board.md', 'docs/')\n"),
                ('whitelist-widens-to-dir',
                 b"DOCS_ONLY_PATH = 'state/'\n"))]
        # #1199 fix-r2（DeepSeek P2-2）：全等退化成 startswith → 前缀路径（board.md.bak 等）
        # 漏判成 docs-only，必须被上面的行为负例抓住（经 read_bytes 补丁流进行为抽查）。
        anchor = b'return bool(paths) and all(path == DOCS_ONLY_PATH for path in paths)\n'
        self.assertEqual(source.count(anchor), 1, '#1199 unique equality mutation anchor')
        cases.append(('equality-to-prefix',
                      source.replace(anchor, b'return bool(paths) and all(path.startswith('
                                             b'DOCS_ONLY_PATH) for path in paths)\n', 1),
                      '#1199 board.md.bak prefix path is not docs-only'))
        self.check_ci_mutations(cases, 'test_ci_scope_whitelist_is_exactly_the_single_board_path',
                                '#1199', reader='read_bytes')

    def test_required_job_runs_shared_isolation_regressions(self):
        command = 'python3 -B src/pilotdeck-gateway/tests/fixtures/test_negative_adaptive_isolation.py'
        jobs = self.ci_jobs('#994')
        steps = self.ci_steps('metadata-observer', jobs['metadata-observer'], '#994')
        matching = [step for step in steps if self.ci_run_commands(step, '#994') == [command]]
        self.assertEqual(len(matching), 1, '#994 required CI job must run isolation regressions')
        fields = self.ci_step_fields(matching[0], '#994')
        for key in ('if', 'continue-on-error'):
            self.assertNotIn(key, fields, '#994 isolation CI step must be unconditional and required')
        self.assertIn(fields.get('working-directory'), (None, ('.', [])),
                      '#994 isolation CI step must run from repository root')

    def test_negative_controls_job_runs_the_gateway_fixture(self):
        # #1632 P2-3：这个 job 存在的唯一理由是让夹具真的跑起来；删掉执行步、
        # 让 --self-check 掉队、把 target 指回 src/ 或丢掉历史克隆都必须被判红，
        # 否则「接进 CI」只是一行没人绑定的 YAML。
        jobs = self.ci_jobs('#1632')
        body = jobs['negative-controls']
        steps = self.ci_steps('negative-controls', body, '#1632')
        checkout = self.ci_step_fields(steps[0], '#1632')
        self.assertEqual(checkout.get('uses'), ('actions/checkout@v4', []),
                         '#1632 negative-controls checks out the repository')
        self.assertIn('fetch-depth: 0', steps[0],
                      '#1632 mutating_sources needs HEAD history for its pristine check')
        self.assertEqual(re.findall(r'^      CARGO_TARGET_DIR: (.*)$', body, re.M),
                         ['${{ github.workspace }}/_build/cargo-target'],
                         '#1632 the scanner builds into the in-tree _build target')
        commands = [self.ci_run_commands(step, '#1632') for step in steps]
        self.assertIn([self.anchor_selfcheck_command, self.self_check_command,
                       self.self_check_controls_command], commands,
                      '#1632 anchors, the slot guard and its meta controls are checked '
                      'before any mutation')
        self.assertIn([self.negative_controls_command + ' "$RUNNER_TEMP/negctl-1316"'], commands,
                      '#1632 the fixture itself runs in CI')
        for step in steps:
            # 只约束跑命令的步骤：证据上传步用 `if: always()` 是刻意的（失败也要留证据），
            # 而 self-check / 夹具执行 / 工具链步骤一旦有条件就会被静默跳过。
            if 'run' not in self.ci_step_fields(step, '#1632'):
                continue
            fields = self.ci_step_fields(step, '#1632')
            for key in ('if', 'continue-on-error'):
                self.assertNotIn(key, fields, '#1632 negative-control steps stay unconditional')
        # 还原 target 等于还原上一轮的扫描器二进制：`_ensure_workspace_scanner` 见到
        # 文件存在就复用，摘要冻在二进制里，改过扫描器输入的 PR 会拿到摘要不等的旧
        # 二进制并被判假红。只允许缓存 cargo 注册表（#1632 评审 P1）。
        self.assertNotIn('Swatinem/rust-cache', body,
                         '#1632 a restored target hands back a stale scanner binary')
        # 按路径语义钉，而不是只查一个缓存动作的名字：手写 actions/cache 把 target
        # 塞进 path 的写法与 rust-cache 一样危险，禁用名单抓不到（#1632 评审 P2）。
        # 按动作语义收全部拼写：`actions/cache/restore@v4` / `save@v4` 同样吃 `path:`，
        # 只认标准写法会漏掉「拆成 restore + save 两步」的复发（两家 r6 评审 P2）。
        caches = [step for step in steps
                  if self.ci_step_fields(step, '#1632').get('uses', ('', []))[0]
                  .startswith('actions/cache')]
        self.assertEqual(len(caches), 1, '#1632 exactly one cache action feeds the job')
        cache = self.ci_step_fields(caches[0], '#1632')
        self.assertEqual(cache.get('uses'), ('actions/cache@v4', []),
                         '#1632 the cache is declared on the cache action itself')
        block = cache.get('with', ('', []))[1]
        self.assertIn('path: |', block, '#1632 the cache declares its paths as a block')
        cached = []
        for line in block[block.index('path: |') + 1:]:
            if re.fullmatch(r'[a-z][a-z-]*:.*', line):
                break
            cached.append(line)
        self.assertEqual(cached, ['~/.cargo/registry', '~/.cargo/git'],
                         '#1632 cached paths are exactly the registry and git sources, never target')
        self.assertIn("key: negctl-cargo-${{ runner.os }}-${{ hashFiles('src/Cargo.lock') }}",
                      block, '#1632 the cache key follows the committed lockfile')
        # 夹具自己按 PDRS_NEGCTL_DEADLINE 收口，job 上界只作兜底，但必须显式
        # 且高于夹具预算（#1632 评审 P2 / #1692 §1）。
        self.assertIn('    timeout-minutes: 150\n', body,
                      '#1632 the job bounds its own wall clock')
        # 失败时最需要证据：夹具已经把 .log/.patch/summary.json 写好，别让 runner 丢掉。
        uploads = [step for step in steps if 'actions/upload-artifact@v4' in step]
        self.assertEqual(len(uploads), 1, '#1632 the run leaves its evidence behind')
        upload = self.ci_step_fields(uploads[0], '#1632')
        self.assertEqual(upload.get('if'), ('${{ always() }}', []),
                         '#1632 evidence is uploaded even when the run fails')
        self.assertIn('${{ runner.temp }}/negctl-1316', uploads[0],
                      '#1632 uploads the fixture output directory')

    def test_negative_controls_job_regressions_are_rejected(self):
        workflow = self.ci_workflow_fixture()
        body = self.ci_jobs('#1632')['negative-controls']
        anchor_pair = ('          ' + self.anchor_selfcheck_command + '\n'
                       '          ' + self.self_check_command + '\n'
                       '          ' + self.self_check_controls_command + '\n')
        run_step = '      - name: session-assembly-1316 负控夹具执行（#1316 / #1632）\n'
        cases = []

        def change(label, old, new, message):
            self.assertEqual(body.count(old), 1, f'#1632 unique job anchor: {label}')
            cases.append((label, workflow.replace(body, body.replace(old, new, 1), 1), message))

        change('fixture-step-removed',
               run_step + '        run: ' + self.negative_controls_command
               + ' "$RUNNER_TEMP/negctl-1316"\n', '',
               '#1632 the fixture itself runs in CI')
        change('fixture-step-conditional', run_step, run_step + '        if: false\n',
               '#1632 negative-control steps stay unconditional')
        change('fixture-step-continue-on-error', run_step,
               run_step + '        continue-on-error: true\n',
               '#1632 negative-control steps stay unconditional')
        change('self-check-dropped', anchor_pair, '          ' + self.anchor_selfcheck_command + '\n',
               '#1632 anchors, the slot guard and its meta controls are checked before any mutation')
        change('anchors-dropped', anchor_pair, '          ' + self.self_check_command + '\n',
               '#1632 anchors, the slot guard and its meta controls are checked before any mutation')
        change('shallow-checkout', '          fetch-depth: 0\n', '          fetch-depth: 1\n',
               '#1632 mutating_sources needs HEAD history for its pristine check')
        change('target-inside-src', 'CARGO_TARGET_DIR: ${{ github.workspace }}/_build/cargo-target',
               'CARGO_TARGET_DIR: ${{ github.workspace }}/src/target',
               '#1632 the scanner builds into the in-tree _build target')
        change('target-cache-reintroduced',
               '      - name: 锁定工具链\n        run: rustup show active-toolchain\n',
               '      - name: 锁定工具链\n        run: rustup show active-toolchain\n'
               '      - uses: Swatinem/rust-cache@v2.9.2\n'
               '        with:\n'
               '          workspaces: src -> ../_build/cargo-target\n',
               '#1632 a restored target hands back a stale scanner binary')
        change('target-in-cache-path',
               '            ~/.cargo/registry\n            ~/.cargo/git\n',
               '            ~/.cargo/registry\n            ~/.cargo/git\n'
               '            _build/cargo-target\n',
               '#1632 cached paths are exactly the registry and git sources, never target')
        change('cache-step-removed',
               '      - uses: actions/cache@v4\n        with:\n'
               '          path: |\n            ~/.cargo/registry\n            ~/.cargo/git\n'
               "          key: negctl-cargo-${{ runner.os }}-${{ hashFiles('src/Cargo.lock') }}\n",
               '', '#1632 exactly one cache action feeds the job')
        change('cache-restore-action-added',
               '      - name: 锁定工具链\n        run: rustup show active-toolchain\n',
               '      - name: 锁定工具链\n        run: rustup show active-toolchain\n'
               '      - uses: actions/cache/restore@v4\n        with:\n'
               '          path: _build/cargo-target\n          key: negctl-restore\n',
               '#1632 exactly one cache action feeds the job')
        change('cache-key-drift',
               "          key: negctl-cargo-${{ runner.os }}-${{ hashFiles('src/Cargo.lock') }}\n",
               '          key: negctl-cargo\n',
               '#1632 the cache key follows the committed lockfile')
        change('cache-path-inline-scalar',
               '          path: |\n            ~/.cargo/registry\n            ~/.cargo/git\n',
               '          path: ~/.cargo/registry\n',
               '#1632 the cache declares its paths as a block')
        change('job-timeout-removed', '    timeout-minutes: 150\n', '',
               '#1632 the job bounds its own wall clock')
        change('evidence-upload-conditional', '        if: ${{ always() }}\n',
               '        if: ${{ success() }}\n',
               '#1632 evidence is uploaded even when the run fails')
        upload_step = ('      - name: 上传负控证据（#1632）\n'
                       '        if: ${{ always() }}\n'
                       '        uses: actions/upload-artifact@v4\n'
                       '        with:\n'
                       '          name: negctl-1316\n'
                       '          path: ${{ runner.temp }}/negctl-1316\n'
                       '          if-no-files-found: warn\n')
        change('evidence-upload-dropped', upload_step, '',
               '#1632 the run leaves its evidence behind')
        self.check_ci_mutations(cases, 'test_negative_controls_job_runs_the_gateway_fixture', '#1632')

    def test_isolation_ci_regressions_are_rejected(self):
        workflow = self.ci_workflow_fixture()
        command = 'run: python3 -B src/pilotdeck-gateway/tests/fixtures/test_negative_adaptive_isolation.py'
        cases = [(name, workflow.replace(command, changed, 1), message) for name, changed, message in (
            ('isolation-ci-removed', 'run: true', '#994 required CI job must run isolation regressions'),
            ('isolation-ci-commented', '# ' + command, '#994 required CI job must run isolation regressions'),
            ('isolation-ci-skipped', 'if: false\n        ' + command,
             '#994 isolation CI step must be unconditional and required'),
            ('isolation-ci-ignored', 'continue-on-error: true\n        ' + command,
             '#994 isolation CI step must be unconditional and required'),
            ('isolation-ci-wrong-cwd', 'working-directory: src\n        ' + command,
             '#994 isolation CI step must run from repository root'))]
        self.check_ci_mutations(cases, 'test_required_job_runs_shared_isolation_regressions', '#994')

    def test_each_npm_job_sets_up_its_own_node_before_installing(self):
        runtime_jobs = set()
        for name, body in self.ci_jobs().items():
            steps = self.ci_steps(name, body)
            node_steps = [i for i, step in enumerate(steps)
                          if re.search(r'^(?:uses:|        uses:) actions/setup-node@v4$', step, re.M)
                          and not re.search(r'^(?:if:|        if:)', step, re.M)]
            for i, step in enumerate(steps):
                # Only run fields/scripts count; names, comments and env values do not.
                commands = self.ci_run_commands(step)
                if any(re.search(r'\b(npm|npx|node|yarn|pnpm|corepack)\b', command)
                       for command in commands if not command.lstrip().startswith('#')):
                    runtime_jobs.add(name)
                    self.assertTrue(any(node < i for node in node_steps),
                                    f'#904 {name} setup-node must precede Node commands in its own job')
        self.assertEqual(runtime_jobs, {'metadata-observer', 'rust', 'a51-coverage', 'gateway-schema-baseline'},
                         '#904 all four Node jobs are checked')

    def test_each_node_job_rejects_missing_late_or_conditional_setup(self):
        workflow = self.ci_workflow_fixture()
        cases = []
        for name, body in self.ci_jobs().items():
            if name in ('check', 'scope', 'negative-controls'):
                continue  # check 只聚合；scope 只跑 Python/git（#1199）；negative-controls 只跑 Python/cargo（#1632）
            setups = re.findall(r'      - uses: actions/setup-node@v4\n'
                                r'(?:        .*\n|          .*\n)*', body)
            self.assertEqual(len(setups), 1, f'#909 {name} unique setup anchor')
            setup = setups[0]
            missing = body.replace(setup, '', 1)
            for kind, changed in (
                    ('missing', missing), ('late', missing.rstrip() + '\n' + setup),
                    ('conditional', body.replace(setup, setup + '        if: false\n', 1))):
                cases.append((f'#909-{name}-{kind}', workflow.replace(body, changed, 1),
                              f'#904 {name} setup-node must precede Node commands in its own job'))
        self.check_ci_mutations(cases,
                               'test_each_npm_job_sets_up_its_own_node_before_installing', '#909')

    def ci_workflow_fixture(self):
        return (Path(__file__).resolve().parents[3]/'.github/workflows/ci.yml').read_text()

    def test_negative_control_table_cannot_shrink_silently(self):
        # #1632 评审（boundary，P2）：夹具自己的 fail-closed 只挡「表为空（n=0）」，
        # 删掉 1..n-1 行照样全绿。这里把行数钉死，并用内存内变异证明少一行会被判红。
        source = (Path(__file__).resolve().parent/'fixtures/session-assembly-1316/'
                  'negative_controls.py').read_text()

        def rows(text):
            # 只认字面量表：脚本后面还有 `mutations = [m for m in mutations if ...]`
            # 的过滤赋值（ListComp），那不是表本身。
            for node in ast.walk(ast.parse(text)):
                if (isinstance(node, ast.Assign)
                        and isinstance(node.value, (ast.List, ast.Tuple))
                        and any(isinstance(target, ast.Name) and target.id == 'mutations'
                                for target in node.targets)):
                    return node.value.elts
            self.fail('#1632 the fixture must define a literal mutations table')

        pinned = rows(source)
        self.assertEqual(len(pinned), 13, '#1632/#1476 thirteen mutations cover the wire-up')
        # 只钉行数等于允许「换掉一条真变异、行数不变」：名字逐一钉死（GLM r7 sibling P2），
        # 换成一条平凡可杀的变异同样会红。
        self.assertEqual([ast.literal_eval(row)[0] for row in pinned],
                         ['registry-drops-reconnect-interaction',
                          'describe-drops-permission-decide',
                          'registry-drops-always-on',
                          'http-store-branch-removed',
                          'registry-drops-elicitation-respond',
                          'registry-drops-permission-decide',
                          'always-on-abort-copies-apply-message',
                          'always-on-rerun-copies-apply-message',
                          'always-on-apply-copies-abort-message',
                          'always-on-code-not-capability-unavailable',
                          'describe-advertises-always-on-apply',
                          'describe-drops-async-hook-result',
                          'describe-drops-delete-session'],
                         '#1632/#1476 the thirteen mutations are exactly these, '
                         'not merely thirteen rows')
        dropped = ast.get_source_segment(source, pinned[0])
        mutant = source.replace(dropped + ',', '', 1)
        self.assertEqual(mutant.count(dropped), 0, '#1632 the mutant row really left the table')
        self.assertEqual(len(rows(mutant)), 12,
                         '#1632 dropping a row must change the pinned count')

    # r2 GLM sibling P1：`--self-check-controls` 的本体 `self_check_controls_run()`
    # 是自检链的第六个函数，r1 只钉了前五个——删掉它任何一条 `problems.append`
    # （INVALID×2 / SURVIVED×1），`--self-check-controls` 会打印 SURVIVED 却 exit 0，
    # 而钉子测试照绿。
    PROBLEM_FUNCTIONS = ('self_check', 'slot_timeout_probe', 'restore_probe',
                         'dispatch_probe', 'deadline_probe', 'self_check_controls_run')

    def problem_sites(self, source, function):
        """某个函数里每一处 `problems.append(...)` 的参数源码，按出现顺序。"""
        for node in ast.walk(ast.parse(source)):
            if isinstance(node, ast.FunctionDef) and node.name == function:
                break
        else:
            self.fail(f'#1843 C the fixture must define {function}()')
        sites = [node for node in ast.walk(node)
                 if isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                 and node.func.attr == 'append'
                 and isinstance(node.func.value, ast.Name)
                 and node.func.value.id == 'problems' and len(node.args) == 1]
        # 原文（不归一空白）：变异时要在源码里逐字替换，归一后多行参数就找不到了。
        return [ast.get_source_segment(source, node.args[0])
                for node in sorted(sites, key=lambda node: (node.lineno, node.col_offset))]

    def self_check_appends(self, source):
        """六个自检函数里的全部见证，按函数分组、组内按出现顺序（#1843 C）。"""
        return {function: self.problem_sites(source, function)
                for function in self.PROBLEM_FUNCTIONS}

    # r2 两家评审共同指出的同族缺口：本 PR 把 1316 的发布改成原子形态，但
    # `native-config`（GLM+MiniMax 各自独立判 P1）、`model-complete`（GLM P1）、
    # `native-stimulus-plan`（MiniMax P1）这三处被变异源的写入仍是「临时文件放在
    # 被扫的 src 里 / 无 chmod / 截断式 write_text」。同一个 `_assert_input_areas`
    # （`git ls-files --others`，故意含被忽略文件）会在 SIGKILL 残留后把整棵树判成
    # untracked build input，跨车道拒跑——所以这里逐条钉住同一个形态。
    SIBLING_ATOMIC_WRITES = (
        ('native-config/negative_controls.py', 'write_atomic'),
        ('model-complete/negative-controls.py', 'atomic_write'),
        ('native-stimulus-plan/negative_controls.py', 'write_atomic'),
    )
    # r3（MiniMax boundary P2）：临时文件「由仓根 scratch 派生」这一条要能被变异杀掉。
    SIBLING_TEMP_MUTANTS = {
        'native-config/negative_controls.py':
            ('tmp = PUBLISH_DIR / ', 'tmp = path.parent / '),
        'model-complete/negative-controls.py':
            ('scratch = PUBLISH_DIR if path.is_relative_to(ROOT) else path.parent',
             'scratch = path.parent'),
        'native-stimulus-plan/negative_controls.py':
            ('tmp = PUBLISH_DIR / ', 'tmp = path.parent / '),
    }
    # r3（MiniMax concurrency P2）：把调用点换回原地截断写时钉子必须红。
    SIBLING_WRITE_MUTANTS = {
        'native-config/negative_controls.py': (
            ('truncating-mutant-write', 'write_atomic(case.path, changed.encode())',
             'case.path.write_bytes(changed.encode())'),
            ('truncating-restore-write', 'write_atomic(case.path, originals[case.path])',
             'case.path.write_bytes(originals[case.path])'),
        ),
        'model-complete/negative-controls.py': (
            ('truncating-mutant-write', 'atomic_write(path, mutated)',
             'path.write_bytes(mutated)'),
            ('truncating-restore-write', 'atomic_write(_active["path"], original)',
             '_active["path"].write_bytes(original)'),
            # r4（MiniMax sibling P1）：`--recover` 的还原点是第三个发布点，接收者叫
            # `mutated_file`——r3 按变量名枚举时它整个漏在覆盖类之外。
            ('truncating-recover-write', 'atomic_write(mutated_file, original)',
             'mutated_file.write_bytes(original)'),
        ),
        'native-stimulus-plan/negative_controls.py': (
            ('truncating-mutant-write', 'write_atomic(path, changed.encode())',
             'path.write_bytes(changed.encode())'),
            ('truncating-restore-write', 'write_atomic(path, originals[path])',
             'path.write_bytes(originals[path])'),
        ),
    }

    def sibling_atomic_body(self, source, function):
        for node in ast.walk(ast.parse(source)):
            if isinstance(node, ast.FunctionDef) and node.name == function:
                return ast.get_source_segment(source, node)
        self.fail(f'#1692 §9 r2 the sibling fixture must define {function}()')

    def sibling_atomic_guards(self, source, function):
        """#1692 §9 r2/r3：同族夹具的发布必须是 tmp + fsync + chmod + 一次 rename。"""
        body = self.sibling_atomic_body(source, function)
        self.assertRegex(source, r"PUBLISH_DIR = (?:ROOT|root) / ['\"]\.negctl-publish['\"]",
                         '#1692 §9 r2 the scratch dir is the ignored root namespace, '
                         'not a directory inside the scanned src area')
        # r3（MiniMax boundary P2）：只判「body 里出现过 PUBLISH_DIR」会被
        # `PUBLISH_DIR.mkdir(...)` 那一行满足——把临时文件挪回目标旁边照样过钉。
        # 钉「临时路径由仓根 scratch 派生」本身。
        self.assertRegex(body, r"(?:tmp = (?:PUBLISH_DIR|scratch) /"
                               r"|mkstemp\(dir=str\((?:PUBLISH_DIR|scratch)\))",
                         '#1692 §9 r2 the temp path must be DERIVED from the root '
                         'scratch dir, not merely mention it')
        self.assertNotIn('tmp = path.parent', body,
                         '#1692 §9 r2 a temp beside the target reintroduces the '
                         'untracked-build-input lockout inside src/')
        if 'scratch' in body:
            # 仓内目标走仓根 scratch（不在任何被扫区）；仓外侧车（状态目录）留在
            # 目标同目录，否则跨卷 `os.replace` 抛 EXDEV（r3 两家评审）。
            self.assertRegex(body, r'scratch = PUBLISH_DIR if path\.is_relative_to\(ROOT\)'
                                   r' else path\.parent',
                             '#1692 §9 r2 the scratch dir follows the target')
        self.assertIn('mode = path.stat().st_mode & 0o7777', body,
                      '#1692 §9 r2 the published mode is taken from the source')
        self.assertIn('os.fsync(', body,
                      '#1692 §9 r2 the bytes reach disk before the rename')
        self.assertIn('os.replace(', body,
                      '#1692 §9 r2 the publish is ONE rename')
        self.assertIn('os.chmod(', body,
                      '#1692 §9 r2 the temp gets the source mode before the rename')
        self.assertLess(body.index('os.fsync('), body.index('os.replace('),
                        '#1692 §9 r2 fsync happens before the rename')
        self.assertLess(body.index('os.chmod('), body.index('os.replace('),
                        '#1692 §9 r2 chmod happens before the rename')
        self.assertRegex(body, r'(?s)finally:.*?unlink\(',
                         '#1692 §9 r2 a failed publish leaves no temp behind')
        return body

    def sibling_write_guards(self, source, relative):
        """#1692 §9 r2/r4：被变异源一处原地截断写都不许有，全走原子发布。

        r4（MiniMax sibling P1 / DeepSeek boundary P2 同源）：r3 的版本按**接收者变量名**
        枚举（`path` / `case.path` / `_active["path"]`），而 `model-complete` 的第三个发布点
        `atomic_write(mutated_file, original)`（`--recover` 的还原，`mutated_file` 就是树内的
        被变异源）不在名单里、也没有对应变异体——把它换回 `mutated_file.write_bytes(original)`
        三条断言全绿，§9 的「一处原地截断写都不许有」对该点假 PASS。
        改成 AST 判「谁在被写」：接收者必须是输出目录（`out` 派生的证据/日志）之外的一律判红，
        接收者叫什么名字都不影响覆盖类。
        """
        offenders = []
        for node in ast.walk(ast.parse(source)):
            if not isinstance(node, ast.Call) or not isinstance(node.func, ast.Attribute):
                continue
            if node.func.attr not in ('write_text', 'write_bytes'):
                continue
            receiver = (ast.get_source_segment(source, node.func.value) or '').strip()
            # 证据目录（`out / …`）是唯一允许的原地写目标；被变异源一律走原子发布。
            if receiver.lstrip('( ').startswith('out'):
                continue
            offenders.append(f'line {node.lineno}: {receiver}.{node.func.attr}(…)')
        self.assertEqual(offenders, [],
                         f'#1692 §9 r2 {relative} writes an in-place truncating target that '
                         'is not an evidence file')
        self.assertNotIn("path.name + '.negctl.tmp'", source, relative)
        self.assertNotIn('mkstemp(dir=str(path.parent)', source, relative)

    def sibling_module(self, relative, source=None):
        """把同族夹具当模块执行（只取模块级常量与发布函数，不跑 main）。"""
        path = Path(__file__).resolve().parent / 'fixtures' / relative
        namespace = {'__file__': str(path), '__name__': 'sibling_atomic_write_probe'}
        exec(compile(source if source is not None else path.read_text(),
                     str(path), 'exec'), namespace)
        return namespace

    def exercise_sibling_atomic_write(self, module):
        """#1692 §9 r3：目标不存在也要能发布；已有目标的 mode 原样保留、无残留。"""
        with tempfile.TemporaryDirectory(prefix='pdrs-1692-atomic-') as directory:
            directory = Path(directory)
            missing = directory / 'original.http.rs'
            try:
                module['atomic_write'](missing, b'first')
            except OSError as error:
                self.fail('#1692 §9 r3 a target that does not exist yet must still '
                          f'be published (first-run recovery sidecar): {error}')
            self.assertEqual(missing.read_bytes(), b'first')
            self.assertEqual(stat.S_IMODE(missing.stat().st_mode), 0o600,
                             '#1692 §9 r3 a new private sidecar is published 0600')
            missing.chmod(0o640)
            module['atomic_write'](missing, b'second')
            self.assertEqual(missing.read_bytes(), b'second')
            self.assertEqual(stat.S_IMODE(missing.stat().st_mode), 0o640,
                             '#1692 §9 r3 an existing target keeps its own mode')
            self.assertEqual(sorted(item.name for item in directory.iterdir()),
                             ['original.http.rs'],
                             '#1692 §9 r3 a successful publish leaves no temp behind')

    def test_sibling_source_writes_are_published_atomically(self):
        root = Path(__file__).resolve().parent / 'fixtures'
        for relative, function in self.SIBLING_ATOMIC_WRITES:
            source = (root / relative).read_text()
            self.sibling_atomic_guards(source, function)
            # 每个变异写入点都要走它：形式各异，所以钉「原地截断写零命中」。
            self.sibling_write_guards(source, relative)
            scratch = re.search(r"PUBLISH_DIR = (?:ROOT|root) / (['\"])\.negctl-publish\1",
                                source)
            self.assertIsNotNone(scratch, f'#1692 §9 r2 {relative} scratch constant')
            constant, quote = scratch[0], scratch[1]
            for label, old, new in (
                    ('loses-fsync', 'os.fsync(', 'os.fdatasync_removed('),
                    ('copies-instead-of-renaming', 'os.replace(', 'shutil.copyfile('),
                    ('loses-the-source-mode', 'os.chmod(', 'os.chmod_removed('),
                    ('scratch-dir-back-into-src', constant,
                     constant.replace('/ ' + quote + '.negctl-publish',
                                      "/ 'src' / " + quote + '.negctl-publish')),
                    ('scratch-dir-unignored', constant,
                     constant.replace(quote + '.negctl-publish',
                                      quote + 'negctl-publish')),
                    ('temp-derived-from-the-target', *self.SIBLING_TEMP_MUTANTS[relative])):
                self.assertIn(old, source, f'#1692 §9 r2 {relative} anchor: {label}')
                with self.assertRaises(AssertionError,
                                       msg=f'#1692 §9 r2 {relative} {label} must be caught'):
                    self.sibling_atomic_guards(source.replace(old, new, 1), function)
                print(f'#1692 §9 r2 sibling-publish mutant {relative} {label}: KILLED',
                      flush=True)
            for label, old, new in self.SIBLING_WRITE_MUTANTS[relative]:
                self.assertIn(old, source, f'#1692 §9 r2 {relative} anchor: {label}')
                with self.assertRaises(AssertionError,
                                       msg=f'#1692 §9 r2 {relative} {label} must be caught'):
                    self.sibling_write_guards(source.replace(old, new, 1), relative)
                print(f'#1692 §9 r2 sibling-write mutant {relative} {label}: KILLED',
                      flush=True)
        # r3（两家 boundary/concurrency）：静态形态钉不住「目标不存在时直接崩」。
        # 真执行一次发布函数：首次创建恢复侧车必须成功，已有目标必须保留 mode。
        module = self.sibling_module('model-complete/negative-controls.py')
        self.exercise_sibling_atomic_write(module)
        print('#1692 §9 r3 sibling-publish behaviour: missing and existing targets '
              'published (live)', flush=True)
        source = (root / 'model-complete/negative-controls.py').read_text()
        for label, old, new in (
                ('missing-target-requires-an-existing-mode',
                 'mode = path.stat().st_mode & 0o7777 if path.exists() else 0o600',
                 'mode = path.stat().st_mode & 0o7777'),
                ('new-sidecar-published-world-readable', 'else 0o600', 'else 0o644')):
            self.assertIn(old, source, f'#1692 §9 r3 model-complete anchor: {label}')
            with self.assertRaises(AssertionError,
                                   msg=f'#1692 §9 r3 model-complete {label} must be caught'):
                self.exercise_sibling_atomic_write(self.sibling_module(
                    'model-complete/negative-controls.py', source.replace(old, new, 1)))
            print(f'#1692 §9 r3 sibling-publish behaviour mutant {label}: KILLED',
                  flush=True)

    def test_self_check_every_problem_is_pinned(self):
        # #1843 C（GLM r2 sibling P2）：r14 只钉了 gates 循环那三处；tree-lock 段与
        # real-gate 段后来新增的每一条诊断都是「删掉仍绿」——见证没了，自检照常打印通过。
        # 这里把自检链上**每一条** `problems.append` 的参数源码钉死，并逐条做内存内变异
        # （换成 `None`）证明删改任何一条都会红；总数一并钉住，防止新增一条却不钉。
        # r1 评审（MiniMax sibling P2）：只钉 `self_check()` 不够——本 PR 新加的
        # `slot_timeout_probe` / `restore_probe` / `dispatch_probe` 里的见证同样是
        # 「删掉仍绿」，`deadline_probe` 也一并纳入。
        source = (Path(__file__).resolve().parent/'fixtures/session-assembly-1316/'
                  'negative_controls.py').read_text()
        raw = self.self_check_appends(source)
        pinned = {function: [' '.join(site.split()) for site in rows]
                  for function, rows in raw.items()}
        self.assertEqual(pinned, {
            'self_check': [
                "f'{label}: the slot guard never returned within {wall}s'",
                "f'{label}: expected rc={expected}, ' f'got rc={result.returncode}'",
                "f'{label}: exit did not report {needle!r}'",
                "f'{label}: took {elapsed:.1f}s for a {timeout}s deadline'",
                "'tree-lock case: the tree lock was not actually held'",
                "f'tree-lock case: the guard never returned within {wall}s'",
                "'tree-lock case: the guard was not bounded while the tree ' "
                "f'lock was held (rc={result.returncode})'",
                "f'tree-lock case: took {elapsed:.1f}s for a {timeout}s ' "
                "'deadline while holding the tree lock'",
                "f'real gate: never returned within {wall}s'",
                "'real gate: rc=0 without the guard reporting a slot'",
                "f'real gate: rc=0 took {elapsed:.1f}s for a ' f'{timeout}s deadline'",
                "'real gate: rc=1 without the bounded-busy verdict ' "
                "'(gate broken rather than busy)'",
                "f'real gate: rc=1 took {elapsed:.1f}s for a ' f'{timeout}s deadline'",
                "'real gate: expected a bounded 0/1 verdict, ' f'got rc={result.returncode}'",
            ],
            'slot_timeout_probe': [
                "f'slot timeout {bad!r}: exited without the refusal ' f'message: {error}'",
                "f'slot timeout {bad!r}: {type(error).__name__} instead of ' "
                "'the explicit sys.exit'",
                "f'slot timeout {bad!r} was not refused'",
                "f'slot timeout: a refused budget still reached the slot wait: ' f'{seen!r}'",
                "'slot timeout: a finite positive budget did not reach the ' "
                "f'shared helper unchanged: {seen!r}'",
            ],
            'restore_probe': [
                "'restore window: the helper thread never sent the signal'",
                "'restore window: the signal missed the masked window ' "
                "f'(sent at +{sent - mask_start:.2f}s of a 0.8 s restore)'",
                "'restore window: the stop signal interrupted the restore (' "
                'f"{record[\'interrupted\']})"',
                "'restore window: the simulated restore did not run to ' "
                'f"completion ({record.get(\'elapsed\')!r}s)"',
                "'stop signal: the handler did not raise RunTerminated'",
                "'stop signal: a second delivery would raise again inside ' "
                "f'the restore ({\", \".join(still_armed)} still handled)'",
                "f'restore watchdog: fired after {elapsed:.1f}s for a ' '0.3 s bound'",
                "'restore watchdog: a stalled restore was not cut off ' "
                "f'after {time.monotonic() - started:.1f}s'",
                "'restore watchdog: exited without the fail-closed ' f'diagnostic: {error}'",
                "'restore watchdog: left via ' f'{type(error).__name__} instead of the "
                "fail-closed exit'",
                "'restore watchdog: a stalled restore returned normally'",
            ],
            'dispatch_probe': [
                "f'dispatch {label}: fell through to the main path ' 'instead of refusing'",
                "f'dispatch {label}: exited 0'",
                "f'dispatch {label}: no usage diagnostic: {text[-200:]}'",
                "f'dispatch: an argument became an output path: {created}'",
            ],
            'self_check_controls_run': [
                "f'control copy: {error}'",
                "f'control copy: --self-check did not finish within {CONTROL_TIMEOUT_SECONDS}s'",
                "f'control copy: an unmutated copy failed (rc={result.returncode}): {tail}'",
                "f'{name}: INVALID {error}'",
                "f'{name}: INVALID (no verdict within ' f'{CONTROL_TIMEOUT_SECONDS}s)'",
                "f'{name}: SURVIVED (rc={result.returncode}, message ' "
                "f'{message!r} was {\"present\" if found else \"absent\"}, ' "
                "f'expected rc={expect_rc})'",
            ],
            'deadline_probe': [
                "f'{label}: {error}'",
                "f'{label}: exited instead of refusing: {error}'",
                "f'{label}: an expired deadline did not refuse'",
                "f'{len(spawned)} subprocess start(s) slipped past the deadline'",
                "'log probe: an expired deadline did not raise'",
                "'log probe: no log was written before the deadline raised'",
                "'log probe: the collected output was not written before ' "
                "'the deadline raised'",
                "'slot-guard exit: exited instead of reporting TIMEOUT: ' f'{error}'",
                "'slot-guard exit: neither TIMEOUT nor exit'",
                "'progress.jsonl: the appended record was not readable at once'",
                "'summary.json: a deadline published no TIMEOUT row'",
                "'summary.json.tmp: the summary was not renamed atomically'",
                "f'{DEADLINE_ENV}={bad!r} was not refused'",
                "f'{DEADLINE_ENV} did not parse whole seconds'",
                "f'{DEADLINE_ENV} refused its own upper bound'",
            ],
        }, '#1843 C every witness on the self-check chain is pinned, in order')
        sites = [site for function in self.PROBLEM_FUNCTIONS for site in raw[function]]
        self.assertEqual({function: len(rows) for function, rows in pinned.items()},
                         {'self_check': 14, 'slot_timeout_probe': 5, 'restore_probe': 11,
                          'dispatch_probe': 4, 'deadline_probe': 15,
                          'self_check_controls_run': 6},
                         '#1843 C the pinned witness count per function')
        self.assertEqual(len(set(sites)), len(sites),
                         '#1843 C each witness is one uniquely anchored site')
        for index, site in enumerate(sites):
            mutant = source.replace(site, 'None', 1)
            self.assertNotEqual(mutant, source, f'#1843 C site {index} is uniquely anchored')
            with self.assertRaises(AssertionError,
                                   msg=f'#1843 C dropping witness {index} must be caught'):
                self.assertEqual(
                    {function: [' '.join(site.split()) for site in rows]
                     for function, rows in self.self_check_appends(mutant).items()},
                    pinned)
            print(f'#1843 C self-check witness {index}: KILLED', flush=True)

    def self_check_controls_rows(self, source):
        """夹具里字面量的 `self_check_controls` 表（#1843 D）。"""
        for node in ast.walk(ast.parse(source)):
            if (isinstance(node, ast.Assign) and isinstance(node.value, ast.List)
                    and any(isinstance(target, ast.Name) and target.id == 'self_check_controls'
                            for target in node.targets)):
                return node.value.elts
        self.fail('#1843 the fixture must define a literal self_check_controls table')

    def test_self_check_control_table_cannot_shrink_silently(self):
        # #1843 D（Claude r2 boundary P2）：这张表只被 `len()` 用来派生 job 余量，
        # 于是删掉 1..n-1 行仍然全绿——少一条 meta 负控等于少一格守卫，没人知道。
        # 对照 `mutations` 表的钉法：行数 + 名字逐一钉死，并证明少一行会红。
        source = (Path(__file__).resolve().parent/'fixtures/session-assembly-1316/'
                  'negative_controls.py').read_text()
        rows = self.self_check_controls_rows(source)
        names = [ast.literal_eval(row.elts[0]) for row in rows]
        self.assertEqual(len(rows), 15,
                         '#1843 D fifteen meta controls cover the self-check guards')
        self.assertEqual(names, [
            'deadline-refusal-removed',
            'progress-flush-removed',
            'tree-lock-not-held',
            'empty-gates-table',
            'empty-gates-guard-removed',
            'deadline-parser-loosened',
            'deadline-log-dropped',
            'slot-guard-exit-not-rerouted',
            'slot-timeout-guard-removed',
            'restore-mask-removed',
            'stop-signal-second-armed',
            'restore-watchdog-removed',
            'restore-failclosed-removed',
            'unknown-mode-guard-removed',
            'bare-invocation-guard-removed',
        ], '#1843 D the meta controls are exactly these, not merely fifteen rows')
        # 只钉行数等于允许「换掉一条真负控、行数不变」：名字逐一钉死后，换名同样会红。
        dropped = ast.get_source_segment(source, rows[-1])
        mutant = source.replace(dropped + ',', '', 1)
        self.assertNotIn(dropped, mutant, '#1843 D the mutant row really left the table')
        with self.assertRaises(AssertionError,
                               msg='#1843 D dropping a meta control must be caught'):
            self.assertEqual(len(self.self_check_controls_rows(mutant)), len(rows))
        print('#1843 D self-check control row-removed: KILLED', flush=True)
        renamed = source.replace("'restore-watchdog-removed'", "'restore-watchdog-renamed'", 1)
        self.assertNotEqual(renamed, source, '#1843 D the renamed control is uniquely anchored')
        with self.assertRaises(AssertionError,
                               msg='#1843 D renaming a meta control must be caught'):
            self.assertEqual([ast.literal_eval(row.elts[0])
                              for row in self.self_check_controls_rows(renamed)], names)
        print('#1843 D self-check control row-renamed: KILLED', flush=True)

    # --- #1692 §9 裁决链：闭集钉子（r9）------------------------------------
    # r4–r7 四轮都在给「哪种写法算安全」列白名单：r4 按接收者变量名枚举写点，r6 按出口
    # 形态枚举、再钉语句序列，r7 给 `If` / `While` / `Assert` 的 test 放行任意 `Name`。
    # 白名单是开集的反面，r8 两家各找到新的放行面：test 里的 `Call` 接收者
    # （`if problems.clear():`）、`AnnAssign` / `NamedExpr` / `for` 目标、把已过审的出口
    # 搬进另一个函数、把调用点注释掉（文本还在、计数不变）。每轮都在补上一轮的写法，
    # 而「写法」这个维度本身从没关掉。r9 换闭集，一次收口：
    #
    #   · 整份字节钉：未列出的任何字节都是失败。插入 / 删除 / 搬移 / 改注释都改字节，
    #     不存在「收了哪种写法」这回事；
    #   · 出口清单整份比对：不当规则用，当人可读的复核面——字节钉只说「变了」，清单说
    #     「多 / 少 / 搬了哪一个出口」，顺序与所属函数都算内容。
    #
    # 代价是夹具要改时必须两处一起改，这正是要的：改动落成一次看得懂的 diff，而不是
    # 盲改一个哈希。下面每条负控都先证明 r7 那版口径（两种形态 + `sorted()` 位置盲）
    # 照绿，再证明闭集判红。
    SESSION_CONTROLS = 'fixtures/session-assembly-1316/negative_controls.py'
    SESSION_CONTROLS_SHA256 = 'f55bfdb8c72d71c926622229f14d9d89d202fd0f0aab1b21226d4548d1266484'
    SESSION_CONTROLS_EXITS = r"""
<module> | sys.exit() | sys.exit('no mutations defined: the negative control would pass without testing anything')
acquire_slot | sys.exit() | sys.exit(f'slot timeout must be finite and positive: {timeout!r}')
acquire_slot | return | return wait_for_slot(script, timeout=timeout)
acquire_slot | sys.exit() | sys.exit(f'cargo slots unavailable for {timeout}s: refusing to run cargo ' 'while the source tree lock is held')
acquire_slot | sys.exit() | sys.exit(f'cargo slot gate failed with rc={error.returncode}: {error.stderr}')
deadline_seconds | return | return DEADLINE_SECONDS
deadline_seconds | return | return seconds
bounded | return | return min(nominal, left)
run | return | return result.returncode, result.stdout + result.stderr
run | return | return 124, f'{collected}\nTIMEOUT after {error.timeout}s\n'
append_progress | return | return
stop_run | raise SystemExit() | raise SystemExit(f'restore did not finish before the run was cut off: {error}; ' 'no summary published, sources may still be mutated')
publish_summary | return | return counts
deadline_probe.gate_exit | raise SystemExit() | raise SystemExit('cargo slots unavailable for 600s')
deadline_probe | return | return problems
slot_timeout_probe.recorder | return | return subprocess.CompletedProcess([], 0, 'cargo-slots: 0 / cap 1\n', '')
slot_timeout_probe | return | return problems
_send_signals_after | return | return threading.Thread(target=run, daemon=True)
restore_probe | return | return problems
dispatch_probe | return | return problems
self_check | sys.exit() | sys.exit('no slot guard cases defined: self-check would pass without testing the guard')
self_check | return | return 1
self_check | return | return 0
_empty_gates_table | return | return text, 0
_empty_gates_table | return | return text[:start] + ' gates = {}\n' + text[end:], 1
<module> | sys.exit() | sys.exit('no meta control cases defined: --self-check-controls would pass ' 'without testing a single guard')
_self_check_variant | return | return copy_path
self_check_controls_run | return | return problems
self_check_controls_run | return | return problems
self_check_controls_run | return | return problems
self_check_controls_run | return | return problems
<module> | sys.exit() | sys.exit(f'--slot-guard-probe takes exactly 2 arguments\n{USAGE}')
<module> | sys.exit() | sys.exit(f'--slot-guard-probe: not a timeout: {arguments[1]!r}')
<module> | sys.exit() | sys.exit(0)
<module> | sys.exit() | sys.exit(f'{mode} takes no arguments: {arguments!r}\n{USAGE}')
<module> | sys.exit() | sys.exit(self_check())
<module> | sys.exit() | sys.exit(1 if control_problems else 0)
<module> | sys.exit() | sys.exit(USAGE)
<module> | sys.exit() | sys.exit(f'unknown mode: {mode}\n{USAGE}')
<module> | sys.exit() | sys.exit(f'unknown mutation name(s): {sorted(set(selected) - {m[0] for m in mutations})}')
<module> | sys.exit() | sys.exit('no mutations selected: the negative control would pass without testing anything')
<module> | sys.exit() | sys.exit('CARGO_TARGET_DIR must be supplied by the caller')
<module> | sys.exit() | sys.exit(str(error))
<module> | raise SystemExit() | raise SystemExit(f'{name}: restore left {restored} != {before}')
<module> | sys.exit() | sys.exit(1)
<module> | sys.exit() | sys.exit(1)
""".strip()
    _EXIT_TARGETS = ('sys.exit', 'exit', 'quit', 'os._exit', 'builtins.exit')

    @staticmethod
    def session_controls_normalise(source):
        """只规范化行尾空白与换行风格；**一条语句都不剔除**。

        剔除注释 / 空行是把 `# if problems:` 这种注释掉的守卫当成不存在，等于又开了
        一个「收了哪种写法」的口子——正是 r4–r7 的病根。空白规范化只让 CRLF 与行尾
        空格不进哈希。
        """
        return '\n'.join(line.rstrip()
                         for line in source.replace('\r\n', '\n').splitlines()).strip('\n') + '\n'

    @classmethod
    def session_controls_sha256(cls, source):
        return hashlib.sha256(cls.session_controls_normalise(source).encode()).hexdigest()

    def session_controls_source(self):
        return (Path(__file__).resolve().parent/self.SESSION_CONTROLS).read_text()

    @classmethod
    def session_controls_exit_shape(cls, source, node):
        """`node` 是哪种进程出口形态；不是出口返回 None。

        三种形态都收：`sys.exit` / `exit` / `quit` / `os._exit` / `builtins.exit`
        调用、`raise SystemExit(...)`、`return`。形态按**全节点**枚举，未列出的一律
        不是出口——r7 只收前两种，`raise SystemExit(0)` 与 `return []` 都不在网里。

        局限说明白：形态比对的是源码字面文本，别名导入（`from os import _exit`）或
        `getattr` 拼接进不了这张清单。它不是判据——那种写法同样改变字节，字节钉照样
        判红；清单只负责回答「变了的是哪个出口」。
        """
        if isinstance(node, ast.Call):
            target = ast.get_source_segment(source, node.func) or ''
            return f'{target}()' if target in cls._EXIT_TARGETS else None
        if isinstance(node, ast.Raise) and isinstance(node.exc, ast.Call):
            if (ast.get_source_segment(source, node.exc.func) or '') == 'SystemExit':
                return 'raise SystemExit()'
            return None
        if isinstance(node, ast.Return):
            return 'return'
        return None

    @classmethod
    def session_controls_exit_rows(cls, source):
        """全文件出口清单，按源码顺序：(所属限定名, 形态, 语句源码)。

        **顺序与所属函数都是内容**：r7 用 `sorted(...)` 比 multiset，把已过审的
        `sys.exit(0)` 从 `--slot-guard-probe` 分支搬进 `self_check` 头部照样全绿
        （GLM r8 sibling P1）。这里搬位置会同时改所属限定名与行序。
        """
        rows = []

        def visit(node, owner):
            for child in ast.iter_child_nodes(node):
                qualname = owner
                if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                    qualname = f'{owner}.{child.name}' if owner else child.name
                shape = cls.session_controls_exit_shape(source, child)
                if shape is not None:
                    text = ' '.join((ast.get_source_segment(source, child) or '').split())
                    rows.append((owner or '<module>', shape, text))
                visit(child, qualname)

        visit(ast.parse(source), '')
        return rows

    @classmethod
    def session_controls_exit_lines(cls, source):
        return [' | '.join(row) for row in cls.session_controls_exit_rows(source)]

    @classmethod
    def session_controls_exit_multiset(cls, source):
        """r7 那版口径：只认两种形态，`sorted()` 丢位置。

        留着它只是为了负控能贴出「这条变异绕过了老钉子」的原始证据。
        """
        return sorted(shape for _, shape, _ in cls.session_controls_exit_rows(source)
                      if shape != 'return')

    def assert_session_controls_closed(self, source):
        """闭集判据：整份字节与出口清单都必须与钉死值逐项相同。"""
        self.assertEqual(
            self.session_controls_sha256(source), self.SESSION_CONTROLS_SHA256,
            '#1692 r9 the fixture changed byte-for-byte: read the diff, then update '
            'SESSION_CONTROLS_SHA256 and SESSION_CONTROLS_EXITS in the same commit')
        self.assertEqual(
            '\n'.join(self.session_controls_exit_lines(source)), self.SESSION_CONTROLS_EXITS,
            '#1692 r9 the fixture exit inventory changed: the byte pin above only says '
            '"something changed", this list says which exit was added, dropped or moved')

    def test_session_controls_verdict_chain_is_a_closed_set(self):
        # r4–r7 的逐点钉子（`problems` / `failures` / `control_problems` / `timed_out`
        # 的累加器白名单、五条链的语句序列、出口形态计数）在 r9 全部删掉，换成整份字节
        # 钉 + 出口清单——覆盖面只增不减：任何与钉死字节不同的源码都判红，逐点钉子只是
        # 它的子集。下面每条负控都先证明 r7 口径照绿、再证明闭集判红；六条覆盖 r8 两家
        # 给的**全部** P1，两条搬移覆盖「字节变了而清单看得出搬了哪一个」。
        source = self.session_controls_source()
        self.assert_session_controls_closed(source)

        def mutate(*pairs):
            mutant = source
            for old, new in pairs:
                self.assertEqual(mutant.count(old), 1,
                                 f'#1692 r9 the mutation anchor is unique: {old!r}')
                mutant = mutant.replace(old, new, 1)
            return mutant

        def assert_killed(label, mutant):
            self.assertEqual(self.session_controls_exit_multiset(mutant),
                             self.session_controls_exit_multiset(source),
                             f'#1692 r9 {label}: the r7 multiset pin stays green')
            with self.assertRaises(AssertionError,
                                   msg=f'#1692 r9 {label} must be caught by the closed set'):
                self.assert_session_controls_closed(mutant)
            print(f'#1692 r9 closed-set mutant {label}: KILLED', flush=True)

        for label, old, new in (
                # r8 DeepSeek concurrency P1-3 / GLM concurrency P1-1 / GLM boundary P1-1：
                # r7 的读侧白名单把 test 里任意位置的 Name 都算「只读消费」，于是
                # `if problems.clear():` 把四个探针累积的见证清空后照样走钉死的裁决。
                ('clear-inside-condition',
                 '    if problems:\n        return 1\n',
                 '    if problems.clear():\n        pass\n    if problems:\n        return 1\n'),
                # r8 DeepSeek concurrency P1-1：探针归还 `[]`，见证被吞掉，探针的出口
                # 形态与累加器初始化都不动。
                ('probe-returns-empty',
                 '    return problems\n\n\ndef self_check(',
                 '    return []\n\n\ndef self_check('),
                # r8 DeepSeek concurrency P1-2：`global timed_out` 撤掉后两处赋值文本
                # 原样保留，写的是局部量，模块级 `timed_out` 恒为 None。
                ('timeout-global-removed',
                 '    global timed_out\n    try:',
                 '    try:'),
                # r8 DeepSeek boundary P1-1 / GLM concurrency P1-2：非 `Assign` 形态的
                # 重绑（walrus）不在 r7 的 `ast.Assign` 扫描里。
                ('timeout-cleared-by-walrus',
                 'counts = publish_summary(summary, timed_out=timed_out)',
                 '(timed_out := None)\ncounts = publish_summary(summary, timed_out=timed_out)'),
                # r8 DeepSeek sibling P1：把还原调用点注释掉，文本计数仍是 1。
                ('publish-call-commented-out',
                 'write_source_atomic(path, original)',
                 '# write_source_atomic(path, original)'),
                # r8 DeepSeek concurrency P1-1（后半）：`self_check` 头部早退，四个探针
                # 一条不跑；`body[-3]` / `body[-1]` 与 extend 文本都不动。
                ('self-check-early-return',
                 '    """\n    # name -> (gate script, expected exit, expected stderr fragment)\n',
                 '    """\n    return 0\n'
                 '    # name -> (gate script, expected exit, expected stderr fragment)\n'),
        ):
            assert_killed(label, mutate((old, new)))

        # 连续性对照：r4/r6 已经钉住的那条（controls 出口永远退 0）现在由字节钉接住，
        # 钉法换了、覆盖面没缩。
        assert_killed('controls-exit-always-zero', mutate(
            ('sys.exit(1 if control_problems else 0)', 'sys.exit(0)')))

        # 搬移型（r8 GLM sibling P1-1）：全文件唯一的 `sys.exit(0)` 从
        # `--slot-guard-probe` 分支搬进 `self_check` 头部。字节变了、r7 的 multiset
        # 一模一样——只有带「所属函数 + 行序」的清单能指出搬了哪一个。
        moved_exit = mutate(
            ("    print(f'slot free: {slots.stdout.strip()}', flush=True)\n    sys.exit(0)\n",
             "    print(f'slot free: {slots.stdout.strip()}', flush=True)\n"),
            ('    """\n    # name -> (gate script, expected exit, expected stderr fragment)\n',
             '    """\n    sys.exit(0)\n'
             '    # name -> (gate script, expected exit, expected stderr fragment)\n'))
        self.assertNotEqual(self.session_controls_exit_lines(moved_exit),
                            self.session_controls_exit_lines(source),
                            '#1692 r9 the inventory itself catches a relocated exit')
        assert_killed('exit-relocated-into-another-function', moved_exit)

        # 搬移型（r8 DeepSeek boundary P1-1）：模块级 `timed_out = None` 挪进
        # `stop_run` 的 finally——文本与条数都不变，位置变了：被 deadline 截停的运行
        # 会被重新清成 None，然后退 0。
        moved_init = mutate(
            ('\ntimed_out = None\nwith stop_run(), mutating_sources(',
             '\nwith stop_run(), mutating_sources('),
            ('    finally:\n        # The restore is done and verified by now',
             '    finally:\n        timed_out = None\n'
             '        # The restore is done and verified by now'))
        assert_killed('timeout-initialiser-relocated', moved_init)

    def write_source_atomic_body(self, source):
        """`write_source_atomic` 的函数体源码（#1692 §9）。"""
        for node in ast.walk(ast.parse(source)):
            if isinstance(node, ast.FunctionDef) and node.name == 'write_source_atomic':
                return '\n'.join(source.splitlines()[node.lineno - 1:node.end_lineno])
        self.fail('#1692 §9 the fixture must define write_source_atomic()')

    def publish_guards(self, source):
        """#1692 §9：原子发布的三条形态 + 残留的落点。

        独立成函数，是为了让每条变异都跑**全部**钉子，而不是各挑一条——
        与 `slot_guard_cases` 同一个用法。
        """
        body = self.write_source_atomic_body(source)
        self.assertIn("open('wb')", body,
                      '#1692 §9 the publish writes the bytes to a temp file first')
        self.assertIn('os.fsync(stream.fileno())', body,
                      '#1692 §9 the bytes are flushed to disk before the rename')
        self.assertIn('os.replace(temporary, path)', body,
                      '#1692 §9 the publish is ONE rename, never a truncating write')
        self.assertLess(body.index('os.fsync(stream.fileno())'), body.index('os.replace('),
                        '#1692 §9 the rename happens only after the fsync')
        self.assertIn('temporary.unlink(missing_ok=True)', body,
                      '#1692 §9 a failed publish leaves no temp behind')
        # 权限位也是钉子（r1 评审 GLM boundary P3）：`os.replace` 发布的是**临时文件**的
        # inode 与它自己的 mode（0666 & ~umask），源文件的 mode 会被丢掉。`core.filemode=true`
        # 下换一个 umask 跑一次夹具就会在 `src/…` 上留下只有 mode 差异的 `git diff`，下一次
        # preflight 同样整棵拒跑。先 chmod 再 rename，且 mode 取自源文件。
        self.assertIn('mode = path.stat().st_mode & 0o7777', body,
                      '#1692 §9 the published mode is taken from the source file')
        self.assertIn('os.chmod(temporary, mode)', body,
                      '#1692 §9 the temp gets the source mode before the rename')
        self.assertLess(body.index('os.chmod(temporary, mode)'), body.index('os.replace('),
                        '#1692 §9 the chmod happens before the publish, not after it')
        # 落点也是钉子（#1843 r1 MiniMax boundary P1）：SIGKILL 不执行 `finally`，
        # 所以「临时文件总会被删」恰好在唯一要紧的那条路径上是假的。放在被变异源旁边
        # 就是放进 `src/`，而 `negative_process._assert_input_areas` 用
        # `git ls-files --others`（故意包含被忽略的文件）扫 `src`，下一次运行会以
        # 「untracked build input」拒跑——正是 §9 要消灭的那种锁死。**保护来自位置**：
        # 仓库根不在扫描区（`src` / `.cargo` / `rust-toolchain*`）内；`.gitignore` 的
        # `.negctl-*` 只让 `git status` 保持干净，不是保护本身（r4 MiniMax sibling P2：
        # 这里的旧说法与本 PR 刚改对的 `native-config/README.md` 相反）。仍在同一个
        # 文件系统上，`os.replace` 依旧是一次原子 rename。
        self.assertEqual(source.count("PUBLISH_DIR = root / '.negctl-publish'"), 1,
                         '#1692 §9 the publish scratch dir is one root-level constant')
        self.assertIn("PUBLISH_DIR / f'.negctl-{path.name}.tmp'", body,
                      '#1692 §9 the temp name lives in the ignored .negctl-* namespace')
        self.assertIn('PUBLISH_DIR.mkdir(parents=True, exist_ok=True)', body,
                      '#1692 §9 the scratch dir exists before the first publish')
        repo = Path(__file__).resolve().parents[3]
        ignored = subprocess.run(
            ['git', 'check-ignore', '-q', str(repo/'.negctl-publish'/'sample.tmp')],
            cwd=repo, capture_output=True)
        self.assertEqual(ignored.returncode, 0,
                         '#1692 §9 .gitignore covers the scratch dir (.negctl-*), so a '
                         'SIGKILL residue leaves `git status` clean -- the LOCKOUT is '
                         'avoided by the location (root, outside the scanned areas), '
                         'not by being ignored')
        # 两个调用点都必须走它：写入变异体一条、还原一条。
        self.assertEqual(source.count('write_source_atomic(path, changed.encode())'), 1,
                         '#1692 §9 the mutant is published atomically')
        self.assertEqual(source.count('write_source_atomic(path, original)'), 1,
                         '#1692 §9 the restore is published atomically')
        # `log_path.write_text(…)` 之类是别的文件，不算源码树；只看变量名恰为 `path` 的。
        self.assertEqual(re.findall(r'(?<![\w.])path\.write_text\(', source), [],
                         '#1692 §9 no truncating in-place write of a mutated source')
        self.assertEqual(re.findall(r'(?<![\w.])path\.write_bytes\(', source), [],
                         '#1692 §9 no truncating in-place restore of a source')

    def test_mutation_source_writes_are_published_atomically(self):
        # #1692 §9：变异写入与还原都是截断式原地写（`write_text` / `write_bytes` 先截断再写）。
        # 一次 SIGKILL 落在两者之间，源码文件就只剩空/半截——下一次 `mutating_sources` 的
        # pristine 检查读到「基线不符」，而且直到有人手工修树为止，每次运行都会这样。
        # 正确形态是 tmp + fsync + 一次 rename：读者只会看到旧字节或新字节。
        source = (Path(__file__).resolve().parent/'fixtures/session-assembly-1316/'
                  'negative_controls.py').read_text()
        self.publish_guards(source)
        for label, old, new in (
                ('atomic-publish-loses-fsync', '            os.fsync(stream.fileno())\n', ''),
                ('atomic-publish-copies-instead-of-renaming', 'os.replace(temporary, path)',
                 'shutil.copyfile(temporary, path)'),
                ('atomic-publish-keeps-the-temp',
                 '        temporary.unlink(missing_ok=True)\n', '        pass\n'),
                # 权限位漂移：丢掉 chmod，发布出来的就是 temp 自己的 0666 & ~umask。
                ('atomic-publish-loses-the-source-mode',
                 '        os.chmod(temporary, mode)\n', ''),
                ('atomic-publish-chmods-drops-the-special-bits',
                 'mode = path.stat().st_mode & 0o7777', 'mode = 0o644'),
                ('mutant-write-back-to-truncating',
                 'write_source_atomic(path, changed.encode())', 'path.write_text(changed)'),
                ('restore-write-back-to-truncating',
                 'write_source_atomic(path, original)', 'path.write_bytes(original)'),
                # 残留落回被变异源旁边 = 下一次 preflight 拒跑。
                ('atomic-publish-leaves-residue-in-src',
                 "    temporary = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
                 "    temporary = path.with_name(f'.negctl-{path.name}.tmp')"),
                # 有 scratch 目录但名字不在 `.gitignore` 覆盖范围内。
                ('atomic-publish-scratch-dir-not-ignored',
                 "PUBLISH_DIR = root / '.negctl-publish'", "PUBLISH_DIR = root / 'negctl-publish'")):
            self.assertEqual(source.count(old), 1, f'#1692 §9 unique anchor: {label}')
            with self.assertRaises(AssertionError, msg=f'#1692 §9 {label} must be caught'):
                self.publish_guards(source.replace(old, new, 1))
            print(f'#1692 §9 atomic-publish mutant {label}: KILLED', flush=True)

    def test_shared_restore_write_is_a_known_gap(self):
        """#1903：共享守卫的还原写仍是截断式，#1692 §9 只覆盖夹具自己那一处。

        这条钉的是**已知缺口**，不是正确行为：`negative_process.py` 正被 #892 /
        PR #1634 改同一个 `restore_critical` 块，本单按协调约束不碰它（同文件同函数
        串行）。修好后这里会红，那就是提醒：把 §9 的断言扩到共享写入点、关掉 #1903。
        没有这条，`test_mutation_source_writes_are_published_atomically` 会被读成
        「被变异源已经全部原子发布」——那正是评审判的假 PASS。
        """
        source = (Path(__file__).resolve().parent/'fixtures/negative_process.py').read_text()
        pattern = r'(?<!\w)path\.write_bytes\(snapshot\[path\]\)'
        self.assertEqual(re.findall(pattern, source), ['path.write_bytes(snapshot[path])'],
                         '#1903 the shared restore write is still truncating; when it becomes '
                         'atomic, extend test_mutation_source_writes_are_published_atomically '
                         'to the shared guard and close #1903')
        fixed = source.replace('path.write_bytes(snapshot[path])',
                               'write_source_atomic(path, snapshot[path])', 1)
        with self.assertRaises(AssertionError,
                               msg='#1903 an atomic shared restore must flip this tripwire'):
            self.assertEqual(re.findall(pattern, fixed), ['path.write_bytes(snapshot[path])'],
                             '#1903 the shared restore write is still truncating')
        print('#1903 shared-restore tripwire mutant atomic-shared-restore: KILLED', flush=True)

    def documented_exit_codes(self, script):
        """脚本头部「退出码…」那张表的码集（#1556 后是多行表，续行不算新码）。"""
        lines = script.splitlines()
        header = next((i for i, line in enumerate(lines)
                       if re.match(r'#\s*退出码', line)), None)
        self.assertIsNotNone(header, '#1632 cargo-slots.sh documents its exit codes')
        codes = set()
        for line in lines[header + 1:]:
            # 表以单独一行 `#` 与后文分段；不越过它，免得把正文里「# 180 s」之类读成码。
            if not line.startswith('#') or line.strip() == '#':
                break
            found = re.match(r'#\s+(\d+)\s+\S', line)
            if found:
                codes.add(int(found[1]))
            elif codes:
                continue  # 续行：解释上一格，不引入新码
        return codes

    def fail_closed_codes(self, script):
        """`scripts/cargo-slots.sh` 头部「…；2 / 3 / 4 / 64 一律 fail-closed…」点名的码集。"""
        header = next((line for line in script.splitlines()
                       if re.match(r'#\s*退出码', line)), None)
        self.assertIsNotNone(header, '#1692 §10 cargo-slots.sh documents its exit codes')
        phrase = re.search(r'([0-9][0-9\s/]*)一律 fail-closed', header)
        self.assertIsNotNone(
            phrase, '#1692 §10 the exit-code header names the fail-closed codes')
        codes = {int(code) for code in re.findall(r'\d+', phrase[1])}
        self.assertTrue(codes, '#1692 §10 the fail-closed set is not empty')
        return codes

    def used_exit_codes(self, script):
        """脚本里真正写出来的非 0 `exit N`（不是文档，是可执行字节）。

        逐行跳过注释（与 `script_guards` 同口径——r1 评审 GLM boundary P3 / MiniMax
        boundary P2）：`scripts/cargo-slots.sh` 头注释里有 `#     → exit 4 → 消费方当硬错`，
        整篇 findall 会把注释读成可执行码。今天 4 本就在 used 里，集合恰好不变；但注释里
        一旦出现 2 或任何未用的码，`reserved == {2}` 就会在脚本零改动时假红。
        """
        used = set()
        for line in script.splitlines():
            if line.lstrip().startswith('#'):
                continue
            used.update(int(code) for code in re.findall(r'\bexit (\d+)', line))
        return used - {0}

    def script_guards(self, script):
        """每一条真实的 `exit N` 守卫：诊断（echo / printf）+ 紧随其后的 `exit N`（#1692 §10）。

        旧口径以「`echo "(cargo-slots: …)"` 这一行」为锚，于是守卫只要换一种同样真实的失败
        形态（`printf '…\\n' >&2` 后再换行 `exit N`）就从表里消失，而脚本本身并没有少一条守卫。
        现在的锚是 **exit**：先取每一处非 0 `exit N`（N≠0），再向回最多 3 行找属于它的
        `cargo-slots:` 诊断（同行也算），诊断写在 `echo` 还是 `printf` 里、消息是否折行都不影响。
        """
        lines = script.splitlines()
        guards = []
        for index, line in enumerate(lines):
            if line.lstrip().startswith('#'):
                continue
            code = re.search(r'\bexit (\d+)', line)
            if not code or code[1] == '0':
                continue
            for back in range(index, max(index - 4, -1), -1):
                # `printf` 的消息以 `\n` 结尾，那是格式串的换行而不是诊断的一部分。
                message = re.search(r'(?:echo|printf)\s+(["\'])(cargo-slots: .*?)(?:\\n)?\1',
                                    lines[back])
                if message:
                    guards.append((message[2], code[1]))
                    break
        return guards

    def slot_guard_cases(self, source, script):
        """按 `scripts/cargo-slots.sh` 的退出码契约核夹具的槽位守卫用例表。"""
        for node in ast.walk(ast.parse(source)):
            if (isinstance(node, ast.Assign) and isinstance(node.value, ast.Dict)
                    and any(isinstance(target, ast.Name) and target.id == 'gates'
                            for target in node.targets)):
                cases = node.value
                break
        else:
            self.fail('#1632 the fixture must define a literal slot-guard case table')
        codes = {'free': 0, 'no-gate': 69, 'busy': 1, 'syntax-error': 2, 'ps-failed': 3,
                 'gate-broken': 4, 'usage': 64}
        self.assertEqual([key.value for key in cases.keys], list(codes),
                         '#1632 the slot guard cases are exactly the documented states')
        # 契约的生产方是 shell 脚本而不是这张表：脚本自己文档化的码集是权威，脚本多/少一个码
        # 而夹具与测试一起没跟，就是自指的假 PASS（#1632 r7 评审 MiniMax sibling / boundary P1）。
        # #1556 之后脚本把退出码改成了一张多行表（0/1/2/3/4/64/69）；夹具逐码建模，所以口径是
        # **两边码集相等**（#1632 r14 评审 MiniMax boundary P1：只核「不多」时，脚本真会走到的
        # 69 / 4 / 64 三格可以永远不被自检覆盖，CI 依然绿）。
        documented = self.documented_exit_codes(script)
        # #1692 §10: 码集口径不再写死 `documented - {0, 1, 2}`。三份事实各有出处：
        #   · used     —— 脚本里真实写出的非 0 `exit N`；
        #   · fail_closed —— 头部用法行点名的「一律 fail-closed」码集（2 / 3 / 4 / 64）；
        #   · reserved —— 其中脚本从不主动写出的那几个（实际只有 2：bash 自身的语法错误）。
        # 文档码集必须恰好 = used ∪ reserved ∪ {0, 1}；0 / 1 是仅有的两个「不靠 exit 行」的裁决码
        # （结尾那条 `[ … ]` 测试的退出状态就是脚本的退出状态）。任一侧漂移都红。
        fail_closed = self.fail_closed_codes(script)
        used = self.used_exit_codes(script)
        reserved = fail_closed - used
        self.assertEqual(documented, used | reserved | {0, 1},
                         '#1692 §10 the documented codes are exactly the raised ones, the '
                         'reserved fail-closed ones, and the two implicit verdicts 0/1')
        self.assertEqual(reserved, {2},
                         '#1692 §10 rc 2 is the only fail-closed code the script never raises')
        self.assertEqual(set(codes.values()), documented,
                         '#1632 the fixture models exactly the codes cargo-slots.sh documents')
        self.assertLessEqual(used, documented,
                             '#1632 every exit in cargo-slots.sh is a documented code')
        # 上面两条只覆盖「不多出一个码」这一向（#1632 r8 评审 MiniMax boundary P1）：
        # 把「ps 输出为空」那条守卫整行删掉后，剩下的 `exit 3` 仍让码集自洽，两条断言照绿，
        # 而空输出会重新变成「在跑 0 → 放行」的假 PASS——正是脚本自己注释禁掉的那一格。
        # 所以把每条「先报诊断、再退出」的守卫逐条钉死（诊断与 `exit` 同行或往上三行都认，
        # #1556 后的守卫多数是分行写的）：增删任意一条守卫、把某条守卫的码改掉，都必须红。
        guards = self.script_guards(script)
        self.assertEqual(Counter(int(code) for _, code in guards),
                         Counter({3: 2, 4: 9, 64: 1, 69: 2}),
                         '#1632 every cargo-slots.sh guard: how many guards exit with which code')
        self.assertEqual({int(code) for _, code in guards}, used,
                         '#1692 §10 every raised non-zero code is a guarded failure, and '
                         'every guard exits with a code the script really raises')
        self.assertEqual([msg for msg, _ in guards if msg.startswith('cargo-slots: ps ')],
                         ['cargo-slots: ps 取样失败，无法判断名额（不按 0 放行）',
                          'cargo-slots: ps 输出为空，无法判断名额（不按 0 放行）'],
                         '#1632 the two sampling guards keep their diagnostics, in order')
        gates = {key.value: ast.literal_eval(value)
                 for key, value in zip(cases.keys, cases.values)}
        for name, code in codes.items():
            text, expected, needle = gates[name]
            self.assertEqual(int(re.search(r'\bexit (\d+)', text)[1]), code,
                             f'#1632 the {name} fake gate really exits {code}')
            self.assertEqual(expected, 0 if code in (0, 69) else 1,
                             f'#1632 only a free slot or a missing gate may proceed (rc={code})')
            if code > 1 and code != 69:
                # 69 是「本机没有统一闸」的放行码，探针打印的是 `slot free`，不带 rc。
                self.assertIn(f'rc={code}', needle, f'#1632 the {code} gate is reported by code')
        # 消费方也要钉：表再全，`self_check` 不遍历它就等于没测。
        loops = [node for node in ast.walk(ast.parse(source))
                 if isinstance(node, ast.For) and isinstance(node.iter, ast.Call)
                 and isinstance(node.iter.func, ast.Attribute)
                 and node.iter.func.attr == 'items'
                 and isinstance(node.iter.func.value, ast.Name)
                 and node.iter.func.value.id == 'gates']
        self.assertEqual(len(loops), 1, '#1632 --self-check iterates the gate table')
        self.assertEqual(len(loops[0].target.elts), 2,
                         '#1632 each gate case yields a name and its (script, rc, needle)')
        # 表再全，`--self-check` 不接它也是空转（#1632 r12 MiniMax boundary P1）：删掉这条分发行，
        # CI 的自检命令会落到主路径——把 `--self-check` 当成输出目录、跑完整 11 条扫描后 exit 0，
        # 步骤照样绿，而「`-O` 下仍有界」这条不变性再无人验证。
        # #1692 r15-4：两个 flag 曾是两套口径（`--slot-guard-probe` 前缀匹配 + 长度校验、
        # `--self-check` 全等），于是 `--self-check extra` 落回主路径被当成「输出目录 + 变异名」，
        # 未知 `--flag` 也能变成目录。现在只有一条分发规则，逐条钉住它的每一段。
        for fragment in (
                'mode, arguments = (sys.argv[1], sys.argv[2:]) if len(sys.argv) > 1 '
                'else (None, [])',
                "if mode == '--slot-guard-probe':",
                '    if len(arguments) != 2:',
                "if mode in ('--self-check', '--self-check-controls'):",
                # 这两段必须连行匹配：同一段源码也出现在本文件的 meta 负控锚点里（那里是
                # 带 `\n` 转义的字符串字面量），只匹配单行会数到 2。
                "    if arguments:\n"
                "        sys.exit(f'{mode} takes no arguments: {arguments!r}\\n{USAGE}')",
                '        sys.exit(self_check())',
                # 零参数与单横线首参各有一条守卫（r1 评审 GLM boundary P3 /
                # MiniMax boundary P2）：旧规则只封 `--*`，`-x` 会滑到主路径被当成
                # 输出目录建出来，裸跑直接 IndexError。
                "if mode is None:\n"
                "    sys.exit(USAGE)",
                "if mode.startswith('-'):\n"
                "    sys.exit(f'unknown mode: {mode}\\n{USAGE}')"):
            self.assertEqual(source.count(fragment), 1,
                             f'#1692 r15-4 the single dispatch rule keeps: {fragment}')
        # 旧的两套口径必须彻底消失：留着任何一条，flag 就又能被当成目录名 / 变异名。
        for legacy in ("sys.argv[1:] ==", "sys.argv[1:2] =="):
            self.assertNotIn(legacy, source,
                             f'#1692 r15-4 no whole-argv dispatch remains ({legacy})')
        # 分发钉住了，裁决还没钉（#1632 r14 评审 GLM boundary P1）：删掉 `if problems: return 1`
        # 后自检照常逐个起子进程、照常往 stderr 打 `self-check FAILED: …`，但 `self_check()`
        # 返回 None → `sys.exit(None)` → exit 0，CI 步骤照样绿；删掉任一条 `problems.append`
        # 更隐蔽——见证那一格的唯一记录没了，剩下的全绿。r6「删一格」与 r12「删分发行」是同一
        # 个洞的上两层，这是第三层，所以三处收集点与聚合分支逐条钉死；#1843 C 把 `self_check()`
        # 里的**每一条** `problems.append` 都钉死（见 test_self_check_every_problem_is_pinned）。
        self.assertEqual(source.count('    if problems:\n        return 1\n'), 1,
                         '#1632 the collected problems decide the exit status')
        return cases

    def self_check_ci_gate(self, source):
        """#1843 F：真闸那格只在 CI 判定，其余主机打印跳过并说明理由。

        真 `scripts/cargo-slots.sh` 的 rc 同时是**本机**的现场（忙 / 冷启动 / ops 正在更新），
        在开发机上随机红绿，那不是在测守卫；把它钉在 `CI == 'true'` 里，六个假闸照旧在所有
        环境覆盖 0/1/2/3/4/64（上面 `slot_guard_cases` 已逐条钉死）。
        """
        function = next((node for node in ast.walk(ast.parse(source))
                         if isinstance(node, ast.FunctionDef) and node.name == 'self_check'), None)
        self.assertIsNotNone(function, '#1843 F the fixture has a self_check()')
        gates = [node for node in ast.walk(function) if isinstance(node, ast.If)
                 and "os.environ.get('CI')" in ast.get_source_segment(source, node.test)]
        self.assertEqual(len(gates), 1,
                         '#1843 F the real gate probe has exactly one host-condition guard')
        self.assertEqual(ast.get_source_segment(source, gates[0].test),
                         "os.environ.get('CI') == 'true'",
                         '#1843 F the guard is the CI condition, not some other environment')
        body = ast.get_source_segment(source, gates[0])
        self.assertIn("'--slot-guard-probe'", body,
                      '#1843 F the real cargo-slots.sh probe runs inside the CI branch')
        self.assertIn('skipped outside CI', body,
                      '#1843 F every other host says the probe was skipped')
        self.assertIn('not a verdict about the guard', body,
                      '#1843 F the skip names why a host verdict is not evidence')
        return gates[0]

    def test_slot_guard_case_table_cannot_shrink_silently(self):
        # #1632 r6 评审（sibling，P1）：`--self-check` 的分支表与变异表是同一类洞——
        # 删掉 invalid-cap / ps-failed 一格，自检少测一态仍打印通过并 exit 0，
        # 上面那条 ast 扫描只认 List/Tuple，捞不到这里的 Dict。
        source = (Path(__file__).resolve().parent/'fixtures/session-assembly-1316/'
                  'negative_controls.py').read_text()
        script = (Path(__file__).resolve().parents[3]/'scripts/cargo-slots.sh').read_text()
        cases = self.slot_guard_cases(source, script)
        # 「跳过注释」这一口径也要能红（r1 评审 GLM boundary P3 / MiniMax boundary P2）：
        # 头注释里本来就有 `#     → exit 4 → 消费方当硬错`，而 4 恰好在 used 里，于是整篇
        # findall 与逐行跳过注释给出**同一个**集合——差异平时不可见，规则被改回去也没人看得出。
        # 往脚本尾部追加一行只存在于注释里的 `exit 5`：整篇读会把它当成「抛出的码」，
        # 逐行口径必须不受影响。（放在这里而不是 slot_guard_cases 里：后者被下面每条变异
        # 各调一次，放进去会把同一行打印 20 遍。）
        used = self.used_exit_codes(script)
        commented = script + '\n# → exit 5 → 只出现在注释里的码\n'
        naive = {int(code) for code in re.findall(r'\bexit (\d+)', commented)} - {0}
        self.assertIn(5, naive,
                      '#1692 §10 the mutant comment is visible to a whole-text read')
        self.assertEqual(self.used_exit_codes(commented), used,
                         '#1692 §10 a code that only appears in a comment is not "raised"')
        print('#1692 §10 exit-code mutant comment-only-code: KILLED', flush=True)
        # #1843 F：真闸探针的宿主条件（见 self_check_ci_gate 的说明），三种失效形态。
        self.self_check_ci_gate(source)
        for label, old, new in (
                ('ci-gate-widened-to-every-host',
                 "if os.environ.get('CI') == 'true':", 'if True:'),
                ('ci-gate-reversed-to-non-CI',
                 "if os.environ.get('CI') == 'true':", "if os.environ.get('CI') != 'true':"),
                ('ci-gate-skip-loses-its-reason',
                 r"'(this host\'s slot state is not a verdict about the guard)'", "''")):
            self.assertEqual(source.count(old), 1, f'#1843 F unique anchor: {label}')
            with self.assertRaises(AssertionError, msg=f'#1843 F {label} must be caught'):
                self.self_check_ci_gate(source.replace(old, new, 1))
            print(f'#1843 F real-gate host gate mutant {label}: KILLED', flush=True)
        # 每条新断言各配一条内存内变异，证明它们真能杀（不落盘，不改夹具字节）。
        for label, old, new in (
                ('gate-exits-free', r"exit 2\n'", r"exit 0\n'"),
                ('gate-expectation-drift', "1, 'cargo slot gate failed with rc=3'",
                 "0, 'cargo slot gate failed with rc=3'"),
                ('gate-needle-loses-code', "'cargo slot gate failed with rc=3'",
                 "'cargo slot gate failed'"),
                ('consumer-ignores-table', 'in gates.items():', 'in {}.items():'),
                # 脚本头部把取样失败文档化成 3；改成 5 后夹具建模的 3 不再被文档化。
                ('script-doc-changes-a-code', '#   3   取样失败', '#   5   取样失败')):
            in_source = source.count(old) == 1
            self.assertEqual(source.count(old) + script.count(old), 1,
                             f'#1632 unique slot-guard anchor: {label}')
            with self.assertRaises(AssertionError, msg=f'#1632 {label} must be caught'):
                self.slot_guard_cases(source.replace(old, new, 1) if in_source else source,
                                      script if in_source else script.replace(old, new, 1))
            print(f'#1632 slot-guard mutant {label}: KILLED', flush=True)
        # 契约核的双向性（r8 评审 P1）：删掉「ps 输出为空」那一整行守卫后，脚本仍会发
        # `exit 3`（取样失败那条），documented 与 exit 集合双双自洽 —— 只有逐条守卫的
        # 钉死能杀它。
        empty_guard = next((line for line in script.splitlines(keepends=True)
                            if 'ps 输出为空' in line and re.search(r'\bexit \d', line)), None)
        self.assertIsNotNone(empty_guard, '#1632 the empty-sample guard line exists')
        self.assertEqual(script.count(empty_guard), 1,
                         '#1632 unique anchor: the empty-sample guard line')
        with self.assertRaises(AssertionError,
                               msg='#1632 dropping the empty-sample guard must be caught'):
            self.slot_guard_cases(source, script.replace(empty_guard, '', 1))
        print('#1632 slot-guard mutant script-drops-empty-output-guard: KILLED', flush=True)
        # #1692 §10：守卫的真实失败形态不止 `echo "…"` 一种。把「ps 输出为空」那条改成
        # `printf '…\n' >&2` + 换行 `exit 3`（脚本行为完全等价），exit-锚口径必须仍认出同一条
        # 守卫；旧口径只数 echo 行，会凭空少一条——那就是「脚本没变形、尺子自己变红」的假 FAIL。

        def echo_only_guards(text):
            """#1692 之前的读法（只在诊断同一行后面找 exit），只用于证明新口径的必要性。"""
            rows = text.splitlines()
            found = []
            for index, line in enumerate(rows):
                if line.lstrip().startswith('#'):
                    continue
                message = re.search(r'echo "(cargo-slots: [^"\n]*)"', line)
                if not message:
                    continue
                code = next((hit[1] for hit in
                             (re.search(r'\bexit (\d+)', follow) for follow in rows[index:index + 4])
                             if hit), None)
                if code is not None and code != '0':
                    found.append((message[1], code))
            return found

        printf_form = ('[ -n "$snap" ] || {\n'
                       "  printf 'cargo-slots: ps 输出为空，无法判断名额（不按 0 放行）\\n' >&2\n"
                       '  exit 3\n'
                       '}\n')
        refactored = script.replace(empty_guard, printf_form, 1)
        self.assertEqual(refactored.count("printf 'cargo-slots: ps 输出为空"), 1,
                         '#1692 §10 the printf-shaped guard is really in the mutant')
        self.assertEqual(self.script_guards(refactored), self.script_guards(script),
                         '#1692 §10 a printf-shaped guard is still recognised as a guard')
        self.assertEqual(len(echo_only_guards(refactored)) + 1, len(echo_only_guards(script)),
                         '#1692 §10 the echo-only reading loses a guard the script still has')
        print('#1692 §10 slot-guard mutant printf-diagnostic: KILLED', flush=True)
        # 反向：诊断整段删掉、只留 `exit 3`，守卫表必须变（那条码不再有可读的失败原因）。
        no_diagnostic = refactored.replace(
            "  printf 'cargo-slots: ps 输出为空，无法判断名额（不按 0 放行）\\n' >&2\n", '', 1)
        self.assertNotEqual(no_diagnostic, refactored,
                            '#1692 §10 the diagnostic line is unique in the mutant')
        with self.assertRaises(AssertionError,
                               msg='#1692 §10 an exit without its diagnostic must be caught'):
            self.slot_guard_cases(source, no_diagnostic)
        print('#1692 §10 slot-guard mutant exit-without-diagnostic: KILLED', flush=True)
        # 分发行负控（r12 评审 P1 / #1692 r15-4）：删掉 `--self-check` 的分发后 CI 步骤仍绿，
        # 只有这条能杀；其余三条是新的「一条分发规则」各自的失效形态。
        for label, old, new in (
                ('script-drops-self-check-dispatch',
                 '        sys.exit(self_check())\n', '        pass\n'),
                ('script-self-check-accepts-extra-args',
                 "    if arguments:\n"
                 "        sys.exit(f'{mode} takes no arguments: {arguments!r}\\n{USAGE}')\n",
                 '    if False:\n        pass\n'),
                ('script-unknown-mode-becomes-main-path',
                 "if mode is None:\n"
                 "    sys.exit(USAGE)\n"
                 "if mode.startswith('-'):\n"
                 "    sys.exit(f'unknown mode: {mode}\\n{USAGE}')\n",
                 "if False:\n    pass\nif False:\n    pass\n"),
                # 旧规则（只封 `--*`）的回归形态：`-x` 与裸跑重新滑到主路径。
                ('script-single-dash-becomes-main-path',
                 "if mode.startswith('-'):\n"
                 "    sys.exit(f'unknown mode: {mode}\\n{USAGE}')\n",
                 'if mode.startswith("--"):\n'
                 "    sys.exit(f'unknown mode: {mode}\\n{USAGE}')\n"),
                ('script-bare-invocation-crashes',
                 "if mode is None:\n    sys.exit(USAGE)\n",
                 'if False:\n    pass\n'),
                ('script-slot-probe-matches-by-prefix',
                 "if mode == '--slot-guard-probe':",
                 "if mode and mode.startswith('--slot-guard-probe'):")):
            self.assertEqual(source.count(old), 1, f'#1692 r15-4 unique anchor: {label}')
            with self.assertRaises(AssertionError, msg=f'#1692 r15-4 {label} must be caught'):
                self.slot_guard_cases(source.replace(old, new, 1), script)
            print(f'#1692 r15-4 dispatch mutant {label}: KILLED', flush=True)
        # 裁决路径负控（r14 评审 GLM boundary P1）：分发钉住以后，删掉聚合分支仍让自检
        # 「打印 FAILED 但 exit 0」。替换成 `pass` 而不是删行，免得把负控变成语法错
        # （那样杀它的是 SyntaxError，证明不了断言有效）。逐条 `problems.append` 的负控
        # 移到 #1843 C 的 `test_self_check_every_problem_is_pinned`（那里覆盖全部 14 条）。
        for label, old, new in (
                ('script-drops-self-check-verdict',
                 '    if problems:\n        return 1\n', '    if problems:\n        pass\n'),):
            self.assertEqual(source.count(old), 1, f'#1632 unique anchor: {label}')
            with self.assertRaises(AssertionError, msg=f'#1632 {label} must be caught'):
                self.slot_guard_cases(source.replace(old, new, 1), script)
            print(f'#1632 slot-guard mutant {label}: KILLED', flush=True)
        # ast 的 col_offset 是 UTF-8 字节偏移，跨行累加也要按字节算，否则表里出现中文
        # （'ps 取样失败'）时切片会错位（GLM r7 boundary P3）。
        data = source.encode()
        lines = [line.encode() for line in source.splitlines(keepends=True)]

        def offset(lineno, column):
            return sum(len(line) for line in lines[:lineno - 1]) + column

        start = offset(cases.keys[-1].lineno, cases.keys[-1].col_offset)
        end = offset(cases.values[-1].end_lineno, cases.values[-1].end_col_offset)
        mutant = (data[:start] + data[end + 1:]).decode()  # 连同分隔逗号一起删掉整格
        self.assertNotIn("'usage'", mutant, '#1632 the mutant case really left the table')
        with self.assertRaises(AssertionError, msg='#1632 dropping a case must be caught'):
            self.slot_guard_cases(mutant, script)
        print('#1632 slot-guard mutant case-removed: KILLED', flush=True)

    def run_cargo_slots(self, script, tmp, ps_body):
        """假 `ps` + 桩闸 + 独立名额缓存，真跑一遍 `cargo-slots.sh`。

        #1556 起脚本不接受上限参数（传了就是 rc=64 用法错误），名额由统一闸决定，
        所以用 `PDRS_CARGO_SLOT_BIN` 指一个立刻答「名额 0/4」的桩闸、把名额缓存指到
        临时目录（否则会读到本机的真缓存，根本走不到取样那一步），再靠 PATH 前置假 `ps`。
        """
        ps = Path(tmp)/'ps'
        ps.write_text(ps_body)
        ps.chmod(0o755)
        gate = Path(tmp)/'cargo-slot'
        gate.write_text('#!/bin/sh\n[ "$1" = --status ] && echo "名额 0/4 有空位"\nexit 0\n')
        gate.chmod(0o755)
        return subprocess.run(['bash', str(script)], capture_output=True, text=True,
                              timeout=120,
                              env={**os.environ,
                                   'PATH': f'{tmp}{os.pathsep}{os.environ["PATH"]}',
                                   'PDRS_CARGO_SLOT_BIN': str(gate),
                                   'PDRS_CARGO_SLOTS_CACHE_DIR': str(Path(tmp)/'cache')})

    def assert_cargo_slots_fails_closed(self, script, tmp, label):
        """取样不可用时必须 rc=3，且绝不打印「在跑 N」——打印了就是把空样本读成 0 放行。"""
        for case, body in (('ps exits non-zero', '#!/bin/sh\nexit 1\n'),
                           ('ps prints nothing', '#!/bin/sh\nexit 0\n'),
                           # 残缺采样：先输出一行再非零退出。只有这一格能把「取样失败」
                           # 那条守卫和「输出为空」那条分开——前两格都会被空输出守卫兜住。
                           ('ps prints rows then fails',
                            '#!/bin/sh\nprintf "  123 1 cargo\\n"\nexit 1\n')):
            done = self.run_cargo_slots(script, tmp, body)
            self.assertEqual(done.returncode, 3, f'#1632 {label} / {case}: {done.stderr}')
            self.assertIn('不按 0 放行', done.stderr, f'#1632 {label} / {case} explains itself')
            self.assertNotIn('在跑', done.stdout, f'#1632 {label} / {case} reports no slot count')

    def test_cargo_slots_script_fails_closed_when_ps_is_unusable(self):
        # #1632 r9 评审（MiniMax sibling / boundary P1）：上面那条只钉脚本文本，而文本钉不住
        # 语义——`||` 翻成 `&&`、把 `exit 3` 包进子 shell、或把整行挪到判定之后，被钉的字符串
        # 一字不变，闸门却回到「空样本 → 在跑 0 → 放行」。所以换一个载体：假 `ps` 前置进
        # PATH，真跑一遍脚本，两条取样失败路径都必须 rc=3 且不打印槽位数。
        script = Path(__file__).resolve().parents[3]/'scripts/cargo-slots.sh'
        text = script.read_text()
        guard = next(line for line in text.splitlines(keepends=True) if 'ps 输出为空' in line)
        sampling = next(line for line in text.splitlines(keepends=True) if 'ps 取样失败' in line)
        with tempfile.TemporaryDirectory() as tmp:
            self.assert_cargo_slots_fails_closed(script, tmp, 'real script')
            for label, mutant in (
                    ('sampling-guard-condition-flipped',
                     text.replace(') || { echo "cargo-slots: ps 取样失败',
                                  ') && { echo "cargo-slots: ps 取样失败', 1)),
                    ('sampling-guard-exit-in-subshell',
                     text.replace(sampling,
                                  sampling.replace('|| {', '|| ( {', 1).rstrip(chr(10)) + ' )\n', 1)),
                    ('guard-condition-flipped',
                     text.replace('] || { echo "cargo-slots: ps 输出为空',
                                  '] && { echo "cargo-slots: ps 输出为空', 1)),
                    ('guard-exit-in-subshell',
                     text.replace(guard, f'( {guard.rstrip(chr(10))} )\n', 1)),
                    ('guard-moved-after-decision', text.replace(guard, '', 1) + guard)):
                self.assertNotEqual(mutant, text, f'#1632 the mutant really changed: {label}')
                path = Path(tmp)/f'{label}.sh'
                path.write_text(mutant)
                with self.assertRaises(AssertionError,
                                       msg=f'#1632 the behaviour check must catch {label}'):
                    self.assert_cargo_slots_fails_closed(path, tmp, label)
                print(f'#1632 slot-guard behaviour mutant {label}: KILLED', flush=True)

    def test_nextest_keeps_workspace_doctests_and_failure_semantics(self):
        jobs = self.ci_jobs(issue='#922')
        steps = {job: self.ci_steps(job, body, issue='#922') for job, body in jobs.items()}
        expected = {
            'cargo-nextest（cargo install 源码编译）': [
                '"$HOME/.cargo/bin/cargo-nextest" --version 2>/dev/null | '
                "grep -qx 'cargo-nextest 0.9.148' \\",
                '|| cargo install cargo-nextest --locked --version 0.9.148 --force',
                'echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"',
                '"$HOME/.cargo/bin/cargo-nextest" --version',
            ],
            '测试': [self.nextest_command],
            '文档测试': [self.doctest_command],
        }
        positions = []
        for title, commands in expected.items():
            found = [(job, i, step) for job, items in steps.items() for i, step in enumerate(items)
                     if self.ci_step_fields(step).get('name') == (title, [])]
            self.assertEqual([job for job, _, _ in found], ['rust'],
                             f'#922 {title} occurs once in rust')
            _, index, step = found[0]
            positions.append(index)
            fields = self.ci_step_fields(step)
            # #1525: only 测试 may carry a step-level env block, and only for the
            # permission-faults switch (#1263). if/shell/timeout/env could all
            # override execution or ignore failure.
            self.assertEqual(set(fields), {'name', 'run'} | ({'env'} if title == '测试' else set()),
                             f'#922 {title} cannot override execution or ignore failure')
            if title == '测试':
                value, block = fields['env']
                self.assertEqual((value, [line for line in block if not line.startswith('#')]),
                                 ('', self.permission_faults_env),
                                 '#922 测试 step env only registers the permission-faults switch')
            self.assertEqual(self.ci_run_commands(step), commands,
                             f'#922 {title} preserves exact commands')
        self.assertEqual(positions, sorted(positions), '#922 install precedes nextest and doctests')
        # Catch duplicate invocations even when a step has a different name or job.
        executions = [(job, i) for job, items in steps.items() for i, step in enumerate(items)
                      for command in self.ci_run_commands(step)
                      if re.search(r'\bcargo (?:nextest run|test)\b', command)]
        self.assertEqual(executions, [('rust', i) for i in positions[1:]],
                         '#922 workspace test invocations are unique and complete')
        caches = [(i, self.ci_step_fields(step)) for i, step in enumerate(steps['rust'])
                  if 'cargo-nextest-' in step and self.ci_step_fields(step).get('uses')
                  == ('actions/cache@v4', [])]
        self.assertEqual(len(caches), 1, '#922 one nextest binary cache')
        index, cache = caches[0]
        self.assertEqual(cache, {
            'uses': ('actions/cache@v4', []),
            'with': ('', ['path: ~/.cargo/bin/cargo-nextest',
                          'key: cargo-nextest-0.9.148-${{ runner.os }}-${{ runner.arch }}']),
        }, '#922 cache is pinned to nextest version and platform')
        self.assertLess(index, positions[0], '#922 cache restore precedes source install')
        rust_structure = '\n'.join(line for line in jobs['rust'].splitlines()
                                   if line.strip() and not line.lstrip().startswith('#')) + '\n'
        self.assertEqual(re.findall(r'^    defaults:\n((?:      [^\n]*\n)+)', rust_structure, re.M),
                         ['      run:\n        working-directory: src\n'], '#922 Rust commands run in src')
        self.test_required_check_gates_observer_failure_instead_of_skipping()
        self.test_b2_and_smoke_keep_commands_in_their_own_jobs()

    def test_nextest_ci_profile_has_no_retries_filters_or_kill_timeout(self):
        path = Path(__file__).resolve().parents[3]/'.config/nextest.toml'
        try:
            config = tomllib.loads(path.read_bytes().decode())
        except (OSError, ValueError) as error:
            self.fail(f'#922 nextest config must parse: {error}')
        # #1525: exactly one override, with exactly the two keys that replay a
        # passing test's output. Anything else (default-filter, retries,
        # threads, terminate-after) changes which tests run or how they fail.
        self.assertEqual(config, {'profile': {'ci': {
            'retries': 0, 'slow-timeout': {'period': '60s'},
            'overrides': [{'filter': self.nextest_success_output_filter,
                           'success-output': 'final'}],
        }}}, '#922 ci profile preserves all tests, default threads, zero retries '
            'and exactly one success-output override')
        self.assert_success_output_filter_names_real_tests(self.nextest_success_output_filter)

    def assert_success_output_filter_names_real_tests(self, filter_text):
        # fix-r3: `test(=name)` is an exact match on the name nextest reports.
        # Integration tests (`tests/*.rs`) report their bare fn name, unit tests
        # (`src/**`) report their module path — a bare leaf never selects them
        # (measured on nextest 0.9.150 and on CI's pinned 0.9.148: `test(=bar)`
        # misses `foo::tests::bar`, `test(=foo::tests::bar)` hits).
        # fix-r4: derive exactly ONE name per declaring file from the module
        # path. Taking a union of guesses let `skills::tests::tests::leaf`
        # (which nextest never reports) pass while selecting nothing.
        entries = re.findall(r'test\(([^)]*)\)', filter_text)
        self.assertTrue(entries, '#922 the success-output filter names its tests')
        crate = Path(__file__).resolve().parents[1]
        sources = {source: source.read_text(errors='replace')
                   for source in crate.rglob('*.rs') if 'target' not in source.parts}
        for entry in entries:
            # `[^)]*` also catches a name the old `[A-Za-z0-9_:]+` regex
            # silently truncated at a `-` or a space.
            self.assertRegex(entry, r'^=[A-Za-z0-9_:]+$',
                             f'#922 the success-output filter entry test({entry}) is not a '
                             f'plain `test(=name)` matcher')
            name = entry[1:]
            leaf = name.rsplit('::', 1)[-1]
            defining = [source for source, text in sources.items() if f'fn {leaf}(' in text]
            self.assertTrue(defining, f'#922 the success-output filter names the real test {name}')
            expected = {self.nextest_visible_name(crate, source, leaf) for source in defining}
            self.assertIn(name, expected,
                          f'#922 the success-output filter must use the nextest-visible name '
                          f'for {leaf}: {sorted(expected)}, not {name}')

    @staticmethod
    def nextest_visible_name(crate, source, leaf):
        relative = source.relative_to(crate)
        if relative.parts[0] != 'src':
            return leaf
        parts = list(relative.relative_to('src').with_suffix('').parts)
        if parts[-1] == 'mod':
            parts.pop()
        # fix-r7 (gate6-anthropic boundary#1): the crate root file cannot be
        # mapped from its path. Measured on this crate (libtest `--list`, the
        # same names nextest reports) after appending two probes to
        # `src/lib.rs`: `probe_at_crate_root` (declared at the crate root) and
        # `tests::probe_inside_tests_module` (declared inside `mod tests`) —
        # two different shapes out of ONE file. The old code returned
        # `lib::tests::<leaf>`, a name `test(=…)` never matches, so the guard
        # would approve a filter entry that silently selects nothing — the
        # exact silent-green hole this check exists to close. Fail closed and
        # make the author paste the name nextest really reports.
        # `src/mod.rs` pops to an empty list and used to raise IndexError here;
        # it is unmappable for the same reason.
        if not parts or (len(parts) == 1 and parts[0] in ('lib', 'main')):
            raise AssertionError(
                f'#922 cannot derive the nextest-visible name for {leaf} declared in '
                f'{relative}: a crate-root or bare-`mod` file reports a name that depends '
                f'on the module nesting, not on the path. Run `cargo nextest list` and '
                f'paste the exact name nextest prints.')
        if parts[-1] != 'tests':
            parts.append('tests')
        return '::'.join([*parts, leaf])

    def test_success_output_filter_rejects_a_renamed_test(self):
        # The filter is a list of nextest-visible test names: a rename would
        # silently drop the success-output replay. These are the negative
        # controls for that check.
        self.assert_success_output_filter_names_real_tests(self.nextest_success_output_filter)
        with self.assertRaisesRegex(AssertionError, 'names the real test no_such_test'):
            self.assert_success_output_filter_names_real_tests(
                self.nextest_success_output_filter + ' | test(=no_such_test)')
        # fix-r3: a unit test addressed by its bare leaf name never matches.
        with self.assertRaisesRegex(AssertionError, 'must use the nextest-visible name'):
            self.assert_success_output_filter_names_real_tests(
                self.nextest_success_output_filter
                + ' | test(=files_sort_keeps_submillisecond_mtime_precision)')
        # fix-r4: a doubled module segment names nothing in nextest; the old
        # union-of-guesses accepted it.
        with self.assertRaisesRegex(AssertionError, 'must use the nextest-visible name'):
            self.assert_success_output_filter_names_real_tests(
                self.nextest_success_output_filter
                + ' | test(=skills::tests::tests::unreadable_scan_child_matches_real_ts_'
                  'before_denial_and_after_restore)')
        # fix-r4: the entry regex must not silently truncate at a stray char.
        with self.assertRaisesRegex(AssertionError, 'is not a plain'):
            self.assert_success_output_filter_names_real_tests(
                self.nextest_success_output_filter + ' | test(=skills::tests::foo-bar)')

    def test_crate_root_file_is_not_guessed(self):
        # fix-r7: a test declared in `src/lib.rs` / `src/main.rs` cannot be
        # named from the path — the same file yields both `probe_at_crate_root`
        # and `tests::probe_inside_tests_module` (measured with libtest
        # `--list` on this crate). The old code answered `lib::tests::<leaf>`,
        # which `test(=…)` never matches, so a future entry would be approved
        # while selecting nothing. These two calls pin the fail-closed answer.
        crate = Path(__file__).resolve().parents[1]
        for crates_root in ('src/lib.rs', 'src/main.rs'):
            with self.assertRaisesRegex(AssertionError,
                                        'cannot derive the nextest-visible name'):
                self.nextest_visible_name(crate, crate / crates_root, 'leaf')
        # `src/mod.rs` pops to an empty module list; it used to raise
        # IndexError from `parts[-1]`, which reads like a bug rather than a
        # refusal. It must now refuse with the same message.
        with self.assertRaisesRegex(AssertionError,
                                    'cannot derive the nextest-visible name'):
            self.nextest_visible_name(crate, crate / 'src/mod.rs', 'leaf')
        # The fix must not swallow the normal cases.
        self.assertEqual(
            self.nextest_visible_name(crate, crate / 'src/skills/tests.rs', 'leaf'),
            'skills::tests::leaf')
        self.assertEqual(
            self.nextest_visible_name(crate, crate / 'src/grep/mod.rs', 'leaf'),
            'grep::tests::leaf')
        self.assertEqual(
            self.nextest_visible_name(crate, crate / 'tests/live_e2e.rs', 'leaf'),
            'leaf')

    def test_ci_rejects_nextest_execution_drift(self):
        workflow = self.ci_workflow_fixture()
        steps = self.ci_steps('rust', self.ci_jobs(issue='#922')['rust'], issue='#922')
        cases = []

        def change(label, old, new, message):
            self.assertEqual(workflow.count(old), 1, f'#922 unique mutation anchor: {label}')
            cases.append((label, workflow.replace(old, new, 1), message))

        def anchor(title):
            matches = [step for step in steps
                       if self.ci_step_fields(step, '#922').get('name') == (title, [])]
            self.assertEqual(len(matches), 1, f'#922 {title} occurs once in rust')
            # ci_steps canonicalises keys and keeps comments verbatim, so
            # re-attaching the sequence bullet reproduces the file bytes.
            return '      - ' + matches[0]

        for title, command in (('测试', self.nextest_command), ('文档测试', self.doctest_command)):
            raw = anchor(title)
            change(f'{title}-missing', raw, '', f'#922 {title} occurs once in rust')
            change(f'{title}-duplicate', raw, raw + raw, f'#922 {title} occurs once in rust')
            fields = ('if: false', "'if': false", '"continue-on-error": true',
                      'shell: bash {0}', 'timeout-minutes: 1', 'working-directory: .')
            if title != '测试':
                fields += ('env: {NEXTEST_RETRIES: 2}',)
            for field in fields:
                change(f'{title}-{field}', raw, raw + f'        {field}\n',
                       f'#922 {title} cannot override execution or ignore failure')
            change(f'{title}-extra-run', raw, raw + f'      - run: {command}\n',
                   '#922 workspace test invocations are unique and complete')
            if title != '测试':
                continue
            # #1525: the one allowed env block is pinned entry by entry — a
            # second env key, a rename, a different value, or dropping the
            # switch entirely must all be rejected.
            change(f'{title}-second-env', raw, raw + '        env: {NEXTEST_RETRIES: 2}\n',
                   '#919 duplicate step key')
            entry = '          PDRS_REQUIRE_PERMISSION_FAULTS: "1"\n'
            self.assertEqual(workflow.count(entry), 1, '#922 unique permission-faults env anchor')
            for label, new in (('env-value', '          PDRS_REQUIRE_PERMISSION_FAULTS: "0"\n'),
                               ('env-renamed', '          PDRS_REQUIRE_PERMISSION_FAULT: "1"\n'),
                               ('env-dropped', ''),
                               ('env-extra-key', entry + '          NEXTEST_RETRIES: 2\n')):
                cases.append((f'{title}-{label}', workflow.replace(entry, new, 1),
                              '#922 测试 step env only registers the permission-faults switch'))
        for flag in (' --workspace', ' --no-fail-fast', ' --profile ci',
                     ' --config-file ../.config/nextest.toml'):
            change(f'nextest-without-{flag}', self.nextest_command,
                   self.nextest_command.replace(flag, '', 1), '#922 测试 preserves exact commands')
        for command in ('cargo test --all', self.nextest_command + ' --retries 1',
                        self.nextest_command + ' || true', self.nextest_command + ' -E all()'):
            change(command, self.nextest_command, command, '#922 测试 preserves exact commands')
        for command in ('cargo test --doc', 'cargo test --workspace', 'true'):
            change(command, self.doctest_command, command, '#922 文档测试 preserves exact commands')
        install = 'cargo install cargo-nextest --locked --version 0.9.148 --force'
        for command in (install.replace('--locked ', ''), install.replace('0.9.148', '0.9.147'),
                        'curl -Ls https://get.nexte.st/latest | tar xz'):
            change(command, install, command, '#922 cargo-nextest（cargo install 源码编译） preserves exact commands')
        for old, new in (('path: ~/.cargo/bin/cargo-nextest', 'path: ~/.cargo/bin'),
                         ('key: cargo-nextest-0.9.148-', 'key: cargo-nextest-latest-'),
                         ('-${{ runner.arch }}', ''),
                         ('        working-directory: src\n', '        working-directory: .\n')):
            message = '#922 Rust commands run in src' if 'working-directory' in old else '#922 cache is pinned'
            change(old, old, new, message)
        cache = ('      - uses: actions/cache@v4\n        with:\n'
                 '          path: ~/.cargo/bin/cargo-nextest\n'
                 '          key: cargo-nextest-0.9.148-${{ runner.os }}-${{ runner.arch }}\n')
        change('missing-cache', cache, '', '#922 one nextest binary cache')
        # Exact ordering and cross-job ownership also need negative controls.
        first = anchor('测试')
        doc = anchor('文档测试')
        self.assertEqual(workflow.count(doc), 1, '#922 unique doctest relocation anchor')
        installer = [step for step in self.ci_steps('rust', self.ci_jobs()['rust'])
                     if step.startswith('name: cargo-nextest（cargo install 源码编译）\n')]
        self.assertEqual(len(installer), 1, '#922 unique install relocation anchor')
        raw_install = '      - ' + installer[0]
        change('missing-install', raw_install, '', '#922 cargo-nextest（cargo install 源码编译） occurs once in rust')
        change('conditional-install', raw_install, raw_install + '        if: false\n',
               '#922 cargo-nextest（cargo install 源码编译） cannot override execution or ignore failure')
        cases.append(('install-after-nextest', workflow.replace(raw_install, '', 1).replace(first, first + raw_install, 1),
                      '#922 install precedes nextest and doctests'))
        cases.append(('cache-after-install', workflow.replace(cache, '', 1).replace(first, cache + first, 1),
                      '#922 cache restore precedes source install'))
        cases.append(('doctest-before-nextest', workflow.replace(doc, '', 1).replace(first, doc + first, 1),
                      '#922 install precedes nextest and doctests'))
        cases.append(('doctest-in-other-job', workflow.replace(doc, '', 1).replace('    steps:\n', '    steps:\n' + doc, 1),
                      '#922 文档测试 occurs once in rust'))
        for old, new, message in (
                ('needs: [scope, metadata-observer, rust, a51-coverage, negative-controls]',
                 'needs: [scope, metadata-observer, a51-coverage, negative-controls]',
                 '#904 exact required needs'),
                ("needs.rust.result != 'success'", "needs.rust.result == 'failure'",
                 '#904 rust rejects every non-success beyond the docs-only skip')):
            change(old, old, new, message)
        self.check_ci_mutations(cases, 'test_nextest_keeps_workspace_doctests_and_failure_semantics', '#922')

    def test_ci_rejects_nextest_profile_drift(self):
        path = Path(__file__).resolve().parents[3]/'.config/nextest.toml'
        config = path.read_bytes()
        cases = []
        for label, old, new in (
                ('retries', b'retries = 0', b'retries = 1'),
                ('missing-retries', b'retries = 0', b''),
                ('missing-slow-timeout', b'slow-timeout = { period = "60s" }', b''),
                ('kill-timeout', b'period = "60s"', b'period = "60s", terminate-after = 1'),
                ('threads', b'retries = 0', b'retries = 0\ntest-threads = 1'),
                ('filter', b'retries = 0', b'retries = 0\ndefault-filter = "test(none)"'),
                ('missing-profile', b'[profile.ci]', b'[profile.other]'),
                ('default-override', b'[profile.ci]', b'[profile.default]\nretries = 1\n[profile.ci]')):
            self.assertEqual(config.count(old), 1, f'#922 unique profile mutation anchor: {label}')
            cases.append((label, config.replace(old, new, 1), '#922 ci profile preserves'))
        cases.append(('invalid-toml', b'[', '#922 nextest config must parse'))
        # #1525: the single override may only add `success-output`; dropping it or
        # smuggling in any other nextest knob must be rejected.
        success = b'success-output = "final"'
        for label, new in (
                ('override-missing-success-output', b''),
                ('override-terminate-after', success + b'\nterminate-after = 1'),
                ('override-default-filter', success + b'\ndefault-filter = "test(none)"'),
                ('override-threads', success + b'\ntest-threads = 1'),
                ('override-retries', success + b'\nretries = 1')):
            self.assertEqual(config.count(success), 1, f'#922 unique profile mutation anchor: {label}')
            cases.append((label, config.replace(success, new, 1), '#922 ci profile preserves'))
        override = b'[[profile.ci.overrides]]'
        self.assertEqual(config.count(override), 1, '#922 unique override mutation anchor')
        cases.append(('override-duplicated',
                      config.replace(override, override + b'\nfilter = "test(none)"\n' + override, 1),
                      '#922 ci profile preserves'))
        self.assertEqual(config.count(b'test(=grep_outputs_match_real_ts) | '), 1,
                         '#922 unique filter-widening mutation anchor')
        cases.append(('override-filter-widened',
                      config.replace(b'test(=grep_outputs_match_real_ts) | ', b'', 1),
                      '#922 ci profile preserves'))
        self.check_ci_mutations(cases, 'test_nextest_ci_profile_has_no_retries_filters_or_kill_timeout',
                                '#922 profile', reader='read_bytes')

    def test_rust_job_uses_line_tables_only_debug(self):
        rust = self.ci_jobs()['rust']
        expected = ('      CARGO_PROFILE_DEV_DEBUG: line-tables-only\n'
                    '      CARGO_PROFILE_TEST_DEBUG: line-tables-only\n')
        self.assertEqual(re.findall(r'^    env:\n((?:      [^\n]*\n)+)', rust, re.M),
                         [expected], '#918 Rust debug profiles are job-level line-tables-only')
        self.assertEqual(re.findall(r'\bCARGO_PROFILE_(DEV|TEST)_DEBUG\b', rust),
                         ['DEV', 'TEST'], '#918 Rust debug profiles have no step overrides')
        # Aliases/expressions could import overrides without spelling either key here.
        self.assert_steps_use_explicit_env_blocks(
            'rust', rust, '#918 Rust step env must be an explicit block mapping')

    def assert_steps_use_explicit_env_blocks(self, name, body, message):
        for step in self.ci_steps(name, body):
            lines = step.splitlines()
            for i, line in enumerate(lines):
                env = re.fullmatch(r'''(?:        |)(?:env|'env'|"env"):(.*)''', line)
                if env is None:
                    continue
                entries = []
                for entry in lines[i + 1:]:
                    if not entry.strip() or entry.lstrip().startswith('#'):
                        continue
                    if len(entry) - len(entry.lstrip(' ')) <= 8:
                        break
                    entries.append(entry)
                self.assertTrue(not env[1].strip() and entries and all(
                    re.fullmatch(r'          [A-Za-z_][A-Za-z0-9_]*: .+', entry) for entry in entries),
                    message)

    def test_negative_controls_job_uses_line_tables_only_debug(self):
        # #1692 §3: the negative-controls job builds and runs the same cargo tests
        # as `rust`, so it needs the same debuginfo cap. Unlike `rust`, its env
        # block also carries CARGO_TARGET_DIR and comments, so the profile keys are
        # asserted individually instead of by whole-block equality.
        job = self.ci_jobs(issue='#1692')['negative-controls']
        message = '#1692 negative-controls debug profiles are job-level line-tables-only'
        blocks = re.findall(r'^    env:\n((?:      [^\n]*\n)+)', job, re.M)
        self.assertEqual(len(blocks), 1, message)
        self.assertEqual(re.findall(r'      CARGO_PROFILE_[A-Z_]+_DEBUG: (.*)', blocks[0]),
                         ['line-tables-only', 'line-tables-only'], message)
        self.assertEqual(re.findall(r'\bCARGO_PROFILE_(DEV|TEST)_DEBUG\b', job),
                         ['DEV', 'TEST'],
                         '#1692 negative-controls debug profiles have no step overrides')
        self.assert_steps_use_explicit_env_blocks(
            'negative-controls', job,
            '#1692 negative-controls step env must be an explicit block mapping')

    def test_negative_controls_job_timeout_exceeds_fixture_deadline(self):
        # #1692 §1: measured (run 42176640534, probe job with timeout-minutes: 1) -
        # a job-level timeout cancels the running step but `if: always()` steps
        # still run and the artifact still uploads. What it does NOT do is let the
        # fixture finish: the process dies wherever it is, so the sources are never
        # restored, progress.jsonl / summary.json are never written, and the job
        # reads as `cancelled`. The fixture therefore bounds itself and unwinds
        # gracefully, which only works if the job timeout sits ABOVE that budget
        # with room for the cold cargo bootstrap and the upload.
        job = self.ci_jobs(issue='#1692')['negative-controls']
        message = '#1692 negative-controls job has one literal timeout-minutes'
        minutes = re.findall(r'^    timeout-minutes: (.+)$', job, re.M)
        self.assertEqual(len(minutes), 1, message)
        self.assertRegex(minutes[0], r'^[1-9][0-9]*$', message)
        fixture = (Path(__file__).resolve().parents[3]
                   / 'src/pilotdeck-gateway/tests/fixtures/session-assembly-1316'
                   / 'negative_controls.py').read_bytes().decode()
        deadlines = re.findall(r'^DEADLINE_SECONDS = (\d+)$', fixture, re.M)
        self.assertEqual(len(deadlines), 1,
                         '#1692 negative-controls fixture declares one overall deadline')
        # The job timeout has to cover the two self-check steps that run BEFORE
        # the fixture AND the fixture's own deadline AND the upload. Derive the
        # self-check worst case from the fixture instead of hardcoding a slack:
        # a fixed 900 s silently stops being enough as soon as a meta control is
        # added (#1692 review P2).
        tree = ast.parse(fixture)
        controls = next(
            len(node.value.elts) for node in ast.walk(tree)
            if isinstance(node, ast.Assign)
            and isinstance(node.value, ast.List)
            and any(isinstance(target, ast.Name) and target.id == 'self_check_controls'
                    for target in node.targets))
        control_timeout = int(re.search(r'^CONTROL_TIMEOUT_SECONDS = (\d+)$',
                                        fixture, re.M)[1])
        walk = re.search(r'def self_check\(timeout=(\d+), wall=(\d+)\)', fixture)
        self.assertIsNotNone(walk, '#1692 self_check keeps its bounded wall clock')
        gates = next(
            len(node.value.keys) for node in ast.walk(tree)
            if isinstance(node, ast.Assign)
            and isinstance(node.value, ast.Dict)
            and any(isinstance(target, ast.Name) and target.id == 'gates'
                    for target in node.targets))
        # Every gate case runs twice (plain and -O), plus the tree-lock and the
        # real-gate probe; each child is capped by `wall`. The meta-control
        # step runs the control copy plus one copy per case at CONTROL_TIMEOUT.
        self_check_worst = (2 * gates + 2) * int(walk[2])
        controls_worst = (controls + 1) * control_timeout
        margin = self_check_worst + controls_worst + 300  # + checkout/cache/upload
        self.assertGreaterEqual(
            int(minutes[0]) * 60, int(deadlines[0]) + margin,
            f'#1692 the job timeout must outlive the fixture deadline plus the '
            f'{self_check_worst}s self-check worst case, the {controls_worst}s '
            f'meta-control worst case and 300s of setup/upload')
        # The budget has to be the fixture's own default. An env override in the
        # job (or in any step of it) could raise the deadline past the job
        # timeout - or shrink it below what the mutations need - without touching
        # the number asserted above.
        env_names = re.findall(r"^DEADLINE_ENV = '([A-Za-z0-9_]+)'$", fixture, re.M)
        self.assertEqual(len(env_names), 1,
                         '#1692 negative-controls fixture declares one deadline env name')
        # Comments name the variable on purpose (they document the default
        # budget), so only YAML content counts here. The scan is the WHOLE
        # workflow, not just the job body: a top-level `env:` sits before
        # `jobs:` (which `ci_jobs()` slices away) yet applies to every job, so a
        # job-scoped scan would leave that override path open (#1692 review P2).
        workflow = self.ci_workflow_fixture()
        code = '\n'.join(line for line in workflow.splitlines()
                         if not line.lstrip().startswith('#'))
        self.assertNotIn(
            env_names[0], code,
            f'#1692 negative-controls must not set {env_names[0]}: the fixture deadline '
            'is fixed by DEADLINE_SECONDS, not by the workflow')

    def assert_deadline_log_call_sites(self, source):
        # `run()` only writes the killed call's output when it is given a log
        # path, so both main-line call sites must pass theirs. Without this pin
        # reverting one to `run(full)` re-loses exactly the evidence the fix
        # exists to keep, and every other check stays green (#1692 r3 review P2).
        anchor = 'run(full, log_path=log_path)'
        message = '#1692 baseline/mutation pass their log path into run()'
        self.assertEqual(source.count(anchor), 2, message)
        self.assertNotIn('run(full)\n', source, message)

    def test_negative_controls_deadline_keeps_partial_logs(self):
        source = (Path(__file__).resolve().parent
                  / 'fixtures/session-assembly-1316/negative_controls.py').read_text()
        self.assert_deadline_log_call_sites(source)
        for reverted in (source.replace('run(full, log_path=log_path)', 'run(full)', 1),
                         source.replace('run(full, log_path=log_path)', 'run(full)', 2)):
            with self.subTest(reverted=reverted.count('run(full)\n')):
                with self.assertRaises(AssertionError):
                    self.assert_deadline_log_call_sites(reverted)
        print('#1692 reverted log_path call site: KILLED', flush=True)

    def test_ci_rejects_missing_wrong_or_step_level_debug_profiles(self):
        workflow = self.ci_workflow_fixture()
        jobs = self.ci_jobs()
        block = ('    env:\n'
                 '      CARGO_PROFILE_DEV_DEBUG: line-tables-only\n'
                 '      CARGO_PROFILE_TEST_DEBUG: line-tables-only\n')
        # #1692 §3: the negative-controls env block also carries a comment and
        # CARGO_TARGET_DIR, so the shared anchor is the two profile lines, not the
        # whole `env:` block. Both jobs must contain that anchor exactly once.
        profiles = ('      CARGO_PROFILE_DEV_DEBUG: line-tables-only\n'
                    '      CARGO_PROFILE_TEST_DEBUG: line-tables-only\n')
        # #1692 §3: the same env block now exists in two jobs, so every mutation is
        # applied INSIDE one job body. Mutating the first textual occurrence in the
        # whole workflow would silently hit negative-controls (it is declared first)
        # and leave `rust` intact - i.e. the control would pass without testing.
        targets = {
            'rust': ('test_rust_job_uses_line_tables_only_debug',
                     '#918 Rust debug profiles are job-level line-tables-only',
                     '#918 Rust debug profiles have no step overrides',
                     '#918 Rust step env must be an explicit block mapping'),
            'negative-controls': (
                'test_negative_controls_job_uses_line_tables_only_debug',
                '#1692 negative-controls debug profiles are job-level line-tables-only',
                '#1692 negative-controls debug profiles have no step overrides',
                '#1692 negative-controls step env must be an explicit block mapping'),
        }
        for job, (target, level, step_level, block_level) in targets.items():
            body = jobs[job]
            self.assertEqual(workflow.count(body), 1, f'#918 unique {job} job body')
            self.assertEqual(body.count(profiles), 1, f'#918 {job} has one debug env block')

            def mutate(*pairs):
                changed = body
                for old, new in pairs:
                    changed = changed.replace(old, new, 1)
                return workflow.replace(body, changed, 1)

            cases = [('missing-env', mutate((profiles, '')), level)]
            for profile in ('DEV', 'TEST'):
                line = f'      CARGO_PROFILE_{profile}_DEBUG: line-tables-only\n'
                for value in ('', f'      CARGO_PROFILE_{profile}_DEBUG: full\n'):
                    cases.append((f'{profile}-{value!r}', mutate((line, value)), level))
            anchor = [line for line in body.splitlines(keepends=True)
                      if 'rustup show active-toolchain' in line]
            self.assertEqual(len(anchor), 1, f'#918 {job} has one toolchain step')
            anchor = anchor[0]
            step_env = ''.join('    ' + line for line in block.splitlines(keepends=True))
            cases.append(('step-only', mutate((profiles, ''), (anchor, anchor + step_env)), level))
            cases.append(('step-override', mutate((anchor, anchor + step_env)), step_level))
            for env in ('        env: {CARGO_PROFILE_DEV_DEBUG: full}\n',
                        "        env:\n          'CARGO_PROFILE_DEV_DEBUG': full\n",
                        '        env:\n          "CARGO_PROFILE_TEST_DEBUG": full\n'):
                cases.append((env.strip(), mutate((anchor, anchor + env)), step_level))
            for value in ('*profiles', '${{ fromJSON(env.PROFILE_JSON) }}'):
                env = f'        env: {value}\n'
                changed = mutate((anchor, anchor + env))
                if value == '*profiles':
                    changed = 'env: &profiles\n  CARGO_PROFILE_DEV_DEBUG: full\n\n' + changed
                else:
                    changed = '''env:\n  PROFILE_JSON: '{"CARGO_PROFILE_DEV_DEBUG":"full"}'\n\n''' + changed
                cases.append((value, changed, block_level))
            for entry in ('*profiles', '<<: *profiles'):
                env = f'        env:\n          {entry}\n'
                changed = ('env: &profiles\n  CARGO_PROFILE_DEV_DEBUG: full\n\n'
                           + mutate((anchor, anchor + env)))
                cases.append((entry, changed, block_level))
            for name, changed, message in cases:
                with self.subTest(job=job, mutation=name):
                    self.assert_ci_rejects(changed, target, message)

    def test_ci_rejects_wrong_negative_controls_timeout(self):
        # #1692 §1: the job timeout must stay literal and above the fixture budget.
        workflow = self.ci_workflow_fixture()
        body = self.ci_jobs(issue='#1692')['negative-controls']
        self.assertEqual(workflow.count(body), 1, '#1692 unique negative-controls job body')

        def mutate(old, new):
            return workflow.replace(body, body.replace(old, new, 1), 1)

        target = 'test_negative_controls_job_timeout_exceeds_fixture_deadline'
        profiles = ('      CARGO_PROFILE_DEV_DEBUG: line-tables-only\n'
                    '      CARGO_PROFILE_TEST_DEBUG: line-tables-only\n')
        for name, changed, message in (
                ('missing', mutate('    timeout-minutes: 150\n', ''),
                 '#1692 negative-controls job has one literal timeout-minutes'),
                ('expression', mutate('    timeout-minutes: 150\n',
                                      '    timeout-minutes: ${{ env.NEGCTL_MINUTES }}\n'),
                 '#1692 negative-controls job has one literal timeout-minutes'),
                ('below-deadline', mutate('    timeout-minutes: 150\n',
                                          '    timeout-minutes: 120\n'),
                 '#1692 the job timeout must outlive the fixture deadline plus the'),
                # An env override silently replaces the fixture's own budget: the
                # literal timeout above would still read 150 while the deadline
                # ran past it.
                ('deadline-env-override',
                 mutate(profiles, profiles + "      PDRS_NEGCTL_DEADLINE: '9000'\n"),
                 '#1692 negative-controls must not set PDRS_NEGCTL_DEADLINE: the fixture '
                 'deadline is fixed by DEADLINE_SECONDS, not by the workflow'),
                # A top-level `env:` is applied to every job but lives before
                # `jobs:`, so a job-body-only scan would miss it entirely.
                ('deadline-env-toplevel-override',
                 "env:\n  PDRS_NEGCTL_DEADLINE: '9000'\n\n" + workflow,
                 '#1692 negative-controls must not set PDRS_NEGCTL_DEADLINE: the fixture '
                 'deadline is fixed by DEADLINE_SECONDS, not by the workflow')):
            with self.subTest(mutation=name):
                self.assert_ci_rejects(changed, target, message)
                print(f'#1692 timeout mutation {name}: KILLED', flush=True)

    def test_b2_and_smoke_keep_commands_in_their_own_jobs(self):
        jobs = self.ci_jobs(issue='#918')
        shell_key = r"""(?:^|[\s{,])['"]?shell['"]?\s*:"""
        self.assertNotRegex(self.ci_workflow_fixture().split('\njobs:\n', 1)[0], shell_key,
                            '#918 workflow preserves default fail-fast shell')
        steps = {job: self.ci_steps(job, body, issue='#918') for job, body in jobs.items()}
        commands = {job: [self.ci_run_commands(step, issue='#918') for step in items]
                    for job, items in steps.items()}
        b2 = [
            'python3 -I src/pilotdeck-gateway/tests/fixtures/frozen-reinsert-guard/test_entry_guard.py',
            'python3 -I src/pilotdeck-gateway/tests/fixtures/frozen-reinsert-guard/test_negative_controls.py',
            'python3 -I scripts/test-approval-provenance.py',
            'python3 -I scripts/test-review-deepseek.py',
            'python3 -I scripts/test-review-minimax.py',
            'python3 -I scripts/test-review-roles.py',
            'python3 -I scripts/test-check-taskbook-segments.py',
            'bash scripts/test-pdrs-loop.sh',
            'bash scripts/test-worktree-count.sh',
            'bash scripts/test-disk-budget.sh',
            'python3 -I scripts/check-approval-provenance.py',
            'python3 -I scripts/ui-server-parity/check_frozen.py',
        ]
        smoke = ['python3 -I scripts/test-smoke-real-provider.py']
        for label, owner, title, expected in (
                ('B2', 'a51-coverage', 'B2 冻结与批准依据（干净检出）', b2),
                ('smoke', 'rust', 'smoke provider 装置正负控（含 Rust probe）', smoke)):
            locations = [(job, i) for job, items in steps.items() for i, step in enumerate(items)
                         if step.startswith(f'name: {title}\n')]
            self.assertEqual([job for job, _ in locations], [owner],
                             f'#918 {label} step belongs only to {owner}')
            index = locations[0][1]
            step = steps[owner][index]
            self.assertEqual(commands[owner][index], expected,
                             f'#918 {label} preserves exact commands and order')
            for command in expected:
                # Match the script path too, so changed flags/extra duplicate invocations fail closed.
                path = command.split()[-1]
                occurrences = [(job, i) for job, blocks in commands.items()
                               for i, block in enumerate(blocks) for value in block if path in value]
                self.assertEqual(occurrences, [(owner, index)],
                                 f'#918 command occurs once in {owner}: {path}')
            self.assertEqual(re.findall(r'^        working-directory: (.*)$', step, re.M), ['.'],
                             f'#918 {label} runs at repository root')
            if label == 'B2':
                self.assertEqual(re.findall(r'^        run: (.*)$', step, re.M), ['|'],
                                 '#918 B2 preserves literal run form')
            self.assertNotRegex(step, r"""(?m)^(?:        |)['"]?(?:if|continue-on-error)['"]?:""",
                                f'#918 {label} step cannot skip or ignore failure')
            prefix = jobs[owner].split('    steps:\n', 1)[0]
            self.assertNotRegex(step + prefix, shell_key,
                                f'#918 {label} preserves default fail-fast shell')
            # #1199：这两个 job 现在经由 scope 门控跳过，但 needs / if 仍是精确字符串，
            # 不许再加别的条件（原来的「无条件」断言放宽到这一对）。
            gating = re.findall(r"""(?m)^    ['"]?(if|needs|continue-on-error)['"]?\s*:\s*(.*)$""",
                                prefix)
            self.assertEqual(gating, [('needs', '[scope]'),
                                     ('if', "${{ needs.scope.outputs.docs_only != 'true' }}")],
                             f'#1199 {owner} skips only on the scope classifier')
            self.assertTrue(steps[owner][0].startswith('uses: actions/checkout@v4\n'),
                            f'#918 {owner} starts with checkout')
            checkout_lines = [line.strip() for line in steps[owner][0].splitlines()
                              if line.strip() and not line.lstrip().startswith('#')]
            expected_checkout = ['uses: actions/checkout@v4']
            if owner == 'rust':
                expected_checkout += ['with:', 'fetch-depth: 0']
            self.assertEqual(checkout_lines, expected_checkout,
                             f'#918 {owner} checkout preserves clean root defaults')
            if label == 'B2':
                self.assertEqual(index, 1, '#918 B2 immediately follows checkout before npm and clone')
            else:
                prior = commands[owner][:index]
                prerequisites = ['rustup show active-toolchain', self.nextest_command, self.doctest_command]
                positions = [[i for i, block in enumerate(prior) if block == [command]]
                             for command in prerequisites]
                self.assertTrue(all(len(found) == 1 for found in positions),
                                '#918 smoke follows Rust toolchain and workspace tests')
                self.assertEqual([found[0] for found in positions], sorted(found[0] for found in positions),
                                '#918 smoke Rust prerequisites keep their order')

    def ci_b2_smoke_mutations(self):
        workflow = self.ci_workflow_fixture()
        jobs = self.ci_jobs(issue='#918')
        titles = {'B2': ('a51-coverage', 'B2 冻结与批准依据（干净检出）'),
                  'smoke': ('rust', 'smoke provider 装置正负控（含 Rust probe）')}
        cases = []

        def change(label, owner, old, new, message):
            self.assertEqual(jobs[owner].count(old), 1, f'#918 unique mutation anchor: {label}')
            changed = jobs[owner].replace(old, new, 1)
            cases.append((label, workflow.replace(jobs[owner], changed, 1), message))

        for label, (owner, title) in titles.items():
            steps = self.ci_steps(owner, jobs[owner], issue='#918')
            matching = [step for step in steps if step.startswith(f'name: {title}\n')]
            self.assertEqual(len(matching), 1, f'#918 unique {label} control step')
            step = matching[0]
            raw = '      - ' + step
            name_message = f'#918 {label} step belongs only to {owner}'
            command_message = f'#918 {label} preserves exact commands and order'
            change(f'{label}-delete-step', owner, raw, '', name_message)
            for destination in jobs:
                if destination == owner:
                    continue
                for action in ('move', 'copy'):
                    changed = workflow.replace(jobs[owner], jobs[owner].replace(raw, '', 1), 1) if action == 'move' else workflow
                    destination_body = jobs[destination] + raw
                    cases.append((f'{label}-{action}-to-{destination}',
                                  changed.replace(jobs[destination], destination_body, 1), name_message))
            lines = [line for line in raw.splitlines(keepends=True)
                     if 'scripts/' in line]
            for i, line in enumerate(lines):
                # For smoke preserve a valid run field when removing its only command.
                replacement = '        run: echo removed\n' if label == 'smoke' else ''
                change(f'{label}-delete-command-{i}', owner, line, replacement, command_message)
                change(f'{label}-change-command-{i}', owner, line,
                       line.replace('scripts/', 'scripts/missing/', 1), command_message)
                command = line.strip().removeprefix('run: ')
                path = command.split()[-1]
                change(f'{label}-duplicate-command-{i}', owner, raw,
                       raw + f'      - run: {command}\n',
                       f'#918 command occurs once in {owner}: {path}')
            if label == 'B2':
                for i in range(len(lines) - 1):
                    change(f'B2-swap-{i}-{i + 1}', owner, raw,
                           raw.replace(lines[i] + lines[i + 1], lines[i + 1] + lines[i], 1), command_message)
                change('B2-folded-run', owner, raw, raw.replace('run: |', 'run: >', 1),
                       '#918 B2 preserves literal run form')
                for prefix in ('      - run: npm ci\n', '      - run: git init pilotdeck-baseline\n'):
                    change(f'B2-after-{prefix.strip()}', owner, raw, prefix + raw,
                           '#918 B2 immediately follows checkout before npm and clone')
            else:
                change('smoke-extra-command', owner, raw,
                       raw.replace('        run: ', '        run: |\n          ', 1) + '          echo extra\n',
                       command_message)
                for command in ('rustup show active-toolchain', self.nextest_command, self.doctest_command):
                    # Relocate the actual smoke step before each prerequisite.
                    removed = jobs[owner].replace(raw, '', 1)
                    prerequisites = [item for item in steps
                                     if self.ci_run_commands(item, issue='#918') == [command]]
                    self.assertEqual(len(prerequisites), 1, f'#918 unique smoke prerequisite: {command}')
                    prerequisite = '      - ' + prerequisites[0]
                    changed = removed.replace(prerequisite, raw + prerequisite, 1)
                    cases.append((f'smoke-before-{command}', workflow.replace(jobs[owner], changed, 1),
                                  '#918 smoke follows Rust toolchain and workspace tests'))
                first, second = 'run: rustup show active-toolchain', 'run: ' + self.nextest_command
                swapped = jobs[owner].replace(first, 'run: SWAP', 1).replace(second, first, 1).replace('run: SWAP', second, 1)
                cases.append(('smoke-swapped-prerequisites', workflow.replace(jobs[owner], swapped, 1),
                              '#918 smoke Rust prerequisites keep their order'))
            for cwd in ('', '        working-directory: src\n'):
                change(f'{label}-cwd-{cwd.strip()}', owner, raw,
                       raw.replace('        working-directory: .\n', cwd, 1),
                       f'#918 {label} runs at repository root')
            for field in ('if: false', "'if': false", '"if": false', 'continue-on-error: true',
                          "'continue-on-error': true", '"continue-on-error": true'):
                change(f'{label}-step-{field}', owner, raw,
                       raw.replace('        working-directory:', f'        {field}\n        working-directory:', 1),
                       f'#918 {label} step cannot skip or ignore failure')
            for field in ('if: false', "'if': false", '"if": false', 'continue-on-error: true',
                          "'continue-on-error': true", '"continue-on-error": true',
                          'needs: metadata-observer', "'needs': metadata-observer", '"needs": metadata-observer'):
                change(f'{label}-job-{field}', owner, '    steps:\n',
                       f'    {field}\n    steps:\n', f'#1199 {owner} skips only on the scope classifier')
            checkout = '      - ' + steps[0]
            change(f'{label}-missing-checkout', owner, checkout, '', f'#918 {owner} starts with checkout')
            for field in ('if: false', "'if': false", '"if": false', 'continue-on-error: true',
                          "'continue-on-error': true", '"continue-on-error": true'):
                change(f'{label}-checkout-{field}', owner, checkout,
                       checkout.replace('uses: actions/checkout@v4\n', f'uses: actions/checkout@v4\n        {field}\n', 1),
                       f'#918 {owner} checkout preserves clean root defaults')
            for field in ('path: nested', "'path': nested", '"path": nested', 'sparse-checkout: src',
                          'filter: blob:none', 'repository: example/other', 'ref: main'):
                # Keep Rust's existing with/fetch-depth mapping valid.
                altered = (checkout.replace('        with:\n', f'        with:\n          {field}\n', 1)
                           if '        with:\n' in checkout else
                           checkout + f'        with:\n          {field}\n')
                change(f'{label}-checkout-{field}', owner, checkout, altered,
                       f'#918 {owner} checkout preserves clean root defaults')
            if owner == 'rust':
                change('smoke-checkout-shallow', owner, checkout,
                       checkout.replace('fetch-depth: 0', 'fetch-depth: 1', 1),
                       '#918 rust checkout preserves clean root defaults')
            for key in ('shell', "'shell'", '"shell"'):
                change(f'{label}-step-{key}', owner, raw,
                       raw.replace('        working-directory:', f'        {key}: bash {{0}}\n        working-directory:', 1),
                       f'#918 {label} preserves default fail-fast shell')
                if owner == 'rust':
                    old = '    defaults:\n      run:\n'
                    new = old + f'        {key}: bash {{0}}\n'
                else:
                    old = '    steps:\n'
                    new = f'    defaults:\n      run:\n        {key}: bash {{0}}\n' + old
                change(f'{label}-job-default-{key}', owner, old, new,
                       f'#918 {label} preserves default fail-fast shell')
        for defaults in ('defaults:\n  run:\n    shell: bash {0}\n',
                         'defaults: {run: {shell: "bash {0}"}}\n',
                         "defaults:\n  run:\n    'shell': bash {0}\n"):
            cases.append((f'workflow-{defaults!r}', defaults + workflow,
                          '#918 workflow preserves default fail-fast shell'))
        return cases

    def test_ci_rejects_b2_and_smoke_placement_drift(self):
        self.check_ci_mutations(self.ci_b2_smoke_mutations(),
                                'test_b2_and_smoke_keep_commands_in_their_own_jobs', '#918')

    def check_ci_mutations(self, cases, target, issue, reader='read_text'):
        full_name = f'{__name__}.EnvironmentControls.{target}'
        results = []
        # The shared negative_suite runs Rust source mutations; these are only in-memory YAML.
        # Run and classify every case, then fail once, including unexpected exceptions as INVALID.
        for name, workflow, message in cases:
            status, reason = 'INVALID', 'positive control did not complete'
            positive = restored = False
            try:
                getattr(self, target)()
                positive = True
                with patch.object(Path, reader, return_value=workflow):
                    try:
                        getattr(self, target)()
                    except AssertionError as error:
                        status = 'KILLED' if message in str(error) else 'INVALID'
                        reason = str(error)
                    else:
                        status, reason = 'SURVIVED', 'bound test passed mutated workflow'
                getattr(self, target)()  # The real workflow must still pass after restoration.
                restored = True
            except Exception as error:
                status, reason = 'INVALID', f'{type(error).__name__}: {error}'
            results.append((name, status))
            print(f'{name}: {status}: positive={positive}, restored={restored}: '
                  f'{full_name}: {message}: {reason}', flush=True)
        counts = {status: sum(result == status for _, result in results)
                  for status in ('KILLED', 'SURVIVED', 'INVALID')}
        print(f'{issue} negative summary: {counts}', flush=True)
        self.assertTrue(results and all(status == 'KILLED' for _, status in results),
                        f'{issue} every mutation must fail its bound assertion: {results}')

    def assert_ci_rejects(self, workflow, test_name, message):
        # Invoke the real CI check on an in-memory workflow mutation, never edit ci.yml.
        # subTest callers continue through every case even if one mutation survives.
        with patch.object(Path, 'read_text', return_value=workflow):
            with self.assertRaisesRegex(AssertionError, re.escape(message),
                                        msg=f'#904 {test_name} must reject at {message}'):
                getattr(self, test_name)()

    def test_ci_job_inventory_rejects_new_missing_and_unparsed_jobs(self):
        workflow = self.ci_workflow_fixture()
        target = 'test_each_npm_job_sets_up_its_own_node_before_installing'
        for name in self.ci_jobs():
            with self.subTest(renamed_job=name):
                self.assert_ci_rejects(workflow.replace(f'  {name}:\n', f'  renamed-{name}:\n', 1),
                                       target, '#904 exact CI job inventory')
        for declaration, message in (
                ('  extra:\n    steps:\n      - run: npm test\n', '#904 exact CI job inventory'),
                ('  "extra": {steps: [{run: npm test}]}\n', '#904 unsupported job declaration'),
                ('  extra: *other-job\n', '#904 unsupported job declaration'),
                ('  rust:\n    steps:\n      - run: npm test\n', '#904 unique job names')):
            with self.subTest(declaration=declaration):
                self.assert_ci_rejects(workflow + declaration, target, message)

    def test_ci_run_blocks_preserve_commands_and_require_prior_node(self):
        workflow = self.ci_workflow_fixture()
        target = 'test_each_npm_job_sets_up_its_own_node_before_installing'
        anchor = '      - run: rustup show active-toolchain\n'
        self.assertEqual(workflow.count(anchor), 1, '#904 block control has one rust step anchor')
        for scalar in ('|', '>', '|-', '>-', '|+', '>+', '|2', '>2-', ' |', '| ', '>- # comment', '|2+ # comment'):
            for indent in (9, 10, 12):
                for inline in (True, False):
                    with self.subTest(scalar=scalar, indent=indent, inline=inline):
                        step = ('run: ' if inline else 'name: block\n        run: ') + scalar + '\n'
                        step += ' ' * indent + 'npm test\n'
                        self.assertEqual(self.ci_run_commands(step), ['npm test'],
                                         '#904 legal run block preserves Node commands')
                        changed = workflow.replace(anchor, '      - ' + step, 1)
                        self.assert_ci_rejects(changed, target,
                            '#904 rust setup-node must precede Node commands in its own job')
                        # The same content passes once this job sets up Node before it.
                        changed = changed.replace('      - ' + step,
                            '      - uses: actions/setup-node@v4\n      - ' + step, 1)
                        with patch.object(Path, 'read_text', return_value=changed):
                            getattr(self, target)()

    def test_ci_unrecognized_and_empty_run_values_fail_closed(self):
        workflow = self.ci_workflow_fixture()
        anchor = '      - run: rustup show active-toolchain\n'
        target = 'test_each_npm_job_sets_up_its_own_node_before_installing'
        self.assertEqual(workflow.count(anchor), 1, '#904 invalid run control has one rust step anchor')
        for value in ('', '# comment', '|bad', '>-2', '[]', '{command: npm test}',
                      '*commands', '&commands npm test', '!!str npm test', '${{ inputs.command }}',
                      '"n\\u0070m test"'):
            with self.subTest(value=value):
                self.assert_ci_rejects(workflow.replace(anchor, f'      - run: {value}\n', 1),
                                       target, '#904 unsupported run scalar')
        for value in ('|', '|2', '> # comment'):
            with self.subTest(empty_block=value):
                self.assert_ci_rejects(workflow.replace(anchor, f'      - run: {value}\n          \n', 1),
                                       target, '#904 run block must not be empty')
        self.assert_ci_rejects(workflow.replace(anchor, '      - run: echo safe\n          npm test\n', 1),
                               target, '#904 single-line run cannot have a continuation')
        self.assert_ci_rejects(workflow.replace(anchor, anchor + '        run: npm test\n', 1),
                               target, '#904 step has at most one run field')

    def test_ci_quoted_run_keys_and_sequence_spacing_cannot_hide_node(self):
        workflow = self.ci_workflow_fixture()
        target = 'test_each_npm_job_sets_up_its_own_node_before_installing'
        anchor = '      - run: rustup show active-toolchain\n'
        self.assertEqual(workflow.count(anchor), 1, '#919 unique run key mutation anchor')
        cases = []
        for spaces in (1, 2, 4):
            indent = ' ' * (7 + spaces)
            for key in ('run', "'run'", '"run"'):
                for inline in (True, False):
                    for value in ('npm test\n', '|\n' + indent + '  npm test\n'):
                        step = '      -' + ' ' * spaces
                        step += (key + ': ' if inline else 'name: node\n' + indent + key + ': ') + value
                        changed = workflow.replace(anchor, step, 1)
                        cases.append((f'{spaces}-{key}-{inline}-{value!r}', changed,
                                      '#904 rust setup-node must precede Node commands in its own job'))
                        # The exact same key/indentation is allowed with a prior unconditional setup.
                        positive = changed.replace(step, '      - "uses": actions/setup-node@v4\n' + step, 1)
                        with patch.object(Path, 'read_text', return_value=positive):
                            getattr(self, target)()
        for key in ('if', "'if'", '"if"'):
            step = f'      - uses: actions/setup-node@v4\n        {key}: false\n      - run: npm test\n'
            cases.append((f'conditional-setup-{key}', workflow.replace(anchor, step, 1),
                          '#904 rust setup-node must precede Node commands in its own job'))
        self.check_ci_mutations(cases, target, '#919 recognized keys')

    def test_ci_unknown_step_key_shapes_fail_closed(self):
        workflow = self.ci_workflow_fixture()
        anchor = '      - run: rustup show active-toolchain\n'
        self.assertEqual(workflow.count(anchor), 1, '#919 unique unknown key mutation anchor')
        cases = []
        for body in ('"r\\u0075n": npm test', '? run\n        : npm test',
                     '{run: npm test}', '*step', '&step run: npm test',
                     '!!str run: npm test', '"run: npm test', 'run', '1: npm test',
                     'name: hidden\n        <<: *step', 'name: hidden\n       run: npm test'):
            cases.append((body, workflow.replace(anchor, '      - ' + body + '\n', 1),
                          '#919 unsupported step'))
        cases.append(('unknown-key', workflow.replace(anchor, '      - runs: npm test\n', 1),
                      '#919 unsupported step key'))
        # #1199：scope 排在 jobs 首位后，首个 `    steps:` 锚点不再属于 metadata-observer，
        # 按 job 头定位它自己的 steps，避免误杀到别的 job。
        start = workflow.index('  metadata-observer:\n')
        flow = workflow.index('    steps:\n', start)
        cases.append(('flow-sequence',
                      workflow[:flow] + '    steps: [{run: npm test}]\n' + workflow[flow + len('    steps:\n'):],
                      '#904 metadata-observer has one steps sequence'))
        cases.append(('quoted-duplicate', workflow.replace(anchor, anchor + '        "run": npm test\n', 1),
                      '#904 step has at most one run field'))
        cases.append(('duplicate-uses', workflow.replace(anchor,
                     '      - uses: actions/setup-node@v4\n        "uses": some/action@v1\n', 1),
                      '#919 duplicate step key'))
        self.check_ci_mutations(cases, 'test_each_npm_job_sets_up_its_own_node_before_installing', '#919 unknown keys')

    def test_ci_node_command_family_requires_setup_for_every_invocation(self):
        workflow = self.ci_workflow_fixture()
        anchor = '      - run: rustup show active-toolchain\n'
        target = 'test_each_npm_job_sets_up_its_own_node_before_installing'
        self.assertEqual(workflow.count(anchor), 1, '#904 Node control has one rust step anchor')
        for command in ('npm ci', 'npm install', 'npm i', 'npm run generate', 'npm test', 'npm exec tool',
                        'npm start', 'npm --version', 'npx', 'node', 'exec node', 'true && node',
                        'yarn test', 'pnpm test', 'corepack enable', '"npm test"', "'npm test'"):
            with self.subTest(command=command):
                self.assert_ci_rejects(workflow.replace(anchor, f'      - run: {command}\n', 1), target,
                                       '#904 rust setup-node must precede Node commands in its own job')
        # Node words in names/env/comments are not commands; longer words have no token boundary.
        harmless = ('      - name: npm test\n        env:\n          EXAMPLE: node\n'
                    '        run: |\n          # npm test\n          echo anode npmx yarnish\n')
        with patch.object(Path, 'read_text', return_value=workflow.replace(anchor, harmless, 1)):
            getattr(self, target)()

    def test_ci_every_required_job_rejects_continue_on_error(self):
        workflow = self.ci_workflow_fixture()
        for name in self.ci_jobs():
            for key in ('continue-on-error', "'continue-on-error'", '"continue-on-error"', 'continue-on-error '):
                with self.subTest(job=name, key=key):
                    self.assert_ci_rejects(workflow.replace(f'  {name}:\n',
                        f'  {name}:\n    {key}: true\n', 1),
                        'test_required_check_gates_observer_failure_instead_of_skipping',
                        f'#904 {name} cannot ignore failure')

    def test_target_output_overlap_file_and_nonempty_target_are_rejected_without_writes(self):
        target = self.root/'new-target'
        for output in (target, target/'inside', self.root):
            with self.assertRaisesRegex(RuntimeError, 'disjoint'):
                driver.validate_output_target(target, output)
            self.assertFalse(target.exists())
        target.write_text('owned sentinel')
        with self.assertRaisesRegex(RuntimeError, 'directory'):
            driver.validate_output_target(target, self.root/'separate')
        self.assertEqual(target.read_text(), 'owned sentinel')
        target.unlink();target.mkdir();(target/'sentinel').write_text('keep')
        with self.assertRaisesRegex(RuntimeError, 'fresh target required'):
            driver.validate_output_target(target,self.root/'separate')
        self.assertEqual((target/'sentinel').read_text(),'keep')

    def test_sources_missing_or_changed_are_not_silent_none(self):
        with patch.object(driver, 'SOURCE_FILES', ('source.rs',)):
            with self.assertRaises(RuntimeError): driver.source_snapshot(self.root)
            source = self.root/'source.rs'
            source.write_text('before')
            before = driver.source_snapshot(self.root)
            source.write_text('after')
            self.assertNotEqual(before, driver.source_snapshot(self.root))

    def test_tool_and_rustup_postflight_changes_are_observable(self):
        before = driver.rustup_snapshot(self.home/'.rustup')
        (self.toolchain/'bin/rustc').write_text('changed mock bytes')
        after = driver.rustup_snapshot(self.home/'.rustup')
        self.assertNotEqual(before, after)
        (self.home/'.rustup/downloads').mkdir()
        self.assertNotEqual(after, driver.rustup_snapshot(self.home/'.rustup'))

    def test_receipt_missing_duplicate_extra_wrong_or_empty_ids_fail(self):
        ids = {str(n) for n in range(10)}
        good = ''.join('MARK '+str(n)+'\n' for n in range(10))
        driver.check_receipts(good, 'MARK', ids)
        for broken in ('', good.rsplit('MARK ', 1)[0], good+'MARK 0\n', good.replace('MARK 9', 'MARK 0'), good.replace('MARK 9', 'MARK other')):
            with self.subTest(broken=broken), self.assertRaises(RuntimeError):
                driver.check_receipts(broken, 'MARK', ids)
        with self.assertRaises(RuntimeError): driver.check_receipts(good, 'MARK', ids-{str(9)}|{''})

    def test_restore_requires_receipt_owner_count_and_real_modes(self):
        import json
        scratch = self.root/'restore'; scratch.mkdir()
        (scratch/'.metadata-oracle-owner').write_text('owner')
        permissions = []
        for n in range(3):
            path = scratch/str(n); path.mkdir(); path.chmod(0o755)
            permissions.append(dict(path=str(path), mode=0o755))
        fixture = self.root/'oracle.json'
        plan=dict(owner='owner', scratch=str(scratch), permissions=permissions)
        fixture.write_text(json.dumps(plan))
        receipt = Path(str(fixture)+'.cleanup.json')
        good = dict(owner='owner', restored=3)
        with self.assertRaises(FileNotFoundError): driver.check_restore(fixture,plan)
        for data in ({}, dict(good, owner='wrong'), dict(good, restored=0), dict(good, restored=True)):
            receipt.write_text(json.dumps(data))
            with self.assertRaises(RuntimeError): driver.check_restore(fixture,plan)
        receipt.write_text(json.dumps(good)); driver.check_restore(fixture,plan)
        Path(permissions[0]['path']).chmod(0o111)
        with self.assertRaises(RuntimeError): driver.check_restore(fixture,plan)
        Path(permissions[0]['path']).chmod(0o755)
        (scratch/'.metadata-oracle-owner').write_text('wrong')
        with self.assertRaises(RuntimeError): driver.check_restore(fixture,plan)

    def test_postflight_rejects_source_binary_component_and_cache_changes(self):
        for change in ('source', 'static-fixture', 'binary', 'components', 'rustup-entry', 'node-cache'):
            with self.subTest(change=change):
                case = EnvironmentControls('test_missing_component_fails_before_any_proxy'); case.setUp()
                try:
                    source=case.root/'source.rs'; source.write_text('original')
                    fixture_name='src/pilotdeck-gateway/tests/fixtures/metadata/describe-io-errors.json'
                    fixture=case.root/fixture_name;fixture.parent.mkdir(parents=True);fixture.write_text('{"original":true}')
                    with patch.object(driver, 'SOURCE_FILES', ('source.rs',fixture_name)):
                        pf=driver.toolchain_preflight(case.toolchain,case.original,case.root)
                        before=driver.source_snapshot(case.root); rustup=driver.rustup_snapshot(case.home/'.rustup')
                        node=case.toolchain/'bin/rustfmt'; node_sha=driver.sha(node); node_home=case.root/'node-home';node_home.mkdir()
                        driver.check_postflight({},case.root,case.toolchain,case.original,pf,before,rustup,node,node_sha,node_home)
                        if change=='source': source.write_text('changed')
                        if change=='static-fixture': fixture.write_text('{"forged":true}')
                        if change=='binary': (case.toolchain/'bin/rustc').write_text('changed binary')
                        if change=='components': (case.toolchain/'lib/rustlib/components').write_text('cargo-mock-host')
                        if change=='rustup-entry': (case.home/'.rustup/tmp').mkdir()
                        if change=='node-cache': (node_home/'.rustup').mkdir()
                        with self.assertRaises((OSError,RuntimeError)):
                            driver.check_postflight({},case.root,case.toolchain,case.original,pf,before,rustup,node,node_sha,node_home)
                finally: case.doCleanups()

    def test_workspace_git_is_rejected_before_execution(self):
        shim=self.root/'git';shim.write_text('never execute');shim.chmod(0o755)
        with patch.object(driver.subprocess,'Popen',side_effect=AssertionError('no process')) as process:
            with self.assertRaises(RuntimeError):driver.external_git(shim,(self.root,))
            process.assert_not_called()

    def test_protected_outputs_are_rejected_without_writes(self):
        before = {str(p.relative_to(self.home)) for p in self.home.rglob('*')}
        for suffix in ('.pilotdeck', '.cargo', '.rustup', 'ws/PilotDeck-rust-program-20261009'):
            destination = self.home/suffix/'new'
            self.assertFalse(os.path.lexists(destination))
            with self.assertRaises(RuntimeError): driver.reject_protected_paths((destination,), self.home)
            self.assertFalse(os.path.lexists(destination))
        alias = self.root/'protected-alias';alias.symlink_to(self.home/'.rustup',target_is_directory=True)
        with self.assertRaises(RuntimeError): driver.reject_protected_paths((alias/'new',),self.home)
        self.assertFalse(os.path.lexists(alias/'new'))
        self.assertEqual(before,{str(p.relative_to(self.home)) for p in self.home.rglob('*')})


class FailureRestoreControls(unittest.TestCase):
    setUp = EnvironmentControls.setUp
    def test_main_failure_still_restores_and_noop_exit_zero_fails(self):
        import contextlib
        import io
        import json
        for scenario in ('replay-failure', 'dynamic-snapshot-failure', 'dynamic-zero-tests', 'dynamic-missing-receipt', 'dynamic-duplicate-receipt', 'dynamic-spurious-receipt', 'dynamic-oracle-drift', 'noop-restore', 'source-head-failure', 'source-head-timeout', 'source-head-spawn-error', 'source-head-empty', 'source-head-polluted', 'source-head-short', 'permission-target-replaced', 'permission-mode-replaced', 'fixture-deleted', 'record-failure', 'record-timeout', 'record-output-limit', 'success'):
            with self.subTest(scenario=scenario):
                output = self.root / scenario
                pf = driver.toolchain_preflight(self.toolchain, self.original, self.root)
                names = []
                permissions, decoys = [], []
                def fake_run(command, cwd, environment, log, **kwargs):
                    name = log.stem; names.append(name)
                    text = ''
                    code, reason = 0, None
                    fixture = output / 'metadata-io.json'
                    if name == 'record':
                        scratch = output / 'recording'
                        (scratch / '.metadata-oracle-owner').write_text('owner')
                        for n in range(1 if scenario.startswith('record-') else 3):
                            path = scratch / str(n); path.mkdir(); path.chmod(0o755)
                            permission = dict(path=str(path), mode=path.stat().st_mode & 0o777)
                            permissions.append(permission)
                            text += 'PDRS_METADATA_PERMISSION '+json.dumps(dict(
                                baselineCommit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',
                                owner='owner',scratch=str(scratch),permission=permission))+'\n'
                            self.assertEqual(path.stat().st_mode & 0o777, permission['mode'])
                            self.assertEqual(permission['mode'], 0o755)
                            path.chmod(0o111)
                            self.assertEqual(path.stat().st_mode & 0o777, 0o111)
                        data=dict(baselineCommit='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',owner='owner',scratch=str(scratch),
                                  permissions=permissions,cases=[dict(id=str(n),status='RUN') for n in range(10)])
                        if scenario.startswith('record-'):
                            code,reason = {'record-failure':(9,None), 'record-timeout':(-9,'timeout'),
                                           'record-output-limit':(-9,'output limit')}[scenario]
                        else:
                            serialized=json.dumps(data);fixture.write_text(serialized)
                            text+='PDRS_METADATA_ORACLE '+json.dumps(serialized)+'\n'
                    if name == 'replay':
                        if scenario == 'replay-failure': code = 9
                        text = 'test result: ok. 3 passed; 0 failed; 2 ignored;\n'
                    if name == 'dynamic-snapshot':
                        if scenario == 'dynamic-snapshot-failure': code = 9
                        text = 'test result: ok. 1 passed; 0 failed; 0 ignored;\n'
                        text += ''.join('PDRS_NUMERIC_SNAPSHOT_OK '+str(n)+'\n' for n in range(10))
                        if scenario == 'dynamic-zero-tests': text = text.replace('1 passed', '0 passed')
                        if scenario == 'dynamic-missing-receipt': text = text.replace('PDRS_NUMERIC_SNAPSHOT_OK 9\n','')
                        if scenario == 'dynamic-duplicate-receipt': text = text.replace('PDRS_NUMERIC_SNAPSHOT_OK 9\n','PDRS_NUMERIC_SNAPSHOT_OK 0\n')
                        if scenario == 'dynamic-spurious-receipt': text = text.replace('PDRS_NUMERIC_SNAPSHOT_OK 9\n','PDRS_NUMERIC_SNAPSHOT_OK unknown\n')
                        if scenario == 'dynamic-oracle-drift': fixture.write_text('changed oracle')
                    if name == 'native-retained':
                        text = 'test result: ok. 2 passed; 0 failed; 0 ignored;\n'
                        text += ''.join('PDRS_NATIVE_METADATA_OK '+str(n)+'\n' for n in range(10))
                        if scenario in ('permission-target-replaced','permission-mode-replaced'):
                            data=json.loads(fixture.read_text())
                            if scenario=='permission-target-replaced':
                                for n in range(3):
                                    path=output/'recording'/('decoy-'+str(n));path.mkdir();path.chmod(0o700)
                                    decoys.append(path)
                                data['permissions']=[dict(path=str(path),mode=0o755) for path in decoys]
                            else:data['permissions']=[dict(item,mode=0o111) for item in permissions]
                            fixture.write_text(json.dumps(data))
                        if scenario=='fixture-deleted':fixture.unlink()

                    if name in ('replay', 'native-retained'):
                        text += ''.join('PDRS_METADATA_PAIR_OK '+str(n)+'\n' for n in range(10))
                    if name == 'source-head':
                        self.assertTrue(kwargs['discard_stderr'])
                        text = 'a'*40+'\n'
                        if scenario == 'source-head-failure': code = 9
                        if scenario == 'source-head-timeout': code, reason = -9, 'timeout'
                        if scenario == 'source-head-spawn-error': raise OSError('mock Git spawn denied')
                        if scenario == 'source-head-empty': text = ''
                        if scenario == 'source-head-polluted': text = 'warning\n'+'a'*40+'\n'
                        if scenario == 'source-head-short': text = 'a'*39+'\n'
                    if name == 'restore':
                        if scenario != 'noop-restore':
                            trusted=json.loads(command[-1])
                            self.assertEqual(trusted['permissions'],permissions)
                            for item in trusted['permissions']: Path(item['path']).chmod(item['mode'])
                        # A plausible receipt plus exit0 still cannot hide wrong modes.
                        Path(str(fixture)+'.cleanup.json').write_text(json.dumps(dict(owner='owner', restored=len(permissions))))
                    log.write_text(text)
                    return dict(exit=code, reason=reason, reaped=True, log=str(log),stdout=text)
                with patch.dict(driver.os.environ, self.original, clear=True), \
                     patch.object(driver, 'toolchain_preflight', return_value=pf), \
                     patch.object(driver, 'source_snapshot', return_value={}), \
                     patch.object(driver, 'rustup_snapshot', return_value={}), \
                     patch.object(driver.shutil, 'which', return_value=str(self.toolchain/'bin/cargo')), \
                     patch.object(driver, 'run_bounded', side_effect=fake_run), \
                     patch.object(driver, 'check_postflight'), \
                     patch.object(driver, 'external_git', side_effect=lambda executable, roots: executable), \
                     patch.object(driver.subprocess, 'Popen', side_effect=AssertionError('no external process')), \
                     contextlib.redirect_stdout(io.StringIO()):
                    code = driver.main(['--baseline',str(self.root), '--output',str(output),
                        '--target',str(self.root/'new-target'), '--toolchain',str(self.toolchain)])
                result = json.loads((output/'verification.json').read_text())
                self.assertEqual(code, 0 if scenario=='success' else 1)
                self.assertEqual(result['success'], scenario=='success')
                if scenario=='success':self.assertEqual(result['sourceHead'],'a'*40)
                self.assertEqual(names[-2:], ['restore','source-head'])
                if scenario.startswith('record-'):
                    self.assertEqual(names,['record','restore','source-head'])
                    expected = {'record-failure':'exit=9', 'record-timeout':'reason=timeout',
                                'record-output-limit':'reason=output limit'}[scenario]
                    self.assertIn(expected, result['error'])
                    self.assertTrue(result['oracleGuard']['restored'])
                if scenario.startswith('dynamic-'):
                    expected_error = {
                        'dynamic-snapshot-failure': 'dynamic-snapshot: exit=9, reason=None',
                        'dynamic-zero-tests': 'expected exactly one dynamic Snapshot test',
                        'dynamic-missing-receipt': 'PDRS_NUMERIC_SNAPSHOT_OK: expected ten unique actual-case receipts',
                        'dynamic-duplicate-receipt': 'PDRS_NUMERIC_SNAPSHOT_OK: expected ten unique actual-case receipts',
                        'dynamic-spurious-receipt': 'PDRS_NUMERIC_SNAPSHOT_OK: expected ten unique actual-case receipts',
                        'dynamic-oracle-drift': 'oracle bytes changed after recorder checkpoint',
                    }[scenario]
                    self.assertIn(expected_error, result['error'])
                    self.assertTrue(result['oracleGuard']['restored'])
                if scenario == 'replay-failure' or scenario.startswith('dynamic-'):
                    expected = ['record', 'replay']
                    if scenario.startswith('dynamic-'): expected.append('dynamic-snapshot')
                    self.assertEqual(names, expected+['restore', 'source-head'])
                    for item in permissions:self.assertEqual(os.stat(item['path']).st_mode & 0o777, 0o755)
                elif scenario == 'noop-restore':
                    self.assertIn('permission not restored', result['error'])
                    for item in permissions:Path(item['path']).chmod(0o755)
                elif scenario.startswith('source-head-'):
                    expected = {'source-head-failure':'exit=9', 'source-head-timeout':'reason=timeout',
                                'source-head-spawn-error':'mock Git spawn denied', 'source-head-empty':'forty lowercase hex',
                                'source-head-polluted':'forty lowercase hex','source-head-short':'forty lowercase hex'}[scenario]
                    self.assertIn(expected,result['error'])
                    self.assertIsNone(result['sourceHead'])
                if scenario in ('permission-target-replaced','permission-mode-replaced','fixture-deleted'):
                    self.assertTrue(result['oracleGuard']['restored'])
                    self.assertFalse(result['oracleGuard']['artifactUnchanged'])
                if scenario != 'noop-restore':
                    for item in permissions:self.assertEqual(os.stat(item['path']).st_mode & 0o777,0o755)
                for path in decoys:self.assertEqual(os.stat(path).st_mode & 0o777,0o700)
                if scenario != 'replay-failure' and not scenario.startswith(('record-', 'dynamic-')):
                    native = next(r for r in result['runs'] if r['name']=='native-retained')
                    replay = next(r for r in result['runs'] if r['name']=='replay')
                    self.assertEqual(native['environment']['PDRS_METADATA_IO_LIVE'],str(output/'metadata-io.json'))
                    self.assertNotIn('PDRS_METADATA_IO_LIVE',replay['environment'])


class WatchdogControls(unittest.TestCase):
    def test_real_python_success_timeout_and_hard_output_limit(self):
        import sys
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            cases=[('success', 'print("ok")', 5, None),
                   ('timeout', 'import time; time.sleep(5)', 0.05, 'timeout'),
                   ('output', 'import os; os.write(1,b"x"*1000000)', 5, 'output limit'),
                   ('exact-cap', 'import os; os.write(1,b"x"*128)', 5, None),
                   ('cap-plus-one', 'import os; os.write(1,b"x"*129)', 5, 'output limit')]
            for name,code,deadline,reason in cases:
                log=root/(name+'.log')
                result=driver.run_bounded([sys.executable,'-I','-c',code],root,{},log,deadline=deadline,limit=128)
                self.assertEqual(result['reason'],reason)
                self.assertTrue(result['reaped'])
                self.assertLessEqual(log.stat().st_size,128)
                if reason is None:self.assertEqual(result['exit'],0)
                if name in ('exact-cap','cap-plus-one'):self.assertEqual(log.read_bytes(),b'x'*128)

    def test_exited_leader_descendants_are_killed_with_and_without_stdout(self):
        import subprocess
        import sys
        import time
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            for holds_stdout,reason in ((True,'timeout'),(False,'surviving descendants')):
                pidfile=root/('held.pid' if holds_stdout else 'closed.pid')
                child_code='import time; time.sleep(60)'
                leader=('import subprocess,sys,pathlib; '
                    'child=subprocess.Popen([sys.executable,"-I","-c",'+repr(child_code)+']'
                    + (')' if holds_stdout else ',stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)')
                    + '; pathlib.Path('+repr(str(pidfile))+').write_text(str(child.pid))')
                result=driver.run_bounded([sys.executable,'-I','-c',leader],root,{},
                    root/(pidfile.stem+'.log'),deadline=0.2)
                self.assertEqual(result['reason'],reason)
                self.assertTrue(result['reaped'])
                pid=int(pidfile.read_text())
                state=''
                for _ in range(100):
                    state=subprocess.run(['/bin/ps','-o','stat=','-p',str(pid)],capture_output=True,text=True).stdout.strip()
                    if not state or state.startswith('Z'):break
                    time.sleep(0.01)
                self.assertTrue(not state or state.startswith('Z'),f'descendant still running: {pid} {state}')
                print(f'descendant stdout={holds_stdout}: leaderReaped={result["reaped"]}, reason={reason}, childState={state or "absent"}')

    def test_identity_stdout_is_not_polluted_by_stderr(self):
        import sys
        with tempfile.TemporaryDirectory() as temp:
            root=Path(temp)
            code='import os; os.write(2,b"warning on stderr\\n"); os.write(1,b"'+('a'*40)+'\\n")'
            merged=driver.run_bounded([sys.executable,'-I','-c',code],root,{},root/'merged.log')
            self.assertEqual(merged['exit'],0)
            self.assertIn('warning on stderr',merged['stdout'])
            self.assertIsNone(driver.re.fullmatch('[0-9a-f]{40}',merged['stdout'].strip()))
            result=driver.run_bounded([sys.executable,'-I','-c',code],root,{},root/'head.log',discard_stderr=True)
            self.assertEqual(result['exit'],0)
            self.assertIsNone(result['reason'])
            self.assertEqual(result['stdout'],'a'*40+'\n')
            self.assertEqual((root/'head.log').read_text(),'a'*40+'\n')

    def test_exit_between_poll_and_kill_is_expected(self):
        from types import SimpleNamespace
        with patch.object(driver.os,'killpg',side_effect=ProcessLookupError) as killpg:
            self.assertTrue(driver.stop_group(SimpleNamespace(pid=123)))
            killpg.assert_called_once_with(123,driver.signal.SIGKILL)


class AggregationAudit(unittest.TestCase):
    """#166 shared status contract and source-guard regressions, no source writes."""

    def setUp(self):
        self.root = Path(__file__).resolve().parents[3]
        self.fixture = self.root / 'src/pilotdeck-gateway/tests/fixtures'

    def test_negative_suites_require_parity_lock_and_report_busy_as_invalid(self):
        import contextlib
        import json
        import sys
        for folder in ('gateway-batch', 'session-domain-cleanup', 'gateway-entry-guard'):
            with self.subTest(folder=folder), tempfile.TemporaryDirectory() as temp:
                path = self.fixture / folder / 'negative_controls.py'
                spec = importlib.util.spec_from_file_location('negative_lock_' + folder, path)
                module = importlib.util.module_from_spec(spec)
                spec.loader.exec_module(module)
                guard = sys.modules['negative_process']
                calls = []

                @contextlib.contextmanager
                def busy(*_args, **kwargs):
                    calls.append(True)
                    raise TimeoutError('#166 controlled busy parity lock')
                    yield  # contextmanager; never enter a mutation window

                output = Path(temp) / 'out'
                names = [row[0] for row in module.MUTATIONS[:2]]
                argv = ['negative_controls.py', '--output', str(output)]
                for name in names:
                    argv.extend(['--only', name])
                raw_read = Path.read_bytes

                def git(_root, *args):
                    return raw_read(self.root / args[1].split(':', 1)[1]) if args[0] == 'show' else b''

                write_text = Path.write_text

                def evidence_only(path, *a, **k):
                    self.assertTrue(path.is_relative_to(Path(temp).resolve()),
                                    '#166 busy lock must never write source')
                    return write_text(path, *a, **k)

                # #1166: the pristine admission now runs a real external
                # scanner that needs genuine git; these #166 wiring probes
                # mock _git, so the scanner admission is pinned out here
                # (its own coverage lives in workspace-pristine). The write
                # paths are pinned symmetrically: a mutant that skips the busy
                # lock reaches the suite's on-disk mutation application, and
                # only pinning both leaves no residue on production source.
                with patch.object(guard, '_parity_mutation_lock', busy), \
                        patch.object(guard, '_git', git), \
                        patch.object(guard, '_workspace_literal_inputs', return_value=()), \
                        patch.object(guard, '_ensure_workspace_scanner'), \
                        patch.object(guard, 'tree_lock', return_value=contextlib.nullcontext()), \
                        patch.object(Path, 'write_text', evidence_only), \
                        patch.object(Path, 'write_bytes', side_effect=AssertionError('#166 busy lock must not write source')), \
                        patch.object(module.sys, 'argv', argv), \
                        patch.object(module, 'run_owned', side_effect=AssertionError('#166 busy lock must not launch a test')):
                    if hasattr(module, 'pgrep_launcher'):
                        module.pgrep_launcher = lambda: ''
                    code = module.main()
                self.assertEqual(calls, [True], '#166 both negative suites must request parity lock')
                self.assertEqual(code, 1 if folder == 'gateway-entry-guard' else 2,
                                 '#166 busy parity lock must exit nonzero per suite contract')
                report = json.loads((output / 'summary.json').read_text())
                rows = (report if folder == 'gateway-batch' else
                        report['controls'] if folder == 'gateway-entry-guard' else report['mutations'])
                key = 'verdict' if folder == 'session-domain-cleanup' else 'status'
                self.assertEqual([row[key] for row in rows], ['INVALID', 'INVALID'],
                                 '#166 busy parity lock must mark all selected mutants INVALID')


    def test_residual_mapping_is_rejected_before_import(self):
        import contextlib
        import importlib.machinery
        import json
        import sys
        for folder in ('gateway-batch', 'session-domain-cleanup', 'gateway-entry-guard'):
            with self.subTest(folder=folder), tempfile.TemporaryDirectory() as temp:
                path = self.fixture / folder / 'negative_controls.py'
                common_path = self.root / 'scripts/gateway-parity/common.py'
                pristine = common_path.read_bytes()
                residue = pristine.replace(b"'INVALID': 'BLOCKED'", b"'INVALID': 'PASS'")
                self.assertNotEqual(residue, pristine, '#166 residue probe must change mapping')
                raw_read = Path.read_bytes
                write_text = Path.write_text
                get_data = importlib.machinery.SourceFileLoader.get_data
                imports = []

                def data(loader, name):
                    if Path(name) == common_path:
                        imports.append(name)
                        return residue
                    return get_data(loader, name)

                def git(_root, *args):
                    if args[0] == 'show':
                        return raw_read(self.root / args[1].split(':', 1)[1])
                    return b''  # clean tracked-product index fixture; no subprocess

                def evidence_only(path, *args, **kwargs):
                    self.assertTrue(path.is_relative_to(Path(temp).resolve()), '#166 dirty guard must never write source')
                    return write_text(path, *args, **kwargs)

                with patch.object(importlib.machinery.SourceFileLoader, 'get_data', data):
                    spec = importlib.util.spec_from_file_location('negative_166_' + folder, path)
                    module = importlib.util.module_from_spec(spec)
                    spec.loader.exec_module(module)
                    guard = sys.modules['negative_process']
                    supplied = []
                    actual_guard = module.mutating_sources

                    def observed_guard(root, paths, **kwargs):
                        supplied.extend(paths)
                        return actual_guard(root, paths, **kwargs)

                    row = next(row for row in module.MUTATIONS
                               if Path(row[1]) != common_path)
                    argv = ['negative_controls.py', '--output', str(Path(temp) / 'out'), '--only', row[0]]
                    caught = None
                    code = None
                    # #1166: same scanner pin-out as the busy-lock probe
                    # above; the residue assertion must be the first
                    # baseline failure this wiring test observes.
                    with patch.object(guard, '_git', git), \
                            patch.object(guard, '_workspace_literal_inputs', return_value=()), \
                            patch.object(guard, '_ensure_workspace_scanner'), \
                            patch.object(module, 'mutating_sources', observed_guard), \
                            patch.object(guard, 'tree_lock', return_value=contextlib.nullcontext()), \
                            patch.object(Path, 'read_bytes', lambda p: residue if p == common_path else raw_read(p)), \
                            patch.object(Path, 'write_text', evidence_only), \
                            patch.object(Path, 'write_bytes', side_effect=AssertionError('#166 dirty guard must never write')), \
                            patch.object(module, 'run_owned', side_effect=AssertionError('#166 dirty guard must never run child')), \
                            patch.object(module.sys, 'argv', argv):
                        if hasattr(module, 'pgrep_launcher'):
                            module.pgrep_launcher = lambda: ''
                        try:
                            code = module.main()
                        except Exception as error:
                            caught = error
                    self.assertIn(common_path, supplied, '#166 common.py must be included in guarded sources')
                    self.assertEqual(imports, [], '#166 residual mapping must be guarded before import')
                    self.assertIsNone(caught, '#166 source guard must report BLOCKED without traceback')
                    self.assertEqual(code, 1 if folder == 'gateway-entry-guard' else 2,
                                     '#166 source guard must exit BLOCKED (entry-guard uses nonzero failure)')
                    report = json.loads((Path(temp) / 'out/summary.json').read_text())
                    rows = (report if folder == 'gateway-batch' else
                        report['controls'] if folder == 'gateway-entry-guard' else report['mutations'])
                    field = 'verdict' if folder == 'session-domain-cleanup' else 'status'
                    detail = 'detail' if folder == 'session-domain-cleanup' else 'reason'
                    self.assertEqual([row[field] for row in rows], ['INVALID'],
                                     '#166 residue must record INVALID for every selected case')
                    self.assertIn('source baseline mismatch: scripts/gateway-parity/common.py', rows[0][detail],
                                  '#166 residue refusal must name common.py')
                    print(f'#166 {folder}: residual INVALID->PASS rejected before import; no writes', flush=True)


    def test_session_suite_continues_after_invalid_mutants(self):
        import contextlib
        import json
        from types import SimpleNamespace
        path = self.fixture / 'session-domain-cleanup/negative_controls.py'
        spec = importlib.util.spec_from_file_location('negative_166_continue', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        names = [row[0] for row in module.MUTATIONS[:2]]
        module.MUTATIONS = [(names[0], module.SOURCE, '#166 absent anchor', 'absent'),
                            (names[1], module.SOURCE, 'def main():', 'def main(:')]
        self.assertEqual(module.SOURCE.read_text().count('def main():'), 1,
                         '#166 compile-invalid fixture needs exactly one real main anchor')
        originals = {p: p.read_bytes() for p in (module.SOURCE, module.COMMON_SOURCE)}

        @contextlib.contextmanager
        def guard(*_a, **_k):
            yield originals

        with tempfile.TemporaryDirectory() as temp:
            output = Path(temp).resolve() / 'out'
            write_text = Path.write_text

            def evidence_only(path, *a, **k):
                self.assertTrue(path.is_relative_to(output), '#166 invalid mutants must never write source')
                return write_text(path, *a, **k)

            restored = SimpleNamespace(returncode=0, stdout='', stderr='')
            summary = {'status': 'PASS', 'results': [
                {'mode': 'wiring', 'passed': True, 'status': 'PASS_WITH_CLEANUP_UNVERIFIED'}]}
            with patch.object(module, 'mutating_sources', guard), \
                    patch.object(module, 'pgrep_launcher', return_value=''), \
                    patch.object(module, 'run_check', return_value=restored) as check, \
                    patch.object(module, 'read_summary', return_value=(summary, None)), \
                    patch.object(module.sys, 'argv', ['negative_controls.py', '--output', str(output)]), \
                    patch.object(module.sys, 'platform', 'darwin'), \
                    patch.object(Path, 'write_text', evidence_only), \
                    patch.object(Path, 'write_bytes', lambda path, value: len(value)):
                code = module.main()
            report = json.loads((output / 'summary.json').read_text())
            self.assertEqual([row['mutation'] for row in report['mutations']], names,
                             '#166 suite must record every selected invalid mutant')
            self.assertEqual([row['verdict'] for row in report['mutations']], ['INVALID', 'INVALID'],
                             '#166 stale and uncompilable mutants are INVALID')
            self.assertTrue(report['mutations'][0]['detail'].startswith('ValueError:'),
                            '#166 stale mutant must fail at anchor validation')
            self.assertTrue(report['mutations'][1]['detail'].startswith('SyntaxError:'),
                            '#166 uncompilable mutant must reach compiler')
            self.assertEqual(code, 2, '#166 invalid batch must exit BLOCKED')
            self.assertEqual(check.call_count, 1, '#166 invalid suite still runs restored positive')
            self.assertTrue(report['restoredPass'], '#166 invalid suite records restoration separately')


    def test_committed_contract_requires_complete_head(self):
        path = self.fixture / 'negative_process.py'
        spec = importlib.util.spec_from_file_location('negative_166_contract', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        source = (self.root / 'scripts/gateway-parity/common.py').read_text()
        variants = (
            source.replace('BATCH_STATUS_ORDER =', 'MISSING_ORDER =', 1),
            source.replace('BATCH_EXIT_CODES =', 'MISSING_CODES =', 1),
            source.replace('MUTATION_STATUS =', 'MISSING_MUTATIONS =', 1),
            source.replace('def aggregate_status(', 'def missing_status(', 1),
            source + '\ndef aggregate_status(*children): return "PASS", 0\n',
        )
        for changed in variants:
            with self.subTest(index=variants.index(changed)):
                self.assertNotEqual(changed, source, '#166 invalid HEAD fixture must change contract')
                caught = None
                with patch.object(module, '_git', return_value=changed.encode()):
                    try:
                        module.committed_status_contract(self.root)
                    except Exception as error:
                        caught = error
                self.assertIsInstance(caught, RuntimeError, '#166 incomplete committed contract must be refused')
                self.assertIn('#166 committed shared status contract is incomplete', str(caught),
                              '#166 incomplete committed contract must be refused')



if __name__ == '__main__':
    unittest.main()
