"""Toolchain starts are mocked; watchdog controls run only this Python interpreter."""
import importlib.util
import os
from pathlib import Path
import re
import tempfile
import tomllib
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('metadata_io_driver', Path(__file__).with_name('check_metadata_io.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


class EnvironmentControls(unittest.TestCase):
    nextest_command = ('cargo nextest run --workspace --no-fail-fast --profile ci '
                       '--config-file ../.config/nextest.toml')
    doctest_command = 'cargo test --workspace --doc'

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
        # scope runs Python/git only and gates the three heavy jobs (#1199);
        # metadata-observer/rust/a51-coverage need their own Node and check gates;
        # check only aggregates; gateway-schema-baseline has Node and its own required check.
        self.assertEqual(set(jobs), {'scope', 'metadata-observer', 'check', 'rust', 'a51-coverage',
                                    'gateway-schema-baseline'}, f'{issue} exact six CI jobs')
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
        heavy = ('metadata-observer', 'rust', 'a51-coverage')
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
        self.assertEqual(len(steps), 4, '#904 exactly four rejection steps')

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
        heavy = ('metadata-observer', 'rust', 'a51-coverage')
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
        change('scope-gate-removed', scope_step, '', '#904 exactly four rejection steps')
        change('scope-gate-relaxed', "needs.scope.result != 'success'",
               "needs.scope.result == 'failure'", '#904 scope failure must reject the merge')
        change('check-needs-drops-scope', 'needs: [scope, metadata-observer, rust, a51-coverage]',
               'needs: [metadata-observer, rust, a51-coverage]', '#904 exact required needs')
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
        # 行为层的完整正负例由 scripts/test-ci-scope.py 在 scope job 里跑；
        # 这里加载真模块做行为抽查，防「文本钉住了、模块本身坏了」。
        spec = importlib.util.spec_from_file_location('ci_scope_guard', path)
        module = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(module)
        self.assertTrue(module.paths_are_docs_only(['state/board.md']),
                       '#1199 board-only path classifies true')
        self.assertFalse(module.paths_are_docs_only(['state/board.md', 'docs/a.md']),
                         '#1199 board plus any other file is not docs-only')
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
            if name in ('check', 'scope'):
                continue  # check 只聚合；scope 只跑 Python/git，无 Node（#1199）
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
            self.assertEqual(set(self.ci_step_fields(step)), {'name', 'run'},
                             f'#922 {title} cannot override execution or ignore failure')
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
        self.assertEqual(config, {'profile': {'ci': {
            'retries': 0, 'slow-timeout': {'period': '60s'},
        }}}, '#922 ci profile preserves all tests, default threads, zero retries and no termination')

    def test_ci_rejects_nextest_execution_drift(self):
        workflow = self.ci_workflow_fixture()
        cases = []

        def change(label, old, new, message):
            self.assertEqual(workflow.count(old), 1, f'#922 unique mutation anchor: {label}')
            cases.append((label, workflow.replace(old, new, 1), message))

        for title, command in (('测试', self.nextest_command), ('文档测试', self.doctest_command)):
            raw = f'      - name: {title}\n        run: {command}\n'
            change(f'{title}-missing', raw, '', f'#922 {title} occurs once in rust')
            change(f'{title}-duplicate', raw, raw + raw, f'#922 {title} occurs once in rust')
            for field in ('if: false', "'if': false", '"continue-on-error": true',
                          'shell: bash {0}', 'timeout-minutes: 1', 'env: {NEXTEST_RETRIES: 2}',
                          'working-directory: .'):
                change(f'{title}-{field}', raw, raw + f'        {field}\n',
                       f'#922 {title} cannot override execution or ignore failure')
            change(f'{title}-extra-run', raw, raw + f'      - run: {command}\n',
                   '#922 workspace test invocations are unique and complete')
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
        first = f'      - name: 测试\n        run: {self.nextest_command}\n'
        doc = f'      - name: 文档测试\n        run: {self.doctest_command}\n'
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
                ('needs: [scope, metadata-observer, rust, a51-coverage]', 'needs: [scope, metadata-observer, a51-coverage]',
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
        for step in self.ci_steps('rust', rust):
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
                    '#918 Rust step env must be an explicit block mapping')

    def test_ci_rejects_missing_wrong_or_step_level_debug_profiles(self):
        workflow = self.ci_workflow_fixture()
        target = 'test_rust_job_uses_line_tables_only_debug'
        block = ('    env:\n'
                 '      CARGO_PROFILE_DEV_DEBUG: line-tables-only\n'
                 '      CARGO_PROFILE_TEST_DEBUG: line-tables-only\n')
        self.assertEqual(workflow.count(block), 1, '#918 debug control has one env block')
        cases = [('missing-env', workflow.replace(block, '', 1),
                  '#918 Rust debug profiles are job-level line-tables-only')]
        for profile in ('DEV', 'TEST'):
            line = f'      CARGO_PROFILE_{profile}_DEBUG: line-tables-only\n'
            for value in ('', f'      CARGO_PROFILE_{profile}_DEBUG: full\n'):
                cases.append((f'{profile}-{value!r}', workflow.replace(line, value, 1),
                              '#918 Rust debug profiles are job-level line-tables-only'))
        anchor = '      - run: rustup show active-toolchain\n'
        self.assertEqual(workflow.count(anchor), 1, '#918 debug control has one Rust step')
        step_env = ''.join('    ' + line for line in block.splitlines(keepends=True))
        cases.append(('step-only', workflow.replace(block, '', 1).replace(anchor, anchor + step_env, 1),
                      '#918 Rust debug profiles are job-level line-tables-only'))
        cases.append(('step-override', workflow.replace(anchor, anchor + step_env, 1),
                      '#918 Rust debug profiles have no step overrides'))
        for env in ('        env: {CARGO_PROFILE_DEV_DEBUG: full}\n',
                    "        env:\n          'CARGO_PROFILE_DEV_DEBUG': full\n",
                    '        env:\n          "CARGO_PROFILE_TEST_DEBUG": full\n'):
            cases.append((env.strip(), workflow.replace(anchor, anchor + env, 1),
                          '#918 Rust debug profiles have no step overrides'))
        for value in ('*profiles', '${{ fromJSON(env.PROFILE_JSON) }}'):
            env = f'        env: {value}\n'
            changed = workflow.replace(anchor, anchor + env, 1)
            if value == '*profiles':
                changed = 'env: &profiles\n  CARGO_PROFILE_DEV_DEBUG: full\n\n' + changed
            else:
                changed = '''env:\n  PROFILE_JSON: '{"CARGO_PROFILE_DEV_DEBUG":"full"}'\n\n''' + changed
            cases.append((value, changed,
                          '#918 Rust step env must be an explicit block mapping'))
        for entry in ('*profiles', '<<: *profiles'):
            env = f'        env:\n          {entry}\n'
            changed = ('env: &profiles\n  CARGO_PROFILE_DEV_DEBUG: full\n\n'
                       + workflow.replace(anchor, anchor + env, 1))
            cases.append((entry, changed, '#918 Rust step env must be an explicit block mapping'))
        for name, changed, message in cases:
            with self.subTest(mutation=name):
                self.assert_ci_rejects(changed, target, message)

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
            'python3 -I scripts/test-review-roles.py',
            'bash scripts/test-pdrs-loop.sh',
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
                                       target, '#904 exact six CI jobs')
        for declaration, message in (
                ('  extra:\n    steps:\n      - run: npm test\n', '#904 exact six CI jobs'),
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

                with patch.object(guard, '_parity_mutation_lock', busy), \
                        patch.object(guard, '_git', git), \
                        patch.object(guard, 'tree_lock', return_value=contextlib.nullcontext()), \
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
                    with patch.object(guard, '_git', git), \
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
