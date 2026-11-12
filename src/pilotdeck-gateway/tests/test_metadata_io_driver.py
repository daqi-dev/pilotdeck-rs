"""Toolchain starts are mocked; watchdog controls run only this Python interpreter."""
import ast
import contextlib
import fcntl
import hashlib
import importlib.util
import json
import os
from collections import Counter
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import time
import textwrap
import tomllib
import types
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('metadata_io_driver', Path(__file__).with_name('check_metadata_io.py'))
driver = importlib.util.module_from_spec(spec)
spec.loader.exec_module(driver)


def restore_wiring_problems(guard):
    """#1903 r2：共享还原是否**按全局名**解析 `write_source_atomic`（探针接线的结构判据）。

    r2 第一版自证写成 `assertIs(__globals__['write_source_atomic'], guard.write_source_atomic)`，
    两边是同一次字典查找 → 恒真（zai boundary P1；DeepSeek boundary / concurrency P2）。
    真实让 #166 探针空转的两种漂移都会落进 `co_varnames`：默认参绑定
    `def mutating_sources(…, write_source_atomic=…)`，与函数内
    `from negative_process import write_source_atomic` 的局部绑定。
    行为侧的证明见 `test_restore_publishes_through_the_patched_module_global`。
    """
    restore = getattr(guard.mutating_sources, '__wrapped__', guard.mutating_sources)
    problems = []
    if restore.__globals__ is not vars(guard):
        problems.append('the restore must stay defined in negative_process')
    if 'write_source_atomic' not in restore.__code__.co_names:
        problems.append('the restore must resolve write_source_atomic as a global')
    if 'write_source_atomic' in restore.__code__.co_varnames:
        problems.append('a parameter or local binding would blind the probe')
    return problems


class EnvironmentControls(unittest.TestCase):
    # #2136 fix-r38（MM concurrency P2a）：#2133 自检步的挂起预算只有一处真相源
    # ——ci.yml 该步的 `timeout-minutes` 派生成秒，既钉该步的键值，也作真跑钉
    # 子进程的超时。r37 的旧值 900 s 比步预算 600 s 松：扫描面涨到 601–900 s
    # 时守钉仍绿（它只证明「900 s 内跑完」）而 CI 步先被 600 s 掐红，守钉给出
    # 与实际步相反的结论。两处从同一常量派生后不会再各自漂移。
    # fix-r39（DS sibling#2 / boundary#2 / concurrency#3 / simplify#1，四家同源）：
    # 其余被钉的步照此各设常量——r38 只改了 #2133 一处，#1666/#1570 的真跑钉
    # 仍写死 900 s（步预算 300 s；模块退化到 301–900 s 时守钉仍绿而 CI 步先
    # 红，假 PASS 方向）；#994 手抄 900 s 恰与步预算同值但无派生。driver 步
    # 自身（本类全套 86 条，r38 实测本机 764 s）同样按实测 + 余量钉 30 min。
    discovery_anchor_step_budget = 600
    optimize_guards_step_budget = 300
    copy_manifest_step_budget = 300
    isolation_step_budget = 900
    driver_step_budget = 1800
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
    # #2017（#1809 r6，DS/MM 两家 r5 撞报）：job 级与聚合 step 级的 `if:` / `needs:`
    # 必须按 PyYAML 解析后的**完整标量**钉死（与 self_check 的 r5 判据同口径）。行式
    # 正则只收键所在的那一行：4 空格 job 级键的 plain 续行把条件折叠成恒假（job 被跳过
    # → GitHub 把被跳过的必需检查报成 success → 合并放行），对它完全不可见；
    # `gateway-schema-baseline` 是必需检查且此前没有任何 job 级键断言。解析器版本
    # 钉死（ci.yml 两个 job 的「安装钉死 PyYAML」步骤与装置 README 同步登记）。
    pinned_pyyaml_version = '6.0.3'
    parsed_docs_only_if = "${{ needs.scope.outputs.docs_only != 'true' }}"
    ci_job_level_registry = {
        'scope': ({'outputs', 'runs-on', 'steps'}, None, None),
        'metadata-observer': ({'if', 'needs', 'runs-on', 'steps'}, ['scope'], parsed_docs_only_if),
        'negative-controls': ({'env', 'if', 'needs', 'runs-on', 'steps', 'timeout-minutes'},
                              ['scope'], parsed_docs_only_if),
        'retirement-outcome-parity': ({'if', 'needs', 'runs-on', 'steps', 'timeout-minutes'},
                                      ['scope'], parsed_docs_only_if),
        'check': ({'if', 'needs', 'runs-on', 'steps'},
                  ['scope', 'metadata-observer', 'rust', 'a51-coverage',
                   'negative-controls', 'retirement-outcome-parity'],
                  '${{ always() }}'),
        'rust': ({'defaults', 'env', 'if', 'needs', 'runs-on', 'steps'},
                 ['scope'], parsed_docs_only_if),
        'a51-coverage': ({'if', 'needs', 'runs-on', 'steps'}, ['scope'], parsed_docs_only_if),
        # 必需检查必须无条件跑：`if` / `needs` 都不在白名单里。
        'gateway-schema-baseline': ({'env', 'runs-on', 'steps'}, None, None),
    }
    # #1809 r8（评审 P1-1）：uses step 的 `with` 键集合按 action 逐键钉死——与
    # self_check.WORKFLOW_USE_WITH_KEYS 同一登记（兄弟守卫两处同步）。给
    # `actions/checkout@v4` 加 `with: {ref: …}` 会让该 job 的闸门跑另一棵树的
    # 脚本，而每个被比对的 `run` 标量逐字不变；r7 只钉了 retirement job 的两个
    # uses step。checkout 有两种合法形态（深克隆 `fetch-depth: 0` 与默认检出），
    # 登记为「允许的键集合族」。
    ci_use_with_registry = {
        'actions/checkout@v4': {frozenset(), frozenset({'fetch-depth'})},
        'actions/setup-node@v4': {frozenset({'node-version'})},
        'actions/cache@v4': {frozenset({'path', 'key'})},
        'Swatinem/rust-cache@v2.9.2': {frozenset({'workspaces', 'cache-bin'})},
        'actions/upload-artifact@v4': {frozenset({'name', 'path', 'if-no-files-found'})},
    }
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
        'test(=skills::tests::real_ts_and_rust_read_each_others_skill_trees) | '
        'test(=runtime::tools::builtin::grep::tests::files_sort_keeps_submillisecond_mtime_precision) | '
        'test(=project_files::tests::node_round4_unreadable_huge_file_reports_open_error_first) | '
        'test(=project_files::tests::special_files_do_not_block_and_unreadable_files_still_list) | '
        'test(=project_files::tests::native_read_errors_match_node_ws_responses) | '
        'test(=dispatch::tests::list_projects_preserves_real_node_scandir_error_suppression) | '
        'test(=runtime::cron::tests::migration_blocked_dir_tasks_eacces_matches_ts) | '
        'test(=runtime::cron::tests::migration_non_utf8_home_keeps_one_physical_key) | '
        'test(=native_process::tests::non_utf8_home_and_sidecar_are_refused) | '
        'test(=storage::tests::temporary_create_failure_reports_the_transcript_not_the_staging_name) | '
        'test(=storage::tests::transcript_open_failure_renders_node_eacces_with_the_supplied_path) | '
        'test(=native_process::services_tests::direct_content_with_an_unopenable_path_stays_silent) | '
        'test(=test_process::an_unreadable_stat_of_a_possible_child_fails_the_child_audit) | '
        'test(=test_process::an_unreadable_holder_of_this_user_fails_the_data_dir_audit) | '
        'test(=channel::sms::tests::dual_stack_probe_uses_the_assigned_port) | '
        'test(=channel::sms::tests::port_busy_on_ipv6_preflight_fails_fast) | '
        'test(=compiler_enforces_gateway_boundary)')

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
        # retirement-outcome-parity is pure Python (#1809) and is gated the same way;
        # check only aggregates; gateway-schema-baseline has Node and its own required check.
        self.assertEqual(set(jobs), {'scope', 'metadata-observer', 'check', 'rust', 'a51-coverage',
                                    'negative-controls', 'retirement-outcome-parity',
                                    'gateway-schema-baseline'},
                         f'{issue} exact CI job inventory')
        return jobs

    def ci_parsed_jobs(self, issue='#2017'):
        """按 PyYAML 解析后的节点取 `jobs:`（单文档、无重复键）：job → {键: 值节点}。

        只在构造变异样本时用行式锚点；**判定**一律走解析后的完整标量（r5 判据）。
        PyYAML 缺失 / 版本不符时 fail-closed（self.fails），绝不降级回行式解析。
        """
        try:
            import yaml
        except ImportError as error:
            self.fail(f'{issue} PyYAML is required for the parsed gate guards (pinned '
                      f'{self.pinned_pyyaml_version}, installed by the CI step '
                      f'安装钉死 PyYAML): {error}')
        self.assertEqual(yaml.__version__, self.pinned_pyyaml_version,
                         f'{issue} PyYAML version is pinned (CI step 安装钉死 PyYAML '
                         'and the device README register it); a different YAML build may '
                         'parse the workflow differently')
        text = (Path(__file__).resolve().parents[3]/'.github/workflows/ci.yml').read_text()
        documents = list(yaml.compose_all(text))
        self.assertEqual(len(documents), 1, f'{issue} exactly one YAML document')
        root = documents[0]
        self.assertIsInstance(root, yaml.MappingNode, f'{issue} the workflow is a mapping')
        root_keys = [key.value for key, _ in root.value]
        self.assertEqual(len(root_keys), len(set(root_keys)), f'{issue} no duplicate root keys')
        jobs_node = next((value for key, value in root.value if key.value == 'jobs'), None)
        self.assertIsInstance(jobs_node, yaml.MappingNode, f'{issue} jobs is a mapping')
        job_names = [key.value for key, _ in jobs_node.value]
        self.assertEqual(len(job_names), len(set(job_names)), f'{issue} no duplicate job keys')
        parsed = {}
        for key, value in jobs_node.value:
            self.assertIsInstance(value, yaml.MappingNode,
                                  f'{issue} the {key.value} job is a mapping')
            fields, names = {}, []
            for field, node in value.value:
                self.assertIsInstance(field, yaml.ScalarNode,
                                      f'{issue} the {key.value} job has scalar keys')
                names.append(field.value)
                fields[field.value] = node
            self.assertEqual(len(names), len(set(names)),
                             f'{issue} the {key.value} job has no duplicate job-level keys')
            parsed[key.value] = fields
        return yaml, parsed

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
        heavy = ('metadata-observer', 'rust', 'a51-coverage', 'negative-controls',
                 'retirement-outcome-parity')
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
        self.assertEqual(len(steps), 6, '#904 exactly six rejection steps')

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

    def test_ci_job_level_gates_are_pinned_by_parsed_yaml(self):
        # #2017（#1809 r6）：上面的行式断言只收键所在的那一行，对 plain 续行折叠不可见；
        # 这里按 PyYAML 解析后的完整标量把每个 job 的 job 级键白名单与 `if:` / `needs:`
        # 值一并钉死（gateway-schema-baseline 首次纳入），聚合 Require step 的 `if:` 同口径。
        yaml, parsed = self.ci_parsed_jobs()
        self.assertEqual(set(parsed), set(self.ci_job_level_registry),
                         '#904 exact CI job inventory (parsed)')
        for name, (allowed, needs, condition) in self.ci_job_level_registry.items():
            fields = parsed[name]
            self.assertEqual(set(fields), allowed,
                             f'#2017 the {name} job-level keys are exactly the registered set '
                             '(a job-level if/needs/continue-on-error/env/defaults can skip '
                             'the job or swallow its gates without touching any step)')
            if needs is not None:
                node = fields['needs']
                self.assertIsInstance(node, yaml.SequenceNode,
                                      f'#2017 the {name} job needs is the parsed registered list')
                self.assertEqual([item.value for item in node.value], needs,
                                 f'#2017 the {name} job needs is the parsed registered list')
            if condition is not None:
                node = fields['if']
                self.assertIsInstance(node, yaml.ScalarNode,
                                      f'#2017 the {name} job if is the parsed registered condition')
                self.assertEqual(node.value, condition,
                                 f'#2017 the {name} job if is the parsed registered condition')
        # 聚合 `check` job 的六个 Require step：键集合与 `run: exit 1` 逐字钉死，
        # `if:` 按**解析后**的完整标量与登记集比对——step 级续行把恒执行条件折叠成
        # 恒假（step 从不执行，job 仍 success）只有这里能看见。
        expected = {"${{ always() && needs.scope.result != 'success' }}"}
        for job in ('metadata-observer', 'rust', 'a51-coverage', 'negative-controls',
                    'retirement-outcome-parity'):
            expected.add('${{ always() && needs.' + job + ".result != 'success' && !(needs."
                         + job + ".result == 'skipped' && needs.scope.outputs.docs_only "
                         "== 'true') }}")
        steps_node = parsed['check']['steps']
        self.assertIsInstance(steps_node, yaml.SequenceNode,
                              '#2017 the check job steps is a sequence')
        seen = []
        for item in steps_node.value:
            self.assertIsInstance(item, yaml.MappingNode,
                                  '#2017 every aggregate step is a mapping')
            step = {}
            for field, node in item.value:
                step[field.value] = node
            self.assertLessEqual(set(step), {'if', 'name', 'run', 'shell', 'working-directory'},
                                 '#2017 aggregate Require step keys are exactly the registered set')
            run = step.get('run')
            self.assertIsInstance(run, yaml.ScalarNode,
                                 '#904 rejection steps exit one (parsed)')
            self.assertEqual(run.value, 'exit 1', '#904 rejection steps exit one (parsed)')
            shell = step.get('shell')
            self.assertIsInstance(shell, yaml.ScalarNode,
                                  '#1809 aggregate Require steps pin their shell (parsed)')
            self.assertEqual(shell.value, 'bash',
                             '#1809 aggregate Require steps pin their shell (parsed)')
            if 'working-directory' in step:
                self.assertEqual(step['working-directory'].value, '.',
                                 '#904 rejection steps run from the repository root (parsed)')
            self.assertIn('if', step,
                          '#2017 aggregate Require step if conditions are the parsed registered set')
            seen.append(step['if'].value)
            self.assertIn(step['if'].value, expected,
                          '#2017 aggregate Require step if conditions are the parsed registered set')
        self.assertEqual(sorted(seen), sorted(expected),
                         '#2017 all six parsed Require conditions appear exactly once')

    def test_ci_rejects_parsed_job_level_gate_drift(self):
        workflow = self.ci_workflow_fixture()
        jobs = self.ci_jobs()
        cases = []

        def change_job(label, owner, old, new, message):
            self.assertEqual(jobs[owner].count(old), 1, f'#2017 unique {owner} anchor: {label}')
            cases.append((label, workflow.replace(
                jobs[owner], jobs[owner].replace(old, new, 1), 1), message))

        # job 级 `if:` plain 续行折叠成恒假：只收键所在行的行式断言（上面那组）看不见，
        # 解析后的条件已经不等——job 永远被跳过，必需检查被 GitHub 报成 success。
        docs_only_line = "    if: ${{ needs.scope.outputs.docs_only != 'true' }}\n"
        change_job('metadata-observer-job-if-continuation', 'metadata-observer',
                   docs_only_line, docs_only_line + '      && false\n',
                   '#2017 the metadata-observer job if is the parsed registered condition')
        change_job('check-job-if-continuation', 'check',
                   '    if: ${{ always() }}\n',
                   '    if: ${{ always() }}\n      && false\n',
                   '#2017 the check job if is the parsed registered condition')
        # 聚合 Require step 的 `if:` 续行折叠成恒假：step 从不执行，job 仍 success。
        change_job('require-scope-if-continuation', 'check',
                   "        if: ${{ always() && needs.scope.result != 'success' }}\n",
                   "        if: ${{ always() && needs.scope.result != 'success' }}\n"
                   '          && false\n',
                   '#2017 aggregate Require step if conditions are the parsed registered set')
        # 必需检查 gateway-schema-baseline 加 job 级条件：被跳过即报 success，合并放行
        # （#2017 场景 A：此前该 job 没有任何 job 级键断言）。
        change_job('baseline-job-conditional', 'gateway-schema-baseline',
                   '    steps:\n', '    if: false\n    steps:\n',
                   '#2017 the gateway-schema-baseline job-level keys are exactly the registered set')
        self.check_ci_mutations(cases, 'test_ci_job_level_gates_are_pinned_by_parsed_yaml', '#2017')

    def test_ci_every_run_step_pins_bash_shell(self):
        # #1809 r7（MM boundary P0）：`defaults.run.shell` 会替换每个不带自身 `shell:` 的
        # run step 的 shell（`/bin/true {0}` 让必需检查无条件 rc=0），而全部检测点都住在
        # 可被同一键废掉的 run step 里。执行层解是给**每个** run step 显式
        # `shell: bash`（step 级 shell 优先于任何层级的 defaults，GitHub workflow syntax
        # 明文）。这里是 gateway 侧的镜像守卫：与 retirement-outcome-parity 装置
        # self_check.check_wiring 的全 workflow 巡走同口径——任何 job 的任何 run step
        # 裸奔（缺 shell / 值不是 bash）都必须红，不限于退休装置那几个 step。
        yaml, parsed = self.ci_parsed_jobs(issue='#1809')
        checked = {}
        for name, fields in parsed.items():
            steps_node = fields.get('steps')
            self.assertIsInstance(steps_node, yaml.SequenceNode,
                                  f'#1809 the {name} job has a step sequence')
            for item in steps_node.value:
                self.assertIsInstance(item, yaml.MappingNode,
                                      f'#1809 every {name} step is a mapping')
                step = {}
                for field, node in item.value:
                    step[field.value] = node
                if 'run' not in step:
                    continue
                shell = step.get('shell')
                self.assertIsInstance(shell, yaml.ScalarNode,
                                      f'#1809 the {name} run step pins `shell: bash` (parsed); '
                                      'missing or non-scalar shell key')
                self.assertEqual(shell.value, 'bash',
                                 f'#1809 the {name} run step pins `shell: bash` (parsed); '
                                 'a bare run step inherits any defaults.run.shell (e.g. '
                                 '`/bin/true {0}`), which silently disables that step')
                checked[name] = checked.get(name, 0) + 1
        # 每个 job 都至少有一个 run step 被巡到：某 job 的 run step 被整体删空不能让
        # 本守卫对着剩余 job 空转（job 清单另由 exact job inventory 钉死）。
        self.assertEqual(sorted(checked), sorted(parsed),
                         '#1809 every CI job has run steps under the shell guard')

    def test_ci_rejects_run_step_shell_drift(self):
        workflow = self.ci_workflow_fixture()
        jobs = self.ci_jobs()
        cases = []

        def change_job(label, owner, old, new, message):
            self.assertEqual(jobs[owner].count(old), 1, f'#1809 unique {owner} anchor: {label}')
            cases.append((label, workflow.replace(
                jobs[owner], jobs[owner].replace(old, new, 1), 1), message))

        # 闸门 step 删掉 `shell: bash`：裸 run step 继承任何 defaults.run.shell。
        # 锚串里的 timeout-minutes 值同源派生（评审 r2 sibling#1）：手抄的 5 是
        # GATE_STEP_TIMEOUT_MINUTES['check 5 轮'] 的第二份拷贝，登记值合法变更后
        # 锚串 count=0，本样本红在「unique anchor」、误导为锚点损坏。
        check_minutes = self.retirement_gate_step_timeouts(issue='#1809')['check 5 轮']
        check_anchor = ('      - name: check 5 轮\n        shell: bash\n'
                         f'        timeout-minutes: {check_minutes}\n'
                         '        run: |\n')
        check_shell_removed = ('      - name: check 5 轮\n'
                               f'        timeout-minutes: {check_minutes}\n'
                               '        run: |\n')
        change_job('retirement-gate-shell-removed', 'retirement-outcome-parity',
                   check_anchor, check_shell_removed,
                   '#1809 the retirement-outcome-parity run step pins `shell: bash` (parsed)')
        # 必需检查的聚合 Require step 换 shell 值：`/bin/true {0}` 把 exit 1 变 rc=0。
        change_job('require-scope-shell-swapped', 'check',
                   "        if: ${{ always() && needs.scope.result != 'success' }}\n"
                   '        working-directory: .\n        shell: bash\n        run: exit 1\n',
                   "        if: ${{ always() && needs.scope.result != 'success' }}\n"
                   '        working-directory: .\n        shell: /bin/true {0}\n        run: exit 1\n',
                   '#1809 the check run step pins `shell: bash` (parsed)')
        # 非闸门 job（rust 的 fmt 是必需检查 `rust` 的一环）删 shell：只有全 workflow
        # 巡走能看见——这条样本专测巡走本身。
        change_job('rust-fmt-shell-removed', 'rust',
                   '      - name: 格式\n        shell: bash\n        run: cargo fmt --all --check\n',
                   '      - name: 格式\n        run: cargo fmt --all --check\n',
                   '#1809 the rust run step pins `shell: bash` (parsed)')
        # metadata-observer 的守卫步（跑本文件的 unittest）同样必须显式钉 shell。
        change_job('observer-driver-shell-removed', 'metadata-observer',
                   '      - name: CI 聚合门控与各 job 的 Node 前置守卫\n'
                   '        working-directory: src/pilotdeck-gateway/tests\n'
                   '        shell: bash\n',
                   '      - name: CI 聚合门控与各 job 的 Node 前置守卫\n'
                   '        working-directory: src/pilotdeck-gateway/tests\n',
                   '#1809 the metadata-observer run step pins `shell: bash` (parsed)')
        self.check_ci_mutations(cases, 'test_ci_every_run_step_pins_bash_shell', '#1809')

    def test_ci_uses_steps_pin_their_with_keys(self):
        # #1809 r8（评审 P1-1）：uses step 的 `with` 逐键钉死的镜像守卫（与 self_check
        # 的全 workflow 巡走同口径）。同一攻击换一个 job 照样成立：checkout 加
        # `with: {ref: …}` 让该 job 的闸门跑另一棵树的脚本，而所有被比对的 `run`
        # 标量与 job 级键逐字不变——必须逐个 uses step 巡走，不限于 retirement job。
        yaml, parsed = self.ci_parsed_jobs(issue='#1809')
        checked = {}
        for name, fields in parsed.items():
            steps_node = fields.get('steps')
            self.assertIsInstance(steps_node, yaml.SequenceNode,
                                  f'#1809 the {name} job has a step sequence')
            for item in steps_node.value:
                self.assertIsInstance(item, yaml.MappingNode,
                                      f'#1809 every {name} step is a mapping')
                step = {}
                for field, node in item.value:
                    step[field.value] = node
                if 'uses' not in step:
                    continue
                uses = step['uses']
                self.assertIsInstance(uses, yaml.ScalarNode,
                                     f'#1809 the {name} uses step is a scalar action')
                families = self.ci_use_with_registry.get(uses.value)
                self.assertIsNotNone(
                    families,
                    f'#1809 the {name} uses step `{uses.value}` is in the registered '
                    'action registry; `with` content is only pinned for registered '
                    'actions')
                with_node = step.get('with')
                if with_node is None:
                    with_keys = frozenset()
                else:
                    self.assertIsInstance(with_node, yaml.MappingNode,
                                         f'#1809 the {name} uses step `{uses.value}` '
                                         'has a mapping `with`')
                    with_keys = frozenset(
                        key.value for key, _ in with_node.value)
                self.assertIn(
                    with_keys, families,
                    f'#1809 the {name} uses step `{uses.value}` carries the '
                    f'unregistered `with` keys {sorted(with_keys)} (registered '
                    f'families: {sorted(sorted(f) for f in families)}); an added '
                    '`with: ref/repository` on a checkout makes the gates run '
                    'scripts from a different tree while every pinned `run` scalar '
                    'stays verbatim-identical')
                checked[name] = checked.get(name, 0) + 1
        # 除聚合 `check` job（只有 Require step，无任何 uses step）外，每个 job 都
        # 至少有一个 uses step 被巡到：某 job 的 uses step 被整体删空不能让本守卫
        # 对着剩余 job 空转（job 清单另由 exact job inventory 钉死）。
        self.assertEqual(sorted(checked),
                         sorted(name for name in parsed if name != 'check'),
                         '#1809 every CI job except the aggregate check has uses '
                         'steps under the with-key guard')

    def test_ci_rejects_uses_with_key_drift(self):
        workflow = self.ci_workflow_fixture()
        jobs = self.ci_jobs()
        cases = []

        def change_job(label, owner, old, new, message):
            self.assertEqual(jobs[owner].count(old), 1, f'#1809 unique {owner} anchor: {label}')
            cases.append((label, workflow.replace(
                jobs[owner], jobs[owner].replace(old, new, 1), 1), message))

        # 三个此前无人拦截的 job（r7 只钉了 retirement job）：同一手法换一个 job 的
        # checkout 加 `with: {ref: …}`——闸门跑另一棵树的脚本，被比对标量逐字不变。
        # negative-controls 的 checkout 本就带 `fetch-depth: 0`，往那个 `with` 块里加。
        change_job('observer-checkout-with-ref', 'metadata-observer',
                   '      - uses: actions/checkout@v4\n'
                   '      - uses: actions/setup-node@v4\n',
                   '      - uses: actions/checkout@v4\n'
                   '        with:\n          ref: attacker-branch\n'
                   '      - uses: actions/setup-node@v4\n',
                   '#1809 the metadata-observer uses step `actions/checkout@v4` '
                   'carries the unregistered `with` keys')
        change_job('negative-controls-checkout-with-ref', 'negative-controls',
                   '          # 变异前要按 git show HEAD:<path> 证明源码未被改动\n'
                   '          fetch-depth: 0\n',
                   '          # 变异前要按 git show HEAD:<path> 证明源码未被改动\n'
                   '          fetch-depth: 0\n          ref: attacker-branch\n',
                   '#1809 the negative-controls uses step `actions/checkout@v4` '
                   'carries the unregistered `with` keys')
        change_job('baseline-checkout-with-ref', 'gateway-schema-baseline',
                   '      - uses: actions/checkout@v4\n'
                   '      - uses: actions/setup-node@v4\n',
                   '      - uses: actions/checkout@v4\n'
                   '        with:\n          ref: attacker-branch\n'
                   '      - uses: actions/setup-node@v4\n',
                   '#1809 the gateway-schema-baseline uses step `actions/checkout@v4` '
                   'carries the unregistered `with` keys')
        self.check_ci_mutations(cases, 'test_ci_uses_steps_pin_their_with_keys', '#1809')

    def pr_gate_parsed(self, issue='#1809'):
        """PyYAML 解析后的 pr-gate.yml 根节点（单文档、无重复根键）。

        #1809 r9（评审 P0）：合并闸门本体 pr-gate.yml 此前在本守卫侧零覆盖
        （r7/r8 的巡走只读 ci.yml）。与 self_check._check_pr_gate_wiring 同一
        口径：只在构造变异样本时用行式锚点，**判定**一律走解析后的完整标量。
        """
        try:
            import yaml
        except ImportError as error:
            self.fail(f'{issue} PyYAML is required for the parsed gate guards (pinned '
                      f'{self.pinned_pyyaml_version}, installed by the CI step '
                      '安装钉死 PyYAML): {error}')
        self.assertEqual(yaml.__version__, self.pinned_pyyaml_version,
                         f'{issue} PyYAML version is pinned (see ci_parsed_jobs); '
                         'a different YAML build may parse the workflow differently')
        text = (Path(__file__).resolve().parents[3]
                / '.github/workflows/pr-gate.yml').read_text()
        documents = list(yaml.compose_all(text))
        self.assertEqual(len(documents), 1, f'{issue} exactly one YAML document')
        root = documents[0]
        self.assertIsInstance(root, yaml.MappingNode, f'{issue} the workflow is a mapping')
        root_keys = [key.value for key, _ in root.value]
        self.assertEqual(len(root_keys), len(set(root_keys)), f'{issue} no duplicate root keys')
        return yaml, root

    def test_pr_gate_wiring_is_pinned_by_parsed_yaml(self):
        # #1809 r9（评审 P0）：pr-gate.yml 承载合并闸门第 2、3 步（git fetch 基线
        # / check-pr-gate.py），三种静默废闸门的手法此前在本守卫侧全绿——step
        # `continue-on-error: true`（步全红而 job 仍 success）、job `if: false`
        # （被跳过的必需检查被 GitHub 报成 success）、`run: exit 0`（换壳）。
        # 判定镜像 self_check._check_pr_gate_wiring：根键白名单、唯一 job、job
        # 直属键恰等、job `if` 逐字、step 顺序与条数、逐 step 键集/env 键集/
        # continue-on-error 登记值/shell/run 标量逐字。
        yaml, root = self.pr_gate_parsed()
        root_keys = [key.value for key, _ in root.value]
        # 根键白名单：workflow 级 `defaults.run.shell` 会替换每个不带自身 `shell:`
        # 的 run step 的 shell（r7 P0 的执行层手法），白名单外出现即红。
        self.assertLessEqual(set(root_keys),
                             {'name', 'on', 'jobs', 'permissions', 'concurrency',
                              'run-name'},
                             '#1809 the pr-gate root keys are inside the registered '
                             'whitelist; a workflow-level `defaults.run.shell` '
                             're-shells every run step and neither the ci.yml '
                             'guards nor a job-level scan would see it')
        self.assertIn('jobs', root_keys, '#1809 the pr-gate workflow has a `jobs:` mapping')
        jobs_node = next(value for key, value in root.value if key.value == 'jobs')
        self.assertIsInstance(jobs_node, yaml.MappingNode,
                              '#1809 the pr-gate `jobs:` is a mapping')
        job_names = [key.value for key, _ in jobs_node.value]
        self.assertEqual(job_names, ['pr-gate'],
                         '#1809 the pr-gate workflow carries only the registered '
                         '`pr-gate` job')
        job = next(value for key, value in jobs_node.value if key.value == 'pr-gate')
        self.assertIsInstance(job, yaml.MappingNode, '#1809 the pr-gate job is a mapping')
        fields = {}
        for field, node in job.value:
            self.assertNotIn(field.value, fields, '#1809 no duplicate pr-gate job keys')
            fields[field.value] = node
        self.assertEqual(set(fields), {'if', 'runs-on', 'steps'},
                          '#1809 the pr-gate job keys are exactly the registered set')
        # P0 形态②：job `if` 逐字。draft 判断是登记过的唯一条件，`if: false`
        # 之外任何漂移（如 draft 反转）都让合并闸门换了一个世界。
        self.assertIsInstance(fields['if'], yaml.ScalarNode,
                              '#1809 the pr-gate job condition is a scalar')
        self.assertEqual(fields['if'].value, 'github.event.pull_request.draft == false',
                         '#1809 the pr-gate job condition is the registered draft '
                         'guard; a skipped required job is reported as success by '
                         'GitHub, so nothing else would block the merge')
        steps_node = fields['steps']
        self.assertIsInstance(steps_node, yaml.SequenceNode,
                              '#1809 the pr-gate job has a step sequence')
        steps = []
        for item in steps_node.value:
            self.assertIsInstance(item, yaml.MappingNode,
                                  '#1809 every pr-gate step is a mapping')
            step = {}
            for field, node in item.value:
                self.assertNotIn(field.value, step, '#1809 no duplicate step keys')
                step[field.value] = node
            steps.append(step)
        self.assertEqual(len(steps), 3,
                         '#1809 the pr-gate job keeps exactly the registered three '
                         'steps (checkout / fetch / gate)')
        checkout, fetch, gate = steps
        # step 1：uses checkout——键只许 uses/with（加 name），uses 值逐字，with
        # 键集合恰为 {fetch-depth}（加 ref/repository 会让闸门跑另一棵树）。
        self.assertLessEqual(set(checkout), {'name', 'uses', 'with'},
                             '#1809 the pr-gate checkout step keys are inside the '
                             'registered set')
        self.assertIn('uses', checkout,
                      '#1809 the pr-gate checkout step stays an action (`uses:`)')
        self.assertEqual(checkout['uses'].value, 'actions/checkout@v4',
                         '#1809 the pr-gate checkout step pins the registered action')
        with_node = checkout.get('with')
        self.assertIsInstance(with_node, yaml.MappingNode,
                              '#1809 the pr-gate checkout `with` is a mapping')
        with_keys = frozenset(key.value for key, _ in with_node.value)
        self.assertEqual(with_keys, {'fetch-depth'},
                         '#1809 the pr-gate checkout `with` keys are exactly the '
                         'registered set; the merge fingerprint needs a full clone '
                         '(fetch-depth: 0) and any other key changes which tree the '
                         'gate reads')
        # steps 2/3：run step 直属键集合恰等（分步登记）——P0 形态①（闸门步加
        # `continue-on-error: true`）与删 shell 都先红在这里。
        self.assertEqual(set(fetch), {'name', 'continue-on-error', 'env', 'shell', 'run'},
                         '#1809 the fetch step keys are exactly the registered set')
        self.assertEqual(set(gate), {'name', 'env', 'shell', 'run'},
                         '#1809 the gate step keys are exactly the registered set')
        self.assertEqual(fetch['name'].value,
                         '显式取 base 分支与 PR head（不依赖 checkout 的默认 refspec）',
                         '#1809 the fetch step keeps its registered name')
        self.assertEqual(gate['name'].value, '角色审计结果块与 issue 关联',
                         '#1809 the gate step keeps its registered name')
        self.assertIsInstance(fetch['continue-on-error'], yaml.ScalarNode,
                              '#1809 the fetch `continue-on-error` is a scalar')
        self.assertEqual(fetch['continue-on-error'].value, 'true',
                         '#1809 the fetch step `continue-on-error` stays exactly '
                         '`true`; it is a registered design decision (a missing base '
                         'only downgrades fingerprinting, it must not gate the job)')
        fetch_env = frozenset(key.value for key, _ in fetch['env'].value)
        self.assertEqual(fetch_env, {'BASE_REF', 'PR_HEAD'},
                         '#1809 the fetch step env keys are exactly the registered set')
        gate_env = frozenset(key.value for key, _ in gate['env'].value)
        self.assertEqual(gate_env, {'PR_BODY', 'PR_HEAD', 'PR_BASE'},
                         '#1809 the gate step env keys are exactly the registered '
                         'set; a dropped or added key silently hands check-pr-gate.py '
                         'a different world while every pinned `run` scalar stays '
                         'verbatim-identical')
        for label, step in (('fetch', fetch), ('gate', gate)):
            self.assertIsInstance(step['shell'], yaml.ScalarNode,
                                  f'#1809 the pr-gate {label} step shell is a scalar')
            self.assertEqual(step['shell'].value, 'bash',
                             f'#1809 the pr-gate {label} step pins `shell: bash` '
                             '(parsed); any other value (e.g. `/bin/true {0}`) '
                             'replaces what actually executes')
        # run 标量逐字（literal 块按解析后的完整值比对，不认行式巧合）。
        self.assertEqual(fetch['run'].value,
                         'git fetch --no-tags origin '
                         '"+refs/heads/${BASE_REF}:refs/remotes/origin/${BASE_REF}" '
                         '"${PR_HEAD}"\n'
                         'git rev-parse --verify "origin/${BASE_REF}^{commit}"\n'
                         'git rev-parse --verify "${PR_HEAD}^{commit}"\n',
                         '#1809 the fetch step runs the registered fetch script')
        self.assertEqual(gate['run'].value, 'python3 -I scripts/check-pr-gate.py',
                         '#1809 the gate step runs the registered gate script; the '
                         'parsed scalar is pinned verbatim, so an `exit 0` swap, an '
                         '`|| true` suffix or a folded block all change the text')

    def test_pr_gate_rejects_wiring_drift(self):
        text = (Path(__file__).resolve().parents[3]
                / '.github/workflows/pr-gate.yml').read_text()
        gate_name = '      - name: 角色审计结果块与 issue 关联\n'
        fetch_coe = ('        continue-on-error: true  # 取不到只影响「沿用」'
                     '（指纹算不出 → 拒收旧块），不挡 head 一致的 PR\n')
        cases = []

        def mutate(label, old, new, message):
            self.assertEqual(text.count(old), 1, f'#1809 unique pr-gate anchor: {label}')
            cases.append((label, text.replace(old, new, 1), message))

        # 根级 defaults.run.shell：根键白名单判定在 pr-gate.yml 上的落点。
        mutate('pr-gate-root-defaults-shell', '\njobs:\n',
               '\ndefaults:\n  run:\n    shell: /bin/true {0}\njobs:\n',
               '#1809 the pr-gate root keys are inside the registered whitelist')
        # P0 形态②：job `if: false`——job `if` 逐字判定的落点。
        mutate('pr-gate-job-if-false', '    if: github.event.pull_request.draft == false\n',
               '    if: false\n',
               '#1809 the pr-gate job condition is the registered draft guard')
        # P0 形态①：闸门步加 step 级 `continue-on-error: true`——步全红而 job 仍
        # success。键集恰等判定当场红。
        mutate('pr-gate-gate-continue-on-error', gate_name,
               gate_name + '        continue-on-error: true\n',
               '#1809 the gate step keys are exactly the registered set')
        # r8 样本删错步的修正：闸门步的 shell 行带行尾注释（pr-gate.yml:37），
        # 旧字面量 `'        shell: bash'` 匹配不到它、`shells[-1]` 落在 fetch 步
        # ——docstring 声称删 check 步、实际删的是 fetch 步。r9 起键集恰等先行
        # 拦下（found 缺 `shell`），shell 值钉死是兜底。
        mutate('pr-gate-step-shell-removed',
               '        shell: bash  # r8 P1-2：合并闸门本体——check-pr-gate step '
               '不许被 defaults 换壳\n',
               '',
               '#1809 the gate step keys are exactly the registered set')
        # P0 形态③：闸门步 run 换壳 `exit 0`——run 标量逐字判定的落点。
        mutate('pr-gate-run-exit-0', '        run: python3 -I scripts/check-pr-gate.py\n',
               '        run: exit 0\n',
               '#1809 the gate step runs the registered gate script')
        # fetch 基线步的登记键被删 / 登记值被换：键集恰等与值钉死两个判定各
        # 自要有违反路径，删错方向才看得见。
        mutate('pr-gate-fetch-continue-on-error-removed', fetch_coe, '',
               '#1809 the fetch step keys are exactly the registered set')
        mutate('pr-gate-fetch-continue-on-error-swapped', fetch_coe,
               '        continue-on-error: false\n',
               '#1809 the fetch step `continue-on-error` stays exactly `true`')
        # env 键集：闸门步 env 加未登记键。
        mutate('pr-gate-env-key-added', gate_name + '        env:\n',
               gate_name + '        env:\n          EXTRA: drift\n',
               '#1809 the gate step env keys are exactly the registered set')
        # 整步删除：step 顺序/条数钉死的落点。
        self.assertEqual(text.count(gate_name), 1, '#1809 unique gate step anchor')
        cases.append(('pr-gate-gate-step-removed', text.split(gate_name, 1)[0],
                      '#1809 the pr-gate job keeps exactly the registered three steps'))
        self.check_ci_mutations(cases, 'test_pr_gate_wiring_is_pinned_by_parsed_yaml',
                                '#1809', mutated='pr-gate.yml')

    def _retirement_same_source_constants(self, print_statement, issue):
        """subprocess 按 spec 加载 self_check.py 后求值 print_statement（同源读，
        不跑 main）。预算常量与 gate step 上限共用这一条加载通道。

        #2437：同族前言（self_check.py 的 LOCK_HOLDER / GUARD_PROBE / WRITE_PROBE
        样板逐字同构）——`-I` 不等于 `-B`，未设 pycache_prefix 时
        spec_from_file_location 会读 scripts/retirement-outcome-parity/__pycache__/
        self_check.*.pyc；树里预置的 PEP 552 unchecked-hash pyc 被无条件采用、
        不比对源码，兄弟侧的「同源推导」就被抽换的常量抽空（170 → 999 即红）。
        前缀只服务于加载期；exec 成败都清（try/finally rmtree，评审 r1：
        exec_module 抛异常 / 外层超时杀子进程时直线 rmtree 不可达）。
        """
        source = (Path(__file__).resolve().parents[3]
                  / 'scripts/retirement-outcome-parity/self_check.py')
        loader = (
            'import importlib.util, json, shutil, sys, tempfile\n'
            'try:\n'
            "    sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-retirement-pyc-')\n"
            'except OSError:\n'
            "    print('budget loader: cannot reserve a fresh pycache prefix', "
            'file=sys.stderr)\n'
            '    raise SystemExit(2)\n'
            'sys.dont_write_bytecode = True\n'
            'own = sys.pycache_prefix\n'
            'spec = importlib.util.spec_from_file_location("self_check", '
            f'{str(source)!r})\n'
            'module = importlib.util.module_from_spec(spec)\n'
            'try:\n'
            '    spec.loader.exec_module(module)\n'
            'finally:\n'
            '    shutil.rmtree(own, ignore_errors=True)\n'
            'module._assert_step_budget_coverage(module.retirement_budget_rows())\n'
            + print_statement + '\n')
        result = subprocess.run([sys.executable, '-I', '-c', loader],
                                capture_output=True, text=True, timeout=120,
                                check=False)
        self.assertEqual(result.returncode, 0,
                         f'{issue} cannot load the retirement constants from '
                         f'self_check.py: {result.stderr.strip()[:400]}')
        return json.loads(result.stdout)

    def retirement_budget_constants(self, issue='#1809'):
        """#1632 / #1692 §1 的推导范式：预算常量从装置源码**同源**读，不手抄第二份。

        r10 P0（评审）：r9 的兄弟断言手抄 `85×60 > 4580 + 300`，装置侧重算后
        这边不会跟着红——第二份裸数字只会静默漂移。subprocess 按 spec 加载
        self_check.py（模块体从实际 job steps 遍历推导完整和，不跑 main），打印
        RETIREMENT_JOB_TIMEOUT_MINUTES / INTERNAL_WORST_CASE_SECONDS /
        JOB_OVERHEAD_MARGIN_SECONDS 三个登记值。
        """
        return self._retirement_same_source_constants(
            'print(json.dumps([module.RETIREMENT_JOB_TIMEOUT_MINUTES, '
            'module.INTERNAL_WORST_CASE_SECONDS, '
            'module.JOB_OVERHEAD_MARGIN_SECONDS]))', issue)

    def retirement_gate_step_timeouts(self, issue='#2451'):
        """#2451：三个直跑 gate 的 step 级声明上限同源读
        （`GATE_STEP_TIMEOUT_MINUTES`——负控值由装置从 `NEGCTL_TIMEOUT_SECONDS`
        派生），兄弟侧不手抄第二份 5/5/30。
        """
        return self._retirement_same_source_constants(
            'print(json.dumps(module.GATE_STEP_TIMEOUT_MINUTES))', issue)

    def test_retirement_job_bounds_its_wall_clock(self):
        # #1809 r11 P0：导出和遍历实际 job steps，包含三个直跑 gate、七项与
        # 变异自检；checkout/setup/upload 显式共享 margin 桶。兄弟断言不再继承
        # 只含两个自检 step 的不完整和。外层先到会硬杀步骤、截断证据。
        minutes, internal, margin = self.retirement_budget_constants()
        body = self.ci_jobs('#1809')['retirement-outcome-parity']
        self.assertIn(f'    timeout-minutes: {minutes}\n', body,
                      '#1809 the retirement job bounds its own wall clock above the '
                      'internal worst case')
        self.assertGreater(minutes * 60, internal + margin,
                            '#1809 the job budget stays above the internal worst '
                            'case sum plus job overhead')

    def test_retirement_job_timeout_regressions_are_rejected(self):
        minutes, _, _ = self.retirement_budget_constants()
        timeout_line = f'    timeout-minutes: {minutes}\n'
        workflow = self.ci_workflow_fixture()
        body = self.ci_jobs('#1809')['retirement-outcome-parity']
        cases = []

        def change(label, old, new, message):
            self.assertEqual(body.count(old), 1, f'#1809 unique job anchor: {label}')
            cases.append((label, workflow.replace(body, body.replace(old, new, 1), 1),
                          message))

        change('retirement-job-timeout-removed', timeout_line, '',
               '#1809 the retirement job bounds its own wall clock above the '
               'internal worst case')
        change('retirement-job-timeout-lowered', timeout_line,
               '    timeout-minutes: 55\n',
               '#1809 the retirement job bounds its own wall clock above the '
               'internal worst case')
        self.check_ci_mutations(cases, 'test_retirement_job_bounds_its_wall_clock',
                                '#1809')

    def test_retirement_gate_steps_declare_step_timeouts(self):
        # #2451：三个直跑 gate 的 step 级声明上限。r11 起外层 job 超时被
        # test_retirement_job_bounds_its_wall_clock 钉住，但 step 声明值本身
        # （5/5/30）此前没有兄弟侧守卫——改小让挂住的检查打不满内部超时（失败退
        # 化成 step 硬杀），改大改变预算遍历的入算和。登记值同源读
        # （GATE_STEP_TIMEOUT_MINUTES），不手抄第二份。
        step_timeouts = self.retirement_gate_step_timeouts()
        self.assertEqual(set(step_timeouts),
                         {'emit 复算与金样逐字节比对', 'check 5 轮',
                          '负控 N1–N17 全 KILLED'},
                         '#2451 the three direct-run gate steps are registered '
                         'with step timeouts')
        body = self.ci_jobs('#1809')['retirement-outcome-parity']
        for label, minutes in step_timeouts.items():
            header = f'      - name: {label}\n'
            self.assertEqual(body.count(header), 1,
                             f'#2451 unique `{label}` step header')
            start = body.index(header)
            end = body.find('\n      - ', start)
            block = body[start:end if end != -1 else len(body)]
            self.assertIn(f'        timeout-minutes: {minutes}\n', block,
                          f'#2451 the `{label}` step declares its registered '
                          'timeout-minutes (same-source from self_check)')

    def test_retirement_step_timeout_regressions_are_rejected(self):
        # #2451：step 级声明上限的删 / 改。装置侧（self_check 的
        # gate-step-timeout-* / negctl-step-timeout-* 样本）钉住「值被预算遍历
        # 消费」；这里钉住「行还在且值同源」——两道守卫独立红，拆掉任何一道，
        # 另一道仍红。emit 的 5 与 check 的 5 同值，锚串带 step 头区分。
        step_timeouts = self.retirement_gate_step_timeouts()
        emit_minutes = step_timeouts['emit 复算与金样逐字节比对']
        negctl_minutes = step_timeouts['负控 N1–N17 全 KILLED']
        emit_anchor = ('      - name: emit 复算与金样逐字节比对\n'
                       '        shell: bash\n'
                       f'        timeout-minutes: {emit_minutes}\n')
        negctl_line = f'        timeout-minutes: {negctl_minutes}\n'
        workflow = self.ci_workflow_fixture()
        body = self.ci_jobs('#1809')['retirement-outcome-parity']
        cases = []

        def change(label, old, new, message):
            self.assertEqual(body.count(old), 1, f'#2451 unique anchor: {label}')
            cases.append((label, workflow.replace(body, body.replace(old, new, 1), 1),
                          message))

        change('emit-step-timeout-removed', emit_anchor,
               emit_anchor.replace(f'        timeout-minutes: {emit_minutes}\n', '', 1),
               '#2451 the `emit 复算与金样逐字节比对` step declares its registered '
               'timeout-minutes (same-source from self_check)')
        change('emit-step-timeout-changed', emit_anchor,
               emit_anchor.replace(f'timeout-minutes: {emit_minutes}\n',
                                   f'timeout-minutes: {emit_minutes - 1}\n', 1),
               '#2451 the `emit 复算与金样逐字节比对` step declares its registered '
               'timeout-minutes (same-source from self_check)')
        change('negctl-step-timeout-removed', negctl_line, '',
               '#2451 the `负控 N1–N17 全 KILLED` step declares its registered '
               'timeout-minutes (same-source from self_check)')
        change('negctl-step-timeout-changed', negctl_line,
               f'        timeout-minutes: {negctl_minutes - 1}\n',
               '#2451 the `负控 N1–N17 全 KILLED` step declares its registered '
               'timeout-minutes (same-source from self_check)')
        self.check_ci_mutations(
            cases, 'test_retirement_gate_steps_declare_step_timeouts', '#2451')

    def test_ci_rejects_docs_only_gate_drift(self):
        workflow = self.ci_workflow_fixture()
        jobs = self.ci_jobs()
        heavy = ('metadata-observer', 'rust', 'a51-coverage', 'negative-controls',
                 'retirement-outcome-parity')
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
        # #1809 r7：Require step 登记键含 `shell: bash`（step 级 shell 优先于任何
        # defaults.run.shell，聚合 job 的 exit 1 靠它才废不掉）。
        scope_step = ('      - name: Require scope success\n'
                      "        if: ${{ always() && needs.scope.result != 'success' }}\n"
                      '        working-directory: .\n'
                      '        shell: bash\n'
                      '        run: exit 1\n')
        change('scope-gate-removed', scope_step, '', '#904 exactly six rejection steps')
        change('scope-gate-relaxed', "needs.scope.result != 'success'",
               "needs.scope.result == 'failure'", '#904 scope failure must reject the merge')
        change('check-needs-drops-scope',
               'needs: [scope, metadata-observer, rust, a51-coverage, negative-controls, '
               'retirement-outcome-parity]',
               'needs: [metadata-observer, rust, a51-coverage, negative-controls, '
               'retirement-outcome-parity]',
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
        # 自测步与分类步都不许跳过或忽略失败（#1809 r7：run step 登记键含 shell）。
        selftest = ('      - name: 分类器自测（正负例与内存内变异负控）\n'
                    '        shell: bash\n'
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
        # #1809 r9：read_bytes 补丁同 check_ci_mutations 的按文件名路由——变异对象
        # 是 scripts/ci_scope.py，不是 ci.yml（r9 前全局 return_value 对任何路径都
        # 喂变异字节，目标测试恰好只读它才没穿帮）。
        self.check_ci_mutations(cases, 'test_ci_scope_whitelist_is_exactly_the_single_board_path',
                                '#1199', reader='read_bytes', mutated='ci_scope.py')

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
        # #2136 fix-r38（MM sibling P2）：套件 r38 干净树实测 23 条约 354 s
        #（CI 约 583 s），挂起时占满 job 默认 360 分钟；步级超时须声明且被钉住。
        # r39（DS boundary#2 P2）：值从 isolation_step_budget 同源派生，不再手抄。
        self.assertEqual(
            fields.get('timeout-minutes'),
            (str(self.isolation_step_budget // 60), []),
            '#994 the isolation CI step must declare timeout-minutes: %d; '
            'without it a hung suite silently falls back to the 360-minute '
            'job default' % (self.isolation_step_budget // 60))

    def test_required_job_runs_the_copy_manifest_self_test(self):
        # #1570: the fix is a derivation, and the only thing that runs its
        # self-test is one CI step. Deleting that step, or making it
        # conditional, must turn a gate red -- otherwise a later refactor can
        # shrink `compile_inputs` back to `src/` and every mutation reads
        # INVALID again, which is exactly the failure this issue is about.
        # Same treatment as the #994 isolation step above.
        command = 'python3 -B -m unittest -v _shared.test_compile_inputs'
        jobs = self.ci_jobs('#1570')
        steps = self.ci_steps('metadata-observer', jobs['metadata-observer'], '#1570')
        matching = [step for step in steps if self.ci_run_commands(step, '#1570') == [command]]
        self.assertEqual(len(matching), 1,
                         '#1570 required CI job must run the copy-manifest self-test')
        fields = self.ci_step_fields(matching[0], '#1570')
        for key in ('if', 'continue-on-error'):
            self.assertNotIn(key, fields, '#1570 self-test CI step must be unconditional')
        self.assertEqual(fields.get('working-directory'),
                         ('src/pilotdeck-gateway/tests/fixtures', []),
                         '#1570 the self-test imports _shared, so it runs from the fixtures dir')
        # #2136 fix-r38（MM sibling P2）：r37 实测约 21 s，同 -O 步钉住步级超时。
        # r39（DS boundary#2 P2）：值从 copy_manifest_step_budget 同源派生。
        self.assertEqual(
            fields.get('timeout-minutes'),
            (str(self.copy_manifest_step_budget // 60), []),
            '#1570 the copy-manifest self-test step must declare '
            'timeout-minutes: %d; without it a hung step silently falls back '
            'to the 360-minute job default'
            % (self.copy_manifest_step_budget // 60))

    def test_required_job_runs_the_discovery_anchor_self_check(self):
        # #2133 fix-r34（MM 评审 P1）：「网关负控发现锚点自检」是唯一执行 #2133 闸门
        # 的 CI 步。删步、加 if、加 continue-on-error 或改错工作目录，此前全仓无红，
        # 闸门在 CI 静默不跑，正是 #2133 要防的「尺子假通过」。同 job 同形自测步的
        # 守卫覆盖此前并不齐（fix-r35 按 DS sibling P1 更正此注释的事实面）：
        # #994 有守卫 + 变异负控；#1570 有守卫但无变异负控；-O 不变性步
        # （#1666/#1734）自本轮起有（test_required_job_runs_the_optimize_guards_
        # self_tests）；#1570 变异负控与 #1203 CLI 循环、gitleaks 自检步的守卫
        # 登记在 #2844（P2-跟进，前置于 #2141）。
        command = 'python3 -B -m unittest -v test_negative_discovery_anchors'
        jobs = self.ci_jobs('#2133')
        steps = self.ci_steps('metadata-observer', jobs['metadata-observer'], '#2133')
        matching = [step for step in steps if self.ci_run_commands(step, '#2133') == [command]]
        self.assertEqual(len(matching), 1,
                         '#2133 required CI job must run the discovery-anchor self-check')
        fields = self.ci_step_fields(matching[0], '#2133')
        for key in ('if', 'continue-on-error'):
            self.assertNotIn(key, fields,
                             '#2133 the self-check CI step must be unconditional')
        self.assertEqual(fields.get('working-directory'),
                         ('src/pilotdeck-gateway/tests/fixtures', []),
                         '#2133 the self-check imports the fixtures tree, '
                         'so it runs from the fixtures dir')
        # #2136 fix-r38（DS sibling P2 / MM concurrency P2a，两家同条）：r37 给
        # 该步新加的 `timeout-minutes: 10`（挂起时占满 job 默认 360 分钟把同
        # job 别步拖死）此前无任何机检——删掉该键全仓仍绿。照 negative-controls
        # job 的 `timeout-minutes: 150` 钉法把它钉住，且与真跑钉的子进程超时
        # 同源（discovery_anchor_step_budget）。
        self.assertEqual(
            fields.get('timeout-minutes'),
            (str(self.discovery_anchor_step_budget // 60), []),
            '#2133 the self-check step must declare timeout-minutes: %d '
            '(the discovery_anchor_step_budget); a deleted or inflated '
            'step timeout silently falls back to the 360-minute job '
            'default and drags the whole job'
            % (self.discovery_anchor_step_budget // 60))

    # #2136 fix-r39（DS sibling#3 P1 / boundary#1 P1，两家独立撞上同条）：
    # `unittest -v` 的 skip 行有三种实测形状（_tmp/20261110-pr2136-revival/
    # r39-skip-format.log，Python 3.14 实跑）：
    #   ① 无 docstring：`test_x (mod.C.test_x) ... skipped 'reason'`——名字
    #      与 skipped 同行，r38 的正则只认这一种；
    #   ② 带 docstring：runner 先单打一行 `test_x (mod.C.test_x)`，再在
    #      docstring 首行那行打 `Doc first line. ... skipped 'reason'`——
    #      名字在前一行，r38 的行首正则两行都抓不到（假 PASS：skip 检查
    #      静默失效，被钉套件里带 docstring 的测试被 skip 不会红）；
    #   ③ subTest 子项：两空格缩进且 `)` 与 `...` 之间多 `[msg]`：
    #      `  test_x (mod.C.test_x) [child] ... skipped 'reason'`——行首
    #      `^(\S+)` 抓不到缩进行。
    # 解析成 (名字, 原因) 列表；解析不了的 `... skipped` 行一律 fail-closed
    # 抛错，格式再漂移时红而不是静默漏判。自身的正反例单测见
    # test_the_verbose_skip_reading_covers_every_channel。
    @staticmethod
    def _verbose_skipped_tests(out):
        skipped = []
        lines = [line.rstrip() for line in out.splitlines()]
        for index, line in enumerate(lines):
            tail = re.search(r" \.\.\. skipped (['\"])(.*)\1$", line)
            if tail is None:
                continue
            reason = tail.group(2)
            # 形状 ① / ③：名字与 skipped 同行（③ 带缩进与 `[msg]`）。
            same = re.match(
                r'\s*(test_\S+) \([^)]*\)(?: \[[^\]]*\])? \.\.\. skipped', line)
            if same is not None:
                skipped.append((same.group(1), reason))
                continue
            # 形状 ②：名字在紧邻的上一行（`test_x (mod.C.test_x)` 独占一行）。
            if index and re.fullmatch(r'(test_\S+) \([^)]*\)', lines[index - 1]):
                skipped.append(
                    (re.match(r'(test_\S+) \(', lines[index - 1]).group(1), reason))
                continue
            raise AssertionError(
                '#2136 unparseable unittest skip line: %r' % line)
        return skipped

    def _assert_skip_channels_agree(self, out, skipped, tag):
        # r39（DS boundary#1 P1）：名字解析与汇总计数两通道互证——
        # `OK (skipped=N)` 把三种形状全部计入；解析漏形（格式漂移）时两者
        # 不等即红，单靠任何一通道都不许。
        counted = re.search(r'skipped=(\d+)', out)
        total = int(counted.group(1)) if counted else 0
        self.assertEqual(
            len(skipped), total,
            '%s the parsed skip lines (%d) must equal the unittest summary '
            'skipped=%d; a mismatch means the verbose format drifted and one '
            'of the two channels is blind' % (tag, len(skipped), total))

    def _assert_suite_really_runs(self, tag, module, minimum, timeout,
                                  allowed_skips=()):
        # r39（DS simplify#1 P2）：四条真跑钉的同构断言抽成一处——r38 前是四份
        # 约 45 行的拷贝，超时值与 skip 判法已经各漂各的。真跑一遍被钉套件并核：
        # 「Ran N」下限（掏空 / 整类 skip 后退 0 的洞）、skip 名字 ⊆ 白名单
        # （条目 = (名字, 原因文本)，无条件 @unittest.skip 冒充条件跳过会因原因
        # 不匹配而红）、两通道计数相等、无 expectedFailure / unexpected
        # successes、套件自身 rc=0。白名单默认为空：无登记条件跳过的套件
        # 任何 skip 都红。
        fixtures = Path(__file__).resolve().parent / 'fixtures'
        try:
            cp = subprocess.run(
                [sys.executable, '-B', '-m', 'unittest', '-v', module],
                cwd=fixtures, capture_output=True, text=True, timeout=timeout)
        except subprocess.TimeoutExpired:
            self.fail('%s the %s subprocess timed out (>%d s, the CI step '
                      'budget); a hung or stalled gate must not pass silently'
                      % (tag, module, timeout))
        out = cp.stdout + cp.stderr
        ran = re.search(r'^Ran (\d+) tests? in ', out, re.M)
        self.assertIsNotNone(
            ran,
            '%s the %s run must print a "Ran N tests" summary line; a broken '
            'or empty entry point must not pass. Tail: %s'
            % (tag, module, out[-800:]))
        self.assertGreaterEqual(
            int(ran.group(1)), minimum,
            '%s %s must run at least %d tests; an emptied, renamed-away, or '
            'skipped module still exits 0 and the gate is silently empty '
            '(ran %s)' % (tag, module, minimum, ran.group(1)))
        skipped = self._verbose_skipped_tests(out)
        self._assert_skip_channels_agree(out, skipped, tag)
        self.assertTrue(
            set(skipped) <= set(allowed_skips),
            '%s only the registered host-conditional skips in %s may be '
            'skipped (allowed: %s); any other skip still exits 0 and the '
            'gate silently passes (skipped: %s)'
            % (tag, module, sorted(allowed_skips), sorted(skipped)))
        for marker in ('expected failures=', 'unexpected successes='):
            self.assertNotIn(
                marker, out,
                '%s %s must not lean on the expectedFailure channel: %s '
                'keeps unittest exit 0 while a real test is not asserted '
                '(found %s)' % (tag, module, marker, marker))
        self.assertEqual(
            cp.returncode, 0,
            '%s the %s suite itself must pass (rc=%d). Tail: %s'
            % (tag, module, cp.returncode, out[-800:]))
        return out

    def test_step_budgets_derive_whole_minute_timeouts(self):
        # r40（DS r39 boundary#1 P2）：budget→`timeout-minutes` 用 `// 60` 截断，
        # 「同一常量同源派生」只在预算是整分钟数时成立——610 s 会让 CI 步被钉成
        # 600 s 而真跑钉拿 610 s（步比钉先红，r38 修掉的假 PASS 方向悄悄复活），
        # 而守卫的等式断言照样过。把「整分钟」机检为不变式，而不是靠巧合。
        for name in ('discovery_anchor_step_budget',
                     'optimize_guards_step_budget',
                     'copy_manifest_step_budget',
                     'isolation_step_budget',
                     'driver_step_budget'):
            budget = getattr(self, name)
            self.assertEqual(
                budget % 60, 0,
                '#2136 %s=%d must be a whole number of minutes: the CI step '
                'pins budget // 60 while the pin passes budget seconds, and a '
                'non-whole-minute budget silently makes the step tighter '
                'than the pin it claims to match' % (name, budget))

    def test_the_verbose_skip_reading_covers_every_channel(self):
        # r39（DS boundary#3 P2）：skip 判读助手自身的负控——r38 版正则对
        # docstring / subTest 形状零命中而 `空集 ⊆ 白名单` 恒真，没有任何用例
        # 能证明「漏掉一个 skip 必须红」。这里用固定文本覆盖三种实测形状 +
        # 计数通道互证 + fail-closed + 空输出，全为纯函数判读，不起子进程。
        transcript = (
            "test_plain (m.C.test_plain) ... skipped 'plain reason'\n"
            "test_docstringed (m.C.test_docstringed)\n"
            "Docstring first line. ... skipped \"docstring reason\"\n"
            "test_ok (m.C.test_ok) ... ok\n"
            "  test_subtest (m.C.test_subtest) [child] ... skipped "
            "'subtest reason'\n"
            "Ran 4 tests in 0.001s\n\nOK (skipped=3)\n")
        self.assertEqual(
            self._verbose_skipped_tests(transcript),
            [('test_plain', 'plain reason'),
             ('test_docstringed', 'docstring reason'),
             ('test_subtest', 'subtest reason')])
        self._assert_skip_channels_agree(
            transcript, self._verbose_skipped_tests(transcript), '#r39')
        # 计数通道与名字通道不符（漂移形状）必须红：
        with self.assertRaises(AssertionError):
            self._assert_skip_channels_agree(
                transcript.replace('skipped=3', 'skipped=2'),
                self._verbose_skipped_tests(transcript), '#r39')
        # 解析不了的 skip 行必须 fail-closed，而不是静默漏判：
        with self.assertRaises(AssertionError):
            self._verbose_skipped_tests(
                "??? ... skipped 'unparseable'\n"
                "Ran 1 test in 0.001s\n\nOK (skipped=1)\n")
        # 空输出：无 skip 行、无 skipped= 汇总，两通道一致为 0。
        empty = 'Ran 0 tests in 0.000s\n\nOK\n'
        self.assertEqual(self._verbose_skipped_tests(empty), [])
        self._assert_skip_channels_agree(empty, [], '#r39')

    def test_the_discovery_anchor_self_check_collects_real_tests(self):
        # #2136 fix-r35（DS boundary P2）→ fix-r36（MiniMax r35 boundary P0）：
        # 上面的守卫只钉「步存在 + 无条件 + cwd」，钉不住该步真的执行了非零且
        # 非 skip 的测试——把模块掏空或整类 @unittest.skip 后，
        # `python3 -B -m unittest -v` 收集 0 个测试仍退 0，CI 绿而 #2133 闸门
        # 静默为空。r35 的旧钉（装载模块走 suite 栈 + 实例 __unittest_skip__）
        # 探不到 setUpModule / setUpClass 抛 SkipTest 的**结果通道**——那条路径
        # unittest 打「Ran 0 tests / OK (skipped=1)」退 0，旧钉仍见 total=201、
        # skipped=[]（实测 _tmp/20261110-pr2136-revival/p0-repro-r36.log）。只有
        # 真跑一遍并看「Ran N tests」与「skipped=」才能同时覆盖方法级 / 类级 /
        # fixture 级 skip 与整体掏空；全套件实测约 60 s。
        # r39（DS simplify#1）：同构断言抽成 _assert_suite_really_runs（含
        # r37 登记的 expectedFailure 通道洞，见
        # _tmp/20261110-pr2136-revival/r37-expected-failure-channel.log）。
        self._assert_suite_really_runs(
            '#2133', 'test_negative_discovery_anchors', 205,
            self.discovery_anchor_step_budget)

    def test_the_optimize_guards_self_tests_collect_real_tests(self):
        # #2136 fix-r37（MM sibling P1）：上面 -O 步的守卫同样只钉「步存在 +
        # 无条件 + cwd」，钉不住八条命令真的收集到测试——掏空 / skip 任一
        # 模块，`python3 -B -m unittest -v` 退 0，#1666/#1734 的 -O 判别力
        # 在 CI 静默为空。逐模块跑一遍并核「Ran N」下限与无 skip/期望失败；
        # 下限取 r37 实测（_tmp/20261110-pr2136-revival/
        # r37-sibling-module-baselines.log：10/13/8/10/6，合计 47，约 15 s）。
        # r41（合并 main c3100a2b7 带入 #2590 fix-r2）：-O 步新增第六模块
        # issue-2495/test_negative_controls.py（r41 实测 Ran 30、约 4 s），
        # 同钉进下限——不钉则该模块在 CI 掏空/整类 skip 无人看见。
        # #1800 fix-r1（PR #2979 concurrency P1，四家 P2 同撞）：-O 步新增
        # 第七模块 sdk-signals-close-order-pin/test_negative_controls.py
        # （fix-r1 实测 Ran 8），同钉进下限与 commands——本 PR
        # 复刻了 r41 修的那个洞：加了 CI 命令却没登记，删掉 CI 全绿。
        # #1800 fix-r2（PR #2979 boundary P1）：判决路径补 invalid-verdict /
        # summary-publish-fault 两用例（Ran 8→10，本轮实测约 2.5 s——
        # busy-lock 用例按构造须等满 2 s flock 超时，是该模块耗时下界；
        # 秒数按实测定，不做预算算术），下限同步抬到 10：不抬则删掉两条
        # 判决路径用例，CI 仍绿——正是 r41/fix-r1 连环修过的下限漂移洞。
        # #1800 fix-r3（评论追加 1）：stdout 写失败用例（Ran 10→11，本轮实测
        # 全模块约 2.5 s 不变——新用例 0.05 s，subprocess 全走 stub），下限
        # 同步抬到 11：不抬则删掉该用例（吞诊断 / 放行 rc 0 两个方向都无人
        # 看见），CI 仍绿——与 fix-r2 同一个下限漂移洞。
        # #1800 fix-r4（DS sibling#2/boundary#1 同撞）：管道分支配对用例
        # （stdout-pipe-fault）Ran 11→12，下限同步抬到 12——两分支合并/调
        # 序（健康 head 读法变 rc 2）同样必须红。
        # #1800 fix-r5（DS boundary P2a）：三态第三腿（invalid-verdict-pipe，
        # 判决路径 INVALID + counts 行管道故障）Ran 12→13，下限同步抬到
        # 13——管道分支折进 os._exit(0) 的坏改法必须红。
        # #2981：增第八模块 sdk-signals/test_negative_controls.py（close-order
        # pin 的同型最近兄弟，该 runner 手写 abort 记账此前无仓内测试；
        # 本轮实测 Ran 12、约 3 s），下限与 commands 同步——不钉则删掉 CI
        # 行全绿，正是 fix-r1 修过的洞。fix-r2（三出口腿 + exit 相位）
        # Ran 12→16（本机实测约 5 s），下限同步抬到 16。
        # #2981 fix-r3（MM sibling P1）：close-order pin 补出场侧冻结账本
        # 证明用例（exit-ledger-drift，删 runner :539 那行此前整套照绿——
        # 冻结件假 PASS），Ran 14→15（本轮实测约 2.7 s），下限同步抬到 15。
        # #2981 fix-r4（MM sibling#1 + concurrency#1）：close-order pin 补
        # 入场侧孪生用例（ledger-drift，删 runner 入场证明行此前整套照绿，
        # 与出场侧同型）Ran 15→16（本轮实测约 2.8 s）；sdk-signals 补驱动
        # 失败腿（hang：piped 驱动对写完首行后卡死的 runner 有界 + 回收）
        # Ran 16→17（本轮实测约 9.8 s，hang 用例按构造须等满 5 s 注入超时，
        # 是该模块耗时下界的一部分）。下限同步抬。

        minimums = (
            ('test_negative_optimize_guards', 10),
            ('settings-gateway/test_negative_controls.py', 13),
            ('test_negative_run_guards', 8),
            ('network-timing/test_negative_controls.py', 10),
            ('test_negative_roster_guards', 6),
            ('issue-2495/test_negative_controls.py', 30),
            ('sdk-signals-close-order-pin/test_negative_controls.py', 16),
            ('sdk-signals/test_negative_controls.py', 17),
        )
        # r39（DS sibling#4 P2）：network-timing 的两条 EACCES 判杀用例按宿主
        # 能力条件跳过（`@unittest.skipUnless(EACCES_STAGEABLE, ...)`——root /
        # CAP_DAC_OVERRIDE 宿主读得了 mode-000 文件，#1922 的拒绝路径无法钉；
        # 本仓明确支持 root 宿主，ci.yml 的 PDRS_REQUIRE_PERMISSION_FAULTS）。
        # 与 #1570 的大小写文件系统跳过同类：环境条件跳过按 (名字, 原因) 登记
        # 白名单，白名单外的任何 skip（含无条件 @unittest.skip）仍红。
        registered_conditional_skips = {
            'network-timing/test_negative_controls.py': {
                ('test_controls_module_read_eacces_is_refused',
                 '#1922 this host can read a mode-000 file; EACCES cannot '
                 'be staged'),
                ('test_private_catalog_eacces_is_refused',
                 '#1922 this host can read a mode-000 file; EACCES cannot '
                 'be staged'),
            },
        }
        # r39（DS concurrency#2 P2）：超时失败不再包在 subTest 里——subTest 吞
        # 异常继续下一模块，八个各挂 300 s 会把钉烧到 40 分钟；首个挂起即整条
        # 红。总耗时一并钉在步预算上：八个各自 299 s 的「都合规」组合同样
        # 超出它所钉的 CI 步预算（300 s），不许守钉比步长寿。
        started = time.monotonic()
        for module, minimum in minimums:
            self._assert_suite_really_runs(
                '#1666', module, minimum, self.optimize_guards_step_budget,
                registered_conditional_skips.get(module, ()))
        self.assertLessEqual(
            time.monotonic() - started, self.optimize_guards_step_budget,
            '#1666 the eight -O self-test modules together must fit the -O '
            'step budget (%d s); eight near-miss modules must not outlive '
            'the CI step they pin' % self.optimize_guards_step_budget)

    def test_the_copy_manifest_self_test_collects_real_tests(self):
        # #2136 fix-r37（MM sibling P1）：#1570 的自测步守卫同样只钉步存在，
        # 钉不住 `_shared.test_compile_inputs` 真的收集到测试——掏空或整类
        # skip 后退 0，compile_inputs 的覆盖面在 CI 静默为空。r37 实测
        # Ran 82（约 21 s，同上 baselines.log）。
        # r38（DS sibling P1）：`_shared.test_compile_inputs` 有一条按文件系统
        # 大小写条件跳过的真测试（`test_compile_inputs.py` 的
        # `test_a_case_alias_of_the_repository_is_not_outside`：临时目录大写后
        # 不存在即 `skipTest('case-sensitive filesystem')`——Linux CI 的
        # case-sensitive ext4 上必发、darwin 上不发）。r37 的零 skip 断言只在
        # 大小写不敏感的文件系统上成立，会把必跑步在 CI 上打红；改为「跳过
        # 集合 ⊆ 已登记条件跳过白名单」——白名单外的任何 skip（含整类掏空，
        # 82 条全 skip）仍红。
        # r39（DS sibling#3 / boundary#1 P1）：白名单条目同时钉名字与原因文本
        # （无条件 @unittest.skip('flaky') 冒充条件跳过会因原因不匹配而红），
        # skip 判读换 fail-closed 解析 + 计数互证（见 _verbose_skipped_tests），
        # 超时从 copy_manifest_step_budget 同源派生。
        registered_conditional_skips = {
            ('test_a_case_alias_of_the_repository_is_not_outside',
             'case-sensitive filesystem'),
        }
        self._assert_suite_really_runs(
            '#1570', '_shared.test_compile_inputs', 82,
            self.copy_manifest_step_budget, registered_conditional_skips)

    def test_the_shared_isolation_regressions_collect_real_tests(self):
        # #2136 fix-r38（DS sibling P2 / MM sibling P1，两家独立撞上同条）：
        # #994 隔离步的守卫同样只钉「步存在 + 无条件 + cwd」，钉不住该套件
        # 真的收集到测试——`test_negative_adaptive_isolation.py` 以
        # `unittest.main(verbosity=2)` 收尾，掏空或整类 skip 后 `Ran 0 tests`
        # 仍退 0，#994 的隔离/候选基线闸门在 CI 静默为空。真跑一遍并核
        # 「Ran N」下限与无 skip / 期望失败；下限取 r38 干净树实测（23 条，
        # 354 s；CI 步约 583 s，均在 900 s 子进程上限内——该套件自身要求
        # 树与 HEAD 一致（SourceBaselineError fail-closed），脏树上本钉红且
        # 报基线不匹配，与 CI 步本身的前提相同，属预期红。实测
        # _tmp/20261110-pr2136-revival/r38-994-baseline-clean.log。
        # r39（DS sibling#3 / boundary#1 P1）：skip 判读换 fail-closed 解析 +
        # 计数互证（docstring / subTest 形状 r38 的行首正则全漏），超时从
        # isolation_step_budget 同源派生。
        self._assert_suite_really_runs(
            '#994', 'test_negative_adaptive_isolation', 23,
            self.isolation_step_budget)

    def test_required_job_runs_the_optimize_guards_self_tests(self):
        # #2136 fix-r35（DS sibling P1）：-O 不变性自测步（#1666/#1734）与
        # #2133 步同 job 同形——它跑的正是负控守卫的 -O 判别力（含 #2309
        # roster-guards），但删步、加 if、加 continue-on-error 或改错 cwd
        # 此前全仓无红。照 #994/#2133 的三件套补上；五形变异负控见
        # test_optimize_guards_self_test_ci_regressions_are_rejected。
        # r41（合并 main）：#2590 fix-r2 给本步加了第六条命令
        # （issue-2495/test_negative_controls.py），一并钉入 commands——
        # 删掉它同样必须红。
        # #1800 fix-r1（PR #2979 concurrency P1，四家 P2 同撞）：第七条命令
        # （sdk-signals-close-order-pin/test_negative_controls.py）同步钉入
        # commands——子集判断下多一条不红，不钉则删掉 CI 里这行仍全绿。
        # #2981：第八条命令（sdk-signals/test_negative_controls.py）同钉入，
        # 与下限同一改动落地。
        commands = ('python3 -B -m unittest -v test_negative_optimize_guards',
                    'python3 -B -m unittest -v settings-gateway/test_negative_controls.py',
                    'python3 -B -m unittest -v test_negative_run_guards',
                    'python3 -B -m unittest -v network-timing/test_negative_controls.py',
                    'python3 -B -m unittest -v test_negative_roster_guards',
                    'python3 -B -m unittest -v issue-2495/test_negative_controls.py',
                    'python3 -B -m unittest -v sdk-signals-close-order-pin/test_negative_controls.py',
                    'python3 -B -m unittest -v sdk-signals/test_negative_controls.py')
        jobs = self.ci_jobs('#1666')
        steps = self.ci_steps('metadata-observer', jobs['metadata-observer'], '#1666')
        matching = [step for step in steps
                    if all(command in self.ci_run_commands(step, '#1666')
                           for command in commands)]
        self.assertEqual(len(matching), 1,
                         '#1666 required CI job must run the -O invariance self-tests')
        fields = self.ci_step_fields(matching[0], '#1666')
        for key in ('if', 'continue-on-error'):
            self.assertNotIn(key, fields,
                             '#1666 the -O self-test step must be unconditional '
                             'and required')
        self.assertEqual(fields.get('working-directory'),
                         ('src/pilotdeck-gateway/tests/fixtures', []),
                         '#1666 the -O self-tests import the fixtures tree, '
                         'so the step runs from the fixtures dir')
        # #2136 fix-r38（MM sibling P2）：五个模块 r37 实测合计约 15 s，挂起时
        # 占满 job 默认 360 分钟；步级超时同样须声明且被钉住。
        # r39（DS boundary#2 P2）：值从 optimize_guards_step_budget 同源派生。
        self.assertEqual(
            fields.get('timeout-minutes'),
            (str(self.optimize_guards_step_budget // 60), []),
            '#1666 the -O self-test step must declare timeout-minutes: %d; '
            'without it a hung step silently falls back to the 360-minute '
            'job default' % (self.optimize_guards_step_budget // 60))

    def test_required_job_runs_the_environment_controls(self):
        # #2136 fix-r39（DS sibling#1 / concurrency#2 P2）：r38 给四个被钉的
        # 步补了 timeout-minutes，唯独**真正跑这些真跑钉的那一步**（本类全套
        # 86 条，含 #994/#1570/#1666/#2133 四条真跑钉，r38 实测本机 764 s）
        # 没有步级超时，挂起时回落 job 默认 360 分钟——正是那四条钉注释里
        # 要消灭的场景搬到了本步。照 #2133 步同法声明并钉住（30 min ≈ 2.4×
        # 余量，从 driver_step_budget 派生）。
        command = ('\'"$RUNNER_TEMP/pyyaml-pinned/bin/python" -m unittest -v '
                   'test_metadata_io_driver.EnvironmentControls\'')
        jobs = self.ci_jobs('#2136')
        steps = self.ci_steps('metadata-observer', jobs['metadata-observer'], '#2136')
        matching = [step for step in steps if self.ci_run_commands(step, '#2136') == [command]]
        self.assertEqual(len(matching), 1,
                         '#2136 required CI job must run the EnvironmentControls suite')
        fields = self.ci_step_fields(matching[0], '#2136')
        for key in ('if', 'continue-on-error'):
            self.assertNotIn(key, fields,
                             '#2136 the EnvironmentControls step must be unconditional')
        self.assertEqual(
            fields.get('working-directory'), ('src/pilotdeck-gateway/tests', []),
            '#2136 the EnvironmentControls suite runs from the gateway tests dir')
        self.assertEqual(
            fields.get('timeout-minutes'),
            (str(self.driver_step_budget // 60), []),
            '#2136 the EnvironmentControls step must declare timeout-minutes: '
            '%d; without it a hung step silently falls back to the '
            '360-minute job default' % (self.driver_step_budget // 60))

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
               run_step + '        shell: bash\n        run: ' + self.negative_controls_command
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
               '      - name: 锁定工具链\n        shell: bash\n        run: rustup show active-toolchain\n',
               '      - name: 锁定工具链\n        shell: bash\n        run: rustup show active-toolchain\n'
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
               '      - name: 锁定工具链\n        shell: bash\n        run: rustup show active-toolchain\n',
               '      - name: 锁定工具链\n        shell: bash\n        run: rustup show active-toolchain\n'
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

    # #2780 fix-r2（MM 三条 P1 的登记半边 + P2-a）：runner 镜像漂移守卫是 ci.yml
    # 里两份 inline 脚本（rust / negative-controls 各一份），此前删除它、只改一份、
    # 把钉值改漂、把扫描面从两份 workflow 收窄回 ci.yml，都没有任何闸门会红。
    # 这里从 ci.yml 之外钉住：存在性、双副本逐字全等、四个钉值的单一来源，以及
    # ci.yml 与 pr-gate.yml 全部非注释 runs-on 值必须等于守卫钉定的标签。
    runner_image_guard_step_name = 'runner 镜像漂移守卫（#2762）'
    runner_image_guard_anchor = '      - name: runner 镜像漂移守卫（#2762）\n'
    runner_image_guard_scan_line = (
        'for guard_target in .github/workflows/ci.yml .github/workflows/pr-gate.yml; do')
    runner_image_guard_os_release_override = (
        'os_release="${PDRS_IMAGE_GUARD_OS_RELEASE:-/etc/os-release}"')

    def runner_image_guard_scripts(self, issue='#2780'):
        """两个守卫 step 的 run 块原文（含 YAML 缩进）与剥缩进后的可执行脚本。

        按 step 锚点行到下一个 step / job 头切片，不经 ci_steps 的规范化——规范化
        会剥缩进、丢空行，「逐字全等」要的是原文比对（sibling#3 / concurrency#3）。
        """
        text = (Path(__file__).resolve().parents[3]
                / '.github/workflows/ci.yml').read_text()
        lines = text.split('\n')
        indexes = [i for i, line in enumerate(lines)
                   if line == '      - name: ' + self.runner_image_guard_step_name]
        self.assertEqual(len(indexes), 2,
                         f'{issue} exactly two runner image guard steps exist; deleting '
                         'one (or both) must turn this red, not pass silently')
        found = {}
        for i in indexes:
            owner = None
            for k in range(i, -1, -1):
                header = re.fullmatch(r'  ([\w-]+):', lines[k])
                if header:
                    owner = header[1]
                    break
            self.assertIsNotNone(owner, f'{issue} the guard step lives inside a job')
            self.assertNotIn(owner, found,
                             f'{issue} one guard step per job, not more')
            end = len(lines)
            for j in range(i + 1, len(lines)):
                line = lines[j]
                # step 直属键 8 空格、run 块 ≥10 空格；任何非空且 <8 缩进的行
                # （下一 step 的 `- ` 项或其 6 空格注释、job 级键、job 头）即边界。
                if line.strip() and (not line.startswith(' ')
                                     or len(line) - len(line.lstrip(' ')) < 8):
                    end = j
                    break
            region = lines[i:end]
            try:
                run_at = region.index('        run: |')
            except ValueError:
                self.fail(f'{issue} the {owner} guard step carries a literal run block')
            block = region[run_at + 1:]
            while block and not block[-1].strip():
                block.pop()
            self.assertTrue(block, f'{issue} the {owner} guard run block is not empty')
            conditional = [line for line in region
                           if re.fullmatch(r"""        ['\"]?(if|continue-on-error)['\"]?:.*""", line)]
            self.assertFalse(conditional,
                             f'{issue} the guard steps stay unconditional; a skipped or '
                             'failure-ignored guard is a silently missing guard')
            raw = '\n'.join(block) + '\n'
            script = ''.join(line[10:] + '\n' if line.startswith(' ' * 10)
                            else line + '\n' for line in block)
            found[owner] = (raw, script)
        self.assertEqual(set(found), {'rust', 'negative-controls'},
                         f'{issue} the guard steps live exactly in the two cargo jobs '
                         'whose KILLED/SURVIVED comparability the guard protects')
        return found

    def runner_image_guard_pins(self, script, issue='#2780'):
        """从守卫脚本文本反向派生四个钉值（expected* 变量）——机检侧不另抄一份，
        钉值的单一来源就是守卫脚本本身（boundary#1 / boundary#2）。fix-r3 起构建
        号是钉值集合 expected_image_versions（空格分隔，命中任一即绿）。"""
        matches = re.findall(r'^(expected[a-z_]*)="([^"]*)"$', script, re.M)
        self.assertEqual(len(matches), 4,
                         f'{issue} the guard defines exactly the four pin variables')
        pins = {}
        for name, value in matches:
            self.assertNotIn(name, pins,
                             f'{issue} each pin is defined exactly once: {name}')
            pins[name] = value
        self.assertEqual(set(pins), {'expected', 'expected_label', 'expected_image_os',
                                     'expected_image_versions'},
                         f'{issue} the guard pins are exactly the registered four')
        # 三值互相锁定：标签 / 镜像族从 OS 版本派生，任何一处单独改钉先红在这里。
        self.assertEqual(pins['expected_label'], 'ubuntu-' + pins['expected'],
                         f'{issue} the pinned label derives from the pinned OS version')
        self.assertEqual(pins['expected_image_os'], 'ubuntu' + pins['expected'].split('.')[0],
                         f'{issue} the pinned image family derives from the pinned OS '
                         'version major ($ImageOS keeps the major only, '
                         'e.g. ubuntu-24.04 -> ubuntu24)')
        # fix-r3（#2780 P2-a 主题 3）：构建号钉值集合的格式契约——非空、单空格
        # 分隔、无重复成员、每个成员都是上游 runner-images 构建号形态
        # （YYYYMMDD.N.M）。空集合运行时也会 fail-closed，但先红在这里。
        members = pins['expected_image_versions'].split(' ')
        self.assertTrue(members and all(members),
                        f'{issue} the pinned image-version set is non-empty and '
                        'single-space separated; an empty or stray-space set would '
                        'fail-closed at runtime and must be rejected here too')
        self.assertEqual(len(members), len(set(members)),
                         f'{issue} no duplicate members in the pinned image-version set')
        for member in members:
            self.assertRegex(member, r'^\d{8}\.\d+\.\d+$',
                             f'{issue} each pinned image version looks like a '
                             'runner-images build stamp (YYYYMMDD.N.M)')
        pins['expected_image_versions_members'] = members
        return pins

    def runner_image_guard_scan_targets(self, script, issue='#2780'):
        """从守卫脚本的扫描行反向派生扫描面（单一来源，不另抄清单）——登记测试
        再把它与 .github/workflows 目录对账：新增工作流文件不进扫描面先红
        （#2780 fix-r3 P2-a 主题 1 的 Python 侧目录派生；让 shell 自己扫目录
        动守卫块扫描逻辑，超出钉值集合化范围，开 P2-跟进单）。"""
        self.assertIn(self.runner_image_guard_scan_line + '\n', script,
                      f'{issue} the guard keeps the runs-on scan for-loop')
        head, sep, _ = self.runner_image_guard_scan_line.partition('; do')
        self.assertTrue(sep, f'{issue} the scan line keeps its for-loop shape')
        prefix = 'for guard_target in '
        self.assertTrue(head.startswith(prefix),
                        f'{issue} the scan line keeps its iteration variable')
        targets = head[len(prefix):].split()
        self.assertTrue(targets, f'{issue} the scan line carries at least one target')
        return targets

    def workflow_runs_on_values(self, text, issue='#2780'):
        """镜像守卫白名单判定的 Python 侧：全部非注释 runs-on 值（键形态连引号键
        一起认，值剥行尾注释 / 引号 / 空白，与守卫脚本同一口径）。"""
        values = []
        for line in text.split('\n'):
            stripped = line.lstrip()
            if not stripped or stripped.startswith('#'):
                continue
            matched = re.fullmatch(r"""['\"]?runs-on['\"]?[ \t]*:(.*)""", stripped)
            if not matched:
                continue
            value = matched[1].split('#', 1)[0].replace('"', '').replace("'", '')
            values.append(re.sub(r'[ \t\r]', '', value))
        return values

    def test_runner_image_guard_steps_are_registered_and_identical(self):
        guards = self.runner_image_guard_scripts('#2780')
        # 单副本分叉（只改一份的钉值 / 逻辑）在这里红：两个 job 的判据不允许各守各的。
        self.assertEqual(guards['rust'][0], guards['negative-controls'][0],
                         '#2780 the two guard run blocks stay verbatim-identical; '
                         'editing only one copy forks what the two jobs check')
        script = guards['rust'][1]
        # 扫描面：#2763 的钉版口径是「全部 runs-on」，pr-gate.yml 的合并闸门 job
        # 不许掉出白名单（sibling#2 / concurrency#2 / boundary#4）。
        self.assertIn(self.runner_image_guard_scan_line + '\n', script,
                      '#2780 the guard scans both workflows, not only ci.yml')
        # 注入口被删会让下方逻辑自测退化成依赖宿主 os-release（不确定性）。
        self.assertEqual(script.count(self.runner_image_guard_os_release_override), 1,
                         '#2780 the guard keeps the self-test os-release override')
        pins = self.runner_image_guard_pins(script, '#2780')
        jobs = self.ci_jobs('#2780')
        for job in ('rust', 'negative-controls'):
            steps = self.ci_steps(job, jobs[job], '#2780')
            guards_in_job = [step for step in steps
                             if self.ci_step_fields(step, '#2780').get('name')
                             == (self.runner_image_guard_step_name, [])]
            self.assertEqual(len(guards_in_job), 1,
                             '#2780 exactly one runner image guard step per cargo job')
            self.assertEqual(steps.index(guards_in_job[0]), 1,
                             '#2780 the guard sits immediately after checkout '
                             '(fail-fast before any heavy step)')
            fields = self.ci_step_fields(guards_in_job[0], '#2780')
            self.assertEqual(fields.get('working-directory'), ('.', []),
                             '#2780 the guard reads the workflow files from the repository root')
            self.assertEqual(fields.get('shell'), ('bash', []),
                             '#2780 the guard pins the fail-fast shell')
            for key in ('if', 'continue-on-error'):
                self.assertNotIn(key, fields,
                                 '#2780 the guard steps stay unconditional')
        # 白名单值等的机检侧：钉定标签从守卫脚本派生（单一来源），扫描面也从守卫
        # 脚本的扫描行派生（单一来源，fix-r3 P2-a 主题 1 Python 侧目录派生），并与
        # .github/workflows 目录对账：新增工作流文件不进扫描面先红在这里（shell 侧
        # 自动扫目录动守卫块扫描逻辑，开 P2-跟进单）。两个 workflow 的全部非注释
        # runs-on 值必须等于钉定标签——arm / 26.04 / 22.04 / ubuntu-latest /
        # matrix 拼装 / 引号键写法全都过不去（sibling#1 / boundary#1）。
        root = Path(__file__).resolve().parents[3]
        targets = self.runner_image_guard_scan_targets(script, '#2780')
        actual_workflows = sorted(
            str(path.relative_to(root)) for path in (root / '.github/workflows').iterdir()
            if path.is_file() and path.suffix in ('.yml', '.yaml'))
        self.assertEqual(sorted(targets), actual_workflows,
                         '#2780 the guard scan surface must cover every workflow file '
                         'under .github/workflows; a workflow outside the scan list '
                         'silently escapes the runs-on whitelist')
        for relative in targets:
            values = self.workflow_runs_on_values((root / relative).read_text(), '#2780')
            self.assertTrue(values,
                            f'#2780 must carry runs-on lines the guard can see: '
                            f'{relative}; zero parsed hits would silently pass the whitelist')
            self.assertEqual(set(values), {pins['expected_label']},
                             f'#2780 every non-comment runs-on must equal the guard-pinned '
                             f'label {pins["expected_label"]} (in {relative})')
            # fix-r3（#2780 anthropic boundary#2 Python 侧）：flow 风格 job（`x:
            # {runs-on: ...}`）与可复用 workflow 调用（job 级 uses:）不产生行首
            # runs-on:，行级白名单（shell 守卫与 Python 镜像同口径）都扫不到——用
            # 「job 数 == runs-on 命中数」对账 + 禁 job 级 uses: 钉死；配对负控见
            # rejects_registration_drift 的 flow-style-job-escapes-whitelist。
            import yaml
            flow = yaml.safe_load((root / relative).read_text())
            flow_jobs = flow.get('jobs') or {}
            self.assertTrue(flow_jobs,
                            f'#2780 {relative} carries jobs the guard can audit')
            self.assertFalse([name for name, body in flow_jobs.items()
                              if isinstance(body, dict) and 'uses' in body],
                             f'#2780 no job may be a reusable-workflow call (uses:) in '
                             f'{relative}; such a job escapes the line-based runs-on '
                             'whitelist entirely')
            self.assertEqual(len(flow_jobs), len(values),
                             f'#2780 every job in {relative} must own exactly one '
                             'line-based runs-on hit; a flow-style job would otherwise '
                             'silently escape the whitelist')
        # fix-r3（#2780 P2-a 主题 2）：守卫读的三个环境变量不许被任何工作流自己
        # 定义——step / job / workflow 任一层 env 一旦钉死 ImageOS / ImageVersion /
        # PDRS_IMAGE_GUARD_OS_RELEASE，守卫读到的就是自产假值，漂移永远不红（配对
        # 负控见 rejects_registration_drift 的 *-env-neuters-* 两条）。正则只认
        # 「键:」形态，守卫脚本正文里的 ${ImageVersion:-} 引用不会误伤。
        for relative in targets:
            offenders = [line for line in (root / relative).read_text().split('\n')
                         if re.match(r"^\s*['\"]?(ImageOS|ImageVersion"
                                     r"|PDRS_IMAGE_GUARD_OS_RELEASE)['\"]?\s*:", line)]
            self.assertFalse(offenders,
                             f'#2780 no workflow may define the guard env vars itself '
                             f'(ImageOS/ImageVersion/PDRS_IMAGE_GUARD_OS_RELEASE) in '
                             f'{relative}; a self-defined env neuters runner drift '
                             'detection')

    def test_runner_image_guard_rejects_registration_drift(self):
        # #2780 fix-r2：登记断言的内存变异负控。每条都必须让上面的登记测试红在
        # 绑定的失败消息上——删守卫步（两个 job 各一条）、单副本分叉、扫描面收窄、
        # 注入口被删、步被条件化、错位、钉值互相矛盾、runs-on 漂移（含 pr-gate.yml
        # 与引号键形态）。改动 ci.yml 的用 ci.yml 路由，改动 pr-gate.yml 的用
        # pr-gate.yml 路由（check_ci_mutations 的按文件名路由，#1809 r9 口径）。
        workflow = self.ci_workflow_fixture()
        guards = self.runner_image_guard_scripts('#2780')
        step = (self.runner_image_guard_anchor + '        working-directory: .\n'
                '        shell: bash\n        run: |\n' + guards['negative-controls'][0])
        self.assertEqual(workflow.count(step), 2, '#2780 both guard steps share one shape')
        ci_cases = [('guard-step-removed-negative-controls',
                     workflow.replace(step, '', 1),
                     '#2780 exactly two runner image guard steps exist')]
        head, _, tail = workflow.partition(step)
        ci_cases.append(('guard-step-removed-rust', head + step + tail.replace(step, '', 1),
                         '#2780 exactly two runner image guard steps exist'))
        ci_cases.append(('guard-copies-diverge',
                         workflow.replace('          expected="24.04"\n',
                                           '          expected="26.04"\n', 1),
                         '#2780 the two guard run blocks stay verbatim-identical'))
        for label, old, new, message in (
                ('scan-target-narrowed',
                 '          for guard_target in .github/workflows/ci.yml '
                 '.github/workflows/pr-gate.yml; do\n',
                 '          for guard_target in .github/workflows/ci.yml; do\n',
                 '#2780 the guard scans both workflows'),
                ('os-release-override-removed',
                 '          os_release="${PDRS_IMAGE_GUARD_OS_RELEASE:-/etc/os-release}"\n',
                 '          os_release=/etc/os-release\n',
                 '#2780 the guard keeps the self-test os-release override'),
                ('pins-inconsistent',
                 '          expected_image_os="ubuntu24"\n',
                 '          expected_image_os="ubuntu-24"\n',
                 '#2780 the pinned image family derives from the pinned OS version')):
            self.assertEqual(workflow.count(old), 2, f'#2780 both copies carry {label}')
            ci_cases.append((label, workflow.replace(old, new), message))
        ci_cases.append(('guard-step-conditional',
                         workflow.replace(self.runner_image_guard_anchor,
                                           self.runner_image_guard_anchor
                                           + '        if: false\n'),
                         '#2780 the guard steps stay unconditional'))
        # 错位：在 negative-controls 的守卫前插一个哑步（锚点用本 job 独有的注释行），
        # 守卫挪到 index 2，fail-fast 语义丢失。
        shift_anchor = ('      # 盲区⑤ fix-r1（#2780 MM P1-2）：同款 runner 镜像漂移守卫'
                        '复制进本 job——\n')
        self.assertEqual(workflow.count(shift_anchor), 1, '#2780 unique shift anchor')
        ci_cases.append(('guard-shifted-past-toolchain',
                         workflow.replace(shift_anchor,
                                          '      - run: \'true\'\n' + shift_anchor, 1),
                         '#2780 the guard sits immediately after checkout'))
        for label, new in (('runs-on-arm-drift', '    runs-on: ubuntu-24.04-arm'),
                           ('runs-on-latest-regression', '    runs-on: ubuntu-latest'),
                           ('runs-on-matrix-assembly',
                            '    runs-on: ubuntu-${{ matrix.os }}')):
            ci_cases.append((label,
                             workflow.replace('    runs-on: ubuntu-24.04', new, 1),
                             '#2780 every non-comment runs-on must equal the guard-pinned '
                             'label'))
        # fix-r3（#2780 P2-a 主题 2/3）：钉值集合与守卫 env 的配对负控。集合类变异
        # 两副本同改（保持逐字全等，让红落在绑定断言上而不是分叉断言上）。
        pin_line = '          expected_image_versions="20261101.360.1"\n'
        self.assertEqual(workflow.count(pin_line), 2,
                         '#2780 both copies carry the image-version pin')
        for label, new, message in (
                ('pin-set-emptied',
                 '          expected_image_versions=""\n',
                 '#2780 the pinned image-version set is non-empty'),
                ('pin-set-duplicate-member',
                 '          expected_image_versions="20261101.360.1 20261101.360.1"\n',
                 '#2780 no duplicate members in the pinned image-version set'),
                ('pin-set-malformed-member',
                 '          expected_image_versions="20261101.360.1 latest"\n',
                 '#2780 each pinned image version looks like a runner-images build '
                 'stamp'),
                ('guard-fifth-pin',
                 pin_line + '          expected_extra="x"\n',
                 '#2780 the guard defines exactly the four pin variables')):
            ci_cases.append((label, workflow.replace(pin_line, new), message))
        # 重复定义钉值：把 expected_label 换成第二个 expected_image_versions——总匹配
        # 数仍是 4，红在「每个钉值只定义一次」而不是总数断言上。
        self.assertEqual(
            workflow.count('          expected_label="ubuntu-24.04"\n'), 2,
            '#2780 both copies carry the label pin (anchor for duplicate-pin mutant)')
        ci_cases.append(('guard-duplicate-pin',
                         workflow.replace('          expected_label="ubuntu-24.04"\n',
                                           '          expected_image_versions='
                                           '"20261231.1.0"\n'),
                         '#2780 each pin is defined exactly once'))
        # 守卫 env 冒犯（主题 2）：两副本同插 step 级 env / 头部插 workflow 级 env，
        # 守卫读到的将是自产假值——必须红在 env 冒犯断言上。
        self.assertTrue(workflow.startswith('name: ci\n'),
                        '#2780 ci.yml keeps its head anchor for the env mutant')
        ci_cases.append(('guard-env-step-neuters-version',
                         workflow.replace(self.runner_image_guard_anchor,
                                           self.runner_image_guard_anchor
                                           + '        env:\n'
                                             '          ImageVersion: 20261101.360.1\n'),
                         '#2780 no workflow may define the guard env vars itself'))
        ci_cases.append(('workflow-env-neuters-imageos',
                         workflow.replace('name: ci\n',
                                           'name: ci\n\nenv:\n  ImageOS: ubuntu24\n', 1),
                         '#2780 no workflow may define the guard env vars itself'))
        # fix-r3（#2780 P2-a 主题 3 补遗 + anthropic boundary#2）：working-directory
        # 漂移、同 job 重复守卫、flow 风格 job 绕过行级白名单——各自红在绑定断言。
        self.assertEqual(workflow.count(self.runner_image_guard_anchor
                                        + '        working-directory: .\n'), 2,
                         '#2780 both guard steps pin working-directory at repo root')
        ci_cases.append(('guard-working-directory-drift',
                         workflow.replace(self.runner_image_guard_anchor
                                          + '        working-directory: .\n',
                                          self.runner_image_guard_anchor
                                          + '        working-directory: src\n'),
                         '#2780 the guard reads the workflow files from the '
                         'repository root'))
        ci_cases.append(('guard-duplicated',
                         workflow.replace(shift_anchor, step + shift_anchor, 1),
                         '#2780 exactly two runner image guard steps exist'))
        # flow 风格 job 绕过行级白名单的配对负控走 pr-gate.yml 路由：ci.yml 侧的
        # 行式 job 声明守卫（ci_jobs 的 unsupported job declaration）会先红，构造
        # 不出只红在本断言上的 ci 变异——那层是 ci.yml 独立的既有防御，这里的牙
        # 由 pr-gate.yml 路由证明（该文件无同款行式守卫）。
        self.check_ci_mutations(ci_cases,
                                'test_runner_image_guard_steps_are_registered_and_identical',
                                '#2780')
        gate_text = (Path(__file__).resolve().parents[3]
                     / '.github/workflows/pr-gate.yml').read_text()
        self.assertEqual(gate_text.count('    runs-on: ubuntu-24.04\n'), 1,
                         '#2780 unique pr-gate runs-on anchor')
        gate_cases = [
            ('pr-gate-runs-on-latest',
             gate_text.replace('    runs-on: ubuntu-24.04\n', '    runs-on: ubuntu-latest\n'),
             '#2780 every non-comment runs-on must equal the guard-pinned label'),
            ('pr-gate-runs-on-quoted-key',
             gate_text.replace('    runs-on: ubuntu-24.04\n',
                               "    'runs-on': ubuntu-latest\n"),
             '#2780 every non-comment runs-on must equal the guard-pinned label'),
            ('pr-gate-runs-on-removed',
             gate_text.replace('    runs-on: ubuntu-24.04\n', ''),
             '#2780 must carry runs-on lines the guard can see'),
            # fix-r3（anthropic boundary#2 配对负控）：flow 风格 job 的 runs-on 行
            # 不在行首，行级白名单（shell 守卫与 Python 镜像同口径）扫不到——红在
            # 「job 数 == runs-on 命中数」对账断言上。
            ('pr-gate-flow-style-job-escapes-whitelist',
             gate_text.replace('jobs:\n',
                               'jobs:\n'
                               '  x: {runs-on: ubuntu-latest, steps: '
                               '[{run: true}]}\n', 1),
             '#2780 every job in .github/workflows/pr-gate.yml must own '
             'exactly one line-based runs-on hit'),
        ]
        self.check_ci_mutations(gate_cases,
                                'test_runner_image_guard_steps_are_registered_and_identical',
                                '#2780', mutated='pr-gate.yml')

    def test_runner_image_guard_logic_self_check(self):
        # #2780 fix-r2（P2-a / concurrency#3 / boundary#3）：守卫逻辑的行为自测。
        # 跑从 ci.yml 提取的真实脚本（不 mock、不另写副本）：正控必须绿，每条拒绝
        # 路径必须红在专属错误标记上——OS 漂移 / $ImageOS 漂移 / $ImageVersion 漂移
        # 与缺失（构建维度）/ runs-on 值漂移（含 ubuntu-latest 回归）/ 目标不可读 /
        # grep rc>=2 / 零命中。fix-r3 增补：os-release 边界（无 VERSION_ID / 不可
        # 读）、pr-gate 值漂移逻辑级负控、引号键绿路径、钉值集合行为（第二成员绿 /
        # 空集合红）。逐条跑完再汇总，任一非 KILLED 即红（不遇第一条失败就中止）。
        # $ImageOS/$ImageVersion 在真实 runner 上的存在性与格式属评审推断（本环境
        # 无 GitHub 托管 runner），守卫按 fail-closed 设计覆盖。
        guards = self.runner_image_guard_scripts('#2780')
        script = guards['negative-controls'][1]
        pins = self.runner_image_guard_pins(script, '#2780')
        root = Path(__file__).resolve().parents[3]
        ci_text = (root / '.github/workflows/ci.yml').read_text()
        gate_text = (root / '.github/workflows/pr-gate.yml').read_text()
        results = []
        greens = []
        positive = None

        def world(guard_ci=ci_text, guard_gate=gate_text, os_version=None, env=None,
                  ci_is_dir=False, skip_gate=False, os_release_missing=False):
            def build(work):
                flows = work / '.github/workflows'
                flows.mkdir(parents=True)
                if ci_is_dir:
                    (flows / 'ci.yml').mkdir()
                else:
                    (flows / 'ci.yml').write_text(guard_ci)
                if not skip_gate:
                    (flows / 'pr-gate.yml').write_text(guard_gate)
                # fix-r3（#2780 P2-a 主题 4）：os_version 三态——None=写钉定值（正
                # 控）、''=文件在但无 VERSION_ID（边界：actual 退回空）、其余=具体漂
                # 移值。旧写法 os_version or pins[...] 会把 '' 吞成钉定值，边界用例
                # 根本跑不到（DS boundary 的原报告点）。
                environment = {'PATH': '/usr/bin:/bin',
                               'ImageOS': pins['expected_image_os'],
                               'ImageVersion': pins['expected_image_versions_members'][0]}
                if os_release_missing:
                    environment['PDRS_IMAGE_GUARD_OS_RELEASE'] = str(
                        work / 'nonexistent-os-release')
                else:
                    release = work / 'os-release'
                    if os_version is None:
                        release.write_text(f'VERSION_ID="{pins["expected"]}"\n')
                    elif os_version == '':
                        release.write_text('NAME="Ubuntu"\n')
                    else:
                        release.write_text(f'VERSION_ID="{os_version}"\n')
                    environment['PDRS_IMAGE_GUARD_OS_RELEASE'] = str(release)
                environment.update(env or {})
                return environment
            return build

        def run_guard(name, build, expect_pass, marker=None, extra_green=False,
                      guard_script=None):
            nonlocal positive
            with tempfile.TemporaryDirectory() as temp:
                completed = subprocess.run(
                    ['bash', '-c', guard_script or script], cwd=temp,
                    env=build(Path(temp)), capture_output=True, text=True, timeout=60)
            if expect_pass:
                ok = (completed.returncode == 0
                      and 'runner 镜像守卫通过' in completed.stdout
                      and 'runs-on 白名单反向断言通过' in completed.stdout)
                detail = (f'rc={completed.returncode} stdout={completed.stdout!r} '
                          f'stderr={completed.stderr!r}')
                if extra_green:
                    greens.append((name, ok, detail))
                else:
                    positive = (name, ok, detail)
                return
            ok = completed.returncode != 0 and marker in completed.stderr
            status = ('KILLED' if ok
                      else 'SURVIVED' if completed.returncode == 0 else 'INVALID')
            results.append((name, status))
            print(f'#2780 {name}: {status}: rc={completed.returncode} '
                  f'stderr={completed.stderr!r}', flush=True)

        run_guard('baseline-green', world(), expect_pass=True)
        run_guard('os-version-drift', world(os_version='26.04'), expect_pass=False,
                  marker='实际 OS 版本="26.04"')
        run_guard('image-os-drift', world(env={'ImageOS': 'ubuntu26'}), expect_pass=False,
                  marker='实际 ImageOS="ubuntu26"')
        run_guard('image-version-drift',
                  world(env={'ImageVersion': '20261101.999.0'}), expect_pass=False,
                  marker='ImageVersion="20261101.999.0"')
        run_guard('image-version-missing', world(env={'ImageVersion': ''}),
                  expect_pass=False, marker='ImageVersion="<未提供>"')
        run_guard('runs-on-arm-drift',
                  world(guard_ci=ci_text.replace('    runs-on: ubuntu-24.04',
                                                 '    runs-on: ubuntu-24.04-arm', 1)),
                  expect_pass=False, marker='runs-on 值 "ubuntu-24.04-arm"')
        run_guard('runs-on-latest-regression',
                  world(guard_ci=ci_text.replace('    runs-on: ubuntu-24.04',
                                                 '    runs-on: ubuntu-latest', 1)),
                  expect_pass=False, marker='runs-on 值 "ubuntu-latest"')
        run_guard('guard-target-unreadable', world(skip_gate=True), expect_pass=False,
                  marker='不可读')
        # grep rc>=2 的确定性触发：目标做成目录（-r 对目录为真、grep 报 rc=2）。
        # BSD grep（macOS）对目录的退出码未实测，非 Linux 显式跳过并记原因，不静默通过。
        if sys.platform == 'linux':
            run_guard('grep-rc2-directory-target', world(ci_is_dir=True),
                      expect_pass=False, marker='异常退出 rc=2')
        else:
            print(f'#2780 grep-rc2-directory-target: SKIPPED: rc>=2 of GNU grep on a '
                  f'directory is not verified on {sys.platform}', flush=True)
        run_guard('no-runs-on-parsed', world(guard_ci=''), expect_pass=False,
                  marker='一条 runs-on 都没解析到')
        # fix-r3（#2780 P2-a 主题 4）：os-release 边界——文件在但无 VERSION_ID /
        # 文件读不到，actual 都退回空，必须红在「<读不到>」上而不是静默过。
        run_guard('os-version-missing', world(os_version=''), expect_pass=False,
                  marker='实际 OS 版本="<读不到>"')
        run_guard('os-release-unreadable', world(os_release_missing=True),
                  expect_pass=False, marker='实际 OS 版本="<读不到>"')
        # pr-gate 侧值漂移的逻辑级负控；引号键是绿路径不是死代码（守卫键形态
        # 连引号键一起认，#2017 先例）。
        run_guard('pr-gate-runs-on-latest',
                  world(guard_gate=gate_text.replace('    runs-on: ubuntu-24.04',
                                                     '    runs-on: ubuntu-latest', 1)),
                  expect_pass=False, marker='runs-on 值 "ubuntu-latest"')
        run_guard('quoted-key-pr-gate-green',
                  world(guard_gate=gate_text.replace('    runs-on: ubuntu-24.04',
                                                     "    'runs-on': ubuntu-24.04", 1)),
                  expect_pass=True, extra_green=True)
        # fix-r3（#2780 anthropic boundary P1，钉值集合）：命中任一成员即绿、空集合
        # fail-closed。只改提取出的脚本文本（不动 ci.yml）：临时扩一个第二成员，
        # 实际值落在第二成员上必须绿；集合清空必须红在专属错误上。
        pin_line = re.search(r'^expected_image_versions="[^"]*"$', script, re.M)[0]
        second_member = '20261231.1.0'
        script_two_members = script.replace(
            pin_line, pin_line[:-1] + f' {second_member}"')
        run_guard('image-version-set-second-member-green',
                  world(env={'ImageVersion': second_member}), expect_pass=True,
                  extra_green=True, guard_script=script_two_members)
        run_guard('pin-set-emptied-at-runtime', world(), expect_pass=False,
                  marker='钉值集合 expected_image_versions 为空',
                  guard_script=script.replace(pin_line, 'expected_image_versions=""'))
        # 「解析到非 runs-on 行」分支是纵深防御：grep 正则 ^\s*['\"]?runs-on['\"]?\s*:
        # 只可能产出键剥引号/空白后等于 runs-on 的行，注入不出别的键——机检侧无法
        # 从 YAML 文本确定性造出该分支的触发输入，不设行为用例（依据本注释，不是
        # 遗漏）。
        self.assertTrue(positive and positive[1],
                        f'#2780 the guard logic positive control must pass: {positive}')
        self.assertTrue(greens and all(ok for _, ok, _ in greens),
                        f'#2780 every extra green path must stay green: {greens}')
        self.assertTrue(results and all(status == 'KILLED' for _, status in results),
                        f'#2780 every guard-logic mutant must die on its own marker: '
                        f'{results}')
        print(f'#2780 guard logic negative summary: {{'
              f'"KILLED": {len(results)}}}', flush=True)

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
        # r38（MM sibling P2）：删掉步级超时，回落 job 默认 360 分钟。
        # r39（DS sibling#2 P2）：补「膨胀」形——把值改成 360 同样失去挂起
        # 保护（#2133 步已两形齐全，其余只有 removed）。
        timeout_message = ('#994 the isolation CI step must declare '
                           'timeout-minutes: 15')
        cases.append(('isolation-ci-timeout-removed',
                      workflow.replace('        timeout-minutes: 15\n', '', 1),
                      timeout_message))
        cases.append(('isolation-ci-timeout-inflated',
                      workflow.replace('        timeout-minutes: 15\n',
                                       '        timeout-minutes: 360\n', 1),
                      timeout_message))
        self.check_ci_mutations(cases, 'test_required_job_runs_shared_isolation_regressions', '#994')

    def test_discovery_anchor_self_check_ci_regressions_are_rejected(self):
        # #2133 fix-r34（MM 评审 P1）：与 #994 同形的变异负控——删步 /
        # 注释掉 / 加 if / 加 continue-on-error / 改错 working-directory，
        # 每形必须杀在上一条守卫的专属断言上，而不是别的失败点（#1570 缺
        # 变异负控，登记在 #2844，见上一条守卫注释）。
        workflow = self.ci_workflow_fixture()
        command = 'run: python3 -B -m unittest -v test_negative_discovery_anchors'
        cases = [(name, workflow.replace(command, changed, 1), message)
                 for name, changed, message in (
            ('discovery-self-check-removed', 'run: true',
             '#2133 required CI job must run the discovery-anchor self-check'),
            ('discovery-self-check-commented', '# ' + command,
             '#2133 required CI job must run the discovery-anchor self-check'),
            ('discovery-self-check-skipped', 'if: false\n        ' + command,
             '#2133 the self-check CI step must be unconditional'),
            ('discovery-self-check-ignored', 'continue-on-error: true\n        ' + command,
             '#2133 the self-check CI step must be unconditional'))]
        # wrong-cwd 不能照 #994 在 run 行前插一行（本步已有 working-directory，
        # 会先死在重复键上）：按步名锚定改掉它本来的值。
        cases.append(('discovery-self-check-wrong-cwd',
                      workflow.replace(
                          '- name: 网关负控发现锚点自检（#2133）\n'
                          '        working-directory: src/pilotdeck-gateway/tests/fixtures\n',
                          '- name: 网关负控发现锚点自检（#2133）\n'
                          '        working-directory: src\n', 1),
                      '#2133 the self-check imports the fixtures tree, '
                      'so it runs from the fixtures dir'))
        # r38（DS sibling P2 / MM concurrency P2a）：timeout-minutes 两形——删掉
        # 该键回落 job 默认 360 分钟；把值改成 360 同样失去挂起保护。
        timeout_message = ('#2133 the self-check step must declare '
                           'timeout-minutes: 10')
        cases.append(('discovery-self-check-timeout-removed',
                      workflow.replace('        timeout-minutes: 10\n', '', 1),
                      timeout_message))
        cases.append(('discovery-self-check-timeout-inflated',
                      workflow.replace('timeout-minutes: 10', 'timeout-minutes: 360', 1),
                      timeout_message))
        self.check_ci_mutations(
            cases, 'test_required_job_runs_the_discovery_anchor_self_check', '#2133')

    def test_optimize_guards_self_test_ci_regressions_are_rejected(self):
        # #2136 fix-r35（DS sibling P1）：-O 不变性步（#1666/#1734）的变异负控，
        # 与 #994 / #2133 同形。该步是 run 块（八条命令），删掉其中任一条
        # （以 #2309 roster-guards 为代表）必须杀在「必须跑全部八条」的断言上；
        # if / continue-on-error / 错 cwd 按步名锚定插入或改值。
        workflow = self.ci_workflow_fixture()
        name_line = '- name: 网关负控守卫 -O 不变性自测（#1666/#1734）\n'
        command = 'python3 -B -m unittest -v test_negative_roster_guards'
        cases = [(name_, workflow.replace(old, new, 1), message)
                 for name_, old, new, message in (
            ('optimize-self-test-removed', command + '\n', '',
             '#1666 required CI job must run the -O invariance self-tests'),
            ('optimize-self-test-commented', command, '# ' + command,
             '#1666 required CI job must run the -O invariance self-tests'),
            ('optimize-self-test-skipped', name_line,
             name_line + '        if: false\n',
             '#1666 the -O self-test step must be unconditional and required'),
            ('optimize-self-test-ignored', name_line,
             name_line + '        continue-on-error: true\n',
             '#1666 the -O self-test step must be unconditional and required'),
            ('optimize-self-test-wrong-cwd',
             name_line + '        working-directory: src/pilotdeck-gateway/tests/fixtures\n',
             name_line + '        working-directory: src\n',
             '#1666 the -O self-tests import the fixtures tree, '
             'so the step runs from the fixtures dir'),
            # r38（MM sibling P2）：删掉步级超时，回落 job 默认 360 分钟。
            # r39（DS sibling#2 P2）：补「膨胀」形；锚串带后续 run 块头——
            # ci.yml 里 `timeout-minutes: 5` 还有 #1570 步一份。
            ('optimize-self-test-timeout-removed',
             'timeout-minutes: 5\n        run: |', 'run: |',
             '#1666 the -O self-test step must declare timeout-minutes: 5'),
            ('optimize-self-test-timeout-inflated',
             'timeout-minutes: 5\n        run: |', 'timeout-minutes: 360\n        run: |',
             '#1666 the -O self-test step must declare timeout-minutes: 5'))]
        self.check_ci_mutations(
            cases, 'test_required_job_runs_the_optimize_guards_self_tests', '#1666')

    def test_environment_controls_step_ci_regressions_are_rejected(self):
        # r39（DS sibling#1 P2）：driver 步（跑 EnvironmentControls 全套）的
        # 超时变异负控——删掉 / 膨胀都必须杀在新守卫的超时断言上；锚串带
        # run 行（该 run 标量全仓唯一），不与其它步的 timeout 行混淆。
        workflow = self.ci_workflow_fixture()
        timeout_message = ('#2136 the EnvironmentControls step must declare '
                           'timeout-minutes: 30')
        timeout_line = '        timeout-minutes: 30\n'
        run_line = ('        run: \'"$RUNNER_TEMP/pyyaml-pinned/bin/python" '
                    '-m unittest -v test_metadata_io_driver.EnvironmentControls\'\n')
        cases = [
            ('environment-controls-timeout-removed',
             workflow.replace(timeout_line + run_line, run_line),
             timeout_message),
            ('environment-controls-timeout-inflated',
             workflow.replace(timeout_line + run_line,
                              '        timeout-minutes: 360\n' + run_line),
             timeout_message),
        ]
        self.check_ci_mutations(
            cases, 'test_required_job_runs_the_environment_controls', '#2136')

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
            if name in ('check', 'scope', 'negative-controls', 'retirement-outcome-parity'):
                # check 只聚合；scope 只跑 Python/git（#1199）；negative-controls 只跑
                # Python/cargo（#1632）；retirement-outcome-parity 只跑 Python（#1809）。
                continue
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
        self.assertEqual(len(pinned), 15,
                         '#1632/#1476/#1313/#2302 fifteen mutations cover the wire-up')
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
                          'always-on-abort-skips-the-input-read',
                          'always-on-envelope-code-rewritten',
                          'always-on-abort-drops-the-empty-string-echo',
                          'describe-advertises-always-on-apply',
                          'describe-drops-async-hook-result',
                          'describe-drops-delete-session'],
                         '#1632/#1476/#1313/#2302 the fifteen mutations are exactly these, '
                         'not merely fifteen rows')
        dropped = ast.get_source_segment(source, pinned[0])
        mutant = source.replace(dropped + ',', '', 1)
        self.assertEqual(mutant.count(dropped), 0, '#1632 the mutant row really left the table')
        self.assertEqual(len(rows(mutant)), 14,
                         '#1632 dropping a row must change the pinned count')

    def test_dispatch_case_table_cannot_shrink_silently(self):
        # #1938 r1（GLM boundary P1）：`dispatch_probe` 的用例表只被 `for` 消费——
        # 删掉 1..n-1 行，探针少驱动几格仍打印通过并 exit 0；整表清空更是零次。
        # 与 `mutations` / `self_check_controls` 同法：行数、顺序与标签逐一钉死，
        # 再用内存内变异证明少一行会红。夹具侧的 fail-closed 守卫单独核在下面。
        source = (Path(__file__).resolve().parent/'fixtures/session-assembly-1316/'
                  'negative_controls.py').read_text()

        def rows(text):
            for node in ast.walk(ast.parse(text)):
                if (isinstance(node, ast.Assign)
                        and isinstance(node.value, ast.Tuple)
                        and any(isinstance(target, ast.Name) and target.id == 'invocations'
                                for target in node.targets)):
                    return node.value.elts
            self.fail('#1938 the fixture must define a literal invocations table')

        pinned = rows(source)
        labels = [ast.literal_eval(row.elts[1]) for row in pinned]
        self.assertEqual(labels, [
            '(no arguments)',
            '-x',
            '-x no-such-mutation',
            '--slot-guard-probe only-one',
            '--slot-guard-probe /bin/true not-a-timeout',
            '--made-up-mode extra',
        ], '#1938 the six dispatch cases are exactly these, not merely six rows')
        self.assertEqual(len(pinned), 6, '#1938 six cases drive the dispatcher')
        # 守卫本身也钉一次：`empty-dispatch-cases` / `empty-dispatch-guard-removed`
        # 两条 meta 负控证明它承重，这里证明它没有被悄悄改成永不触发。
        self.assertEqual(source.count(
            '    if not invocations:\n'
            "        sys.exit('no dispatch cases defined: the probe would pass without '\n"
            "                 'driving the dispatcher')\n"), 1,
            '#1938 the emptied-table guard is present exactly once')
        dropped = ast.get_source_segment(source, pinned[0])
        mutant = source.replace(dropped + ',', '', 1)
        self.assertNotIn(dropped, mutant, '#1938 the mutant row really left the table')
        with self.assertRaises(AssertionError,
                               msg='#1938 dropping a dispatch case must be caught'):
            self.assertEqual([ast.literal_eval(row.elts[1]) for row in rows(mutant)],
                             labels)
        print('#1938 dispatch case row-removed: KILLED', flush=True)
        with self.assertRaises(AssertionError,
                               msg='#1938 dropping one row must change the pinned count'):
            self.assertEqual(len(rows(mutant)), len(pinned))
        print('#1938 dispatch case row-count: KILLED', flush=True)
        # GLM sibling P2（当轮修）：三根桩名同样是 for 消费的字面量表——删掉 `git`
        # 就少掉「取锁之后」那层证据（bare flock 不留 mtime），探针照样打印通过。
        self.assertEqual(source.count(
            "        for stub_name in ('cargo', 'rustc', 'git'):\n"), 1,
            '#1938 the three stubs, git included, are pinned exactly once')
        with self.assertRaises(AssertionError,
                               msg='#1938 dropping the git stub must be caught'):
            self.assertEqual(source.replace(
                "        for stub_name in ('cargo', 'rustc', 'git'):",
                "        for stub_name in ('cargo', 'rustc'):", 1).count(
                    "        for stub_name in ('cargo', 'rustc', 'git'):\n"), 1)
        print('#1938 dispatch stub-name git-removed: KILLED', flush=True)

    def test_bad_value_tables_cannot_shrink_silently(self):
        # #1938 r3（GLM sibling P2）：同一文件里另外两张「for 消费的字面量表」没有
        # 行级钉——`deadline_probe` 的 deadline 坏值表与 `slot_timeout_probe` 的
        # 坏预算表。删掉其中一行（例如 `'１２０'` 或 `float('nan')`），探针少驱动
        # 一格仍打印通过，witness 钉子也照绿（那几个 f-string 还在源里）。
        # 整表清空由 `deadline-parser-loosened` / `slot-timeout-guard-removed`
        # 两条 meta 负控间接兜住；这里补的是「少一行」这一档。
        source = (Path(__file__).resolve().parent/'fixtures/session-assembly-1316/'
                  'negative_controls.py').read_text()

        def table(text, function, target):
            for node in ast.walk(ast.parse(text)):
                if isinstance(node, ast.FunctionDef) and node.name == function:
                    break
            else:
                self.fail(f'#1938 the fixture must define {function}()')
            for inner in ast.walk(node):
                if (isinstance(inner, ast.For) and isinstance(inner.target, ast.Name)
                        and inner.target.id == target):
                    return [ast.get_source_segment(text, element)
                            for element in inner.iter.elts]
            self.fail(f'#1938 {function}() must iterate a literal {target} table')

        deadline = table(source, 'deadline_probe', 'bad')
        self.assertEqual(deadline, [
            "'0'", "'-3'", "'abc'", "'1.5'", "'1_000'", "' 120 '", "'+120'",
            "'１２０'", 'str(MAX_DEADLINE_SECONDS + 1)', "'9' * 400",
        ], '#1938 the ten rejected deadline values are exactly these, not merely ten')
        slots = table(source, 'slot_timeout_probe', 'bad')
        self.assertEqual(slots, ["float('inf')", "float('-inf')", "float('nan')",
                                 '-1', '0'],
                         '#1938 the five rejected slot budgets are exactly these')
        for label, function, target, needle in (
                ('deadline value 1.5-removed', 'deadline_probe', 'bad', ", '1.5'"),
                ('slot budget nan-removed', 'slot_timeout_probe', 'bad',
                 ", float('nan')")):
            mutant = source.replace(needle, '', 1)
            self.assertNotEqual(mutant, source,
                                f'#1938 {label}: the mutant really left the table')
            with self.assertRaises(AssertionError,
                                   msg=f'#1938 {label} must be caught'):
                self.assertEqual(table(mutant, function, target),
                                 deadline if function == 'deadline_probe' else slots)
            print(f'#1938 {label}: KILLED', flush=True)

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
    # r14（zai 6 P1 + deepseek 2 P1）：r13 按「模块级 + 属性名 ∈ 保留名单」枚举，
    # 于是 setattr 调用 / Subscript 目标 / 函数体 / 接收者反向 / 写方法调用 / 别名导入
    # 六种拼写全部漏判。这里每种拼写各配一条存活变异，锚点是仓根常量行（插在它之前，
    # 正文一字不动），逐个跑 `sibling_atomic_guards` / `publish_guards` 必须抛 AssertionError。
    # 共享还原咽喉的那一次发布调用：判据、变异与自证都以它为唯一锚点。
    # r19：带上 `mode=snapshot_modes[path]`（目标被删时也要还原原 mode），
    # 否则 0o644 兜底会把可执行位抹掉、0o600 兜底会留下 mode-only diff。
    SHARED_RESTORE_CALL = ('write_source_atomic(root, path, snapshot[path],\n'
                           '                                            '
                           'mode=snapshot_modes[path])')
    NAMESPACE_TAMPER_MUTANTS = (
        ('setattr-on-builtins',
         'import builtins\n\nsetattr(builtins, "str", lambda value: "../src")\n\n'),
        ('globals-item-assignment',
         'globals()["Path"] = lambda value: value\n\n'),
        ('attribute-write-in-a-function-body',
         'import builtins\n\n\ndef _pin_namespace():\n'
         '    builtins.str = lambda value: "../src"\n\n\n_pin_namespace()\n\n'),
        ('reverse-receiver-attribute-write',
         'os.replace = __import__("shutil").copyfile\n\n'),
        ('module-object-mutator-call',
         'os.__dict__.update(replace=__import__("shutil").copyfile)\n\n'),
        ('guarded-module-imported-under-another-name',
         'import os as _pin_os\n\n'),
        # r16（zai review P2 #1）：r15 点名的 `__builtins__.str = …` 原来只写在
        # docstring 里、零覆盖。`__builtins__` 进了 `GUARDED_NAMESPACES`，这条拼写
        # 现在由静态闭集判红；本条变异保证它不是纸面声明。
        ('builtins-dunder-namespace-write',
         '__builtins__.str = lambda value: "../src"\n\n'),
        ('exec-of-a-literal-string',
         'exec("os.replace = __import__(\'shutil\').copyfile")\n\n'),
    )
    # r16：r15 两家十条 P1 的共同形态 —— 静态闭集**判绿**、运行期照样把
    # `os.replace` 换成 `shutil.copyfile`。这里逐条真跑，证明静态判绿不是放行：
    # 每一条都必须被 `observe_publish` 的运行期观测判红。锚点同样是仓根常量行。
    BEHAVIOURAL_ONLY_TAMPER_FORMS = (
        ('setattr-on-builtins-through-the-module',
         "import builtins\n\nbuiltins.setattr(os, 'replace', "
         "__import__('shutil').copyfile)\n\n"),
        ('builtins-reached-through-sys-modules',
         "def _pin_namespace():\n"
         "    b = sys.modules['builtins']\n"
         "    b.setattr(os, 'replace', __import__('shutil').copyfile)\n\n\n"
         "_pin_namespace()\n\n"),
        ('module-object-mutator-through-a-function-local-alias',
         "def _pin_namespace():\n"
         "    d = os.__dict__\n"
         "    d.update(replace=__import__('shutil').copyfile)\n\n\n"
         "_pin_namespace()\n\n"),
        ('getattr-forwarded-mutator',
         "getattr(os.__dict__, 'update')"
         "(replace=__import__('shutil').copyfile)\n\n"),
        ('guarded-module-imported-through-a-function-local-alias',
         "def _pin_namespace():\n"
         "    m = __import__('os')\n"
         "    m.replace = __import__('shutil').copyfile\n\n\n"
         "_pin_namespace()\n\n"),
    )
    SIBLING_ROOT_ANCHORS = {
        'native-config/negative_controls.py': 'ROOT = Path(__file__).resolve().parents[5]',
        'model-complete/negative-controls.py': 'ROOT = Path(__file__).resolve().parents[5]',
        'native-stimulus-plan/negative_controls.py':
            'root = Path(__file__).resolve().parents[5]',
    }
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
    # r3（zai sibling P1）：先正确派生、之后再重绑 —— 子串/正则钉都只看「出现过」，
    # 重绑后真正落盘的是后者。三处各一条，且刻意避开 `tmp = path.parent` 那条已有钉子
    # （native-config / native-stimulus-plan 重绑到 `src/` 里的其它位置）。
    # r4（zai sibling/boundary P1 + DeepSeek boundary P1）：每种**绑定形态**各一条 ——
    # 只配 `name = ` 一种拼写时，注解赋值 / 元组解包 / 无空格重绑能整段绕过计数钉；
    # model-complete 的健康体本身就是 `fd, tmp = mkstemp(...)`，文本正则对它恒 0 命中。
    SIBLING_REBIND_MUTANTS = {
        'native-config/negative_controls.py': (
            ('temp-rebound-after-the-correct-derivation',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'\n"
             "    tmp = ROOT / 'src' / f'.negctl-{path.name}.tmp'"),
            ('temp-rebound-via-annotation',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'\n"
             "    tmp: Path = path.parent / f'.negctl-{path.name}.tmp'"),
            ('temp-rebound-via-tuple-unpacking',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'\n"
             "    tmp, _sign = path.parent / f'.negctl-{path.name}.tmp', None"),
            # r8（zai sibling P2 + DeepSeek sibling P2）：按 r3 的「三处各一条」惯例，
            # native-config 也要有极性负控 —— 两种形态与 native-stimulus-plan 成对。
            ('published-temp-mentions-the-root-without-deriving',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = path.parent / PUBLISH_DIR.name / f'.negctl-{path.name}.tmp'"),
            # pathlib 的除法语义：右侧是绝对路径时**左侧整条被丢弃**，
            # 临时文件就是被变异源本身 → 原地截断。
            ('temp-base-overridden-by-an-absolute-target',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / path.parent / f'.negctl-{path.name}.tmp'"),
            # r9（zai boundary P1 + DeepSeek boundary P1）：f-string 插值位也能塞进绝对路径。
            ('temp-fstring-interpolates-the-target',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / f'{path}'"),
            # r9（zai sibling P2 + DeepSeek sibling P2）：r3 的「三处各一条」配对补齐。
            ('temp-escapes-the-scratch-dir',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / '..' / 'src' / f'.negctl-{path.name}.tmp'"),
            ('root-constant-rebound-to-another-tree',
             'ROOT = Path(__file__).resolve().parents[5]',
             "ROOT = Path(__file__).resolve().parents[5]\nROOT = Path('/tmp')"),
            # r9（zai boundary P2）：计数仍是 1，只有「派生逐字钉」判得出来。
            ('root-constant-retargeted-one-level-up',
             'ROOT = Path(__file__).resolve().parents[5]',
             'ROOT = Path(__file__).resolve().parents[4]'),
            # r10（zai sibling P1 + DeepSeek sibling P1）：函数体里一次 `Path` 绑定都没有，
            # 重绑放在模块顶层（`ROOT = …` 之前）—— 只扫函数体的钉子全绿，而 `Path(root)`
            # 已经换成自定义可调用对象。
            ('path-callable-shadowed-at-module-level',
             'from pathlib import Path',
             'from pathlib import Path\n\n\ndef Path(value):  # noqa: N802 - 影子函数\n'
             "    return __import__('pathlib').Path('/tmp', value)"),
            # r11（DeepSeek sibling/boundary P1）：把 import 整行**换成**一条赋值，绑定次数
            # 仍是 1 —— 只数次数时全绿；按形态钉才判红。
            ('path-import-replaced-by-a-single-binding',
             'from pathlib import Path',
             "Path = lambda value: __import__('pathlib').Path('/tmp', value)"),
            # r11（DeepSeek sibling P1 #3）：判据读 `os.replace` 的接收者时只读属性名。
            ('os-import-shadowed-by-a-shim',
             'import os',
             'import os\n\n\nclass _OsShim:\n'
             '    def __getattr__(self, name):\n'
             "        return getattr(__import__('os'), name)\n\n\nos = _OsShim()"),
            # r11（zai sibling P2）：native-config 的 f-string 族与 native-stimulus-plan 成对。
            ('temp-fstring-interpolates-a-foreign-attribute',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    class _E:\n        name = '../src'\n"
             "    tmp = PUBLISH_DIR / f'.negctl-{_E.name}.tmp'"),
            ('temp-fstring-uses-a-format-spec',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / f'{path.name:/>100}.tmp'"),
            # r13（zai sibling P1 #3）：仓根常量的右侧只是**文本**钉死的，`__file__` 却是
            # 运行期按名字读的 —— 在它之前插一句赋值，仓根整体搬家、文本一字未变。
            ('root-constant-file-dunder-rebound',
             'ROOT = Path(__file__).resolve().parents[5]',
             "__file__ = '/a/b/c/d/e/f/g.py'\n"
             'ROOT = Path(__file__).resolve().parents[5]'),
            # r13（zai sibling P1 #2）：绑定表只枚举 `Name` 目标，属性赋值目标被丢掉。
            ('str-shadowed-via-a-builtins-attribute',
             'ROOT = Path(__file__).resolve().parents[5]',
             "import builtins\nbuiltins.str = lambda value: '../src'\n"
             'ROOT = Path(__file__).resolve().parents[5]'),
            # r13（zai sibling P1 #1）：AugAssign 只进 `bound_names`、不进
            # `bound_expressions` —— 两张允许集各判各的时，`path /= …` 两边都「合法」。
            ('target-rebound-via-an-augmented-assignment',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    path /= Path('/tmp/evil.py')\n"
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'"),
        ),
        'model-complete/negative-controls.py': (
            ('scratch-rebound-after-the-correct-derivation',
             '    scratch = PUBLISH_DIR if path.is_relative_to(ROOT) else path.parent',
             '    scratch = PUBLISH_DIR if path.is_relative_to(ROOT) else path.parent\n'
             '    scratch = path.parent'),
            # 健康体的 tmp 绑定是解包形态：这条变异证明计数钉真的在看它。
            ('temp-rebound-after-the-mkstemp-unpacking',
             '    fd, tmp = tempfile.mkstemp(dir=str(scratch), prefix=".negctl-" + path.name, '
             'suffix=".tmp")',
             '    fd, tmp = tempfile.mkstemp(dir=str(scratch), prefix=".negctl-" + path.name, '
             'suffix=".tmp")\n'
             "    tmp = ROOT / 'src' / f'.negctl-{path.name}.tmp'"),
            # r7（DeepSeek boundary P1）：只判「表达式里出现过 PUBLISH_DIR」不判极性 ——
            # 两支对调后判据照绿，而仓内目标反而把临时文件写进被扫的 `src/`
            # （`is_relative_to(ROOT)` 为真 ⇔ 目标在仓内）。
            ('scratch-branches-swapped',
             '    scratch = PUBLISH_DIR if path.is_relative_to(ROOT) else path.parent',
             '    scratch = path.parent if path.is_relative_to(ROOT) else PUBLISH_DIR'),
            # r8（DeepSeek boundary P2）：判据读到的仓根名字不许被重绑。
            ('root-constant-rebound-to-another-tree',
             'ROOT = Path(__file__).resolve().parents[5]',
             "ROOT = Path(__file__).resolve().parents[5]\nROOT = Path('/tmp')"),
            # r11（zai sibling P2）：三处夹具各一条模块级 `Path` 影子，与 native-config 成对。
            ('path-callable-shadowed-at-module-level',
             'from pathlib import Path',
             'from pathlib import Path\n\n\ndef Path(value):  # noqa: N802 - 影子函数\n'
             "    return __import__('pathlib').Path('/tmp', value)"),
            # r11（DeepSeek sibling P1 #3）：`tempfile.mkstemp` 的接收者同样可能被顶掉。
            ('tempfile-import-shadowed-by-a-rebind',
             'import tempfile',
             "import tempfile\n\n\ntempfile = __import__('tempfile')"),
            # r11（zai sibling P1 + DeepSeek sibling P2）：prefix 是路径片段，
            # `'../src/'` 让临时文件（与 SIGKILL 残留）落回被扫的 `src/`。
            ('mkstemp-prefix-escapes-the-scratch-dir',
             'prefix=".negctl-" + path.name',
             'prefix="../src/.negctl-" + path.name'),
            # 同族的「接收者不是被发布目标」形态：`scratch.name` 可以是任意目录名。
            ('mkstemp-prefix-interpolates-a-foreign-attribute',
             'prefix=".negctl-" + path.name',
             'prefix=f"{scratch.name}-" + path.name'),
            # r12（zai sibling P1 + zai/deepseek concurrency/boundary P1）：`mkstemp` 的位置序
            # 同样能塞进 prefix；`**{...}` 摊平同理。
            ('mkstemp-prefix-passed-positionally',
             'tempfile.mkstemp(dir=str(scratch), prefix=".negctl-" + path.name, suffix=".tmp")',
             'tempfile.mkstemp(".tmp", "../src/.negctl-" + path.name, dir=str(scratch))'),
            ('mkstemp-arguments-unpacked',
             'tempfile.mkstemp(dir=str(scratch), prefix=".negctl-" + path.name, suffix=".tmp")',
             "tempfile.mkstemp(dir=str(scratch), **{'prefix': '../src/.negctl-' + "
             "path.name, 'suffix': '.tmp'})"),
            # r12（zai sibling P1 #2）：`mkstemp(dir=str(scratch))` 的 `str` 被模块级重绑。
            ('str-shadowed-by-a-module-rebind',
             'ROOT = Path(__file__).resolve().parents[5]',
             "ROOT = Path(__file__).resolve().parents[5]\nstr = lambda value: '../src'"),
            # r13（zai boundary P1 #4）：`tempfile.mkstemp` 的接收者没钉 —— 换成 shim 对象
            # 的同名方法后属性名不变、`dir=` 的派生链却由 shim 决定。
            ('mkstemp-receiver-shimmed',
             '    fd, tmp = tempfile.mkstemp(dir=str(scratch), prefix=".negctl-" + path.name, '
             'suffix=".tmp")',
             '    class _Shim:\n        mkstemp = staticmethod(tempfile.mkstemp)\n'
             '    _t = _Shim()\n'
             '    fd, tmp = _t.mkstemp(dir=str(scratch), prefix=".negctl-" + path.name, '
             'suffix=".tmp")'),
            # r13（zai sibling P1 #3 / #2 / #1）：与 native-config 成对的三条。
            ('root-constant-file-dunder-rebound',
             'ROOT = Path(__file__).resolve().parents[5]',
             "__file__ = '/a/b/c/d/e/f/g.py'\n"
             'ROOT = Path(__file__).resolve().parents[5]'),
            ('str-shadowed-via-a-builtins-attribute',
             'ROOT = Path(__file__).resolve().parents[5]',
             "import builtins\nbuiltins.str = lambda value: '../src'\n"
             'ROOT = Path(__file__).resolve().parents[5]'),
            ('target-rebound-via-an-augmented-assignment',
             '    scratch = PUBLISH_DIR if path.is_relative_to(ROOT) else path.parent',
             "    path /= Path('/tmp/evil.py')\n"
             '    scratch = PUBLISH_DIR if path.is_relative_to(ROOT) else path.parent'),
        ),
        'native-stimulus-plan/negative_controls.py': (
            ('temp-rebound-after-the-correct-derivation',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'\n"
             "    tmp = root / 'src' / f'.negctl-{path.name}.tmp'"),
            ('temp-rebound-without-spaces',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'\n"
             "    tmp=path.parent / f'.negctl-{path.name}.tmp'"),
            # r6（zai sibling P1）：保留正确的派生行当诱饵，另起一个名字直接
            # 指向目标旁边再发布 —— 只钉「出现过 tmp = PUBLISH_DIR / …」时全绿。
            ('published-temp-swapped-to-a-decoy',
             '        os.replace(tmp, path)',
             "        staging = path.parent / f'.negctl-{path.name}.tmp'\n"
             '        os.replace(staging, path)'),
            # r6（DeepSeek boundary P2）：把常量真值改掉、再用一条注释保留旧文本 ——
            # 文本计数与 AST 绑定数都还是 1，只有读 AST 字面量才判得出来。
            ('scratch-constant-retargeted-behind-a-comment',
             "PUBLISH_DIR = root / '.negctl-publish'",
             "# PUBLISH_DIR = root / '.negctl-publish'\n"
             "PUBLISH_DIR = root / 'src' / '.negctl-publish'"),
            # r7（zai sibling P1）：常量仍在表达式里、但已经不是除法的基 ——
            # 「提到过」型判据全绿，临时文件其实落在目标旁边。
            ('published-temp-mentions-the-root-without-deriving',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = path.parent / PUBLISH_DIR.name / f'.negctl-{path.name}.tmp'"),
            # r8（DeepSeek boundary P1）：基还是 PUBLISH_DIR，但右侧用 `..` 把基抵消掉，
            # 落盘回到被扫的 `src/`。
            ('temp-escapes-the-scratch-dir',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / '..' / 'src' / f'.negctl-{path.name}.tmp'"),
            # r8（DeepSeek sibling P2）：镜像形状 —— 右侧是绝对路径，pathlib 会丢弃左侧整条，
            # 临时文件就是被变异源本身。
            ('temp-base-overridden-by-an-absolute-target',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / path.parent / f'.negctl-{path.name}.tmp'"),
            # r9（DeepSeek sibling P1）：插值位里放目录，同样把基抵消掉。
            ('temp-fstring-interpolates-a-directory',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / f'{path.parent}' / f'.negctl-{path.name}.tmp'"),
            # r9（DeepSeek sibling P2）：小写仓根常量的重绑也要有 kill 证明。
            ('root-constant-rebound-to-another-tree',
             'root = Path(__file__).resolve().parents[5]',
             "root = Path(__file__).resolve().parents[5]\nroot = Path('/tmp')"),
            # r10（DeepSeek boundary P1）：只认「属性名 ∈ {name,stem,suffix} 且接收者是
            # 某个名字」时，任意对象的 `.name` 都能是目录 —— 这里 `_E.name == '../src'`，
            # 落盘直接逃出暂存目录。接收者必须正是被 `os.replace()` 发布的目标。
            ('temp-fstring-interpolates-a-foreign-attribute',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    class _E:\n        name = '../src'\n"
             "    tmp = PUBLISH_DIR / f'.negctl-{_E.name}.tmp'"),
            # r10（DeepSeek boundary P1 同一族）：format spec 能把填充符变成前导 `/`，
            # `f'{path.name:/>100}'` 生成的是绝对路径。
            ('temp-fstring-uses-a-format-spec',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    tmp = PUBLISH_DIR / f'{path.name:/>100}.tmp'"),
            # r11（zai sibling P2）：三处夹具各一条模块级 `Path` 影子。
            ('path-callable-shadowed-at-module-level',
             'from pathlib import Path',
             'from pathlib import Path\n\n\ndef Path(value):  # noqa: N802 - 影子函数\n'
             "    return __import__('pathlib').Path('/tmp', value)"),
            # r11（DeepSeek boundary P1）：接收者只比对**名字字符串** —— 目标名先被重绑成
            # 伪 PathLike（`.name == '../src'`），`f'{path.name}'` 于是重新变成目录。
            ('target-name-rebound-to-a-fake-pathlike',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    class _E:\n        name = '../src'\n"
             "    path = _E()\n    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'"),
            # r12（zai concurrency P1 + deepseek sibling/boundary P1）：非赋值形态的绑定
            # （for / with-as / except-as）同样能改掉目标名。
            ('target-name-rebound-via-a-for-loop',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    class _E:\n        name = '../src'\n"
             "    for path in (_E(),):\n        pass\n"
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'"),
            # r13（zai sibling P1 #3 / #2 / #1 + zai boundary P1 #4）：三处夹具成对。
            ('root-constant-file-dunder-rebound',
             'root = Path(__file__).resolve().parents[5]',
             "__file__ = '/a/b/c/d/e/f/g.py'\n"
             'root = Path(__file__).resolve().parents[5]'),
            ('str-shadowed-via-a-builtins-attribute',
             'root = Path(__file__).resolve().parents[5]',
             "import builtins\nbuiltins.str = lambda value: '../src'\n"
             'root = Path(__file__).resolve().parents[5]'),
            ('target-rebound-via-an-augmented-assignment',
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
             "    path /= Path('/tmp/evil.py')\n"
             "    tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'"),
            ('replace-receiver-shimmed',
             '        os.replace(tmp, path)',
             '        class _Shim:\n            replace = staticmethod(os.replace)\n'
             '        _t = _Shim()\n        _t.replace(tmp, path)'),
        ),
    }
    # r5（DeepSeek boundary P1）：r4 的计数钉按 `('tmp', 'scratch')` 查表，且「零次绑定」
    # 被 `if bound:` 当成通过 —— 把临时变量**整体改名**再追加一次重绑，查表恒 0 命中，
    # 钉子全绿而残留落回 `src/`。这里按真实语法定点改名（字符串里的 `".tmp"` 不动），
    # 再在改名后的绑定后面补一句重绑。
    SIBLING_RENAME_MUTANTS = {
        'model-complete/negative-controls.py': (
            ('temp-renamed-and-rebound',
             '    fd, tmp = tempfile.mkstemp(dir=str(scratch), '
             'prefix=".negctl-" + path.name, suffix=".tmp")'),
        ),
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

    @staticmethod
    def calls_on(func, module, attribute):
        """`func` 是不是 `module.attribute` 形态（接收者正是名字 `module`）。

        r13（zai boundary P1 #4）：判据按属性名 `replace` / `mkstemp` 找发布点，却不管
        接收者是谁 —— 函数里补 `_t = _Shim()` 再把调用改成 `_t.mkstemp(...)` /
        `_t.replace(...)`，正文的 `os.fsync` / `os.chmod` 一字未动、全部钉子照绿，而发布
        与暂存目录整体改道。接收者固定为模块级 `os` / `tempfile`（它们的 import 形态由
        `module_import_form_problems` 钉死）。
        """
        return (isinstance(func, ast.Attribute) and func.attr == attribute
                and isinstance(func.value, ast.Name) and func.value.id == module)

    @classmethod
    def published_temp_names(cls, body):
        """`os.replace(<tmp>, <target>)` 里 `<tmp>` 的变量名（AST，不硬编码夹具变量名）。"""
        names = []
        for node in ast.walk(ast.parse(textwrap.dedent(body))):
            if (isinstance(node, ast.Call) and cls.calls_on(node.func, 'os', 'replace')
                    and len(node.args) >= 2
                    and isinstance(node.args[0], ast.Name)):
                names.append(node.args[0].id)
        return names

    @classmethod
    def mkstemp_dir_node(cls, node):
        """`tempfile.mkstemp(dir=str(<scratch>))` → `<scratch>` 的 Name 节点，否则 None。

        r8（DeepSeek simplify P2）：`mkstemp_scratch_names` 与 `roots_at` 曾各写一份同样的
        提取逻辑 —— 两份判据拼写一旦不同步，一边空转、一边假 FAIL。收敛到这一处。
        r13（zai boundary P1 #4）：接收者同样要钉 —— `_t = _Shim()` 之后的
        `_t.mkstemp(dir=str(scratch))` 属性名不变，暂存目录却由 shim 决定。
        """
        if not (isinstance(node, ast.Call)
                and cls.calls_on(node.func, 'tempfile', 'mkstemp')):
            return None
        for keyword in node.keywords:
            value = keyword.value
            if (keyword.arg == 'dir' and isinstance(value, ast.Call)
                    and isinstance(value.func, ast.Name) and value.func.id == 'str'
                    and value.args and isinstance(value.args[0], ast.Name)):
                return value.args[0]
        return None

    @classmethod
    def mkstemp_scratch_names(cls, body):
        """`tempfile.mkstemp(dir=str(<scratch>))` 里 `<scratch>` 的变量名（AST）。"""
        names = []
        for node in ast.walk(ast.parse(textwrap.dedent(body))):
            target = cls.mkstemp_dir_node(node)
            if target is not None:
                names.append(target.id)
        return names

    @staticmethod
    def bound_expressions(body):
        """`body` 里所有 `名字 = 表达式` / `名字: T = 表达式` / 海象 → `[(名字, 表达式节点)]`。

        解包形态按下标对齐取值（`fd, tmp = mkstemp(...)` 里 `tmp` 是第二个元素）。
        """
        pairs = []

        def bind_target(target, value):
            if isinstance(target, ast.Name):
                pairs.append((target.id, value))
            elif isinstance(target, (ast.Tuple, ast.List)):
                paired = (isinstance(value, (ast.Tuple, ast.List))
                          and len(value.elts) == len(target.elts))
                for index, element in enumerate(target.elts):
                    bind_target(element, value.elts[index] if paired else value)
            elif isinstance(target, ast.Starred):
                bind_target(target.value, value)

        for node in ast.walk(ast.parse(textwrap.dedent(body))):
            if isinstance(node, ast.Assign):
                for target in node.targets:
                    bind_target(target, node.value)
            elif isinstance(node, ast.AnnAssign) and node.value is not None:
                bind_target(node.target, node.value)
            elif isinstance(node, ast.NamedExpr):
                bind_target(node.target, node.value)
        return pairs

    @staticmethod
    def names_in(node):
        return {sub.id for sub in ast.walk(node) if isinstance(sub, ast.Name)}

    def binding_source(self, body, name):
        """`name` 唯一那处绑定的右侧表达式源码；不唯一或缺失则返回空串。"""
        values = [value for bound, value in self.bound_expressions(body) if bound == name]
        return ast.unparse(values[0]) if len(values) == 1 else ''

    @staticmethod
    def module_constant(source, name):
        """模块顶层 `name = <字面量>` → `(None, 值)`；`name = <X> / <字面量>` → `(X 源码, 值)`。

        r6（DeepSeek boundary P2）：常量值本身不能只靠「整行文本出现一次」判 ——
        把真行改成 `PUBLISH_DIR = 'src/.negctl-publish'`、再补一条**注释**
        `# PUBLISH_DIR = '.negctl-publish'`，文本计数与 AST 绑定数都还是 1，钉子全绿。
        这里直接读赋值右侧的 AST 字面量，注释骗不过。
        """
        for node in ast.parse(source).body:
            if not (isinstance(node, ast.Assign) and len(node.targets) == 1
                    and isinstance(node.targets[0], ast.Name)
                    and node.targets[0].id == name):
                continue
            value = node.value
            if isinstance(value, ast.Constant):
                return None, value.value
            if isinstance(value, ast.BinOp) and isinstance(value.right, ast.Constant):
                return ast.unparse(value.left), value.right.value
        return None, None

    @classmethod
    def replacement_target_names(cls, body):
        """`os.replace(<tmp>, <target>)` 里的 `<target>` 名字集合（AST）。

        r13：接收者与 `published_temp_names` 同钉 —— 否则 `_t.replace(...)` 会让这条
        判据读到错误的「被发布目标」，而真正的发布点无人检查。
        """
        names = set()
        for node in ast.walk(ast.parse(textwrap.dedent(body))):
            if (isinstance(node, ast.Call) and cls.calls_on(node.func, 'os', 'replace')
                    and len(node.args) >= 2
                    and isinstance(node.args[1], ast.Name)):
                names.add(node.args[1].id)
        return names

    def plain_relative_component(self, node, body):
        """`<基> / <右侧>` 的右侧只能是单个相对名：不许 `..`、不许绝对路径、不许再套除法。

        r8（DeepSeek boundary P1）：只判左侧是 `PUBLISH_DIR` 时，右侧能用 `'..'` 把基
        抵消掉 —— `PUBLISH_DIR / '..' / 'src' / f'…'` 判绿，而落盘正好是被扫的 `src/`。
        r9（zai boundary P1 + DeepSeek boundary/sibling P1）：f-string 的插值位原先无条件
        放行 —— `f'{path}'` 插进来就是绝对路径，pathlib 丢掉整条左侧基，临时文件**就是**
        被变异源本身（`open('wb')` 原地截断）。插值只收「文件名原子」：`X.name/.stem/.suffix`。
        r10（DeepSeek boundary P1）：光看属性名还不够 —— `f'{E.name}'` 里任意对象的
        `.name` 都能是 `'../src'`，`f'{path.name:/>100}'` 的 format spec 也能把填充符变成
        前导 `/`。接收者必须是**被发布目标的那个名字**，且不许带 format spec。
        r11（zai sibling P1 + DeepSeek sibling P1）：同一条「不许变成路径」的规则还要能
        用在 `mkstemp(prefix=…, suffix=…)` 上 —— 那里的健康形态是**字符串拼接**
        `".negctl-" + path.name`，所以也收 `BinOp(Add)`（两侧分别判）与裸的
        `<目标>.name/.stem/.suffix`。
        """
        if isinstance(node, ast.Constant) and isinstance(node.value, str):
            value = node.value
            if not value:
                return True
            return not value.startswith('/') and '..' not in value.split('/')
        if isinstance(node, ast.JoinedStr):
            return all(self.plain_relative_component(part, body) for part in node.values)
        if isinstance(node, ast.FormattedValue):
            return (node.format_spec is None
                    and self.plain_relative_component(node.value, body))
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Add):
            return (self.plain_relative_component(node.left, body)
                    and self.plain_relative_component(node.right, body))
        if isinstance(node, ast.Attribute):
            return (node.attr in ('name', 'stem', 'suffix')
                    and isinstance(node.value, ast.Name)
                    and node.value.id in self.replacement_target_names(body))
        return False

    @staticmethod
    def first_parameter(body):
        """函数体的第一个位置形参名（没有则空串）。"""
        for node in ast.walk(ast.parse(textwrap.dedent(body))):
            if isinstance(node, ast.FunctionDef):
                args = node.args.posonlyargs + node.args.args
                return args[0].arg if args else ''
        return ''

    @staticmethod
    def module_binding_source(source, name):
        """模块顶层 `name = <表达式>` 的右侧源码；不是唯一一条顶层绑定时返回空串。"""
        values = []
        for node in ast.parse(source).body:
            if (isinstance(node, ast.Assign) and len(node.targets) == 1
                    and isinstance(node.targets[0], ast.Name)
                    and node.targets[0].id == name):
                values.append(ast.unparse(node.value))
        return values[0] if len(values) == 1 else ''

    def roots_at(self, body, node, root_constant, seen=None):
        """表达式是否**以** `root_constant` 为基底派生（不是「里面提到过」）。

        r7（zai sibling P1 + DeepSeek sibling/boundary P1）：r6 的链尾判据是
        `root_constant in self.names_in(value)` —— 遍历整棵表达式树，只要常量在**任何
        位置**出现过就算派生。于是 `tmp = path.parent / PUBLISH_DIR.name / f'…'` 全绿，
        而临时文件落在目标旁边；把 `scratch = PUBLISH_DIR if … else path.parent` 的两支
        对调也照样绿（常量仍在树里）。判据必须看**极性**：

        - `PUBLISH_DIR`（名字）        → 是；否则沿它的唯一绑定回溯
        - `<X> / <右侧>`               → 只看左侧（除法的基），右侧必须是单个相对名
          （r8 DeepSeek boundary P1：`PUBLISH_DIR / '..' / 'src' / …` 用 `..` 抵消掉基，
          落盘回到被扫的 `src/`，只判左侧时会假 PASS）
        - `Path(root) / PUBLISH_DIR`   → 是，且 `root` 在函数体里不许再绑
          （r8 zai boundary P1：体里补一句 `root = path.parent` 就让特例整体短路，
          publish 实际落到目标旁边）
        - `mkstemp(dir=str(X))`        → 看 X 的绑定
        - `<A> if path.is_relative_to(ROOT) else path.parent` → 仓内支看 A，仓外支必须
          正是 `path.parent`（跨卷 EXDEV 例外，r3 两家评审）
        - 其它（含 `path.parent / PUBLISH_DIR.name / …`）→ 否
        """
        seen = set() if seen is None else seen
        if isinstance(node, ast.Name):
            if node.id == root_constant:
                return True
            if node.id in seen:
                return False
            seen.add(node.id)
            values = [value for bound, value in self.bound_expressions(body)
                      if bound == node.id]
            return len(values) == 1 and self.roots_at(body, values[0], root_constant, seen)
        if isinstance(node, ast.BinOp) and isinstance(node.op, ast.Div):
            if (ast.unparse(node.left) == 'Path(root)' and isinstance(node.right, ast.Name)
                    and node.right.id == root_constant):
                # r9（zai concurrency P2 + DeepSeek boundary P2）：只数绑定次数分不出
                # 「形参」与「本地重绑」—— 把形参改名再补 `root = path.parent`，计数仍是 1。
                # 要求 `root` 正是第一个形参、且全函数只绑这一次；`Path` 本身也不许被重绑。
                return (self.first_parameter(body) == 'root'
                        and self.body_binding_count(body, 'root') == 1
                        and self.body_binding_count(body, 'Path') == 0)
            if not self.plain_relative_component(node.right, body):
                return False
            return self.roots_at(body, node.left, root_constant, seen)
        if (isinstance(node, ast.Call)
                and self.mkstemp_dir_node(node) is not None):
            return self.roots_at(body, self.mkstemp_dir_node(node), root_constant, seen)
        if isinstance(node, ast.IfExp):
            return (ast.unparse(node.test) == 'path.is_relative_to(ROOT)'
                    and ast.unparse(node.orelse) == 'path.parent'
                    and self.roots_at(body, node.body, root_constant, seen))
        return False

    # 判据按**名字**在运行期读的模块。`module_import_form_problems` 钉「这个名字被绑成
    # 什么形态」，管不到「绕开名字、直接改命名空间」。
    GUARDED_MODULES = ('os', 'sys', 'tempfile', 'pathlib', 'builtins', 'shutil', 'importlib')
    # `__builtins__` 不是一个模块对象（模块里是 dict、`__main__` 里是模块），但判据
    # 运行期同样按名字从它读内建（`str` / `len` / `open`）。r15 的
    # `__builtins__.str = …` 就是从这里改道的，所以它和上面几个模块一样在管辖内；
    # 不做别名追踪（`b = __builtins__; b.str = …` 仍归行为观测兜底）。
    GUARDED_NAMESPACES = GUARDED_MODULES + ('__builtins__',)

    # 健康夹具里**今天真实存在**的全部命名空间写入与写方法调用。判据是**闭集**：
    # 任何别的写入（不论语法怎么拼）都不在白名单里 —— r13/r14 那种「换一种拼写再来
    # 一次」的绕过路径因此不存在，新增写入必须显式改这张表（评审能看到这一行）。
    ALLOWED_NAMESPACE_WRITES = (
        'sys.dont_write_bytecode',   # native-stimulus-plan：禁 __pycache__ 落盘
        'sys.modules[name]',         # published_module 的发布与还原
        'common.__file__',           # 动态 pristine 模块的身份
        'common.guard',              # 动态 pristine 模块的租约包装
        'module.__file__',           # published_module 的身份
        'globals()[_HELD_STATE]',    # 进程内 held-state 缓存（键是私有常量）
    )
    GUARDED_MUTATORS = ('update', 'setdefault', '__setitem__', '__delitem__', 'pop',
                        'popitem', 'clear', 'append', 'extend', 'insert', 'remove',
                        'discard', 'add')
    ALLOWED_MUTATOR_CALLS = (
        'sys.modules.pop',                 # published_module 的清理
        'builtins.__dict__.setdefault',    # held-state 的原子首次登记
        'sys.path.insert',                 # 夹具把自己所在目录挂进 import 路径
    )

    @classmethod
    def module_scope_statements(cls, source):
        """模块级作用域的语句（递归进 if/try/for/while/with，但不进 def/class 体）。"""
        statements = []

        def visit(node):
            if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                return
            statements.append(node)
            for child in ast.iter_child_nodes(node):
                visit(child)

        for node in ast.parse(source).body:
            visit(node)
        return statements

    @classmethod
    def module_scope_names(cls, source):
        """模块级作用域绑定的名字集合（含 import / 赋值 / 解包 / 循环 / with-as）。"""
        names = set()

        def bind(node):
            if isinstance(node, ast.Name):
                names.add(node.id)
            elif isinstance(node, (ast.Tuple, ast.List)):
                for element in node.elts:
                    bind(element)
            elif isinstance(node, ast.Starred):
                bind(node.value)

        for node in cls.module_scope_statements(source):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    names.add(alias.asname or alias.name.split('.')[0])
            elif isinstance(node, ast.ImportFrom):
                for alias in node.names:
                    names.add(alias.asname or alias.name)
            elif isinstance(node, ast.Assign):
                for target in node.targets:
                    bind(target)
            elif isinstance(node, (ast.AnnAssign, ast.AugAssign, ast.NamedExpr)):
                bind(node.target)
            elif isinstance(node, (ast.For, ast.AsyncFor, ast.comprehension)):
                bind(node.target)
            elif isinstance(node, (ast.With, ast.AsyncWith)):
                for item in node.items:
                    if item.optional_vars is not None:
                        bind(item.optional_vars)
            elif isinstance(node, ast.ExceptHandler) and node.name:
                names.add(node.name)
            elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                names.add(node.name)
        return names

    @classmethod
    def module_aliases(cls, source):
        """一跳别名：`o = os` —— 判据按名字读的模块被挂到另一个名字上。"""
        return {target.id for node in ast.walk(ast.parse(source))
                if isinstance(node, ast.Assign) and len(node.targets) == 1
                and isinstance(node.targets[0], ast.Name)
                and isinstance(node.value, ast.Name)
                and node.value.id in cls.GUARDED_MODULES
                for target in node.targets}

    @staticmethod
    def receiver_root(node):
        """属性 / 下标链的根接收者（`a.b[c].d` → `a`）。"""
        while isinstance(node, (ast.Attribute, ast.Subscript)):
            node = node.value
        return node

    @classmethod
    def namespace_write_targets(cls, target):
        """写目标里的属性 / 下标节点：只看最外层，元组解包递归。"""
        if isinstance(target, (ast.Attribute, ast.Subscript)):
            return [target]
        if isinstance(target, (ast.Tuple, ast.List)):
            return [sub for element in target.elts
                    for sub in cls.namespace_write_targets(element)]
        if isinstance(target, ast.Starred):
            return cls.namespace_write_targets(target.value)
        return []

    def namespace_tamper_problems(self, source):
        """静态侧的第二线判据：判据按名字读的模块，常见拼写下不许被写。

        **这不是保证**。r15（zai 6 P1 + deepseek 4 P1）已经证明：只要判据读的是
        源码文本，换个拼写就再绕开一轮（`__builtins__.str = …`、
        `b = sys.modules['builtins']; b.str = …`、`m = __import__('os')` 后
        `m.replace = …`、`getattr(os.__dict__, 'update')(…)`、`builtins.setattr(…)`、
        `_d = os.__dict__; _d.update(…)`）。Python 里这条链是枚举不完的，所以
        「不可能被绕过」的承诺**不在这里**，而在 `observe_publish` 的运行期观测：
        真跑一次发布，看临时文件是不是从仓根暂存目录来、目标在发布后是不是**就是**
        那个临时文件（同一 inode）。任何真改道都会在行为上现形；没有改道就没有危害。
        下面点名的六种拼写，其中五条已在
        `test_shared_restore_write_is_atomic` 里逐条真跑过：静态判据 `[]`，观测必红；
        第六条 `__builtins__.str = …` 归静态侧管（`__builtins__` 在
        `GUARDED_NAMESPACES` 里，变异 `builtins-dunder-namespace-write` 守着）。

        本判据因此只做「挡常见回归、给评审一眼能读的形态」，判红不代表一定有事，
        判绿也不代表一定没事。

        r13 只钉了「模块级、属性名恰是 `Path`/`os`/`str`/`tempfile` 的赋值目标」。
        r14（zai sibling P1 #1/#2 + boundary P1 #1/#2 + concurrency P1 #1/#2，
        deepseek sibling P1 #1/#2）证明这是枚举不完的：换个拼写就能改道而正文一字
        未动 —— `setattr(builtins, 'str', …)`（调用形态）、`globals()['str'] = …`
        / `sys.modules['os'].replace = …`（Subscript 目标，`bind_target` 直接丢）、
        函数体内 `builtins.str = …`（r13 把作用域剪到模块级）、反方向的
        `os.replace = …` / `Path.is_relative_to = …`（接收者是判据读的模块，属性名
        不在名单）、`os.__dict__.update(replace=…)`（写方法调用）、`import os as o`
        （别名）。每一条都让静态钉子全绿而运行期改道。

        今天覆盖的形态（r14 的六类拼写各配一条存活变异）：
          * 属性 / 下标写目标（含 `del`）只有 `ALLOWED_NAMESPACE_WRITES` 里的六条；
            目标是函数内局部对象（`self.x` / `row['k']` / `state['held']`）时放行 ——
            局部对象不是判据读的命名空间。模块级绑定名、判据读的模块、以及它们的
            一跳别名（`o = os`）都在管辖内。
          * `setattr(` / `delattr(` 一律判红（健康夹具里零使用）。
          * `os.__dict__.update(…)` 这类**写方法**调用：接收者根是被监测模块或别名时
            判红，只放行 `ALLOWED_MUTATOR_CALLS` 三条（`sys.modules.pop` /
            `builtins.__dict__.setdefault` / `sys.path.insert`）。
          * 被监测模块的 import：不带头名（`import os as o`）、且只能在模块级
            （函数体内 `import os` 之后 `os.replace = …` 同样改道）。
          * `exec(<字面量>)` / `eval(<字面量>)` 判红 —— 它们是「用字符串改命名空间」。

        已知的静态边界（不是缺陷，是这条路的尽头）：`exec(<动态表达式>)`、跨函数的
        别名链（`b = sys.modules['builtins']` 后再写 `b.str`）、以及借白名单条目的
        运行期重绑（`_HELD_STATE = 'os'` 之后 `globals()[_HELD_STATE] = shim`）——
        这些都由 `observe_publish` 兜底，见
        `test_shared_restore_write_is_atomic` 里那条「静态绿、观测红」的对杀负控。
        """
        tree = ast.parse(source)
        guarded = set(self.GUARDED_NAMESPACES) | self.module_aliases(source)
        module_names = self.module_scope_names(source)
        problems = []
        for node in ast.walk(tree):
            if isinstance(node, ast.Assign):
                targets = node.targets
            elif isinstance(node, (ast.AnnAssign, ast.AugAssign)):
                targets = [node.target]
            elif isinstance(node, ast.Delete):
                targets = node.targets
            else:
                targets = []
            for target in targets:
                for sub in self.namespace_write_targets(target):
                    text = ast.unparse(sub)
                    if text in self.ALLOWED_NAMESPACE_WRITES:
                        continue
                    root = self.receiver_root(sub)
                    local = (isinstance(root, ast.Name)
                             and root.id not in guarded and root.id not in module_names)
                    if not local:
                        problems.append(
                            f'line {node.lineno}: namespace write "{text}" is not in the '
                            f'closed set the guard allows')
            if not isinstance(node, ast.Call):
                continue
            func = node.func
            spelled = ast.unparse(func)
            if (isinstance(func, ast.Name) and func.id in ('exec', 'eval')
                    and node.args and isinstance(node.args[0], (ast.Constant, ast.JoinedStr))):
                problems.append(
                    f'line {node.lineno}: {spelled}(<literal>) rewrites the module '
                    f'namespace: {ast.unparse(node)[:100]}')
            elif isinstance(func, ast.Name) and func.id in ('setattr', 'delattr'):
                problems.append(
                    f'line {node.lineno}: {spelled}(…) rewrites a namespace: '
                    f'{ast.unparse(node)[:100]}')
            elif (isinstance(func, ast.Attribute) and func.attr in self.GUARDED_MUTATORS
                    and spelled not in self.ALLOWED_MUTATOR_CALLS):
                root = self.receiver_root(func)
                if not isinstance(root, ast.Name) or root.id in guarded:
                    problems.append(
                        f'line {node.lineno}: {spelled}(…) mutates a module object the '
                        f'guard reads')
        for node in ast.walk(tree):
            if isinstance(node, ast.Import):
                for alias in node.names:
                    if alias.name.split('.')[0] in self.GUARDED_MODULES and alias.asname:
                        problems.append(
                            f'line {node.lineno}: the guard reads '
                            f'"{alias.name.split(".")[0]}" by name; it must not be '
                            f'imported under another name: {ast.unparse(node)}')
            elif isinstance(node, ast.ImportFrom) and node.module:
                if node.module.split('.')[0] in self.GUARDED_MODULES:
                    for alias in node.names:
                        if alias.asname:
                            problems.append(
                                f'line {node.lineno}: the guard reads these module names '
                                f'by name; alias import: {ast.unparse(node)}')
        problems.extend(self.nested_guarded_import_problems(tree))
        return problems

    def nested_guarded_import_problems(self, tree):
        """函数 / 类体内的 `import os` 同样是改道入口（局部名 shadow 掉模块名）。"""
        problems = []

        def visit(node, nested):
            for child in ast.iter_child_nodes(node):
                inside = nested or isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef,
                                                     ast.ClassDef, ast.Lambda))
                if inside and isinstance(child, ast.Import):
                    for alias in child.names:
                        if alias.name.split('.')[0] in self.GUARDED_MODULES:
                            problems.append(
                                f'line {child.lineno}: a guarded module must be imported '
                                f'at module scope only: {ast.unparse(child)}')
                visit(child, inside)

        visit(tree, False)
        return problems

    def module_import_form_problems(self, source, expected):
        """判据读到的模块名只准由**指定形态**的那一条绑定引入，不是「只数次数」。

        r11（DeepSeek sibling/boundary P1）：r10 只断言 `body_binding_count(source, 'Path') == 1`
        —— 把 `from pathlib import Path` 整行**换成**一条 `Path = lambda value: …`，计数
        仍是 1，断言消息里那个不变量（「唯一那条是 import」）根本没被检查。形态串与
        `fixture_bindings` 同源，`from local_impl import Path` / `import pathlib as Path` /
        `Path = …` 都会落成别的形态。
        r11（DeepSeek sibling P1 #3）：同一动机对 `os` / `tempfile` 也成立 —— 判据只读
        `os.replace` / `tempfile.mkstemp` 的属性名，不读接收者，模块级 `os = _Shim()`
        能让整条发布链改道而正文一字未动。
        r12（zai sibling P1 #2）：`mkstemp(dir=str(scratch))` 里的 `str` 也是判据按名字读的
        —— 模块级 `str = lambda value: '../src'` 让 dir 整体改道。`form` 传 `None` 表示
        「这个模块名一次都不许被绑定」。
        r13（zai sibling P1 #3）：`__file__` 也在此列 —— 仓根常量逐字钉成
        `Path(__file__).resolve().parents[5]`，只钉这段**文本**时，在同一行之前插一句
        `__file__ = '/a/b/c/d/e/f/g.py'` 就把仓根搬到别处，而文本一字未变。它同样是
        「判据按名字在运行期读的东西」，必须钉成「模块里不许有绑定」。
        `namespace_tamper_problems` 补另一半：整个命名空间写入面（r14 起是闭集）。
        """
        problems = []
        for name, form in expected.items():
            got = self.fixture_bindings(source, name)
            if form is None:
                if got:
                    problems.append(f'{name} must not be bound at module scope, got {got}')
            elif got != [form]:
                problems.append(f'{name} must be bound only by "{form}", got {got}')
        return problems

    def mkstemp_name_problems(self, body):
        """`mkstemp(prefix=…, suffix=…)` 的拼法也只准是相对名片段（r11 zai/DeepSeek P1）。

        `dir=` 由 `roots_at` 沿绑定链回溯；prefix/suffix 同样是**路径片段**：CPython 用
        `os.path.join(dir, prefix + … + suffix)` 拼接，prefix 里带 `'/'` 或 `'..'` 时临时
        文件直接逃出暂存目录、落回被扫的 `src/`，SIGKILL 残留就是 untracked build input。
        r12（zai sibling P1 #1 + zai/deepseek concurrency/boundary P1）：只看 `prefix=` /
        `suffix=` 两个关键字还不够 —— `mkstemp(suffix, prefix, dir)` 的位置序、`*args`、
        `**{'prefix': …}` 都能把同一个片段塞进去，而 `dir=` 仍是关键字、派生链照绿。
        健康形态全是纯关键字调用，所以位置实参 / 星号 / 双星一律判红。
        r13（zai boundary P1 #4）：接收者也钉死为 `tempfile` —— `_t = _Shim()` 之后的
        `_t.mkstemp(...)` 属性名不变，暂存目录由 shim 决定（`mkstemp_dir_node` 同钉）。
        """
        problems = []
        for node in ast.walk(ast.parse(textwrap.dedent(body))):
            if not (isinstance(node, ast.Call) and isinstance(node.func, ast.Attribute)
                    and node.func.attr == 'mkstemp'):
                continue
            if not self.calls_on(node.func, 'tempfile', 'mkstemp'):
                problems.append(
                    f'mkstemp(...) must be called on the tempfile module, got: '
                    f'{ast.unparse(node.func)[:120]}')
                continue
            if node.args:
                problems.append('mkstemp(...) must pass suffix/prefix/dir by keyword; '
                                'positional arguments can smuggle a path fragment')
            for keyword in node.keywords:
                if keyword.arg is None:
                    problems.append('mkstemp(**{...}) can smuggle a path fragment')
                    continue
                if keyword.arg not in ('prefix', 'suffix'):
                    continue
                if not self.plain_relative_component(keyword.value, body):
                    problems.append(
                        f'mkstemp({keyword.arg}=…) may only be a relative name fragment, '
                        f'got: {ast.unparse(keyword.value)[:120]}')
        return problems

    def temp_derivation_problems(self, body, root_constant='PUBLISH_DIR'):
        """被 `os.replace()` 发布的那个名字，派生链必须**以**仓根 scratch 为基底。"""
        published = self.published_temp_names(body)
        if len(published) != 1:
            return [f'os.replace() must publish exactly one variable, got {published}']
        values = [value for bound, value in self.bound_expressions(body)
                  if bound == published[0]]
        if len(values) != 1:
            return [f'{published[0]} must be bound exactly once, got {len(values)}']
        if not self.roots_at(body, ast.Name(id=published[0], ctx=ast.Load()), root_constant):
            return [f'{published[0]} must derive FROM {root_constant} (as the base of the '
                    f'chain), got: {ast.unparse(values[0])[:160]}']
        # r11（DeepSeek boundary P1）：`replacement_target_names` 只比对**名字字符串** ——
        # 在 f-string 之前插一句 `path = _E()`（`_E.name == '../src'`）后，被发布的目标名
        # 仍是 `path`、判据照绿，而临时文件落到 `src/`。目标名在函数体里只准出现一次绑定，
        # 且必须是把自身规范化成 `Path` 的那一句（共享实现的 `path = Path(path)`）；
        # 夹具里它本来就是形参，一次绑定都不该有。
        # r12（zai concurrency P1 + deepseek sibling/boundary P1）：枚举不能只看
        # `bound_expressions`（Assign/AnnAssign/海象）—— `for path in …` / `with … as path` /
        # `except … as path` 同样是绑定，`bound_names` 早已把它们立为必须枚举。这里与
        # `body_binding_count` 同源：先按 `bound_names` 数形态，再核对那唯一一次的右值。
        # r13（zai sibling P1 #1 + deepseek sibling/boundary P1）：两张允许集必须**配对**
        # 成立，不能各判各的。`path /= Path('/tmp/evil.py')` 只出现在 `bound_names`
        # （`assign path`），不出现在 `bound_expressions`（那里不枚举 AugAssign）——
        # forms 命中 `['assign path']`、sources 命中 `[]`，两条「∈ 允许集」同时成立，
        # 而目标名在运行期已被改成 `/tmp/evil.py`。健康形态只有两种：一次绑定都没有
        # （目标是裸形参），或唯一一次就是把自身规范化成 `Path(<target>)`。
        problems = []
        for target in sorted(self.replacement_target_names(body)):
            allowed = (([], []), ([f'assign {target}'], [f'Path({target})']))
            forms = [how for bound, how in self.bound_names(textwrap.dedent(body))
                     if bound == target]
            # 形参本身是合法的那一次绑定（夹具的 `path` 就是形参）。
            if forms and forms[0] == f'parameter {target}':
                forms = forms[1:]
            sources = [ast.unparse(value) for bound, value in self.bound_expressions(body)
                       if bound == target]
            if (forms, sources) not in allowed:
                problems.append(
                    f'the published target {target} may only be rebound as '
                    f'"Path({target})", got: {forms} / {sources}')
        return problems

    def sibling_atomic_guards(self, source, function):
        """#1692 §9 r2/r3：同族夹具的发布必须是 tmp + fsync + chmod + 一次 rename。"""
        body = self.sibling_atomic_body(source, function)
        self.assertRegex(source, r"PUBLISH_DIR = (?:ROOT|root) / ['\"]\.negctl-publish['\"]",
                         '#1692 §9 r2 the scratch dir is the ignored root namespace, '
                         'not a directory inside the scanned src area')
        # r6（DeepSeek boundary P2）：上面这条是文本搜索，注释能伪造命中。值从 AST 读。
        base, value = self.module_constant(source, 'PUBLISH_DIR')
        self.assertIn(base, ('ROOT', 'root'),
                      '#1692 §9 r2 the scratch dir is derived from the repository root, '
                      f'got: {base} / {value!r}')
        self.assertEqual(value, '.negctl-publish',
                         '#1692 §9 r2 the scratch dir is the ignored root namespace')
        # r8（DeepSeek boundary P2）：判据读到的仓根名字也要计数 —— 末尾补一句
        # `ROOT = Path('/tmp')` 后 `path.is_relative_to(ROOT)` 文本不变，但仓内目标全被
        # 判成仓外，临时文件落回被变异源旁边。
        self.assertEqual(self.body_binding_count(source, base), 1,
                         f'#1692 §9 r2 the repository-root constant {base} is bound exactly once')
        # r9（zai boundary P2）：只数次数不看值 —— `parents[5]` 改成 `parents[4]` 让
        # PUBLISH_DIR 落进被扫的 `src/`，或整体改 `Path('/tmp')` 让 `is_relative_to` 极性翻转，
        # 计数都还是 1。仓根常量的派生逐字钉死。
        self.assertEqual(self.module_binding_source(source, base),
                         'Path(__file__).resolve().parents[5]',
                         f'#1692 §9 r9 the repository-root constant {base} is the tree root')
        # r10（zai sibling P1 + DeepSeek sibling P1）：r9 的「`Path` 不许被重绑」只扫
        # **函数体**。在 `ROOT = …` 之前补一句模块级 `Path = lambda value: Path('/tmp', value)`
        # （或干脆 `Path = pathlib.Path` 之外的自定义可调用对象），函数体里一次绑定都没有，
        # `body_binding_count(body, 'Path') == 0` 照样成立，而 `Path(root)` 走的是重绑后的
        # 对象 —— 仓根判定与 `PUBLISH_DIR` 派生全被改掉、钉子却全绿。
        # r11（DeepSeek sibling/boundary P1）：只数次数也不行 —— 把 import 整行**换成**
        # 一条 `Path = …`，计数仍是 1。判据读到的模块名一律按**形态**钉
        # （r12 zai sibling P1：`str` 也在此列 —— `mkstemp(dir=str(scratch))` 按名字读它）。
        # r13（zai sibling P1 #3）：仓根常量的右侧逐字钉了 `Path(__file__)…`，但 `__file__`
        # 是**运行期按名字读**的，文本钉子管不到在它之前插一句 `__file__ = '/a/b/…'`。
        # 与 `str` 同办：模块里一次都不许有绑定。
        expected_imports = {'Path': 'from pathlib import Path', 'os': 'import os',
                            'str': None, '__file__': None}
        # r12（deepseek boundary P1）：要不要钉 `tempfile` 不能由正文文本 `'tempfile.mkstemp('`
        # 决定 —— 对手控制这串拼写（`tempfile .mkstemp(` 变体 AST 等价）。改为按**绑定表**
        # 决定：模块里一旦出现 `tempfile` 绑定，就必须是唯一那条 plain import。
        if self.fixture_bindings(source, 'tempfile'):
            expected_imports['tempfile'] = 'import tempfile'
        self.assertEqual(self.module_import_form_problems(source, expected_imports), [],
                         '#1692 §9 r11 the module names the guard reads are bound exactly '
                         'once, by the plain import form')
        # r13（zai sibling P1 #2）：上面按 `Name` 目标数绑定，`builtins.str = …` 这类**属性
        # 目标**不在枚举里：`import builtins` 也不在任何 import-form 名单上，绑定表里看不到
        # 任何 `str`，闸门全绿而 `mkstemp(dir=str(scratch))` 已被换掉。
        # r14（两家六条 P1）：按形态枚举属性目标永远少一种，改成**命名空间写入闭集**
        # （`namespace_tamper_problems`）：setattr / Subscript 目标 / 函数体 / 接收者反向
        # / 写方法 / 别名导入一并判红。
        self.assertEqual(self.namespace_tamper_problems(source), [],
                         '#1692 §9 r14 the namespaces the guard reads are read-only: no '
                         'setattr, no attribute/item write, no module-object mutator call')
        # r11（zai sibling P1 + DeepSeek sibling P1）：mkstemp 的 prefix/suffix 同样是路径
        # 片段，`prefix="../src/" + path.name` 会把临时文件拼到被扫的 `src/`。
        self.assertEqual(self.mkstemp_name_problems(body), [],
                         '#1692 §9 r11 mkstemp prefix/suffix must stay relative name '
                         'fragments of the published target')
        # r3（MiniMax boundary P2）：只判「body 里出现过 PUBLISH_DIR」会被
        # `PUBLISH_DIR.mkdir(...)` 那一行满足——把临时文件挪回目标旁边照样过钉。
        # r6（zai sibling/boundary P1 + DeepSeek boundary P1）：r5 的派生判据还是
        # 「出现过」型正则 + 硬编码 `tmp` 名，且 `for … in mkstemp_scratch_names(body)`
        # 在空列表时整段静默跳过。改成从**被 `os.replace()` 发布的那个名字**出发沿
        # 绑定链回溯到仓根常量：每一跳必须恰好绑一次，链尾必须出现 `PUBLISH_DIR`。
        # 诱饵变量、整体改名、多跳中转都由这一条判。
        self.assertEqual(self.temp_derivation_problems(body), [],
                         f'#1692 §9 r2 {function}() the published temp must DERIVE from the '
                         'root scratch dir, not merely mention it')
        for scratch_variable in self.mkstemp_scratch_names(body):
            # 跨卷例外仍要钉（r3 两家评审）：仓外目标留在同目录，否则 `os.replace` 抛 EXDEV。
            self.assertRegex(
                self.binding_source(body, scratch_variable),
                r'^PUBLISH_DIR if path\.is_relative_to\(ROOT\) else path\.parent$',
                '#1692 §9 r2 the mkstemp scratch dir follows the target only outside '
                'the tree')
        # r3（zai sibling P1）：上面全是「出现过」型钉子 —— 先正确派生、之后再补一句
        # `tmp = ROOT / 'src' / …` 或 `scratch = path.parent`，每条都照绿，而落盘走的是后者，
        # 残留回到被扫的 `src/`。派生变量与常量都只准绑一次。
        # r4（zai sibling/boundary P1 + DeepSeek boundary P1）：计数不能用文本正则 ——
        # 注解赋值 `tmp: Path = …`、无空格 `tmp=…`、元组解包 `fd, tmp = mkstemp(...)`、
        # 海象都不在 `^[ \t]*tmp = ` 的命中集里；model-complete 的健康体正是解包形态，
        # 于是那条钉子在它上面**恒空转**。改用与 `bound_names` 同一套 AST 口径。
        # r5（DeepSeek boundary P1）：连变量名本身也不许硬编码 —— 按 `('tmp', 'scratch')`
        # 查表时「零次绑定」被 `if bound:` 当成通过，把 model-complete 的临时变量整体
        # 改名（`tmp` → `temp`）再追加一次重绑，查表恒 0 命中、钉子全绿而残留落回 `src/`。
        # 改为从 AST 里读出「真正被 `os.replace()` 发布的那个名字」与「`mkstemp(dir=str(…))`
        # 用的那个名字」，再要求它们各只绑一次：名字叫什么、被改成什么都不影响判据。
        published = self.published_temp_names(body)
        self.assertEqual(len(published), 1,
                         '#1692 §9 r2 exactly one variable may be published by os.replace()')
        self.assertEqual(self.body_binding_count(body, published[0]), 1,
                         '#1692 §9 r2 the published temp is derived exactly once: '
                         + published[0])
        for scratch_variable in self.mkstemp_scratch_names(body):
            self.assertEqual(self.body_binding_count(body, scratch_variable), 1,
                             '#1692 §9 r2 the mkstemp scratch dir is bound exactly once: '
                             + scratch_variable)
        self.assertEqual(self.body_binding_count(source, 'PUBLISH_DIR'), 1,
                         '#1692 §9 r2 the scratch constant is bound exactly once')
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

    def observe_publish(self, publish, target, scratch):
        """真跑一次发布，**观测**临时文件从哪个目录来 —— 判行为，不判拼写。

        r15（zai 6 P1 + deepseek 4 P1）：静态判据按语法枚举一轮，评审就能用另一种
        拼写绕开一轮（`__builtins__`、`b = sys.modules['builtins']`、
        `m = __import__('os')`、`getattr(os.__dict__, 'update')(…)`、
        `builtins.setattr(…)`、`_d = os.__dict__` …）。这类「换一种写法改道」在
        Python 里是枚举不完的：只要判据读的是**源码文本**，就总有下一种拼写。

        这里换成判**运行期事实**：装一个 `os.replace` 观察器，真跑一次发布，
        记下 `(临时文件, 目标)`，然后断言临时文件的父目录正是仓根暂存目录。
        不论源码怎么写，只要行为真被改道，观测就会看见；行为没被改道，就没有危害。
        静态判据（`namespace_tamper_problems`）退居第二线：它挡常见回归，**不**承担
        「不可能被绕过」的承诺。
        """
        observed = []
        real_replace = os.replace

        def observed_replace(source, destination, *args, **kwargs):
            # 发布**之前**记下临时文件的身份：真 `os.replace` 会把它的 inode
            # 原样搬到目标上；拷贝则只覆盖目标内容，目标保留自己的 inode。
            item = Path(source).stat()
            observed.append((str(source), str(destination), item.st_dev, item.st_ino))
            return real_replace(source, destination, *args, **kwargs)

        os.replace = observed_replace
        try:
            publish(target, b'#1903 observed publish\n')
        finally:
            os.replace = real_replace
        self.assert_observed_publish(observed, target, scratch)

    # 子进程里跑一次发布并回传观测。源码经 stdin 传入（`__file__` 仍指向真实夹具，
    # 否则夹具按 `parents[5]` 算出来的 ROOT 会跑偏），这样变异体也能原样跑。
    OBSERVE_CHILD = textwrap.dedent(
        r'''
        import json, os, sys
        from pathlib import Path
        module_path, function, target = sys.argv[1:4]
        namespace = {'__file__': module_path,
                     '__name__': 'sibling_atomic_write_probe'}
        exec(compile(sys.stdin.read(), module_path, 'exec'), namespace)
        observed = []
        real_replace = os.replace

        def observed_replace(source, destination, *args, **kwargs):
            item = Path(source).stat()
            observed.append([str(source), str(destination), item.st_dev, item.st_ino])
            return real_replace(source, destination, *args, **kwargs)

        os.replace = observed_replace
        try:
            namespace[function](Path(target), b'#1903 observed publish\n')
        finally:
            os.replace = real_replace
        print('OBSERVED ' + json.dumps(observed))
        ''')

    def observe_publish_in_child(self, relative, function, target, scratch,
                                 source=None):
        """在 `python3 -I` 子进程里真跑一次发布，回传 `(源, 目标, dev, ino)`。

        `native-stimulus-plan/negative_controls.py` 模块级就拒绝非隔离解释器
        （`if not sys.flags.isolated: raise SystemExit`），而 CI 跑的是
        `python3 -m unittest`（不带 `-I`）。进程内 `exec` 会在 CI 上抛 SystemExit、
        把整条用例打成 ERROR（r16 deepseek concurrency P1，已实测：
        非 `-I` 下 `exec` 该文件 → `SystemExit: run with python3 -I`）。
        这条守卫是夹具自己的运行契约，测试不去削弱它，改成按契约用 `-I` 跑。
        """
        path = Path(__file__).resolve().parent / 'fixtures' / relative
        source_text = source if source is not None else path.read_text()
        completed = subprocess.run(
            [sys.executable, '-I', '-c', self.OBSERVE_CHILD,
             str(path), function, str(target)],
            input=source_text, capture_output=True, text=True, timeout=180)
        self.assertEqual(
            completed.returncode, 0,
            f'#1692 §9 r16 {relative} the isolated child must publish: '
            f'{completed.stderr[-800:]}')
        rows = [line[len('OBSERVED '):] for line in completed.stdout.splitlines()
                if line.startswith('OBSERVED ')]
        self.assertEqual(len(rows), 1,
                         f'#1692 §9 r16 {relative} the child must report one '
                         f'observation, got {completed.stdout[-400:]}')
        return [tuple(row) for row in json.loads(rows[0])]

    def assert_observed_publish(self, observed, target, scratch):
        """把一次发布的观测判死：源目录、落点、字节、inode、无残留。"""
        self.assertEqual(len(observed), 1,
                         '#1903 the publish must reach os.replace exactly once, got '
                         f'{observed}')
        source, destination, device, inode = observed[0]
        source = Path(source)
        self.assertEqual(Path(destination), target,
                         '#1903 the publish must land on the target itself')
        self.assertEqual(source.parent, scratch,
                         '#1903 the temp file must come from the repository-root scratch '
                         f'directory, got {source}')
        self.assertEqual(target.read_bytes(), b'#1903 observed publish\n',
                         '#1903 the published bytes must be the observed ones')
        self.assertEqual((target.stat().st_dev, target.stat().st_ino), (device, inode),
                         '#1903 the target must BE the temp file after one rename; a copy '
                         'would overwrite it in place and keep its own inode (not atomic)')
        self.assertEqual(sorted(item.name for item in target.parent.iterdir()),
                         [target.name],
                         '#1903 a successful publish leaves no temp file beside the target')

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
            for label, old, new in self.SIBLING_REBIND_MUTANTS[relative]:
                self.assertIn(old, source, f'#1692 §9 r2 {relative} anchor: {label}')
                with self.assertRaises(AssertionError,
                                       msg=f'#1692 §9 r2 {relative} {label} must be caught'):
                    self.sibling_atomic_guards(source.replace(old, new, 1), function)
                print(f'#1692 §9 r2 sibling-publish mutant {relative} {label}: KILLED',
                      flush=True)
            root_anchor = self.SIBLING_ROOT_ANCHORS[relative]
            for label, snippet in self.NAMESPACE_TAMPER_MUTANTS:
                self.assertEqual(source.count(root_anchor), 1,
                                 f'#1692 §9 r14 {relative} unique anchor: {label}')
                with self.assertRaises(AssertionError,
                                       msg=f'#1692 §9 r14 {relative} {label} must be caught'):
                    self.sibling_atomic_guards(
                        source.replace(root_anchor, snippet + root_anchor, 1), function)
                print(f'#1692 §9 r14 namespace-tamper mutant {relative} {label}: KILLED',
                      flush=True)
            for label, anchor in self.SIBLING_RENAME_MUTANTS.get(relative, ()):
                self.assertIn(anchor, source, f'#1692 §9 r2 {relative} anchor: {label}')
                # 只改语义上的标识符：`".tmp"` 这类字符串字面量里的 `tmp` 保持原样
                # （前面是 `.` 时不算标识符）。
                renamed = re.sub(r'(?<![\w.])tmp(?!\w)', 'temp', source)
                self.assertNotEqual(renamed, source,
                                    f'#1692 §9 r2 {relative} {label}: the rename must bite')
                renamed_anchor = re.sub(r'(?<![\w.])tmp(?!\w)', 'temp', anchor)
                self.assertIn(renamed_anchor, renamed,
                              f'#1692 §9 r2 {relative} {label}: anchor after the rename')
                mutated = renamed.replace(
                    renamed_anchor,
                    renamed_anchor + "\n    temp = path.parent / f'.negctl-{path.name}.tmp'", 1)
                with self.assertRaises(AssertionError,
                                       msg=f'#1692 §9 r2 {relative} {label} must be caught'):
                    self.sibling_atomic_guards(mutated, function)
                print(f'#1692 §9 r2 sibling-publish mutant {relative} {label}: KILLED',
                      flush=True)
            for label, old, new in self.SIBLING_WRITE_MUTANTS[relative]:
                self.assertIn(old, source, f'#1692 §9 r2 {relative} anchor: {label}')
                with self.assertRaises(AssertionError,
                                       msg=f'#1692 §9 r2 {relative} {label} must be caught'):
                    self.sibling_write_guards(source.replace(old, new, 1), relative)
                print(f'#1692 §9 r2 sibling-write mutant {relative} {label}: KILLED',
                      flush=True)
        # r15（两家 10 条 P1）：静态判据对「换一种拼写改道」永远差一步 —— 改判行为。
        # 三处夹具各在 `python3 -I` 子进程里真跑一次发布，观测 `os.replace` 的**源**
        # 是不是仓根暂存目录、目标是不是就是那个临时文件（同一 inode）。
        # 目标名带 pid：三个夹具的临时名是 `.negctl-{目标名}.tmp`，写死会与并发跑
        # 同一用例的另一个进程撞名（r16 deepseek concurrency P2）。
        repository_root = Path(__file__).resolve().parents[3]
        scratch = repository_root / '.negctl-publish'
        for relative, function in self.SIBLING_ATOMIC_WRITES:
            probe_root = Path(tempfile.mkdtemp(prefix='.negctl-probe-',
                                               dir=repository_root))
            try:
                target = probe_root / f'probe-source-{os.getpid()}.rs'
                target.write_bytes(b'#1692 probe\n')
                self.assert_observed_publish(
                    self.observe_publish_in_child(relative, function, target, scratch),
                    target, scratch)
                print(f'#1692 §9 r15 observed publish {relative}: temp from the '
                      f'repository-root scratch dir (live)', flush=True)
            finally:
                shutil.rmtree(probe_root, ignore_errors=True)
        # 行为判据的对杀负控：把 scratch 派生改回目标旁边，观测必须红
        # （静态钉子也红，但这里证明**观测本身**能判）。
        for relative, function, old, new in (
                ('model-complete/negative-controls.py', 'atomic_write',
                 'scratch = PUBLISH_DIR if path.is_relative_to(ROOT) else path.parent',
                 'scratch = path.parent'),
                ('native-config/negative_controls.py', 'write_atomic',
                 "tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
                 "tmp = path.parent / f'.negctl-{path.name}.tmp'"),
                ('native-stimulus-plan/negative_controls.py', 'write_atomic',
                 "tmp = PUBLISH_DIR / f'.negctl-{path.name}.tmp'",
                 "tmp = path.parent / f'.negctl-{path.name}.tmp'")):
            source_text = (root / relative).read_text()
            self.assertIn(old, source_text, f'#1692 §9 r15 {relative} anchor')
            probe_root = Path(tempfile.mkdtemp(prefix='.negctl-probe-',
                                               dir=repository_root))
            try:
                target = probe_root / f'probe-source-{os.getpid()}.rs'
                target.write_bytes(b'#1692 probe\n')
                with self.assertRaises(AssertionError,
                                       msg=f'#1692 §9 r15 {relative} the observation must '
                                           f'see a temp beside the target'):
                    self.assert_observed_publish(
                        self.observe_publish_in_child(
                            relative, function, target, scratch,
                            source_text.replace(old, new, 1)),
                        target, scratch)
                print(f'#1692 §9 r15 observed-publish mutant {relative} '
                      f'temp-beside-the-target: KILLED', flush=True)
            finally:
                shutil.rmtree(probe_root, ignore_errors=True)
        # 暂存目录留在工作树根（`.gitignore:51` 的 `.negctl-*` 盖住）。
        # r19（r18 两家 concurrency P2，同一条）：这里**不删**它 —— `rmdir` 与并发
        # 发布者的 `mkdir` → `open` 之间有 TOCTOU 空窗，删掉正要用它的目录会让对方
        # 的临时文件创建失败、中断真实还原循环。空目录本身无害。
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
                "'dispatch marker: a probe child ran the main path ' '(no refusal within the probe timeout)'",
                "f'dispatch marker: a probe child ran the main path: ' f'{text[-200:]}'",
                "f'dispatch: the probe output directory vanished before it ' f'could be checked: {directory}'",
                "f'dispatch: an argument became an output path: {created}'",
                "f'dispatch: the stub call record vanished before it could ' f'be checked: {calls}'",
                "f'dispatch: the probe started a stubbed cargo, rustc or ' f'git: {stub_calls.strip()[:200]}'",
                "f'dispatch: the probe touched the real tree lock: {lock} ' f'went from {lock_before} to {lock_after}'",
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
                          'dispatch_probe': 10, 'deadline_probe': 15,
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
        self.assertEqual(len(rows), 18,
                         '#1843 D eighteen meta controls cover the self-check guards')
        self.assertEqual(names, [
            'deadline-refusal-removed',
            'progress-flush-removed',
            'tree-lock-not-held',
            'empty-gates-table',
            'empty-gates-guard-removed',
            'empty-dispatch-cases',
            'empty-dispatch-guard-removed',
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
            'dispatch-probe-marker-removed',
        ], '#1843 D the meta controls are exactly these, not merely eighteen rows')
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
    SESSION_CONTROLS_SHA256 = 'bbbdafeb1196b7cefd7b05b3cab0174b4a30faa268811aff5ada425e0982e4e4'
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
_lock_snapshot | return | return path.stat().st_mtime_ns
_lock_snapshot | return | return None
dispatch_probe | sys.exit() | sys.exit('no dispatch cases defined: the probe would pass without ' 'driving the dispatcher')
dispatch_probe | return | return problems
self_check | sys.exit() | sys.exit('no slot guard cases defined: self-check would pass without testing the guard')
self_check | return | return 1
self_check | return | return 0
_empty_gates_table | return | return text, 0
_empty_gates_table | return | return text[:start] + ' gates = {}\n' + text[end:], 1
_empty_invocations_table | return | return text, 0
_empty_invocations_table | return | return text[:start] + ' invocations = ()\n' + text[end:], 1
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
<module> | sys.exit() | sys.exit(f'dispatch probe: refused to run the main path for {mode!r}; the ' f'probe child must fail on a usage guard\n{USAGE}')
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
                # #1903：实现上提到共享守卫后，调用点带上 root，锚点跟着换。
                ('publish-call-commented-out',
                 'write_source_atomic(root, path, original)',
                 '# write_source_atomic(root, path, original)'),
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
        """`write_source_atomic` 的函数体源码（#1692 §9 / #1903）。"""
        for node in ast.walk(ast.parse(source)):
            if isinstance(node, ast.FunctionDef) and node.name == 'write_source_atomic':
                return '\n'.join(source.splitlines()[node.lineno - 1:node.end_lineno])
        self.fail('#1692 §9 the module must define write_source_atomic()')

    @staticmethod
    def bound_names(source):
        """`source` 里所有把名字绑进**本模块命名空间**的语法点 → `[(名字, 形态)]`。

        #1903 r2/r3（DeepSeek boundary P2 + zai boundary P1）：第一版只枚举
        `def` / `class` / 简单赋值 / 导入 / 参数，而 `for X in …`、`with … as X`、
        `except E as X`、海象 `(X := …)`、元组解包 `a, X = …`、`from M import *`、
        `*X` / `**X` 形参、match 捕获都能在**不动两个调用点**的前提下把本地副本绑进来，
        于是「夹具只从 negative_process 导入」这条钉子对它们假 PASS。
        这里把 AST 里**每一个**产生名字绑定的节点都摊平（含解包递归），
        调用方按名字过滤即可 —— 判据是「恰好一条共享导入」，形态漏一个就不成立。
        """
        found = []

        def bind_target(node, how):
            if isinstance(node, ast.Name):
                found.append((node.id, f'{how} {node.id}'))
            elif isinstance(node, (ast.Tuple, ast.List)):
                for element in node.elts:
                    bind_target(element, how)
            elif isinstance(node, ast.Starred):
                bind_target(node.value, how)

        for node in ast.walk(ast.parse(source)):
            if isinstance(node, ast.ImportFrom):
                for alias in node.names:
                    if alias.name == '*':
                        found.append(('*', f'from {node.module} import *'))
                    elif alias.asname:
                        found.append((alias.asname,
                                      f'from {node.module} import {alias.name} as {alias.asname}'))
                    else:
                        found.append((alias.name, f'from {node.module} import {alias.name}'))
            elif isinstance(node, ast.Import):
                for alias in node.names:
                    bound = alias.asname or alias.name.split('.')[0]
                    spelled = f'import {alias.name}' + (f' as {bound}' if alias.asname else '')
                    found.append((bound, spelled))
            elif isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                found.append((node.name, f'def {node.name}'))
            elif isinstance(node, ast.Assign):
                for target in node.targets:
                    bind_target(target, 'assign')
            elif isinstance(node, (ast.AnnAssign, ast.AugAssign, ast.NamedExpr)):
                bind_target(node.target, 'assign')
            elif isinstance(node, (ast.For, ast.AsyncFor, ast.comprehension)):
                bind_target(node.target, 'loop')
            elif isinstance(node, (ast.With, ast.AsyncWith)):
                for item in node.items:
                    if item.optional_vars is not None:
                        bind_target(item.optional_vars, 'with-as')
            elif isinstance(node, ast.ExceptHandler):
                if node.name:
                    found.append((node.name, f'except as {node.name}'))
            elif isinstance(node, ast.arguments):
                parameters = node.posonlyargs + node.args + node.kwonlyargs
                parameters += [argument for argument in (node.vararg, node.kwarg)
                               if argument is not None]
                for argument in parameters:
                    found.append((argument.arg, f'parameter {argument.arg}'))
            elif isinstance(node, (ast.MatchAs, ast.MatchStar)):
                if node.name:
                    found.append((node.name, f'match as {node.name}'))
            elif isinstance(node, ast.MatchMapping):
                if node.rest:
                    found.append((node.rest, f'match **{node.rest}'))
        return found

    def body_binding_count(self, source, name):
        """`source` 里 `name` 的绑定次数，口径与 `bound_names` 完全一致（AST，非文本正则）。

        片段（函数体 / 整个模块）都能吃：先 `dedent` 再 parse。
        r4（zai sibling/boundary P1 + DeepSeek boundary P1）：三处「只准绑一次」的钉子
        原先用 `^[ \\t]*name = ` 文本计数，漏掉注解赋值 / 无空格 / 元组解包 / 海象，
        而 `bound_names` 里同一批形态早已被立为必须枚举 —— 两处口径必须同源，
        否则「加了形态枚举」只保护了一处钉子。
        """
        return len([how for bound, how in self.bound_names(textwrap.dedent(source))
                    if bound == name])

    def fixture_bindings(self, source, name):
        """`source` 里所有把 `name` 绑进本模块命名空间的语法点（#1903 r2/r3）。

        只有**恰好一条** `from negative_process import name` 才允许通过。
        枚举口径见 `bound_names`：形态漏一个，钉子就能被绕过。
        星导入绑进来的名字在静态上**不可知**（`from local_impl import *` 可能顶掉任何符号），
        所以 `*` 按通配处理：出现即算作该名字的一条绑定。
        """
        return [how for bound, how in self.bound_names(source)
                if bound in (name, '*')]

    def assert_restore_reads_the_patched_global(self, guard, label):
        """#1903 r2：见 `restore_wiring_problems`（判据在模块级，两个测试类共用）。"""
        self.assertEqual(restore_wiring_problems(guard), [],
                         f'#1903 {label}: the restore must resolve write_source_atomic '
                         'through the module global the #166 probes patch')

    def publish_guards(self, source, *, scratch, scratch_name):
        """#1692 §9 / #1903：原子发布的三条形态 + 残留的落点。

        ``source`` 是**定义** `write_source_atomic` 的那个模块，``scratch`` 是它
        声明暂存目录的那唯一一行，``scratch_name`` 是那个变量名。#1903 把两个写入点
        （夹具自己的变异/还原、共享守卫 `mutating_sources` 的还原）收成**一份**实现
        ——断言作用域因此从「夹具源码」扩到「被变异源的全部写入点」。
        """
        body = self.write_source_atomic_body(source)
        # r10（zai boundary P1 + DeepSeek sibling P1）：与同族夹具同口径 —— r9 只在函数体里
        # 禁止重绑 `Path`，模块另一处补 `Path = <自定义可调用对象>` 就能让 `Path(root)` 与
        # `Path(path).resolve()` 同时改道，函数体一字未动、全部钉子绿。
        # r11（DeepSeek sibling/boundary P1）：次数不够，形态也要钉死；`os` / `tempfile`
        # 同理（判据只读 `os.replace` / `tempfile.mkstemp` 的属性名，模块级 shim 能整条改道）。
        # r12（zai/DeepSeek）：`str` 与 `tempfile` 的闸门口径同上（按绑定表，不看正文拼写）。
        # r13：`__file__` 同样按名字在运行期被读（判据读它做仓根）；属性 / 下标
        # 写入面另由 `namespace_tamper_problems` 的闭集堵（r14）。
        expected_imports = {'Path': 'from pathlib import Path', 'os': 'import os',
                            'str': None, '__file__': None}
        if self.fixture_bindings(source, 'tempfile'):
            expected_imports['tempfile'] = 'import tempfile'
        self.assertEqual(self.module_import_form_problems(source, expected_imports), [],
                         '#1692 §9 r11 the module names the guard reads are bound exactly '
                         'once, by the plain import form')
        self.assertEqual(self.namespace_tamper_problems(source), [],
                         '#1692 §9 r14 the namespaces the guard reads are read-only: no '
                         'setattr, no attribute/item write, no module-object mutator call')
        self.assertEqual(self.mkstemp_name_problems(body), [],
                         '#1692 §9 r11 mkstemp prefix/suffix must stay relative name '
                         'fragments of the published target')
        # r6（DeepSeek sibling P1）：r5 只在同族夹具那侧把「临时变量名」改成从
        # `os.replace()` 的 AST 实参读，共享实现这里仍把字面量 `temporary` 当判据 ——
        # 换个变量名再重绑就绕开了。这里同样从 AST 取真正被发布的名字。
        self.assertEqual(self.temp_derivation_problems(body), [],
                         '#1692 §9 the published temp must derive from the root scratch dir')
        published = self.published_temp_names(body)[0]
        self.assertIn("open('wb')", body,
                      '#1692 §9 the publish writes the bytes to a temp file first')
        self.assertIn('os.fsync(stream.fileno())', body,
                      '#1692 §9 the bytes are flushed to disk before the rename')
        self.assertIn(f'os.replace({published}, path)', body,
                      '#1692 §9 the publish is ONE rename, never a truncating write')
        self.assertLess(body.index('os.fsync(stream.fileno())'), body.index('os.replace('),
                        '#1692 §9 the rename happens only after the fsync')
        self.assertIn(f'{published}.unlink(missing_ok=True)', body,
                      '#1692 §9 a failed publish leaves no temp behind')
        # 权限位也是钉子（r1 评审 GLM boundary P3）：`os.replace` 发布的是**临时文件**的
        # inode 与它自己的 mode（0666 & ~umask），源文件的 mode 会被丢掉。`core.filemode=true`
        # 下换一个 umask 跑一次夹具就会在 `src/…` 上留下只有 mode 差异的 `git diff`，下一次
        # preflight 同样整棵拒跑。先 chmod 再 rename，且 mode 取自源文件。
        self.assertIn('mode = path.stat().st_mode & 0o7777', body,
                      '#1692 §9 the published mode is taken from the source file')
        self.assertIn(f'os.chmod({published}, mode)', body,
                      '#1692 §9 the temp gets the source mode before the rename')
        self.assertLess(body.index(f'os.chmod({published}, mode)'), body.index('os.replace('),
                        '#1692 §9 the chmod happens before the publish, not after it')
        # 落点也是钉子（#1843 r1 MiniMax boundary P1）：SIGKILL 不执行 `finally`，
        # 所以「临时文件总会被删」恰好在唯一要紧的那条路径上是假的。放在被变异源旁边
        # 就是放进 `src/`，而 `negative_process._assert_input_areas` 用
        # `git ls-files --others`（故意包含被忽略的文件）扫 `src` / `.cargo` /
        # `rust-toolchain*`，下一次运行会以「untracked build input」拒跑——正是 §9 要
        # 消灭的那种锁死。**保护来自位置**：仓库根不在扫描区内；`.gitignore` 的
        # `.negctl-*` 只让 `git status` 保持干净，不是保护本身（r4 MiniMax sibling P2：
        # 这里的旧说法与本 PR 刚改对的 `native-config/README.md` 相反）。仍在同一个
        # 文件系统上，`os.replace` 依旧是一次原子 rename。
        self.assertEqual(source.count(scratch), 1,
                         '#1692 §9 the publish scratch dir is one root-level constant: '
                         + scratch)
        # 派生链也要钉（r1 DeepSeek concurrency P2）：上提后旧的
        # `assertEqual(source.count("PUBLISH_DIR = root / '.negctl-publish'"), 1)` 没了，
        # 只剩「常量行唯一」，把 `publish = Path(root) / PUBLISH_DIR` 改成
        # `path.parent / PUBLISH_DIR` 能存活全部钉子，于是临时文件落回 `src/`，
        # SIGKILL 残留被 `git ls-files --others` 读到 → 整棵拒跑，而尺子报 PASS。
        # r5（zai sibling P1 + DeepSeek sibling P1）：这条钉子在 r4 里被漏掉了 ——
        # `source.count(scratch)`（scratch 是整行 `PUBLISH_DIR = '.negctl-publish'`）
        # 只认**逐字照抄**的那一行：在模块末尾追加 `PUBLISH_DIR = 'src/.negctl-publish'`
        # 或 `PUBLISH_DIR: str = …`，字面量行仍只出现一次，钉子全绿，而运行期
        # `publish = Path(root) / PUBLISH_DIR` 取最后绑定 → 临时目录落进被扫的 `src/`。
        # 与 `sibling_atomic_guards` 同口径：按名字数绑定，不按行文本数。
        scratch_constant = scratch.split('=', 1)[0].strip()
        self.assertEqual(self.body_binding_count(source, scratch_constant), 1,
                         '#1692 §9 the publish scratch constant is bound exactly once: '
                         + scratch_constant)
        # r6（DeepSeek boundary P2）：绑定次数对了，值还得对 —— 注释能伪造文本匹配。
        base, value = self.module_constant(source, scratch_constant)
        self.assertIsNone(base,
                          '#1692 §9 the publish scratch constant is a plain literal at the '
                          f'module top level, got: {base} / {value!r}')
        self.assertEqual(value, '.negctl-publish',
                         '#1692 §9 the publish scratch dir is the ignored root namespace')
        self.assertIn(f'{scratch_name} = Path(root) / {scratch_constant}', body,
                      '#1692 §9 the scratch dir is derived from the repo ROOT, not from the '
                      'mutated file: a residue beside a `src/` file is an untracked build '
                      'input and locks the tree out')
        # 子串存在性挡不住「先正确赋值、之后再重绑」的漂移（r2 DeepSeek boundary P2 /
        # concurrency P3）：在被钉行之后补一句 `publish = path.parent / PUBLISH_DIR`，
        # 上面三条仍然全绿，而落盘走的是后者。整个函数体里 `publish =` 只准出现一次。
        self.assertEqual(self.body_binding_count(body, scratch_name), 1,
                         f'#1692 §9 the scratch dir is bound exactly once: {scratch_name}')
        # 同一类漂移对**临时路径**照样成立（r3 DeepSeek sibling/boundary P2）：
        # `temporary` 才是 `open()` / `os.replace()` 真正用的变量，只钉 `publish`
        # 时在它后面补一句 `temporary = path.parent / …` 仍全绿。
        # r4（zai boundary P2 / DeepSeek boundary P1）：计数走 AST，注解赋值、
        # 元组解包、无空格拼写都在内。r6：名字本身也从 AST 取（见上方
        # `temp_derivation_problems`），不再写死 `temporary`。
        self.assertEqual(self.body_binding_count(body, published), 1,
                         f'#1692 §9 the temp path is bound exactly once: {published}')
        self.assertIn(f"{published} = {scratch_name} / f'.negctl-{{path.name}}.tmp'", body,
                      '#1692 §9 the temp name lives in the ignored .negctl-* namespace')
        self.assertIn(f'{scratch_name}.mkdir(parents=True, exist_ok=True)', body,
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

    def fixture_call_sites(self, source):
        """#1903：夹具侧只剩两个调用点，都不许落回截断写，也不许长出本地实现。"""
        # #1903 把实现上提到共享守卫后，`publish_guards` 只读共享那份（r1 DeepSeek
        # boundary P2）：夹具若再长出一份本地 `write_source_atomic`，两个调用点名字不变、
        # 截断写正则也不匹配 `open(path, 'wb')`，两条断言照绿，而真正跑的是本地副本。
        # 「夹具与 `mutating_sources` 的还原共用同一份实现」必须由断言钉住 —— 而且只数
        # `def` + 一条赋值正则不够（r2 DeepSeek boundary P2：`from local_impl import
        # write_source_atomic`、`import local_impl as write_source_atomic`、缩进变体都逃得掉）。
        # 这里改成 AST 枚举**所有**能把名字绑进本模块命名空间的语法点，恰好只允许一条共享导入。
        # 范围说明（r3 DeepSeek boundary P2 #2）：这条钉子只管**绑定形态**，不管调用点的
        # 接收者写法 —— 两处调用点用的是 `write_source_atomic(...)` 子串计数，改成
        # `negative_process.write_source_atomic(...)` 会同时让计数钉与这里变红，不会静默放过；
        # 真放过的只有「另一条同名绑定 + 调用点仍写非限定名」，那正被这里挡掉。
        for name in ('write_source_atomic', 'mutating_sources'):
            self.assertEqual(self.fixture_bindings(source, name),
                             [f'from negative_process import {name}'],
                             f'#1903 the fixture must bind {name} only from negative_process')
        # r11（zai sibling P2 #4）：调用方自己也算 `root = Path(__file__).resolve().parents[5]`
        # 后把 root 交给共享实现 —— 模块级 `Path` 影子能把这个 root 指到别处，而共享那份的
        # 钉子只看 `negative_process.py`。同一形态钉在调用方这边也补一条。
        self.assertEqual(self.module_import_form_problems(
            source, {'Path': 'from pathlib import Path', 'os': 'import os',
                     'tempfile': 'import tempfile', 'str': None, '__file__': None}), [],
            '#1903 r11 the call-site fixture binds the module names by the plain import form')
        self.assertEqual(self.namespace_tamper_problems(source), [],
                         '#1903 r14 the namespaces the guard reads are read-only')
        self.assertEqual(source.count('write_source_atomic(root, path, changed.encode())'), 1,
                         '#1692 §9 the mutant is published atomically')
        self.assertEqual(source.count('write_source_atomic(root, path, original)'), 1,
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
        # #1903：实现已上提到共享守卫，夹具与 `mutating_sources` 的还原共用同一份。
        #
        # 覆盖边界（r18，zai sibling P1 的裁决）：本判据钉的是**共享还原咽喉**
        # （`mutating_sources` 的出口，所有走它的夹具共用）与三处自带
        # `write_atomic`/`atomic_write` 的 native 夹具。各夹具**循环内**的
        # `path.write_text(mutant)` / `path.write_bytes(snapshot)` 仍是原地写
        # —— 那是 41 个夹具文件（连同各自的负控锚点与 SHA 钉）的逐处改造，
        # 已由 #1908 第 1 条跟踪，前置于下一轮负控夹具改造。所以这里成立的
        # 说法是「共享咽喉与三处 native 夹具」，**不是**「全部还原已原子化」。
        base = Path(__file__).resolve().parent/'fixtures'
        fixture = (base/'session-assembly-1316'/'negative_controls.py').read_text()
        shared = (base/'negative_process.py').read_text()
        self.fixture_call_sites(fixture)
        self.publish_guards(shared, scratch="PUBLISH_DIR = '.negctl-publish'",
                            scratch_name='publish')
        # #1903：共享还原点是**唯一**的还原咽喉，它自己也不许有截断写。
        self.assertEqual(shared.count(self.SHARED_RESTORE_CALL), 1,
                         '#1903 the shared restore publishes atomically')
        self.assertEqual(re.findall(r'(?<![\w.])path\.write_(?:text|bytes)\(', shared), [],
                         '#1903 the shared restore has no truncating in-place write')
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
                # 残留落回被变异源旁边 = 下一次 preflight 拒跑。
                ('atomic-publish-leaves-residue-in-src',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    temporary = path.with_name(f'.negctl-{path.name}.tmp')"),
                # 有 scratch 目录但名字不在 `.gitignore` 覆盖范围内。
                ('atomic-publish-scratch-dir-not-ignored',
                 "PUBLISH_DIR = '.negctl-publish'", "PUBLISH_DIR = 'negctl-publish'"),
                # 派生点漂移（r1 DeepSeek concurrency P2）：scratch 跟着目标文件走，
                # 残留就落回 `src/` 里 —— 保护来自位置，位置错了保护就没了。
                ('atomic-publish-scratch-follows-the-target',
                 '    publish = Path(root) / PUBLISH_DIR',
                 '    publish = path.parent / PUBLISH_DIR'),
                # 先正确赋值、之后再重绑（r2 DeepSeek boundary P2）：子串钉子全绿，
                # 实际落盘跟着目标文件走 → 残留回到 `src/`。
                ('atomic-publish-scratch-rebound-after-the-fact',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 '    publish = path.parent / PUBLISH_DIR\n'
                 "    temporary = publish / f'.negctl-{path.name}.tmp'"),
                # 临时路径自己重绑（r3 DeepSeek sibling/boundary P2）：`publish` 仍正确，
                # 落盘的 tmp 却在目标旁边。r4 起补注解赋值与元组解包两种拼写 ——
                # 计数钉走 AST，形态漏一个就等于没钉。
                ('atomic-publish-temp-rebound-after-the-fact',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    temporary = path.parent / f'.negctl-{path.name}.tmp'"),
                ('atomic-publish-temp-rebound-via-annotation',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    temporary = publish / f'.negctl-{path.name}.tmp'\n"
                 "    temporary: Path = path.parent / f'.negctl-{path.name}.tmp'"),
                ('atomic-publish-temp-rebound-via-tuple-unpacking',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    temporary = publish / f'.negctl-{path.name}.tmp'\n"
                 "    temporary, _sign = path.parent / f'.negctl-{path.name}.tmp', None"),
                ('atomic-publish-temp-rebound-via-walrus',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    temporary = publish / f'.negctl-{path.name}.tmp'\n"
                 "    _ignored = (temporary := path.parent / f'.negctl-{path.name}.tmp')"),
                ('atomic-publish-temp-rebound-via-augmented-assignment',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    temporary = publish / f'.negctl-{path.name}.tmp'\n"
                 "    temporary += path.parent / f'.negctl-{path.name}.tmp'"),
                # r5（zai sibling P1 + DeepSeek sibling P1）：常量本身也要按名计绑定 ——
                # 追加第二条不同拼写的 `PUBLISH_DIR`，逐字行仍只出现一次。
                ('atomic-publish-scratch-constant-rebound',
                 "PUBLISH_DIR = '.negctl-publish'",
                 "PUBLISH_DIR = '.negctl-publish'\nPUBLISH_DIR = 'src/.negctl-publish'"),
                ('atomic-publish-scratch-constant-annotated-rebound',
                 "PUBLISH_DIR = '.negctl-publish'",
                 "PUBLISH_DIR = '.negctl-publish'\n"
                 "PUBLISH_DIR: str = 'src/.negctl-publish'"),
                # r6（DeepSeek sibling P1）：共享实现里「被发布的是哪个名字」也走 AST ——
                # 保留 `temporary` 的正确派生当诱饵，另起 `decoy` 指向目标旁边再发布。
                ('atomic-publish-temp-swapped-to-a-decoy',
                 '        os.replace(temporary, path)',
                 "        decoy = path.parent / f'.negctl-{path.name}.tmp'\n"
                 '        os.replace(decoy, path)'),
                # r6（DeepSeek boundary P2）：常量真值改掉 + 注释保留旧文本。
                ('atomic-publish-scratch-constant-retargeted-behind-a-comment',
                 "PUBLISH_DIR = '.negctl-publish'",
                 "# PUBLISH_DIR = '.negctl-publish'\n"
                 "PUBLISH_DIR = 'src/.negctl-publish'"),
                # r8（zai boundary P1）：`Path(root) / PUBLISH_DIR` 这个特例只匹配文本时，
                # 体里补一句 `root = path.parent` 就让派生判据整体短路 —— 字面钉、绑定计数
                # 全照绿，而 publish 实际落在目标旁边。
                ('atomic-publish-root-rebound-before-the-derivation',
                 '    publish = Path(root) / PUBLISH_DIR',
                 '    root = path.parent\n    publish = Path(root) / PUBLISH_DIR'),
                # r9（DeepSeek boundary P1）：共享实现的插值位同样能塞进绝对路径。
                ('atomic-publish-temp-fstring-interpolates-the-target',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    temporary = publish / f'{path}'"),
                # r9（zai sibling P2 + DeepSeek sibling P2）：核心 body 上的 `..` 逃逸与
                # 绝对右操作数也要各有 kill 证明（`roots_at` 是同一份判据，但接线要证）。
                ('atomic-publish-temp-escapes-the-scratch-dir',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    temporary = publish / '..' / 'src' / f'.negctl-{path.name}.tmp'"),
                ('atomic-publish-temp-base-overridden-by-an-absolute-target',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    temporary = publish / path.parent / f'.negctl-{path.name}.tmp'"),
                # r9（zai concurrency P2 + DeepSeek boundary P2）：形参改名 + 本地重绑，
                # 只数 `root` 的绑定次数时仍是 1。
                ('atomic-publish-root-parameter-renamed-and-rebound',
                 'def write_source_atomic(root, path, data, *, mode=None):\n',
                 'def write_source_atomic(tree_root, path, data, *, mode=None):\n'
                 '    root = path.parent\n'),
                # r9（DeepSeek sibling P2）：判据里的 `Path` 本身也不许被重绑。
                ('atomic-publish-path-callable-rebound',
                 '    publish = Path(root) / PUBLISH_DIR',
                 '    Path = lambda value: path.parent\n    publish = Path(root) / PUBLISH_DIR'),
                # r10（zai boundary P1 + DeepSeek sibling P1）：r9 只扫函数体，模块顶层另起
                # 一条 `Path` 绑定就让 `Path(root)` / `Path(path).resolve()` 同时改道 ——
                # `write_source_atomic` 一字未动。
                ('atomic-publish-path-callable-shadowed-at-module-level',
                 'from pathlib import Path',
                 'from pathlib import Path\n\n\ndef Path(value):  # noqa: N802 - 影子函数\n'
                 "    return __import__('pathlib').Path('/tmp', value)"),
                # r11（DeepSeek sibling/boundary P1）：import 整行**换成**一条赋值，计数仍是 1。
                ('atomic-publish-path-import-replaced-by-a-single-binding',
                 'from pathlib import Path',
                 "Path = lambda value: __import__('pathlib').Path('/tmp', value)"),
                # r11（DeepSeek sibling P1 #3）：`os.replace` 的接收者没有钉，模块级 shim
                # 可以让整条发布链改道而正文一字未动。
                ('atomic-publish-os-import-shadowed-by-a-shim',
                 'import os',
                 'import os\n\n\nclass _OsShim:\n'
                 '    def __getattr__(self, name):\n'
                 "        return getattr(__import__('os'), name)\n\n\nos = _OsShim()"),
                # r11（DeepSeek boundary P1）：目标名被重绑成伪 PathLike。
                ('atomic-publish-target-name-rebound-to-a-fake-pathlike',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    path = type('E', (), {'name': '../src'})()\n"
                 "    temporary = publish / f'.negctl-{path.name}.tmp'"),
                # r10（DeepSeek boundary P1）：任意对象的 `.name` 都能是目录名。
                ('atomic-publish-temp-fstring-interpolates-a-foreign-attribute',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    _E = type('E', (), {'name': '../src'})\n"
                 "    temporary = publish / f'.negctl-{_E.name}.tmp'"),
                # r10（DeepSeek boundary P1 同一族）：format spec 的填充符能拼出绝对路径。
                ('atomic-publish-temp-fstring-uses-a-format-spec',
                 "    temporary = publish / f'.negctl-{path.name}.tmp'",
                 "    temporary = publish / f'.negctl-{path.name:/>100}.tmp'"),
                # #1903 的核心回归：共享还原落回截断写。
                ('shared-restore-write-back-to-truncating',
                 self.SHARED_RESTORE_CALL,
                 'path.write_bytes(snapshot[path])'),
                # r13（zai boundary P1 #4）：`os.replace` 的接收者同样没钉 —— 换一个 shim
                # 对象的同名方法，正文的 `os.fsync` / `os.chmod` 一字未动，发布点却改道。
                ('atomic-publish-replace-receiver-shimmed',
                 '        os.replace(temporary, path)',
                 '        class _Shim:\n            replace = staticmethod(os.replace)\n'
                 '        _t = _Shim()\n        _t.replace(temporary, path)'),
                # r13（zai sibling P1 #1 + deepseek P1）：AugAssign 只在 `bound_names` 里 ——
                # 两张允许集各判各的时，`path /= …` 同时落进两边的「合法」集。
                ('atomic-publish-target-rebound-via-an-augmented-assignment',
                 '    publish = Path(root) / PUBLISH_DIR',
                 "    path /= Path('/tmp/evil.py')\n    publish = Path(root) / PUBLISH_DIR"),
                # r13（zai sibling P1 #2）：属性赋值目标不在绑定表里，`builtins.str = …`
                # 让 `str(root)` 这类按名读取整体改道，而 `str` 的绑定表仍是空的。
                ('atomic-publish-str-shadowed-via-a-builtins-attribute',
                 'import os',
                 "import builtins\nimport os\n\nbuiltins.str = lambda value: '../src'"),
                # r13（zai sibling P1 #3）：仓根不是从 `__file__` 派生的（root 是形参），
                # 但同一条「运行期按名字读的东西不许被绑」在这里也要有 kill 证明 ——
                # 判据读的是 `Path` 与 `os`，属性目标侧由上面那条覆盖。
                ('atomic-publish-path-shadowed-via-a-builtins-attribute',
                 'from pathlib import Path',
                 "import builtins\nfrom pathlib import Path\n\n"
                 'builtins.Path = lambda value: '
                 "__import__('pathlib').Path('/tmp', value)"),
                # r14（zai sibling P1 #1 / boundary P1 #2 + deepseek sibling P1 #1/#2）：
                # 共享实现这一侧同样要覆盖「调用形态 / 下标目标 / 接收者反向」三种拼写，
                # 证明闭集判据不是只在兄弟夹具那一侧接线。
                ('atomic-publish-setattr-on-builtins',
                 'import os',
                 'import builtins\nimport os\n\n'
                 'setattr(builtins, "str", lambda value: "../src")'),
                ('atomic-publish-globals-item-assignment',
                 "PUBLISH_DIR = '.negctl-publish'",
                 'globals()["Path"] = lambda value: value\n'
                 "PUBLISH_DIR = '.negctl-publish'"),
                ('atomic-publish-reverse-receiver-attribute-write',
                 'import os',
                 'import os\n\nos.replace = __import__("shutil").copyfile'),
                ('atomic-publish-module-object-mutator-call',
                 'import os',
                 'import os\n\nos.__dict__.update(replace=__import__("shutil").copyfile)')):
            self.assertEqual(shared.count(old), 1, f'#1692 §9 unique anchor: {label}')
            mutant = shared.replace(old, new, 1)
            with self.assertRaises(AssertionError, msg=f'#1692 §9 {label} must be caught'):
                self.publish_guards(mutant, scratch="PUBLISH_DIR = '.negctl-publish'",
                                    scratch_name='publish')
                self.assertEqual(
                    mutant.count(self.SHARED_RESTORE_CALL), 1,
                    '#1903 the shared restore publishes atomically')
                self.assertEqual(re.findall(r'(?<![\w.])path\.write_(?:text|bytes)\(', mutant), [],
                                 '#1903 the shared restore has no truncating in-place write')
            print(f'#1692 §9 atomic-publish mutant {label}: KILLED', flush=True)
        # 夹具命名空间的钉子也要有对杀负控（r2 DeepSeek sibling P2 / boundary P2）：
        # 一条变异只触发一条钉子，否则先抛的那条会把后面的判定逻辑遮成零覆盖。
        anchor_after_import = ("root = Path(__file__).resolve().parents[5]\n"
                               "source = root / 'src/pilotdeck-gateway/src'")
        for label, old, new in (
                # 1) 导入模块名写错 → 「只允许从 negative_process 导入」这条钉子必须红。
                ('fixture-imports-another-module',
                 'from negative_process import ', 'from negative_process_local import '),
                # 2) 追加一条同名别名导入 → 绑定表出现第二条 → 必须红。
                ('fixture-imports-an-alias-shadow',
                 anchor_after_import,
                 'from local_impl import write_source_atomic\n\n' + anchor_after_import),
                # 3) 真 shadow：`def` 放在 import **之后**，运行期用的就是本地副本。
                ('fixture-defines-a-local-write-source-atomic',
                 anchor_after_import,
                 'def write_source_atomic(root, path, data):\n'
                 "    Path(path).write_bytes(data)\n\n\n" + anchor_after_import),
                # 4) 普通重绑定。
                ('fixture-rebinds-write-source-atomic',
                 anchor_after_import, 'write_source_atomic = None\n\n' + anchor_after_import),
                # 5) 同一夹具还能把共享入口本身顶掉（r2 DeepSeek sibling P2 #4）。
                ('fixture-shadows-mutating-sources',
                 anchor_after_import,
                 'def mutating_sources(root, paths, **kwargs):\n'
                 "    raise AssertionError('local shadow')\n\n\n" + anchor_after_import),
                # 6-12）不经过 `def` / 赋值 / import 的其余绑定形态（r3 zai boundary P1）：
                # 第一版枚举漏了它们，两个调用点原样不动、截断正则也不匹配本地副本，
                # 钉子会全绿。每个形态一条，确保 `bound_names` 的每一支都有对杀负控。
                ('fixture-binds-via-for-target',
                 anchor_after_import, 'for write_source_atomic in (_local,):\n    pass\n\n'
                 + anchor_after_import),
                ('fixture-binds-via-with-alias',
                 anchor_after_import, 'with open(__file__) as write_source_atomic:\n    pass\n\n'
                 + anchor_after_import),
                ('fixture-binds-via-except-alias',
                 anchor_after_import, 'try:\n    pass\nexcept Exception as write_source_atomic:\n'
                 '    pass\n\n' + anchor_after_import),
                ('fixture-binds-via-walrus',
                 anchor_after_import, '(write_source_atomic := None)\n\n' + anchor_after_import),
                ('fixture-binds-via-tuple-unpacking',
                 anchor_after_import, '_, write_source_atomic = None, None\n\n'
                 + anchor_after_import),
                ('fixture-shadowed-by-star-import',
                 anchor_after_import, 'from local_impl import *\n\n' + anchor_after_import),
                ('fixture-binds-via-vararg',
                 anchor_after_import, 'def _local(*write_source_atomic):\n    pass\n\n\n'
                 + anchor_after_import),
                ('mutant-write-back-to-truncating',
                 'write_source_atomic(root, path, changed.encode())', 'path.write_text(changed)'),
                ('restore-write-back-to-truncating',
                 'write_source_atomic(root, path, original)', 'path.write_bytes(original)'),
                # r13（zai sibling P1 #3）：调用方自己的仓根常量也是 `Path(__file__)…`，
                # `__file__` 必须在模块里一次都不许被绑（文本钉子看不见运行期按名读取）。
                ('fixture-file-dunder-rebound', anchor_after_import,
                 "__file__ = '/a/b/c/d/e/f/g.py'\n" + anchor_after_import),
                # r13（zai sibling P1 #2）：属性赋值目标绕开 `Name` 绑定表。
                ('fixture-str-shadowed-via-a-builtins-attribute', anchor_after_import,
                 "import builtins\nbuiltins.str = lambda value: '../src'\n"
                 + anchor_after_import),
                # r14（deepseek sibling P1 #2 + zai concurrency P1 #1）：调用方这侧也补
                # 一条「下标目标」与一条「函数体里是改道」的拼写。
                ('fixture-globals-item-assignment', anchor_after_import,
                 'globals()["Path"] = lambda value: value\n' + anchor_after_import),
                ('fixture-attribute-write-in-a-function-body', anchor_after_import,
                 'import builtins\n\n\ndef _pin_namespace():\n'
                 '    builtins.str = lambda value: "../src"\n\n\n_pin_namespace()\n'
                 + anchor_after_import)):
            self.assertEqual(fixture.count(old), 1, f'#1692 §9 unique anchor: {label}')
            with self.assertRaises(AssertionError, msg=f'#1692 §9 {label} must be caught'):
                self.fixture_call_sites(fixture.replace(old, new, 1))
            print(f'#1692 §9 atomic-publish mutant {label}: KILLED', flush=True)

    def test_shared_restore_write_is_atomic(self):
        """#1903：共享守卫的还原写是原子的，与夹具那一处同一份实现。

        #2008 曾在这里钉一条**已知缺口** tripwire（`negative_process.py` 仍是
        `path.write_bytes(snapshot[path])`，指向 #1903）。#1903 修好后它按设计变红；
        这里翻成正向断言：共享还原**不存在**截断写、**调用** `write_source_atomic`，
        并且那个函数仍满足 §9 的全部发布钉子。删掉这条断言而不是改写它，会让
        `test_mutation_source_writes_are_published_atomically` 再次被读成「被变异源
        已经全部原子发布」——那正是评审判的假 PASS。

        覆盖边界：本条管**共享还原咽喉**（`mutating_sources` 的出口）与三处 native
        夹具的本地实现。各夹具循环内的逐变异体写入 / 循环内还原仍是原地写，由
        #1908 第 1 条跟踪（前置于下一轮负控夹具改造），不在本 PR。
        """
        source = (Path(__file__).resolve().parent/'fixtures/negative_process.py').read_text()
        self.assertEqual(re.findall(r'(?<!\w)path\.write_bytes\(snapshot\[path\]\)', source),
                         [],
                         '#1903 the shared restore must not truncate the source in place')
        self.assertEqual(source.count(self.SHARED_RESTORE_CALL), 1,
                         '#1903 the shared restore must publish through write_source_atomic')
        self.publish_guards(source, scratch="PUBLISH_DIR = '.negctl-publish'",
                            scratch_name='publish')
        fixed = source.replace(self.SHARED_RESTORE_CALL,
                               'path.write_bytes(snapshot[path])', 1)
        with self.assertRaises(AssertionError,
                               msg='#1903 a truncating shared restore must flip this guard'):
            self.assertEqual(re.findall(r'(?<!\w)path\.write_bytes\(snapshot\[path\]\)', fixed),
                             [],
                             '#1903 the shared restore must not truncate the source in place')
        print('#1903 shared-restore guard mutant truncating-shared-restore: KILLED', flush=True)
        # r15（两家 10 条 P1）：`write_source_atomic` 的暂存目录同样改成**观测**，
        # 不再靠源码文本推断。
        with tempfile.TemporaryDirectory(prefix='pdrs-1903-root-') as scratch_root:
            scratch_root = Path(scratch_root)
            (scratch_root / 'src').mkdir()
            target = scratch_root / 'src' / 'probed-source.rs'
            target.write_bytes(b'#1903 probe\n')
            shared_module = self.sibling_module('negative_process.py')
            self.observe_publish(
                lambda target, data: shared_module['write_source_atomic'](
                    scratch_root, target, data),
                target, scratch_root / '.negctl-publish')
        print('#1903 observed publish write_source_atomic: temp from <root>/.negctl-publish '
              '(live)', flush=True)
        # r18（deepseek concurrency P2）：目标可能在变异窗口里被删掉（变异体删文件是
        # 合法形态）。还原是**顺序循环**，无条件的 `path.stat()` 会在写第一份之前抛
        # FileNotFoundError，把后面所有受守卫文件留在变异状态 —— 比缺一个文件更糟。
        # r19（r18 两家 P1，同一条）：兜底 mode 不能是 0o600 —— 重建出来的文件与
        # HEAD 的 100644 只差 mode，`core.filemode=true` 的 `git diff --name-only`
        # 照样列出，下一次 preflight 整树拒跑，正是本函数要消灭的锁死。兜底取
        # 0o644；知道原 mode 的调用方（`mutating_sources`）显式传 `mode=`。
        with tempfile.TemporaryDirectory(prefix='pdrs-1903-root-') as scratch_root:
            scratch_root = Path(scratch_root)
            (scratch_root / 'src').mkdir()
            first = scratch_root / 'src' / 'deleted.rs'
            second = scratch_root / 'src' / 'kept.rs'
            second.write_bytes(b'#1903 second\n')
            shared_module = self.sibling_module('negative_process.py')
            shared_module['write_source_atomic'](scratch_root, first, b'#1903 first\n')
            first.unlink()
            restored = []
            for path, data in ((first, b'#1903 first\n'),
                               (second, b'#1903 second\n')):
                shared_module['write_source_atomic'](scratch_root, path, data)
                restored.append(path.read_bytes())
            self.assertEqual(restored, [b'#1903 first\n', b'#1903 second\n'],
                             '#1903 r18 a restore loop must keep going after the target '
                             'was deleted mid-window; one missing mode must not abort it')
            self.assertEqual(stat.S_IMODE(first.stat().st_mode), 0o644,
                             '#1903 r19 the fallback mode is git 100644: a 0o600 rebuild '
                             'leaves a mode-only diff and the next preflight refuses '
                             'the whole tree')
        print('#1903 deleted-target restore: recreated with the git-canonical mode (live)',
              flush=True)
        # 可执行位同样要能原样还原：`mutating_sources` 在快照时记下 mode，删除后靠
        # 显式 `mode=` 传回去。
        with tempfile.TemporaryDirectory(prefix='pdrs-1903-root-') as scratch_root:
            scratch_root = Path(scratch_root)
            (scratch_root / 'src').mkdir()
            executable = scratch_root / 'src' / 'tool.sh'
            shared_module = self.sibling_module('negative_process.py')
            shared_module['write_source_atomic'](scratch_root, executable,
                                                 b'#!/bin/sh\n', mode=0o755)
            executable.unlink()
            shared_module['write_source_atomic'](scratch_root, executable,
                                                 b'#!/bin/sh\n', mode=0o755)
            self.assertEqual(stat.S_IMODE(executable.stat().st_mode), 0o755,
                             '#1903 r19 an explicit mode survives a deleted target')
        print('#1903 deleted-target restore: explicit mode preserved (live)', flush=True)
        # 静态钉：共享还原必须把快照里的 mode 传回去（丢了就是上面那条 mode-only diff）。
        source_text = (Path(__file__).resolve().parent/'fixtures/negative_process.py'
                       ).read_text()
        restore_call = self.SHARED_RESTORE_CALL
        self.assertEqual(source_text.count(restore_call), 1,
                         '#1903 r19 the shared restore passes the snapshot mode; without '
                         'it a deleted target comes back 0o644 and an executable one '
                         'loses its bit')
        mode_mutant = self.sibling_module(
            'negative_process.py',
            source_text.replace(restore_call,
                                'write_source_atomic(root, path, snapshot[path])', 1))
        with tempfile.TemporaryDirectory(prefix='pdrs-1903-root-') as scratch_root:
            scratch_root = Path(scratch_root)
            (scratch_root / 'src').mkdir()
            executable = scratch_root / 'src' / 'tool.sh'
            mode_mutant['write_source_atomic'](scratch_root, executable,
                                               b'#!/bin/sh\n', mode=0o755)
            executable.unlink()
            mode_mutant['write_source_atomic'](scratch_root, executable,
                                               b'#!/bin/sh\n')
            self.assertEqual(stat.S_IMODE(executable.stat().st_mode), 0o644,
                             '#1903 r19 the mutant must reproduce the mode drift this '
                             'nail exists to catch')
        print('#1903 dropped-snapshot-mode mutant: 0o755 → 0o644 drift reproduced '
              '(the static nail above is what kills it)', flush=True)
        # 对杀负控：把 `mode is None` 兜底去掉，同一场景必须抛 FileNotFoundError。
        fallback = ('    if mode is None:\n'
                    '        mode = path.stat().st_mode & 0o7777 if path.exists() '
                    'else DEFAULT_SOURCE_MODE\n')
        self.assertEqual(source_text.count(fallback), 1,
                         '#1903 r19 the fallback-mode branch is unique')
        mutant = self.sibling_module(
            'negative_process.py',
            source_text.replace(fallback, '    mode = path.stat().st_mode & 0o7777\n', 1))
        with tempfile.TemporaryDirectory(prefix='pdrs-1903-root-') as scratch_root:
            scratch_root = Path(scratch_root)
            (scratch_root / 'src').mkdir()
            missing = scratch_root / 'src' / 'deleted.rs'
            with self.assertRaises(FileNotFoundError,
                                   msg='#1903 r19 without the mode fallback the '
                                       'restore loop dies on a deleted target'):
                mutant['write_source_atomic'](scratch_root, missing, b'#1903 first\n')
        print('#1903 deleted-target mutant stat-without-fallback: KILLED', flush=True)
        # 行为判据的对杀负控：临时文件改到目标旁边，观测必须红。
        for label, old, new in (
                ('temp-beside-the-target', "publish = Path(root) / PUBLISH_DIR",
                 "publish = path.parent"),
                # 只改运行期、静态判据**看不见**的那一类：借白名单里的
                # `globals()[_HELD_STATE]` 写把模块全局 `os` 换成 shim（deepseek
                # boundary P1 #1 的原始构造）。静态侧按设计全绿，观测必须红 ——
                # 这就是「判行为」而不是「判拼写」的意义。
                ('os-shadowed-through-the-allowlisted-held-state-write',
                 "PUBLISH_DIR = '.negctl-publish'",
                 "class _PinnedOs:\n"
                 "    def __getattr__(self, item):\n"
                 "        return getattr(__import__('os'), item)\n"
                 "    replace = staticmethod(__import__('shutil').copyfile)\n"
                 "_HELD_STATE = 'os'\n"
                 "globals()[_HELD_STATE] = _PinnedOs()\n"
                 "_HELD_STATE = '_pdrs_negative_process_held'\n"
                 "PUBLISH_DIR = '.negctl-publish'")):
            source_text = (Path(__file__).resolve().parent/'fixtures/negative_process.py'
                           ).read_text()
            self.assertIn(old, source_text, f'#1903 r15 {label} anchor')
            mutated_source = source_text.replace(old, new, 1)
            if label == 'os-shadowed-through-the-allowlisted-held-state-write':
                self.assertEqual(self.namespace_tamper_problems(mutated_source), [],
                                 '#1903 r15 the static lint is blind to this by design; '
                                 'the observation is what must catch it')
            with tempfile.TemporaryDirectory(prefix='pdrs-1903-root-') as scratch_root:
                scratch_root = Path(scratch_root)
                (scratch_root / 'src').mkdir()
                target = scratch_root / 'src' / 'probed-source.rs'
                target.write_bytes(b'#1903 probe\n')
                mutant = self.sibling_module('negative_process.py', mutated_source)
                with self.assertRaises(AssertionError,
                                       msg=f'#1903 r15 {label} the observation must bite'):
                    self.observe_publish(
                        lambda target, data: mutant['write_source_atomic'](
                            scratch_root, target, data),
                        target, scratch_root / '.negctl-publish')
            print(f'#1903 observed-publish mutant {label}: KILLED', flush=True)
        # r16：把 r15 两家点名、而静态闭集**判绿**的那几种拼写逐条真跑。
        # 每一条都只做一件事：把 `os.replace` 换成 `shutil.copyfile`（非原子发布）。
        # 静态侧对它们全绿（这正是 r15 十条 P1 的共同点），行为侧必须红 ——
        # 红的判据是「目标在发布后就是那个临时文件（同一 inode）」：rename 搬 inode，
        # `copyfile` 只在原地覆盖内容、目标保留自己的 inode（先截断再写）。
        original_replace = os.replace
        try:
            for label, snippet in self.BEHAVIOURAL_ONLY_TAMPER_FORMS:
                source_text = (Path(__file__).resolve().parent/'fixtures/negative_process.py'
                               ).read_text()
                anchor = "PUBLISH_DIR = '.negctl-publish'"
                self.assertEqual(source_text.count(anchor), 1,
                                 f'#1903 r16 {label} unique anchor')
                mutated_source = source_text.replace(anchor, snippet + anchor, 1)
                self.assertEqual(
                    self.namespace_tamper_problems(mutated_source), [],
                    f'#1903 r16 {label}: the static closed set is blind to this '
                    f'spelling by design; the observation is what must catch it')
                with tempfile.TemporaryDirectory(prefix='pdrs-1903-root-') as root:
                    root = Path(root)
                    (root / 'src').mkdir()
                    target = root / 'src' / 'probed-source.rs'
                    target.write_bytes(b'#1903 probe\n')
                    mutant = self.sibling_module('negative_process.py', mutated_source)
                    self.assertIsNot(os.replace, original_replace,
                                     f'#1903 r16 {label}: the mutant must really have '
                                     f'redirected os.replace, else this proves nothing')
                    with self.assertRaises(AssertionError,
                                           msg=f'#1903 r16 {label} the observation '
                                               f'must bite'):
                        self.observe_publish(
                            lambda target, data: mutant['write_source_atomic'](
                                root, target, data),
                            target, root / '.negctl-publish')
                    print(f'#1903 static-blind / observation-red {label}: KILLED',
                          flush=True)
                os.replace = original_replace
        finally:
            os.replace = original_replace

    def test_polarity_rule_alone_catches_the_branch_swap(self):
        """r8（DeepSeek boundary P3）：极性判据要能**单独**杀掉分支对调。

        `scratch-branches-swapped` 那条变异即使 `roots_at` 的 IfExp 支退回 r6 的「提及」
        语义也照样 KILLED —— 因为 `binding_source` 的精确正则会先把它拦下。判据被别的
        钉子代劳，等于这一支没有对杀覆盖。这里直接喂一个只有极性问题的函数体：没有
        `mkstemp(dir=str(...))` 可查、没有字面钉可命中，唯一能判红的就是 `roots_at`。
        """
        body = textwrap.dedent('''
            def write_atomic(path, data):
                ROOT = Path(__file__).resolve().parents[5]
                PUBLISH_DIR = ROOT / '.negctl-publish'
                scratch = path.parent if path.is_relative_to(ROOT) else PUBLISH_DIR
                with open(scratch, 'wb') as stream:
                    stream.write(data)
                os.replace(scratch, path)
            ''')
        self.assertEqual(self.mkstemp_scratch_names(body), [],
                         'the probe body must not give the mkstemp pin anything to chew on')
        self.assertNotEqual(self.temp_derivation_problems(body), [],
                            '#1692 §9 r8 the IfExp polarity must be judged by the '
                            'derivation rule alone')
        healthy = body.replace(
            'scratch = path.parent if path.is_relative_to(ROOT) else PUBLISH_DIR',
            'scratch = PUBLISH_DIR if path.is_relative_to(ROOT) else path.parent', 1)
        self.assertEqual(self.temp_derivation_problems(healthy), [],
                         '#1692 §9 r8 the healthy polarity must stay green')
        print('#1692 §9 r8 IfExp polarity is pinned by the derivation rule alone: KILLED',
              flush=True)

    def negative_process_module(self):
        """进程内唯一的 `negative_process` 模块（夹具与 #166 探针用的就是这一个）。"""
        import sys
        module = sys.modules.get('negative_process')
        if module is None:
            path = Path(__file__).resolve().parent/'fixtures/negative_process.py'
            spec = importlib.util.spec_from_file_location('negative_process', path)
            module = importlib.util.module_from_spec(spec)
            sys.modules['negative_process'] = module
            spec.loader.exec_module(module)
        return module

    def test_restore_publishes_through_the_patched_module_global(self):
        """#1903 r2：探针接线的**行为**正控。

        r2 第一版接线自证是恒真式（zai boundary P1；DeepSeek boundary / concurrency P2）。
        只做结构判据仍可能漏掉「还原搬去别的 helper」这类漂移，所以这里真跑一次
        `mutating_sources`：进入变异窗口 → 还原，记录型包装器必须被调用**恰好一次**，
        且源文件字节不变。#166 的两处探针 patch 的是同一个模块属性，这条正控覆盖它们两处。

        r3（DeepSeek boundary P2 #4 / concurrency P2 #1-3）：`mutating_sources` 的外围守卫
        会 `_ensure_workspace_scanner()`（缺二进制时起 cargo build，上限 1800 s）并对**真实
        工作树**做 pristine 检查 —— 放进 CI 的 `metadata-observer` 既慢又非 hermetic（`src/`
        任何未提交文件都会让它红，且与被测行为无关）。这里把外围守卫换成 no-op，
        只留被测的**快照 + 还原 + 回读校验**真路径：判据仍是「还原按模块全局解析」，
        但不再依赖扫描器二进制、工作树洁净度或 `.negative-source.lock` 的拥塞。
        """
        import contextlib
        guard = self.negative_process_module()
        root = Path(__file__).resolve().parents[3]
        relative = 'src/pilotdeck-gateway/tests/fixtures/negative-restore/test_restore.py'
        target = root/relative
        before = target.read_bytes()
        calls = []
        real = guard.write_source_atomic

        def recording(*args, **kwargs):
            calls.append(str(args[1]))
            return real(*args, **kwargs)

        with patch.object(guard, 'write_source_atomic', side_effect=recording), \
                patch.object(guard, '_assert_workspace_sources', lambda *a, **k: None), \
                patch.object(guard, '_ensure_workspace_scanner', lambda *a, **k: None), \
                patch.object(guard, '_assert_product_pristine', lambda *a, **k: None), \
                patch.object(guard, 'tree_lock',
                             return_value=contextlib.nullcontext()):
            with guard.mutating_sources(root, [relative], lock_timeout=120):
                pass
        self.assertEqual(calls, [str(target.resolve())],
                         '#1903 the restore must publish through the module global the '
                         '#166 probes patch -- otherwise their call_count is always 0')
        self.assertEqual(target.read_bytes(), before,
                         '#1903 the behavioural control must not change the source bytes')
        self.assert_restore_reads_the_patched_global(guard, '#1903 behavioural control')
        print('#1903 restore publishes through the patched module global: 1 call, bytes '
              'unchanged', flush=True)

    def test_restore_wiring_self_proof_can_fail(self):
        """#1903 r2：接线判据自己的负控 —— 两种真实漂移必须被它抓住。

        `assert_restore_reads_the_patched_global` 若不是恒真就必须能报错；这里把两种让
        #166 探针空转的形态塞进同构的假模块（`__globals__` 与 `vars(guard)` 同一份字典，
        与真模块同构），逐条要求它失败，再用真模块确认它不误报。
        """
        shadow = types.ModuleType('negative_process_wiring_control')
        exec(compile(
            'def default_parameter(root, paths, *, write_source_atomic=None):\n'
            '    return write_source_atomic\n'
            '\n'
            'def local_import(root, paths):\n'
            '    from negative_process import write_source_atomic\n'
            '    return write_source_atomic\n', '<wiring-control>', 'exec'), shadow.__dict__)
        for label, name in (('default-parameter', 'default_parameter'),
                            ('local-import', 'local_import')):
            with self.subTest(label=label):
                shadow.mutating_sources = getattr(shadow, name)
                with self.assertRaises(AssertionError,
                                       msg=f'#1903 {label} must blind the probes and be caught'):
                    self.assert_restore_reads_the_patched_global(shadow, f'#1903 control {label}')
        self.assert_restore_reads_the_patched_global(self.negative_process_module(),
                                                     '#1903 real module')
        print('#1903 restore-wiring self-proof: default-parameter and local-import are both '
              'caught, the real module passes', flush=True)


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
                         Counter({3: 4, 4: 9, 64: 1, 69: 2}),
                         '#1632 every cargo-slots.sh guard: how many guards exit with which code')
        self.assertEqual({int(code) for _, code in guards}, used,
                         '#1692 §10 every raised non-zero code is a guarded failure, and '
                         'every guard exits with a code the script really raises')
        # #2506 / #2516 fix-r3：四条取样守卫逐条钉死（取样失败 / 输出为空 / 零可解析行 /
        # 缺调用者 pid），删任意一条都会让该形态回到「在跑 0 → 放行」。
        self.assertEqual([msg for msg, _ in guards if msg.startswith('cargo-slots: ps ')],
                         ['cargo-slots: ps 取样失败，无法判断名额（不按 0 放行）',
                          'cargo-slots: ps 输出为空，无法判断名额（不按 0 放行）',
                          'cargo-slots: ps 输出零可解析行，无法判断名额（不按 0 放行）',
                          'cargo-slots: ps 快照缺调用者 pid，无法判断名额（不按 0 放行）'],
                         '#1632 the sampling guards keep their diagnostics, in order')
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
        """取样不可用时必须 rc=3，且绝不打印「在跑 N」——打印了就是把空样本读成 0 放行。

        #2506 / #2516 fix-r3：rc=3 之外还逐格钉**该格应有的守卫消息**。取样守卫从两条
        变成四条（零可解析行 / 缺调用者 pid）后，只看 rc 的翻转变异体会被更深的守卫
        兜住——守卫条件被翻掉、退出被包进子 shell 或整行被搬走，样本照样在更深一层
        exit 3，rc 与「不按 0 放行」两格断言照绿。消息是哪条守卫开口的唯一可判差异：
        真实脚本每格开口不变，变异体必落错消息。
        """
        for case, body, expected in (
                ('ps exits non-zero', '#!/bin/sh\nexit 1\n', 'ps 取样失败'),
                ('ps prints nothing', '#!/bin/sh\nexit 0\n', 'ps 输出为空'),
                # 残缺采样：先输出一行再非零退出。只有这一格能把「取样失败」
                # 那条守卫和「输出为空」那条分开——前两格都会被空输出守卫兜住。
                ('ps prints rows then fails',
                 '#!/bin/sh\nprintf "  123 1 cargo\\n"\nexit 1\n', 'ps 取样失败')):
            done = self.run_cargo_slots(script, tmp, body)
            self.assertEqual(done.returncode, 3, f'#1632 {label} / {case}: {done.stderr}')
            self.assertIn(expected, done.stderr,
                          f'#1632 {label} / {case} fails via its own guard, not a deeper one')
            # 而且只允许这一条开口：exit 被包进子 shell / 守卫行被搬走的形态里，诊断
            # 照发（子 shell 里 echo 不受 exit 影响），单看「消息在场」分不出来；其余
            # 三条取样守卫的消息必须缺席，双消息即红。
            for fragment in ('ps 取样失败', 'ps 输出为空', 'ps 输出零可解析行',
                             'ps 快照缺调用者 pid'):
                if fragment != expected:
                    self.assertNotIn(fragment, done.stderr,
                                     f'#1632 {label} / {case} leaks another sampling guard: '
                                     f'{fragment}')
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

    def test_cargo_slots_script_fails_closed_on_unparseable_or_callerless_sample(self):
        # #2506 / #2516 fix-r3（fix-r1 评审 sibling P1 的 bash 半条，与 model-complete
        # 探针 `slots_from_ps` 的 #1770 fix-r1/fix-r2 守卫对称）：「非空但零可解析行」
        # 「有可解析行但表内缺调用者 pid」都必须按取样失败（rc=3）处理。旧 awk 对
        # 不可解析输入照样 `print c+0`，ps 版本/格式漂移打出整表诊断（rc=0）时满槽会被
        # 读成「有空位」（CLAUDE.md 每机并发上限被穿透）；一张不含本脚本自身 pid 的表
        # 同样无法作证——真取样里本脚本必然在场。
        script = Path(__file__).resolve().parents[3]/'scripts/cargo-slots.sh'
        text = script.read_text()
        with tempfile.TemporaryDirectory() as tmp:
            for case, body, needle in (
                    ('ps prints only diagnostics',
                     '#!/bin/sh\necho "ps: unsupported output format"\nexit 0\n',
                     '零可解析行'),
                    # 有可解析行（一条真形态 cargo 树）但表里没有调用者 pid：旧实现照常给数。
                    ('ps omits the caller pid',
                     '#!/bin/sh\nprintf "  123 122 cargo build\\n"\nexit 0\n',
                     '缺调用者 pid')):
                done = self.run_cargo_slots(script, tmp, body)
                self.assertEqual(done.returncode, 3, f'#2506 {case}: {done.stderr}')
                self.assertIn(needle, done.stderr, f'#2506 {case} names its failure shape')
                self.assertIn('不按 0 放行', done.stderr, f'#2506 {case} explains itself')
                self.assertNotIn('在跑', done.stdout, f'#2506 {case} reports no slot count')
            # 正桩：一张含调用者 pid、含一条真形态 cargo 树、含一条不可解析杂行的正常表
            # 必须照常判名额（在跑 1 / 上限 4，杂行不计数）。两条新守卫只在快照无法作证
            # 时才拦——条件翻转与「awk 不再校验行」的负控正是靠这一格杀。
            # `os.getppid()` 即脚本 `$$`：命令替换的子进程由脚本 shell 直接 fork（实测
            # outer.sh/fakeps 三次运行 ppid==$$），与 slot-probe-residual 夹具 FAKE_PS
            # 的 {CALLER_PID} 同一依据。
            good = self.run_cargo_slots(script, tmp, (
                '#!/usr/bin/env python3\n'
                'import os, sys\n'
                'me = os.getppid()\n'
                'rows = [f"{me} {me - 1} bash cargo-slots.sh",\n'
                '        "  200 100 cargo build",\n'
                '        " 100 50 zsh",\n'
                '        "  50 1 launchd",\n'
                '        "   1 0 launchd",\n'
                '        "junk 100 cargo build"]\n'
                'sys.stdout.write("".join(row + chr(10) for row in rows))\n'))
            self.assertEqual(good.returncode, 0, f'#2506 healthy table: {good.stderr}')
            self.assertIn('在跑 1 / 上限 4', good.stdout,
                          '#2506 the healthy table counts one tree and ignores the junk row')

            def healthy_table_verdict(script_path):
                done = self.run_cargo_slots(script_path, tmp, (
                    '#!/usr/bin/env python3\n'
                    'import os, sys\n'
                    'me = os.getppid()\n'
                    'rows = [f"{me} {me - 1} bash cargo-slots.sh",\n'
                    '        "  200 100 cargo build",\n'
                    '        " 100 50 zsh",\n'
                    '        "  50 1 launchd",\n'
                    '        "   1 0 launchd",\n'
                    '        "junk 100 cargo build"]\n'
                    'sys.stdout.write("".join(row + chr(10) for row in rows))\n'))
                self.assertEqual(done.returncode, 0,
                                 f'#2506 the healthy table must pass: {done.stderr}')
                self.assertIn('在跑 1 / 上限 4', done.stdout,
                              '#2506 the healthy table counts one tree and ignores the junk row')

            for label, mutant in (
                    # 两条新守卫的 `||` 翻成 `&&`：健康表（parsed>0 且 me_seen=1）反而被拦，
                    # 正桩必红。
                    ('rows-guard-condition-flipped',
                     text.replace('] || { echo "cargo-slots: ps 输出零可解析行',
                                  '] && { echo "cargo-slots: ps 输出零可解析行', 1)),
                    ('caller-guard-condition-flipped',
                     text.replace('] || { echo "cargo-slots: ps 快照缺调用者 pid',
                                  '] && { echo "cargo-slots: ps 快照缺调用者 pid', 1)),
                    # awk 行校验删掉：杂行被当可解析行且其 exe 首词是 cargo，
                    # 健康表数出第二棵树（在跑 2）。
                    ('row-validation-dropped',
                     text.replace('$1 ~ /^[0-9]+$/ && $2 ~ /^[0-9]+$/ && NF >= 3',
                                  '1', 1)),
                    # 调用者行不再被标记：任何表都过不了 me_seen 守卫，正桩必红。
                    ('caller-row-not-tracked',
                     text.replace('if ($1 == me) me_seen=1', 'if ($1 == 0) me_seen=1', 1))):
                self.assertNotEqual(mutant, text, f'#2506 the mutant really changed: {label}')
                path = Path(tmp)/f'{label}.sh'
                path.write_text(mutant)
                with self.assertRaises(AssertionError,
                                       msg=f'#2506 the healthy-table check must catch {label}'):
                    healthy_table_verdict(path)
                print(f'#2506 slot-guard behaviour mutant {label}: KILLED', flush=True)

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
            # permission-faults switch (#1263). if/timeout/env could all override
            # execution or ignore failure; #1809 r7 adds `shell` as a REGISTERED key —
            # the step-level shell outranks any defaults.run.shell, so its value is
            # pinned to `bash` (fail-fast: GitHub runs it as `bash -e` semantics,
            # a custom template like `bash {0}` silently drops fail-fast).
            self.assertEqual(set(fields), {'name', 'run', 'shell'} | ({'env'} if title == '测试' else set()),
                             f'#922 {title} cannot override execution or ignore failure')
            self.assertEqual(fields['shell'], ('bash', []),
                             f'#922 {title} pins the fail-fast shell `bash`')
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
        # #1900: and the mirror — every test that prints a skip line must be
        # named by this filter, or the note it writes is captured and lost.
        self.assert_every_skip_line_reaches_nextest(self.nextest_success_output_filter)
        # #1937: and the pairing — every skip line travels with its
        # require_permission_faults assert, through the one shared pair.
        self.assert_skip_lines_pair_their_requirement()

    def assert_success_output_filter_names_real_tests(self, filter_text):
        # fix-r3: `test(=name)` is an exact match on the name nextest reports.
        # Integration tests (`tests/*.rs`) report their bare fn name, unit tests
        # (`src/**`) report their module path — a bare leaf never selects them
        # (measured on nextest 0.9.150 and on CI's pinned 0.9.148: `test(=bar)`
        # misses `foo::tests::bar`, `test(=foo::tests::bar)` hits).
        # #2115: the defining item and its module nesting are read from the
        # proc-macro2 token inventory, not from text: a comment between `fn`
        # and the name, an `r#` prefix or an `fn x(` inside a comment or a
        # string are token questions the old literal regex answered wrong in
        # both directions. fix-r4's "exactly ONE name per declaring file"
        # becomes "one name per declaring *record*": a leaf declared both at
        # file level and inside an inner `mod` is reported by nextest under
        # both spellings, so both are acceptable answers here.
        entries = re.findall(r'test\(([^)]*)\)', filter_text)
        self.assertTrue(entries, '#922 the success-output filter names its tests')
        crate = Path(__file__).resolve().parents[1]
        sources = self.read_crate_sources(crate)
        inventory = self._lex_fn_items(sources)
        for entry in entries:
            # `[^)]*` also catches a name the old `[A-Za-z0-9_:]+` regex
            # silently truncated at a `-` or a space.
            self.assertRegex(entry, r'^=[A-Za-z0-9_:]+$',
                             f'#922 the success-output filter entry test({entry}) is not a '
                             f'plain `test(=name)` matcher')
            name = entry[1:]
            leaf = name.rsplit('::', 1)[-1]
            defining = [(source, record) for source, record in inventory.items()
                        if self.fn_name_exists(record, leaf)]
            self.assertTrue(defining, f'#922 the success-output filter names the real test {name}')
            expected = {self.nextest_visible_name(crate, source, item['mod'], leaf)
                        for source, record in defining
                        for item in record['fns'] if item['name'] == leaf}
            self.assertIn(name, expected,
                          f'#922 the success-output filter must use the nextest-visible name '
                          f'for {leaf}: {sorted(expected)}, not {name}')

    @staticmethod
    def read_crate_sources(crate):
        # #1900 simplify P3: both guards scan the same tree; read it once.
        return {source: source.read_text(errors='replace')
                for source in crate.rglob('*.rs') if 'target' not in source.parts}

    # #2115: inventory cache, one entry per request. The key is the sha256 of
    # the whole stdin document, not of a single file: the example's macro join
    # is batch-wide (a file's `macro_calls` depends on which metavariable-fn
    # macros the *other* files in the request define), so caching one file's
    # record could replay an answer computed against a different batch.
    _lex_fn_inventory = {}

    @classmethod
    def _lex_fn_items(cls, texts):
        # #2115: the token-level truth behind every Rust-name check in this
        # driver. Counting `fn <name>` with a literal regex both missed real
        # items (a comment between `fn` and the name, an `r#` prefix) and
        # counted fake ones (an `fn x(` in a comment or a string literal);
        # both are token questions, not text questions. This runs the ONE
        # proc-macro2 lexer (`tests/common/source_inputs.rs`, the one
        # `unsafe_boundary.rs` already uses — #449/#1166: never a hand-written
        # Rust lexer) through the `examples/lex_fn_items.rs` entry and
        # returns, per file key of `texts`, either
        # `{"fns": [{"name", "mod", "macro"}…], "macro_rules": …,
        # "macro_calls": …, "unresolved": […]}` or `{"error": …}`. The
        # consumers decide what each field means for them; the entry itself
        # only refuses to guess. Contents travel over stdin, not paths:
        # the guards pass sources dicts whose text the controls below
        # rewrote in memory.
        request = json.dumps({'files': {str(source): text
                                        for source, text in texts.items()}},
                             sort_keys=True).encode()
        key = hashlib.sha256(request).hexdigest()
        cached = cls._lex_fn_inventory.get(key)
        if cached is not None:
            return cached
        document = cls._run_lex_fn_items(request)
        files = document.get('files')
        if not isinstance(files, dict) or set(files) != {str(source) for source in texts}:
            raise AssertionError('#2115 the lex entry must answer with exactly the '
                                 'files it was given')
        inventory = {source: files[str(source)] for source in texts}
        cls._lex_fn_inventory[key] = inventory
        return inventory

    _lex_entry_negative_process = None

    @classmethod
    def _lex_entry_process(cls):
        # #2115 fix-r1: the lex entry's bootstrap is a sibling of
        # `_ensure_workspace_scanner`, so it borrows the same shared
        # process/source machinery — `tests/fixtures/negative_process.py`,
        # the M6-owned module (imported here, never copied: "sibling runners
        # must not keep their own allow-lists" cuts both ways). Loaded once
        # per process; a control that needs to intercept the lease patches
        # the loaded module's attributes, not this file's copies.
        if cls._lex_entry_negative_process is None:
            path = (Path(__file__).resolve().with_name('fixtures')
                    / 'negative_process.py')
            spec = importlib.util.spec_from_file_location(
                'lex_entry_negative_process', path)
            module = importlib.util.module_from_spec(spec)
            spec.loader.exec_module(module)
            cls._lex_entry_negative_process = module
        return cls._lex_entry_negative_process

    # fix-r3 (DS concurrency #1): the unconditional tree-lock wait is
    # budgeted to the caller's own 300 s — the scan's run_owned below and
    # the control-side run_test subprocess (the gateway entry-guard and
    # batch mutation controls both default timeout=300) agree on that
    # figure, so a sibling mutation window longer than it can never be
    # waited out inside any caller's budget.
    _LEX_ENTRY_LOCK_WAIT = 300

    @classmethod
    @contextlib.contextmanager
    def _lex_entry_tree_lock(cls, negative, root):
        # fix-r3 (DS concurrency #1): a window longer than the budget must
        # surface as this guard's own AssertionError — a bare TimeoutError
        # reads as a crash in the harness log, not as a refusal. The
        # refusal keeps the waited budget and the busy-lock source for
        # the owner; TreeLock reentry makes nesting with a caller's own
        # lease safe.
        wait = cls._LEX_ENTRY_LOCK_WAIT
        try:
            with negative.tree_lock(root, timeout=wait):
                yield
        except TimeoutError as error:
            raise AssertionError(
                '#2115 the lex entry refused to wait out a sibling mutation '
                f'window longer than its {wait} s tree-lock budget: '
                f'{error}') from error

    @classmethod
    def _run_lex_fn_items(cls, request):
        # #2115: run (and, once, bootstrap) the lexing entry. fix-r1 (DS
        # concurrency #1): the bootstrap follows the ONE shared discipline
        # `_ensure_workspace_scanner` uses (negative_process.py, imported —
        # never copied): a missing binary is built under a pristine-HEAD
        # lease (`tree_lock` + `_assert_workspace_sources` + `wait_for_slot`
        # + `run_owned`), and an EXISTING binary is never rebuilt here. A
        # digest mismatch means the binary was built from different rules
        # than this tree declares — a killed negative control leaving
        # mutated sources behind must surface as a refusal, not as a
        # rebuild that quietly compiles the mutant into the rules the
        # guards then trust. Rebuilding a stale entry is the owner's job.
        # The digest follows the #1166 identity discipline the negative
        # controls apply to `workspace_inputs`: the example source, the
        # shared lexer, both manifests and the lockfile, in the order the
        # example's `scanner_identity()` hashes them.
        negative = cls._lex_entry_process()
        root = Path(__file__).resolve().parents[3]
        crate = root/'src/pilotdeck-gateway'
        sources = (crate/'examples/lex_fn_items.rs',
                   crate/'tests/common/source_inputs.rs',
                   crate/'Cargo.toml',
                   root/'src/Cargo.toml',
                   root/'src/Cargo.lock')

        def build_inputs_digest():
            # fix-r2 (boundary P1): the five build-input summaries answer
            # only under the tree lock. Read outside it, a sibling
            # control's window (a `mutating_sources` hold in another
            # process, which keeps the lease until its restore) can
            # interleave the read: the digest then describes the mutant
            # while the existing binary still reports the pristine tree,
            # and the refusal below fires against a healthy binary — a
            # false red. TreeLock reentry makes nesting with a caller's
            # own lease safe, and the kernel releases the flock if this
            # process dies mid-read.
            return hashlib.sha256(b''.join(path.read_bytes()
                                           for path in sources)).hexdigest()

        # #1166: the target directory goes through the one shared admission
        # (fix-r1, DS concurrency #3): the value is resolved and rejected by
        # negative_process's own rule, not by a second local allow-list — a
        # relative CARGO_TARGET_DIR would otherwise resolve against two
        # different cwds on the Python and the cargo side.
        try:
            environment = negative.target_environment(root)
        except RuntimeError as error:
            raise AssertionError(f'#2115 the lex entry refuses this '
                                 f'CARGO_TARGET_DIR: {error}') from error
        target = Path(environment['CARGO_TARGET_DIR'])
        executable = target/'debug/examples/lex_fn_items'
        # fix-r2 (boundary P1): the summaries are read holding the same
        # lease the cold bootstrap below takes — never outside it (see
        # build_inputs_digest). A sibling's window is either closed
        # before the lease or this read waits it out and answers on the
        # restored bytes. fix-r3 (DS concurrency #1): the wait is
        # budgeted to the caller's own 300 s and an overlong window
        # surfaces as this guard's own refusal, never a bare
        # TimeoutError (see _lex_entry_tree_lock).
        with cls._lex_entry_tree_lock(negative, root):
            digest = build_inputs_digest()
        if not executable.is_file():
            with negative.tree_lock(root):
                # Residue → refusal: a killed predecessor can leave any
                # product source mutated, and building from a mutated tree
                # compiles the mutant into the binary the guards would then
                # trust. The lease also serializes cold callers of the
                # shared target directory.
                try:
                    negative._assert_workspace_sources(root, 'HEAD')
                except Exception as error:
                    raise AssertionError(
                        '#2115 the lex entry bootstrap refuses a tree whose build '
                        'inputs differ from HEAD — a killed negative control can '
                        'leave mutated sources behind, and rebuilding here would '
                        'quietly compile the mutant into the scan rules: '
                        + str(error)) from error
                # Another cold caller may have completed while this one waited.
                if not executable.is_file():
                    negative.wait_for_slot(root/'scripts/cargo-slots.sh',
                                           timeout=900, interval=15).check_returncode()
                    # --locked, not --offline: a cold registry (fresh CI
                    # runner) must be allowed to download exactly the committed
                    # Cargo.lock versions, while the lockfile itself stays
                    # pinned (same rationale as `_ensure_workspace_scanner`).
                    # cwd is the tree root so ./rust-toolchain.toml applies.
                    try:
                        built = negative.run_owned(
                            ['cargo', 'build', '--locked',
                             '--manifest-path', 'src/Cargo.toml',
                             '-p', 'pilotdeck-gateway',
                             '--example', 'lex_fn_items'],
                            cwd=root, env=environment, timeout=1800, idle_timeout=600)
                    except subprocess.TimeoutExpired as error:
                        raise AssertionError('#2115 building the lex entry exceeded '
                                             'its 1800 s budget (idle 600 s)') from error
                    if built.returncode != 0:
                        raise AssertionError('#2115 cannot build the lex entry: '
                                             + built.stderr[-4000:])
                    if not executable.is_file():
                        raise AssertionError('#2115 the lex entry build produced no '
                                             'executable under ' + str(target))
        # The request travels over stdin (contents, not paths). run_owned
        # takes no input, so the bytes go through a file redirect while the
        # run itself keeps run_owned's process-group ownership; the entry
        # spawns no children of its own.
        with tempfile.NamedTemporaryFile() as handle:
            handle.write(request)
            handle.flush()
            try:
                result = negative.run_owned(
                    ['bash', '-c', 'exec "$1" < "$2"', 'lex-entry',
                     str(executable), handle.name], timeout=300)
            except subprocess.TimeoutExpired as error:
                raise AssertionError('#2115 the lex entry exceeded its 300 s '
                                     'budget') from error
        if result.returncode != 0:
            raise AssertionError('#2115 the lex entry failed: '
                                 + result.stderr[-4000:])
        document = json.loads(result.stdout)
        if document.get('scanner_sha256') != digest:
            # fix-r2 (boundary P1): re-read the five summaries under the
            # tree lock before refusing. The first digest can be stale by
            # minutes at this point — the scan itself runs up to 300 s
            # after it — and transient bytes it caught (a plain-written
            # mutant no lease covered, residue a sibling restored
            # mid-flight) need not still be on disk. A binary that
            # matches the tree as it stands under the lock is not stale;
            # a refusal that survives must quote the digest it refused
            # against, so the owner can verify it.
            with cls._lex_entry_tree_lock(negative, root):
                digest = build_inputs_digest()
            if document.get('scanner_sha256') != digest:
                raise AssertionError(
                    '#2115 the lex entry binary does not match this source '
                    'tree (scanner_sha256 mismatch): the five build inputs '
                    'hash to ' + digest + ' under the tree lock while the '
                    'binary reports ' + str(document.get('scanner_sha256'))
                    + "; refusing to scan Rust names with stale rules — a "
                      "stale binary is the owner's job to rebuild. "
                      'From the tree root run: CARGO_TARGET_DIR=' + str(target)
                    + ' cargo build --locked --manifest-path src/Cargo.toml '
                      '-p pilotdeck-gateway --example lex_fn_items')
        return document

    @classmethod
    def call_sites(cls, text, name):
        # Line index of every `<name>(` in `text`, matched over the whole file
        # rather than line by line: a call whose name and `(` straddle a line
        # break would otherwise be invisible to both guards (#1900 concurrency
        # P3 — a fail-open scan in the check whose job is to catch silent skips).
        return [text.count('\n', 0, found.start())
                for found in re.finditer(rf'\b{re.escape(name)}\s*\(', text)]

    @staticmethod
    def fn_name_exists(record, leaf):
        # One matcher for both guards (#1900 simplify P3): the forward check and
        # the exception check must accept the same spellings, or a generic
        # `fn foo<T>(` passes one and fails the other. #2115: the spelling is
        # read from the proc-macro2 item records, so `fn /* c */ leaf(` and
        # `fn r#leaf(` count while a comment or a string literal holding the
        # same text does not. An unlexable file reports no items — existence
        # is not provable there, and the callers refuse.
        return 'error' not in record and any(item['name'] == leaf for item in record['fns'])

    @staticmethod
    def nextest_visible_name(crate, source, module, leaf):
        # #2115: `module` is the lexical `mod` nesting of the *declaring
        # record* (from the token inventory), replacing the text heuristics
        # the path alone forced before. nextest reports the path-derived
        # segments (src files) followed by the lexical segments followed by
        # the leaf; an integration-test file reports the lexical segments
        # followed by the leaf (measured with libtest `--list`: a `tests/*.rs`
        # with `mod first` reports `first::duplicated`).
        relative = source.relative_to(crate)
        if relative.parts[0] == 'src':
            parts = list(relative.relative_to('src').with_suffix('').parts)
            if parts[-1] == 'mod':
                parts.pop()
        else:
            # An integration-test file is its own crate: libtest reports the
            # lexical mod nesting followed by the leaf and NO file stem
            # (measured with `--list` on this crate's tests/live_e2e.rs and
            # tests/workspace_snapshot.rs — sibling #4: the old code ignored
            # the nesting and returned the bare leaf, so a file declaring
            # `mod first` could never have its nested tests named).
            parts = []
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
        # it is unmappable for the same reason. An integration-test file has
        # NO path-derived segments by design (its `parts` stays empty), so the
        # refusal must not fire there — only a `src/**` file can hit it.
        if relative.parts[0] == 'src' and (
                not parts or (len(parts) == 1 and parts[0] in ('lib', 'main'))):
            raise AssertionError(
                f'#922 cannot derive the nextest-visible name for {leaf} declared in '
                f'{relative}: a crate-root or bare-`mod` file reports a name that depends '
                f'on the module nesting, not on the path. Run `cargo nextest list` and '
                f'paste the exact name nextest prints.')
        return '::'.join([*parts, *module, leaf])

    @staticmethod
    def file_module_path(crate, source):
        # #1943: the module path a `use` statement in `source` resolves from,
        # as far as the file path proves it. `src/**` segments become the
        # crate path (`src/skills/tests.rs` → `skills::tests`, `mod.rs` pops);
        # an integration-test file is its own crate, so only its stem counts.
        relative = source.relative_to(crate)
        if relative.parts[0] == 'src':
            parts = list(relative.relative_to('src').with_suffix('').parts)
            if parts and parts[-1] == 'mod':
                parts.pop()
            return parts
        return [relative.with_suffix('').parts[-1]]

    @staticmethod
    def expand_module_path(segments, base_module):
        # #1943: expand `crate::`, `self` and `super::` chains of a use path
        # against the module it was declared in (file path + the lexical
        # `mod` nesting the inventory recorded for the statement). A bare
        # first segment stays crate-root-relative, which is how the flat
        # integration-test spellings (`use workspace_snapshot::helper;`)
        # match their home. `None` when a `super` pops past the crate root.
        segments = list(segments)
        if not segments:
            return None
        if segments[0] == 'crate':
            return segments[1:]
        module = list(base_module)
        while segments and segments[0] in ('self', 'super'):
            if segments[0] == 'super':
                if not module:
                    return None
                module = module[:-1]
            segments = segments[1:]
        return module + segments

    # #1900: the reverse direction of the check above. `stderr_skip_line` exists
    # so that a skip note survives the harness's output capture, but nextest
    # only replays the output of tests named in `nextest_success_output_filter`
    # (`success-output = "final"`). A call site outside that list is captured and
    # lost again — #1263's silent skip coming back through a test nobody
    # registered. Key is `path::fn`; the value is (why it needs no entry, the
    # filter entries that cover it[, relay helpers the note also travels
    # through]). Coverage is checked against the filter, so a stale "covered
    # by" cannot rot silently, and an entry whose call site is gone fails as
    # stale. For a helper entry the guard also walks every caller: direct
    # callers and callers of each declared relay both have to be registered,
    # so the `covered` list is a claim the guard keeps true rather than a list
    # of the callers the author happened to remember.
    #
    # Known blind spots — collected by #1937: the hand-rolled writer
    # (`tests/unsafe_boundary.rs`'s `stderr_boundary_line`) now delegates to
    # the shared writer and is registered below like any other helper, and
    # the four `#1339 skipped:` `eprintln!` macro writes in
    # `channel/sms/tests.rs` go through the writer too. The four `#2691
    # skipped:` `eprintln!` writes in the same file (the IPv6/port-busy
    # probes) are still hand-rolled — neither this scan nor the pairing
    # walk sees them, because they never call the writer; converting them
    # is registered follow-up work, and until then this "none left" claim
    # is scoped to writer call sites. Anything new should call the shared
    # writer, not copy it.
    skip_line_exceptions = {
        'src/module_host/tests.rs::process_audit_is_linux_only': (
            '#1349 `#[cfg(not(target_os = "linux"))]`: the test does not exist in the '
            'Linux CI job at all, so no nextest entry can name it', ()),
        'src/native_process/review_tests.rs::session_writer_audit_is_linux_only': (
            '#1316 `#[cfg(not(target_os = "linux"))]`: the test does not exist in the '
            'Linux CI job at all, so no nextest entry can name it', ()),
        'src/handle_path.rs::handle_path_rejects_a_non_utf8_name': (
            '#1990 `#[cfg(target_os = "macos")]`: the test does not exist in the Linux '
            'CI job at all, so no nextest entry can name it. The note is still visible '
            'where the test does run — the macOS host run is `cargo test` (libtest), and '
            '`stderr_skip_line` writes the stderr handle directly, which libtest does not '
            'capture', ()),
        'tests/common/permission_faults.rs::skip_or_require': (
            '#1937 helper: the ONE skip/require pair — the note is printed and the '
            'requirement asserted on behalf of the test that called it', (),
            # The pair is also reached through the skills helpers, which live
            # beside their callers rather than beside the pair (#1937: a
            # relay may be declared with its own file). Naming only the
            # callers the author remembered would make the covered list a
            # claim, not a check (#1900 boundary P2).
            ('src/skills/tests.rs::permission_not_run',
             'src/skills/tests.rs::replay')),
        'tests/workspace_snapshot.rs::replay_scenario': (
            '#1263 helper: the note is printed on behalf of the scenario it replays',
            ('scenarios_match_ts_outcomes_and_trees',)),
        'tests/unsafe_boundary.rs::stderr_boundary_line': (
            '#448 helper: the boundary note is printed on behalf of the test that '
            'calls it — the #1937 collection, the same shared writer as every '
            'other skip line',
            ('compiler_enforces_gateway_boundary',),
            ('cargo_invocation_boundary',)),
    }
    defining_function = re.compile(
        r'^\s*(?:pub(?:\([^)]*\))?\s+)?(?:const\s+)?(?:async\s+)?(?:unsafe\s+)?'
        r'(?:extern\s+"[^"]*"\s+)?fn\s+([A-Za-z0-9_]+)\s*[<(]')

    # #1937: the pairing registry. #1900's reverse guard answers "is every
    # skip line named in the success-output filter"; the pairing check below
    # answers the second question — "is every skip line paired with its
    # `require_permission_faults()` assert". After the consolidation the pair
    # is written once, in `skip_or_require`; every OTHER fn that touches one
    # half of the pair must be registered here as not a permission-fault
    # contract at all (an environment probe, a boundary acknowledgement, a
    # platform gate), because a skip with no require is a silently-green
    # privileged-host arm and a require with no skip loses its reason.
    permission_fault_pair_exceptions = {
        'src/channel/sms/tests.rs::dual_stack_probe_uses_the_assigned_port': (
            '#1339 an environment-probe note (the host has no IPv6), not a '
            'permission-fault contract — there is no unprivileged-host arm to '
            'require'),
        'src/channel/sms/tests.rs::port_busy_on_ipv6_preflight_fails_fast': (
            '#1339 an environment-probe note (the port is busy), not a '
            'permission-fault contract — there is no unprivileged-host arm to '
            'require'),
        'src/native_process/tests.rs::non_utf8_home_and_sidecar_are_refused': (
            '#2036 an environment-probe note (the filesystem accepts non-UTF-8 '
            'file names), not a permission-fault contract'),
        'src/runtime/cron/tests.rs::migration_non_utf8_home_keeps_one_physical_key': (
            '#1525 an environment-probe note (the filesystem accepts non-UTF-8 '
            'file names), not a permission-fault contract'),
        'src/runtime/tools/builtin/grep.rs::files_sort_keeps_submillisecond_mtime_precision': (
            'an environment-probe note (the filesystem does not preserve '
            'sub-millisecond mtimes), not a permission-fault contract'),
        'src/module_host/tests.rs::process_audit_is_linux_only': (
            '#1349 a platform-gate note (`#[cfg(not(target_os = "linux"))]`), not '
            'a permission-fault contract'),
        'src/native_process/review_tests.rs::session_writer_audit_is_linux_only': (
            '#1316 a platform-gate note (`#[cfg(not(target_os = "linux"))]`), not '
            'a permission-fault contract'),
        'src/handle_path.rs::handle_path_rejects_a_non_utf8_name': (
            '#1990 a platform-gate note (`#[cfg(target_os = "macos")]`), not a '
            'permission-fault contract'),
        'tests/grep_tool.rs::grep_outputs_match_real_ts': (
            'an environment-probe note (PDRS_ALLOW_NO_RG unset, no ripgrep to '
            'compare against), not a permission-fault contract'),
        'tests/write_tools.rs::case_insensitive_cwd_spelling_writes_and_edits': (
            'an environment-probe note (the volume is case-sensitive), not a '
            'permission-fault contract'),
        'tests/write_tools.rs::nfd_directory_with_nfc_cwd_writes_and_edits': (
            'an environment-probe note (the volume does not distinguish '
            'NFC/NFD), not a permission-fault contract'),
        'tests/workspace_snapshot.rs::replay_scenario': (
            '#1263 a scenario-replay note, not a permission-fault contract'),
        'tests/unsafe_boundary.rs::stderr_boundary_line': (
            '#448 a boundary-acknowledgement note, not a permission-fault '
            'contract'),
    }

    def assert_every_skip_line_reaches_nextest(self, filter_text, crate=None, sources=None):
        # #1900: forward check = every filter entry names a real test. This is
        # the mirror: every test that prints a skip line must be named by the
        # filter (or be a documented exception), otherwise the note it writes to
        # the stderr handle is thrown away with the rest of the captured output.
        # A covering test has to live in the call site's own file: the existence
        # check below is file-local, so an exception that points somewhere else
        # fails loudly (false red) instead of passing quietly.
        crate = crate or Path(__file__).resolve().parents[1]
        sources = sources if sources is not None else self.read_crate_sources(crate)
        inventory = self._lex_fn_items(sources)
        # Only the shared writer's own *definition* line is exempt: it declares
        # `stderr_skip_line`, so the signature line matches the scan. Excluding
        # the whole file — or every file named `skip_line.rs` — silently
        # excused a second call site written next to the writer (#1900 simplify
        # P3); the exemption is the definition site, nothing else.
        writer = crate/'tests'/'common'/'skip_line.rs'
        named = {entry[1:] for entry in re.findall(r'test\(([^)]*)\)', filter_text)
                 if entry.startswith('=')}
        seen = set()
        unregistered = []
        for source in sorted(sources):
            text = sources[source]
            # Split on `\n` exactly as `call_sites` counts them, so the index
            # cannot drift on a file holding a ` `-style separator
            # (#1900 simplify P3).
            lines = text.split('\n')
            record = inventory[source]
            for index in self.call_sites(text, 'stderr_skip_line'):
                if source == writer and self.defining_function.match(lines[index]):
                    continue
                owner = self.defining_function_at(lines, index)
                key = f'{source.relative_to(crate)}::{owner}'
                seen.add(key)
                exception = self.skip_line_exceptions.get(key)
                if exception is not None:
                    self.assertIn(len(exception), (2, 3),
                                  f'#1900 the exception for {key} is (why, covered[, relays])')
                    reason, covered = exception[0], exception[1]
                    relays = tuple(exception[2]) if len(exception) > 2 else ()
                    self.assertTrue(reason, f'#1900 the exception for {key} states why')
                    # #1900 fix-r1 codex P2: the key carries the file and the
                    # *leaf* fn name only, so a second function of the same
                    # name in another `mod` of the same file would inherit this
                    # exception without anyone registering it (measured: the
                    # guard stayed green for such a duplicate). The register
                    # excuses one call site; refuse the ambiguous file instead
                    # of silently excusing all of them. #2115: the count is
                    # taken from the proc-macro2 item records, so the shapes
                    # the literal regex misread now count — a comment between
                    # `fn` and the name, an `r#` prefix, a second definition
                    # inside a `macro_rules!` body — while an `fn x(` inside a
                    # comment or a string literal no longer inflates it. The
                    # file must also be provably enumerable: a lex failure, a
                    # real `include!` token (another file's items spliced in)
                    # or a metavariable-fn macro invoked with this name can
                    # add an uncounted same-named fn, and a register entry
                    # must not be provable on such a file.
                    self.assertNotIn('error', record,
                                     f'#1900 the exception for {key} is keyed inside a file '
                                     f'the lexer refuses: {record.get("error")}')
                    definitions = [item for item in record['fns']
                                  if item['name'] == owner]
                    self.assertEqual(
                        len(definitions), 1,
                        f'#1900 the exception for {key} is keyed by a function name that '
                        f'occurs {len(definitions)} times in {source.relative_to(crate)}; a '
                        f'duplicate would inherit the exception unregistered — key the '
                        f'exception by module path or rename the duplicate')
                    self.assertNotIn('include!', record['unresolved'],
                                     f'#1900 the exception for {key} is keyed inside a file '
                                     f'that splices other items in via include!, so its fn '
                                     f'set cannot be enumerated')
                    instantiable = owner in {name for idents in record['macro_calls'].values()
                                             for name in idents}
                    self.assertFalse(
                        instantiable,
                        f'#1900 the exception for {key} is keyed by a name a '
                        f'metavariable-fn macro can instantiate in '
                        f'{source.relative_to(crate)}, so a second same-named fn can appear '
                        f'there that no register entry covers')
                    for name in covered:
                        self.assertIn(name, named,
                                      f'#1900 the exception for {key} claims {name} covers it, '
                                      f'but the filter does not name that test')
                        # #1900 boundary P2: a filter entry is necessary but not
                        # sufficient — the covering test has to still exist, or
                        # a rename would leave the exception excusing nothing
                        # while the guard stays green. #2115 (sibling #1): the
                        # claim is stronger than existence — the covered name
                        # has to be the nextest-visible name of one of the
                        # file's real records, or the entry still selects
                        # nothing.
                        leaf = name.rsplit('::', 1)[-1]
                        visibles = {self.nextest_visible_name(crate, source, item['mod'], leaf)
                                    for item in record['fns'] if item['name'] == leaf}
                        self.assertTrue(
                            name in visibles,
                            f'#1900 the exception for {key} claims {name} covers it, but no '
                            f'record of {source.relative_to(crate)} is nextest-visible under '
                            f'that name ({sorted(visibles)})')
                    # #1900 boundary P2: a helper's covered list is hand-kept, so
                    # on its own it proves nothing about the callers the author
                    # did not think of. Walk the callers — of the helper itself
                    # (a *direct* caller needs registering just as much as one
                    # that goes through a declared relay) and of every declared
                    # relay. The walk starts at the helper unless the exception
                    # is for a test, which has no callers.
                    if not self.is_test_fn(lines, self.defining_line_at(lines, index)):
                        self.assert_relay_callers_are_named(
                            crate, sources, inventory, source, key, (owner, *relays), named)
                    continue
                # #2115 (sibling #3): a same-named record is indistinguishable
                # from the call site's owner by key, so every record of that
                # name must be named, not just the one the line mapping found.
                # fix-r1 (DS boundary #3): an owner the token records cannot
                # prove (zero same-named fn records — a text-mapped ghost,
                # e.g. an `fn` inside a raw-string fixture above the call)
                # used to pass silently; unprovability is a demand, not an
                # exemption by absence.
                matching = [item for item in record['fns'] if item['name'] == owner]
                if not matching:
                    unregistered.append(
                        f'{key} (no fn record of {source.relative_to(crate)} is named '
                        f'{owner}; the line-based owner cannot be proven)')
                for item in matching:
                    visible = self.nextest_visible_name(crate, source, item['mod'], owner)
                    if visible not in named:
                        unregistered.append(f'{key} (nextest-visible {visible})')
        self.assertFalse(
            unregistered,
            '#1900 no nextest success-output entry covers these skip-line call sites, so the '
            'note they print is captured and lost: ' + ', '.join(unregistered)
            + '. Either add the nextest-visible name to nextest_success_output_filter or add a '
              'skip_line_exceptions entry saying why no entry can exist.')
        # Only the entries that point into the crate being scanned can go
        # stale; an entry for a file this crate does not have (the controls
        # below scan a temporary crate) is simply not in play.
        applicable = {key for key in self.skip_line_exceptions
                      if (crate/key.split('::')[0]).exists()}
        stale = sorted(applicable - seen)
        self.assertEqual(stale, [],
                         '#1900 these skip_line_exceptions entries no longer match a '
                         'stderr_skip_line call site, so they would excuse nothing: '
                         + ', '.join(stale))

    def assert_skip_lines_pair_their_requirement(self, crate=None, sources=None):
        # #1937: the second question the reverse guard does not answer — is
        # every skip line paired with its `require_permission_faults()`
        # assert? A root arm that copies only half of the pair drifts green
        # exactly the way #1263 removed for unregistered skips: a skip with
        # no require silently skips on a privileged host, and a require with
        # no skip fires (or passes) with its reason lost. The consolidation
        # leaves exactly one hand-written pair — `skip_or_require` in
        # `tests/common/permission_faults.rs` — so every OTHER fn that calls
        # the shared writer or asserts the requirement has to be registered
        # in `permission_fault_pair_exceptions` as not a permission-fault
        # contract. The scan is token-level (the same inventory the other
        # name checks use), so a mention inside a comment or a string cannot
        # fake either half.
        crate = crate or Path(__file__).resolve().parents[1]
        sources = sources if sources is not None else self.read_crate_sources(crate)
        inventory = self._lex_fn_items(sources)
        pair = crate/'tests'/'common'/'permission_faults.rs'
        unpaired, lost, seen = [], [], set()
        for source in sorted(sources):
            record = inventory[source]
            if 'error' in record:
                self.fail(f'#1937 the pairing walk refuses a file that does not lex: '
                          f'{record["error"]}')
            # fix-r1 (DS boundary #2): `_owners_of_call` reads only the calls
            # the lexer could attribute to a fn, so a half-pair written inside
            # a macro body (or outside any fn) was invisible to BOTH halves —
            # the exact "print the skip, never require" arm this guard exists
            # to catch. Refuse the spelling itself, the same refusal
            # `_cross_file_relay_callers` makes for a relay it cannot
            # attribute.
            for name in ('stderr_skip_line', 'require_permission_faults'):
                for spelling in sorted(record['unattributed_calls']):
                    if spelling.rsplit('::', 1)[-1] == name:
                        self.fail(
                            f'#1937 the pairing walk refuses {source.relative_to(crate)} '
                            f'calling `{spelling}` where the lexer cannot attribute a '
                            f'caller (macro input or no enclosing fn); route the arm '
                            f'through tests/common/permission_faults.rs::skip_or_require, '
                            f'or reshape the file so the call site is inside the fn '
                            f'that owns it')
            for owner in sorted(self._owners_of_call(record, 'stderr_skip_line')):
                if source == pair and owner == 'skip_or_require':
                    continue
                key = f'{source.relative_to(crate)}::{owner}'
                seen.add(key)
                if key not in self.permission_fault_pair_exceptions:
                    unpaired.append(key)
            for owner in sorted(self._owners_of_call(record, 'require_permission_faults')):
                if source == pair and owner == 'skip_or_require':
                    continue
                key = f'{source.relative_to(crate)}::{owner}'
                seen.add(key)
                if key not in self.permission_fault_pair_exceptions:
                    lost.append(key)
        self.assertTrue(
            unpaired == [],
            '#1937 these fns print a skip line with no require_permission_faults '
            'assert, so a privileged host skips them silently green: '
            + ', '.join(unpaired)
            + '. Route the arm through tests/common/permission_faults.rs::'
              'skip_or_require, or register the note as not a permission-fault '
              'contract in permission_fault_pair_exceptions.')
        self.assertTrue(
            lost == [],
            '#1937 these fns assert the permission-fault requirement without '
            'printing a skip note, so the reason is lost: ' + ', '.join(lost)
            + '. Route the arm through tests/common/permission_faults.rs::'
              'skip_or_require, or register it in permission_fault_pair_exceptions.')
        # fix-r1 (DS sibling #3): the same stale-entry refusal the reverse
        # guard makes for skip_line_exceptions — an entry that no longer
        # matches a call site of either half excuses nothing, and a registry
        # that only grows is exactly the "excuse nothing" rot the stale check
        # exists to catch. Only entries that point into the crate being
        # scanned can go stale (the controls below patch in a synthetic one).
        applicable = {key for key in self.permission_fault_pair_exceptions
                      if (crate/key.split('::')[0]).exists()}
        self.assertEqual(
            sorted(applicable - seen), [],
            '#1937 these permission_fault_pair_exceptions entries no longer match a '
            'call site of either half, so they would excuse nothing: '
            + ', '.join(sorted(applicable - seen)))

    @staticmethod
    def _owners_of_call(record, name):
        # #1937: the fns of one file that call `name` — bare, or through any
        # path spelling (`crate::…::name`, `module::name`). The caller set of
        # a qualified spelling is the inventory's own answer, not a lookup of
        # the spelling among the bare-call tables.
        owners = set(record.get('calls', {}).get(name, ()))
        for spelling, callers in record.get('qualified_calls', {}).items():
            if spelling.rsplit('::', 1)[-1] == name:
                owners |= set(callers)
        return owners

    def assert_relay_callers_are_named(self, crate, sources, inventory, source, key, relays,
                                        named):
        # #1900 boundary P2: `key` is a helper that prints on someone else's
        # behalf. Every step in `relays` (the helper itself, plus any declared
        # relay) must still be called, and every test that reaches the note —
        # directly or through a relay — has to be in the filter, or the note is
        # silently captured when that test runs. Colour-coding the `covered`
        # list by hand is what this replaces.
        #
        # #1943: the walk used to stop at the helper's own file, so a caller
        # in another file was invisible (#1263's silent skip coming back
        # through a test nobody registered). The whole-crate scan below is
        # token-level for the same reason every other name check here is: the
        # text regex matches `replay (` inside doc comments, and same-named
        # helpers of other modules (`use model_mock::*;` then a bare
        # `replay(…)`) are not this relay. A call site in another file counts
        # only when that file provably reaches *this* relay: it has no own
        # `fn <relay>` item (a local definition answers bare sites; importing
        # over one is an E0255 collision), and either a use binding that
        # spells the home item path or a qualified call whose leading segment
        # resolves to it. The registered limitation is the mirror image: a
        # wildcard import (`use home::*;`) or a module-alias path call
        # (`use home as h; h::relay(`) is not matched, and a macro-generated
        # import is collected but its expansion site is not — those shapes
        # stay less covered than an explicit binding, which is the shape a
        # real cross-file caller needs anyway.
        #
        # #1937: a relay may live in the helper's own file (a bare name) or
        # in another one (`'path/to/file.rs::name'`) — the pairing helper's
        # relays sit beside their callers, not beside the pair. A discovered
        # intermediate caller matches a declared relay only when the file it
        # was found in is that relay's home, so a same-named helper of a
        # foreign file is demanded rather than walked through.
        owner = key.rsplit('::', 1)[-1]

        def relay_home(spelling):
            if '::' in spelling:
                path, name = spelling.rsplit('::', 1)
                home = crate/path
                if not home.is_file():
                    self.fail(f'#1937 the exception for {key} declares the relay '
                              f'{spelling}, but {path} is not a file in this crate')
                return name, home
            return spelling, source

        pending = list(dict.fromkeys([relay_home(spelling)
                                      for spelling in (owner, *relays)]))
        declared = {relay_home(spelling) for spelling in relays}
        visited = set()
        while pending:
            relay, home = pending.pop()
            if (relay, home) in visited:
                continue
            visited.add((relay, home))
            # In-file callers: the line-based walk that also feeds
            # `entry_point_at`.
            text = sources[home]
            lines = text.split('\n')
            callers = {}
            for index in self.call_sites(text, relay):
                match = self.defining_function.match(lines[index])
                # #1943: skip only the relay's OWN definition line. The bare
                # `defining_function.match` also skipped a single-line
                # `fn caller() { relay(); }` — the definition line of a
                # *caller* — hiding that caller from the walk.
                if match and match.group(1) == relay:
                    continue
                callers.setdefault(self.entry_point_at(lines, index), index)
            # #1943: cross-file callers of the same relay, from the token
            # inventory. A relay only called from other files is as called as
            # one with in-file callers — the "nothing calls it" refusal must
            # see both halves before it fires.
            foreign = self._cross_file_relay_callers(crate, inventory, home, key, relay)
            self.assertTrue(
                callers or foreign,
                f'#1900 the exception for {key} expects the note to travel through '
                f'`{relay}(`, but nothing in the crate calls it any more')
            record = inventory[home]
            for caller in sorted(callers):
                if (caller, home) in declared:
                    pending.append((caller, home))
                    continue
                via = 'directly' if relay == owner else f'through {relay}'
                for visible in self._visible_names_of(crate, home, record, caller):
                    self.assertIn(
                        visible, named,
                        f'#1900 the exception for {key} only holds while every caller is '
                        f'registered too: {caller} reaches the note {via}, but '
                        f'`{visible}` is not in the success-output filter')
            for other, others in sorted(foreign.items()):
                other_record = inventory[other]
                for caller in others:
                    if (caller, other) in declared:
                        pending.append((caller, other))
                        continue
                    via = 'directly' if relay == owner else f'through {relay}'
                    for visible in self._visible_names_of(crate, other, other_record, caller):
                        self.assertIn(
                            visible, named,
                            f'#1943 the exception for {key} only holds while every caller is '
                            f'registered too: {caller} in {other.relative_to(crate)} reaches '
                            f'the note {via}, but `{visible}` is not in the success-output '
                            f'filter')

    def _cross_file_relay_callers(self, crate, inventory, source, key, relay):
        # #1943: every file that provably reaches *this* relay (declared in
        # `source`) and the fns that call it there, as {file: sorted caller
        # names}. A file that cannot be scanned at all is refused rather than
        # skipped (fail closed on lex errors and include!) — the walk's whole
        # job is to be complete.
        #
        # #1937: the home item is no longer a single path. A shared file
        # pulled in under several module names (`mod permission_faults;` in
        # the integration crates, `mod test_permission_faults;` at the lib
        # root) is reachable under each of those spellings, so every
        # candidate the crate's `mod name;` declarations prove counts. And a
        # qualified call's leading segment can be one of those declared
        # modules — a use binding is no longer the only way a flat
        # `permission_faults::skip_or_require(…)` resolves.
        home_items = {tuple(candidate) for candidate
                      in self._home_item_candidates(crate, inventory, source, relay)}
        foreign = {}
        for other, other_record in sorted(inventory.items()):
            if other == source:
                continue
            if 'error' in other_record:
                self.fail(f'#1943 the exception for {key} walks the whole crate, and '
                          f'{other.relative_to(crate)} does not lex: '
                          f'{other_record["error"]}')
            if 'include!' in other_record['unresolved']:
                self.fail(f'#1943 the exception for {key} walks the whole crate, and '
                          f'{other.relative_to(crate)} splices other items in via '
                          f'include!, so its callers cannot be enumerated')
            if any(marker in other_record['unresolved']
                   for marker in ('mod path attribute', 'mod declaration in macro input')):
                self.fail(f'#1937 the exception for {key} walks the whole crate, and '
                          f'{other.relative_to(crate)} declares a module whose file the '
                          f'walker cannot resolve, so its callers cannot be enumerated')
            if any(item['name'] == relay for item in other_record['fns']):
                # A local `fn <relay>` answers this file's bare sites.
                continue
            module = self.file_module_path(crate, other)
            local_mods = {decl['name']: self._decl_module_base(crate, other, decl)
                          for decl in other_record.get('mod_decls', ())}
            # A use binding that imports the relay item itself: the fns that
            # call the bound name reach the home item.
            owners = set()
            for binding in other_record['uses']:
                expanded = self.expand_module_path(binding['source'],
                                                   [*module, *binding['module']])
                if expanded is not None and tuple(expanded) in home_items:
                    owners |= set(other_record['calls'].get(binding['site'], ()))
            # A qualified call resolves through a use binding or a declared
            # module of this file (or the `crate` prefix); the spelling's own
            # caller set — `qualified_calls` — is the demand.
            for spelling, callers in other_record['qualified_calls'].items():
                segments = spelling.split('::')
                resolved = None
                if segments[0] == 'crate':
                    resolved = segments[1:]
                else:
                    for binding in other_record['uses']:
                        if binding['site'] != segments[0]:
                            continue
                        expanded = self.expand_module_path(
                            binding['source'], [*module, *binding['module']])
                        if expanded is not None:
                            resolved = [*expanded, *segments[1:]]
                            break
                    else:
                        if segments[0] in local_mods:
                            resolved = [*local_mods[segments[0]], *segments[1:]]
                if resolved is not None and tuple(resolved) in home_items:
                    owners |= set(callers)
            if relay in other_record['unattributed_calls']:
                self.fail(f'#1943 the exception for {key} walks the whole crate, and '
                          f'{other.relative_to(crate)} calls `{relay}` where the walker '
                          f'cannot attribute a caller (macro input or no enclosing fn); '
                          f'register the caller or reshape the file')
            if owners:
                foreign[other] = sorted(owners)
        return foreign

    def _home_item_candidates(self, crate, inventory, source, relay):
        # #1937: every item path this file's `relay` is reachable under. The
        # naive one comes from the file's own location; every `mod name;`
        # declaration in the crate whose target is this file contributes the
        # module path of that declaration too, because a caller spelling that
        # path reaches the same item. Over-approximating is the safe
        # direction here — an extra candidate can only demand more callers —
        # but an unreadable declaration is refused: a missing candidate would
        # hide a caller.
        candidates = [[*self.file_module_path(crate, source), relay]]
        for other, other_record in sorted(inventory.items()):
            if 'error' in other_record:
                self.fail(f'#1937 the walk needs every file\'s module declarations, and '
                          f'{other.relative_to(crate)} does not lex: '
                          f'{other_record["error"]}')
            if any(marker in other_record['unresolved']
                   for marker in ('mod path attribute', 'mod declaration in macro input')):
                self.fail(f'#1937 the walk needs every file\'s module declarations, and '
                          f'{other.relative_to(crate)} declares a module whose file the '
                          f'walker cannot resolve')
            for decl in other_record.get('mod_decls', ()):
                if self._mod_decl_target(crate, other, decl) == source:
                    candidates.append([*self._decl_module_base(crate, other, decl), relay])
        return candidates

    def _mod_decl_target(self, crate, other, decl):
        # The file a `mod name;` declaration loads. `#[path]` is literal and
        # relative to the declaring file's directory; without it the module
        # loads from the module's own directory — a sibling of the declaring
        # file when that file is a crate root, a subdirectory of it otherwise.
        if decl.get('path') is not None:
            return Path(os.path.normpath(other.parent/decl['path']))
        relative = other.relative_to(crate)
        if relative.parts[0] == 'src':
            root = len(relative.parts) == 2 and relative.parts[1] in ('lib.rs', 'main.rs')
        else:
            root = len(relative.parts) == 2
        directory = other.parent if root or other.name == 'mod.rs' else other.parent/other.stem
        return Path(os.path.normpath(
            directory.joinpath(*decl['module'], decl['name'] + '.rs')))

    def _decl_module_base(self, crate, other, decl):
        # The module path a `mod name;` declaration creates — the path a
        # caller's `crate::…` spelling or a flat integration-crate module
        # name reaches. A crate-root file (src/lib.rs, an integration test)
        # contributes no path segments of its own; any other file contributes
        # its path segments plus the declaration's lexical nesting.
        relative = other.relative_to(crate)
        if relative.parts[0] == 'src':
            root = len(relative.parts) == 2 and relative.parts[1] in ('lib.rs', 'main.rs')
        else:
            root = len(relative.parts) == 2
        base = [] if root else self.file_module_path(crate, other)
        return [*base, *decl['module'], decl['name']]

    @classmethod
    def _visible_names_of(cls, crate, source, record, leaf):
        # #2115: every nextest-visible name a same-named record of `leaf` in
        # `source` can report — the union the registration checks demand
        # (sibling #3: same-named records are indistinguishable by key, so
        # all of them have to be named).
        return {cls.nextest_visible_name(crate, source, item['mod'], leaf)
                for item in record['fns'] if item['name'] == leaf}

    @classmethod
    def defining_line_at(cls, lines, index):
        for back in range(index, -1, -1):
            if cls.defining_function.match(lines[back]):
                return back
        raise AssertionError(
            f'#1900 cannot map the stderr_skip_line call on line {index + 1} to a function')

    @classmethod
    def entry_point_at(cls, lines, index):
        # Which item does the caller belong to? Usually the innermost `fn`, but
        # a *nested* item — `impl Drop for Restore { fn drop(&mut self) { … } }`
        # declared inside a test body — is not an entry point of its own: the
        # note is printed while that test runs, so the test is what has to be
        # registered. Rustfmt puts top-level items in column 0 and nested items
        # indented, so the enclosing item is the first non-blank, unindented
        # line above. Attribute upwards only when that item really is a test;
        # otherwise fail closed on the nested name, so a note printed from a
        # top-level `impl` method still has to be registered on its own.
        definition = cls.defining_line_at(lines, index)
        name = cls.defining_function.match(lines[definition]).group(1)
        if not lines[definition][:1].isspace():
            return name
        for back in range(definition - 1, -1, -1):
            line = lines[back]
            if not line.strip() or line[:1].isspace():
                continue
            match = cls.defining_function.match(line)
            if match and cls.is_test_fn(lines, back):
                return match.group(1)
            break
        return name

    @classmethod
    def is_test_fn(cls, lines, index):
        # Is the `fn` at `index` a `#[test]` / `#[tokio::test]` rather than a
        # helper? An exception for a test has no callers to walk (the two
        # `#[cfg(not(target_os = "linux"))]` entries); an exception for a
        # helper does. Reading the attributes is also what tells them apart:
        # a Linux-only test is not in the filter either, so "is it named" would
        # misclassify it as a helper.
        back = index - 1
        while back >= 0:
            stripped = lines[back].strip()
            if not stripped or stripped.startswith('//'):
                back -= 1
                continue
            if not stripped.startswith('#['):
                return False
            if re.match(r'#\[(?:tokio::)?test\b', stripped):
                return True
            back -= 1
        return False

    @classmethod
    def defining_function_at(cls, lines, index):
        # Walk back to the `fn` that encloses the call. Both the scan above and
        # the negative control rely on this mapping, so it fails loudly instead
        # of guessing a key that would silently excuse a call site.
        match = cls.defining_function.match(lines[cls.defining_line_at(lines, index)])
        return match.group(1)

    def test_lex_fn_items_reports_tokens_not_text(self):
        # #2115 controls for the lexing entry every Rust-name check in this
        # driver now goes through. Each shape below is one the old literal
        # regex `\bfn\s+<name>\s*[(<]` miscounted in one direction or the
        # other; the assertions are on the record set, never on line text.
        inventory = self._lex_fn_items({
            'tokens.rs': '\n'.join([
                'fn plain() {}',
                'fn /* a comment between the keyword and the name */ duplicated() {}',
                'fn r#duplicated() {}',
                '// fn commented_out() {}',
                'const NOTE: &str = "fn in_a_string(";',
                'mod inner { fn nested() {} }',
            ]),
            'macro.rs': '\n'.join([
                'macro_rules! gen { () => { fn generated() {} } }',
                'macro_rules! stamp { ($name:ident) => { fn $name() {} } }',
                'gen!();',
                'stamp!(duplicated);',
            ]),
            'broken.rs': 'fn broken( {',
        })
        # Missed by the regex: a comment between `fn` and the name, and the
        # `r#` prefix, are both ordinary tokens — exactly two records of
        # `duplicated`, in declaration order.
        self.assertEqual([item['name'] for item in inventory['tokens.rs']['fns']],
                         ['plain', 'duplicated', 'duplicated', 'nested'])
        # False positives the regex counted: a comment and a string literal
        # hold no tokens, so they hold no `fn` items.
        self.assertEqual([item['macro'] for item in inventory['tokens.rs']['fns']],
                         [False, False, False, False])
        # The module path is lexical, so a nested item carries the segments
        # nextest reports after the path-derived ones.
        self.assertEqual(inventory['tokens.rs']['fns'][-1]['mod'], ['inner'])
        # A named fn inside a macro body only exists after expansion: it is
        # reported (conservatively, in the file that defines the macro) with
        # the macro flag set, so consumers can require more than its count.
        self.assertEqual([item['name'] for item in inventory['macro.rs']['fns']],
                         ['generated'])
        self.assertTrue(all(item['macro'] for item in inventory['macro.rs']['fns']))
        # A metavariable name is not enumerable: the file is marked
        # unresolved, and the invocation's argument idents are joined against
        # the metavariable-fn macros this batch defines, so a consumer can
        # refuse exactly the names this file can instantiate — `generated`
        # comes from a macro body, `duplicated` from an argument ident, and
        # only the latter says this file can gain a fn of that name.
        self.assertIn('macro fn name', inventory['macro.rs']['unresolved'])
        self.assertEqual(inventory['macro.rs']['macro_calls'], {'stamp': ['duplicated']})
        # Fail closed: a file that does not lex reports an error, never a
        # best-effort fn list.
        self.assertIn('error', inventory['broken.rs'])

    def test_lex_entry_refuses_a_stale_binary(self):
        # fix-r1 (DS concurrency #1): an existing binary whose embedded
        # rules do not match this tree must be refused with the rebuild
        # command in the message. The old `for rebuild in (False, True)`
        # loop rebuilt in the test path instead — a killed negative control
        # leaving mutated sources behind would have had the mutant quietly
        # compiled into the very rules the guards then trust. The fake
        # executable answers with a foreign scanner_sha256, so no cargo is
        # involved in either verdict.
        with tempfile.TemporaryDirectory() as target:
            executable = Path(target)/'debug/examples/lex_fn_items'
            executable.parent.mkdir(parents=True)
            executable.write_text(
                '#!/bin/sh\n'
                'printf \'{"scanner_sha256": "not-this-tree"}\\n\'\n')
            executable.chmod(0o755)
            with patch.dict(os.environ, {'CARGO_TARGET_DIR': target}):
                with self.assertRaisesRegex(
                        AssertionError,
                        r"scanner_sha256 mismatch.*owner's job to rebuild.*"
                        r'cargo build --locked --manifest-path src/Cargo.toml'):
                    self._lex_fn_items({'probe.rs': 'fn probe() {}\n'})

    def test_lex_entry_bootstrap_refuses_a_mutated_tree(self):
        # fix-r1 (DS concurrency #1): a cold bootstrap on a tree whose build
        # inputs differ from HEAD must refuse BEFORE any slot wait or cargo
        # launch, and say why — residue is a refusal, never a
        # rebuild-and-continue. The pristine check is the shared
        # `_assert_workspace_sources` (intercepted here: this test process
        # must not mutate the real tree); the end-to-end shape, real
        # mutation and all, is the mutation control bound to this test.
        negative = self._lex_entry_process()

        def no_launch(*args, **kwargs):
            self.fail('#2115 the lex entry bootstrap must not launch cargo or '
                      'wait for a slot on a mutated tree')

        with tempfile.TemporaryDirectory() as target:
            with patch.dict(os.environ, {'CARGO_TARGET_DIR': target}):
                with patch.object(negative, '_assert_workspace_sources',
                                  side_effect=RuntimeError(
                                      'src/pilotdeck-gateway/examples/lex_fn_items.rs '
                                      'differs from HEAD')):
                    with patch.object(negative, 'run_owned', side_effect=no_launch), \
                         patch.object(negative, 'wait_for_slot', side_effect=no_launch):
                        with self.assertRaisesRegex(
                                AssertionError,
                                'bootstrap refuses a tree whose build inputs '
                                'differ from HEAD.*quietly compile the mutant'):
                            self._lex_fn_items({'probe.rs': 'fn probe() {}\n'})

    def test_lex_entry_admits_only_the_shared_target_rule(self):
        # fix-r1 (DS concurrency #3): the target directory must go through
        # negative_process's ONE shared admission, never a second local
        # interpretation — the old code read os.environ itself and would
        # have accepted a blank, dot, repository-root or src-inside value
        # (and resolved a relative one against the caller's cwd, which is
        # not the cwd cargo sees).
        root = Path(__file__).resolve().parents[3]
        for bad in ('', '.', 'src/target', str(root)):
            with patch.dict(os.environ, {'CARGO_TARGET_DIR': bad}):
                with self.assertRaisesRegex(
                        AssertionError,
                        '#2115 the lex entry refuses this CARGO_TARGET_DIR'):
                    self._lex_fn_items({'probe.rs': 'fn probe() {}\n'})

    def test_lex_entry_reads_build_inputs_under_the_tree_lock(self):
        # fix-r2 (boundary P1): the five build-input summaries were read
        # before the tree lock, so a sibling control's window (a
        # `mutating_sources` hold in another process, which keeps the lease
        # until its restore) could interleave the read: the digest described
        # the mutant while the existing binary still reported the pristine
        # tree, and the mismatch refusal fired against a healthy binary —
        # a false red. The summaries are now read holding the same lease,
        # so the read either beats the window or waits it out. The window
        # is simulated at the read (this test process must not mutate the
        # real tree; the end-to-end shape is the mutation control bound to
        # this test): the shared lexer answers as its mutant bytes
        # whenever the lease is not held, and as the real bytes while it
        # is. fix-r3 (DS sibling #1 / boundary #1 / concurrency #2): the
        # simulated window now covers the fixture's own snapshot too (it
        # must answer under the same lease — outside it, the window it
        # exists to simulate would corrupt its own healthy value), and
        # every digest read is counted so a read path the probe cannot
        # see fails loudly instead of passing silently.
        negative = self._lex_entry_process()
        root = Path(__file__).resolve().parents[3]
        crate = root/'src/pilotdeck-gateway'
        sources = (crate/'examples/lex_fn_items.rs',
                   crate/'tests/common/source_inputs.rs',
                   crate/'Cargo.toml', root/'src/Cargo.toml',
                   root/'src/Cargo.lock')
        held = []
        served_mutant = []
        digest_reads = []
        fixture_ready = []
        real_lock = negative.tree_lock
        real_read = Path.read_bytes

        @contextlib.contextmanager
        def leased_lock(path, **kwargs):
            with real_lock(path, **kwargs):
                held.append(True)
                try:
                    yield
                finally:
                    held.pop()

        def windowed_read(path):
            if sources[1] == path:
                if not held:
                    served_mutant.append(True)
                    return real_read(path) + b'\n// fix-r2 sibling window\n'
                if fixture_ready:
                    digest_reads.append(True)
            return real_read(path)

        with patch.object(negative, 'tree_lock', leased_lock), \
                patch.object(Path, 'read_bytes', windowed_read):
            with negative.tree_lock(root):
                pristine = {path: path.read_bytes() for path in sources}
            self.assertTrue(
                served_mutant == [],
                '#2115 the fixture must snapshot the build inputs under '
                'the tree lock — a sibling mutation window was served to '
                'the snapshot')
            fixture_ready.append(True)
            healthy = hashlib.sha256(b''.join(pristine[path]
                                              for path in sources)).hexdigest()
            with tempfile.TemporaryDirectory() as target:
                executable = Path(target)/'debug/examples/lex_fn_items'
                executable.parent.mkdir(parents=True)
                executable.write_text(
                    '#!/bin/sh\n'
                    'printf \'{"scanner_sha256": "' + healthy
                    + '", "files": {"window.rs": {"fns": '
                      '[{"name": "window"}]}}}\\n\'\n')
                executable.chmod(0o755)
                with patch.dict(os.environ, {'CARGO_TARGET_DIR': target}):
                    try:
                        inventory = self._lex_fn_items(
                            {'window.rs': 'fn window() {}\n'})
                    except AssertionError as error:
                        self.assertTrue(
                            served_mutant == [],
                            '#2115 the lex entry read its build-input '
                            'summaries outside the tree lock — a sibling '
                            'mutation window was served to the digest')
                        self.fail('#2115 the lex entry must wait out a '
                                  'sibling mutation window and admit the '
                                  'binary that matches the tree it finds '
                                  f'under the lock: {error}')
            self.assertTrue(
                digest_reads == [True],
                '#2115 the digest reads never went through the probed '
                'read path — the entry read its build inputs another way')
            self.assertTrue(
                served_mutant == [],
                '#2115 the lex entry read its build-input summaries '
                'outside the tree lock — a sibling mutation window was '
                'served to the digest')
            self.assertTrue(
                [item['name'] for item in inventory['window.rs']['fns']]
                == ['window'],
                '#2115 a waited-out sibling window must leave the lex '
                'entry answering, not refusing')

    def test_lex_entry_re_reads_the_tree_before_refusing(self):
        # fix-r2 (boundary P1): reading under the lease still only proves
        # the first digest; the scan itself runs up to 300 s afterwards,
        # and transient bytes the read caught (a plain-written mutant no
        # lease covered, residue a sibling restored mid-flight) need not
        # still be on disk at refusal time. The refusal therefore re-reads
        # the five summaries under the lock first: a binary that matches
        # the tree as it stands is not stale, whatever the first digest
        # said. The transient is simulated at the read (same
        # no-real-mutation rule as the sibling-window control above): the
        # shared lexer answers as its mutant bytes exactly once — on the
        # first summary read — and as the real bytes afterwards. fix-r3
        # (DS boundary #1): the sibling window (mutant bytes whenever the
        # lease is not held) now covers the re-read too, so a re-read
        # outside the lease is caught here and in the refusal control
        # below, not only the first read; fix-r3 (DS sibling #1 /
        # concurrency #2): the fixture snapshot answers under the lease
        # and the digest reads are counted.
        negative = self._lex_entry_process()
        root = Path(__file__).resolve().parents[3]
        crate = root/'src/pilotdeck-gateway'
        sources = (crate/'examples/lex_fn_items.rs',
                   crate/'tests/common/source_inputs.rs',
                   crate/'Cargo.toml', root/'src/Cargo.toml',
                   root/'src/Cargo.lock')
        held = []
        served_mutant = []
        digest_reads = []
        fixture_ready = []
        reads = []
        real_lock = negative.tree_lock
        real_read = Path.read_bytes

        @contextlib.contextmanager
        def leased_lock(path, **kwargs):
            with real_lock(path, **kwargs):
                held.append(True)
                try:
                    yield
                finally:
                    held.pop()

        def transient_read(path):
            if sources[1] == path:
                if not held:
                    served_mutant.append(True)
                    return real_read(path) + b'\n// fix-r2 sibling window\n'
                if fixture_ready:
                    digest_reads.append(True)
                    if not reads:
                        reads.append(True)
                        return (real_read(path)
                                + b'\n// fix-r2 transient bytes\n')
            return real_read(path)

        with patch.object(negative, 'tree_lock', leased_lock), \
                patch.object(Path, 'read_bytes', transient_read):
            with negative.tree_lock(root):
                pristine = {path: path.read_bytes() for path in sources}
            self.assertTrue(
                served_mutant == [],
                '#2115 the fixture must snapshot the build inputs under '
                'the tree lock — a sibling mutation window was served to '
                'the snapshot')
            fixture_ready.append(True)
            healthy = hashlib.sha256(b''.join(pristine[path]
                                              for path in sources)).hexdigest()
            with tempfile.TemporaryDirectory() as target:
                executable = Path(target)/'debug/examples/lex_fn_items'
                executable.parent.mkdir(parents=True)
                executable.write_text(
                    '#!/bin/sh\n'
                    'printf \'{"scanner_sha256": "' + healthy
                    + '", "files": {"reread.rs": {"fns": '
                      '[{"name": "reread"}]}}}\\n\'\n')
                executable.chmod(0o755)
                with patch.dict(os.environ, {'CARGO_TARGET_DIR': target}):
                    try:
                        inventory = self._lex_fn_items(
                            {'reread.rs': 'fn reread() {}\n'})
                    except AssertionError as error:
                        self.assertTrue(
                            served_mutant == [],
                            '#2115 the lex entry read its build-input '
                            'summaries outside the tree lock — a sibling '
                            'mutation window was served to the digest')
                        self.fail('#2115 the lex entry caught transient '
                                  'bytes in its first digest; the refusal '
                                  'must re-read under the tree lock and '
                                  'admit the binary that matches the tree '
                                  f'as it stands: {error}')
            self.assertTrue(
                digest_reads == [True, True],
                '#2115 the digest reads never went through the probed '
                'read path — the entry read its build inputs another way')
            self.assertTrue(
                served_mutant == [],
                '#2115 the lex entry read its build-input summaries '
                'outside the tree lock — a sibling mutation window was '
                'served to the digest')
            self.assertTrue(
                [item['name'] for item in inventory['reread.rs']['fns']]
                == ['reread'],
                '#2115 a transient first digest must not stop the lex '
                'entry from answering once the re-read clears it')

    def test_lex_entry_refusal_cites_the_in_lock_digest(self):
        # fix-r2 (boundary P1): when the binary really is stale, the
        # refusal must describe the tree as it stands under the lock at
        # refusal time — the owner verifies the rebuild against that
        # digest, so the message may not quote the transient bytes the
        # first digest caught. Same one-shot transient simulation as the
        # re-read control above; the binary this time reports a foreign
        # digest, so the refusal is genuine. fix-r3 (DS sibling #1 /
        # boundary #1 / concurrency #2): the fixture snapshot answers
        # under the lease, the sibling window covers the re-read, and the
        # digest reads are counted.
        negative = self._lex_entry_process()
        root = Path(__file__).resolve().parents[3]
        crate = root/'src/pilotdeck-gateway'
        sources = (crate/'examples/lex_fn_items.rs',
                   crate/'tests/common/source_inputs.rs',
                   crate/'Cargo.toml', root/'src/Cargo.toml',
                   root/'src/Cargo.lock')
        held = []
        served_mutant = []
        digest_reads = []
        fixture_ready = []
        reads = []
        real_lock = negative.tree_lock
        real_read = Path.read_bytes

        @contextlib.contextmanager
        def leased_lock(path, **kwargs):
            with real_lock(path, **kwargs):
                held.append(True)
                try:
                    yield
                finally:
                    held.pop()

        def transient_read(path):
            if sources[1] == path:
                if not held:
                    served_mutant.append(True)
                    return real_read(path) + b'\n// fix-r2 sibling window\n'
                if fixture_ready:
                    digest_reads.append(True)
                    if not reads:
                        reads.append(True)
                        return (real_read(path)
                                + b'\n// fix-r2 transient bytes\n')
            return real_read(path)

        with patch.object(negative, 'tree_lock', leased_lock), \
                patch.object(Path, 'read_bytes', transient_read):
            with negative.tree_lock(root):
                pristine = {path: path.read_bytes() for path in sources}
            self.assertTrue(
                served_mutant == [],
                '#2115 the fixture must snapshot the build inputs under '
                'the tree lock — a sibling mutation window was served to '
                'the snapshot')
            fixture_ready.append(True)
            transient = dict(pristine)
            transient[sources[1]] = (pristine[sources[1]]
                                     + b'\n// fix-r2 transient bytes\n')

            def digest_of(tree):
                return hashlib.sha256(b''.join(tree[path]
                                              for path in sources)).hexdigest()

            healthy = digest_of(pristine)
            transient_digest = digest_of(transient)
            with tempfile.TemporaryDirectory() as target:
                executable = Path(target)/'debug/examples/lex_fn_items'
                executable.parent.mkdir(parents=True)
                executable.write_text(
                    '#!/bin/sh\n'
                    'printf \'{"scanner_sha256": "not-this-tree"}\\n\'\n')
                executable.chmod(0o755)
                with patch.dict(os.environ, {'CARGO_TARGET_DIR': target}):
                    with self.assertRaises(AssertionError) as caught:
                        self._lex_fn_items({'cites.rs': 'fn cites() {}\n'})
        message = str(caught.exception)
        self.assertTrue(
            digest_reads == [True, True],
            '#2115 the digest reads never went through the probed read '
            'path — the entry read its build inputs another way')
        self.assertTrue(
            served_mutant == [],
            '#2115 the lex entry read its build-input summaries outside '
            'the tree lock — a sibling mutation window was served to the '
            'digest')
        self.assertTrue(healthy in message,
                        '#2115 the refusal must cite the digest re-read '
                        'under the tree lock')
        self.assertTrue(transient_digest not in message,
                        '#2115 the refusal must not cite the transient '
                        'first digest')

    def test_lex_entry_names_an_overlong_window_as_its_own_refusal(self):
        # fix-r3 (DS concurrency #1): the unconditional tree-lock wait is
        # budgeted to the caller's own 300 s (the scan's run_owned and the
        # control-side run_test subprocess alike), and a window longer
        # than the budget must surface as this guard's own AssertionError
        # — a bare TimeoutError reads as a crash in the harness log, not
        # as a refusal. The lease is held through a second flock on the
        # same lock file: flock ownership is per open file description, so
        # a second fd held by this very process conflicts exactly like
        # another process's lease (probed at fix-r3), and closing it in
        # the finally releases the kernel lock — no orphan process to
        # reap.
        negative = self._lex_entry_process()
        root = Path(__file__).resolve().parents[3]
        fd = os.open(root/'.negative-source.lock', os.O_RDWR | os.O_CREAT)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            with tempfile.TemporaryDirectory() as target:
                with patch.dict(os.environ, {'CARGO_TARGET_DIR': target}):
                    with patch.object(type(self), '_LEX_ENTRY_LOCK_WAIT',
                                      2.0):
                        try:
                            self._lex_fn_items(
                                {'lockwait.rs': 'fn lockwait() {}\n'})
                        except AssertionError as error:
                            message = str(error)
                        except TimeoutError as error:
                            self.fail('#2115 an overlong sibling window '
                                      'must surface as the lex entry\'s '
                                      'own AssertionError, not a raw '
                                      f'TimeoutError: {error!r}')
                        else:
                            self.fail('#2115 the lex entry must refuse '
                                      'while the tree lock is held past '
                                      'its budget, not answer')
        finally:
            os.close(fd)
        self.assertRegex(message, '#2115 the lex entry refused to wait out '
                                  'a sibling mutation window longer than '
                                  'its')
        self.assertIn('2.0 s', message)
        self.assertIn('.negative-source.lock', message)

    def test_pair_exception_registry_cannot_go_stale(self):
        # fix-r1 (DS sibling #3): permission_fault_pair_exceptions had no
        # stale-entry check while its skip_line_exceptions twin did — an
        # entry whose call site disappeared (a rename, a move) would excuse
        # nothing, silently, forever. The registry is a claim the guard
        # keeps true; the same refusal the reverse guard makes applies.
        crate = self.root/'pairing-stale-crate'
        (crate/'tests').mkdir(parents=True)
        (crate/'tests'/'probe.rs').write_text(
            'fn environment_probe_arm() {\n'
            '    stderr_skip_line("skip: environment probe\\n");\n'
            '}\n')
        with patch.object(self, 'permission_fault_pair_exceptions',
                          {'tests/probe.rs::environment_probe_arm':
                           ('an environment probe, not a permission-fault '
                            'contract',),
                           'tests/probe.rs::renamed_away':
                           ('an entry whose call site is gone',)}):
            with self.assertRaisesRegex(
                    AssertionError,
                    'permission_fault_pair_exceptions entries no longer match.*'
                    'tests/probe.rs::renamed_away'):
                self.assert_skip_lines_pair_their_requirement(crate=crate)

    def test_skip_line_reverse_guard_demands_a_provable_owner(self):
        # fix-r1 (DS boundary #3): the text scan maps a call site to the
        # nearest `fn` line above it. When that name has no token record (a
        # raw-string fixture holding `fn ghost_test() {` above the call),
        # the old loop iterated zero records and passed silently — an
        # owner the inventory cannot prove must stay a demand, not an
        # exemption by absence. Naming the real caller in the filter does
        # not silence the ghost, so the refusal can only be the provability
        # one.
        crate = self.root/'ghost-owner-crate'
        (crate/'tests').mkdir(parents=True)
        (crate/'tests'/'fixture.rs').write_text(
            'const SNIPPET: &str = r#"\n'
            'fn ghost_test() {\n'
            '    stderr_skip_line("in a raw string\\n");\n'
            '}\n'
            '"#;\n'
            '\n'
            'fn real_caller() {\n'
            '    stderr_skip_line("skip: unregistered\\n");\n'
            '}\n')
        with self.assertRaisesRegex(
                AssertionError, 'no fn record.*ghost_test'):
            self.assert_every_skip_line_reaches_nextest('test(=real_caller)',
                                                        crate=crate)

    def test_skip_line_reverse_guard_rejects_an_unregistered_call_site(self):
        # #1900 controls for the reverse guard: a skip-printing test the filter
        # does not name must fail the guard, and every exception entry must
        # still be backed by a real call site and a real covering entry.
        crate = self.root/'reverse-guard-crate'
        (crate/'tests').mkdir(parents=True)
        (crate/'tests'/'sample.rs').write_text(
            'fn registered() {\n'
            '    skip_line::stderr_skip_line("skip: registered\\n");\n'
            '}\n'
            '\n'
            'fn unregistered() {\n'
            '    skip_line::stderr_skip_line("skip: unregistered\\n");\n'
            '}\n')
        self.assert_every_skip_line_reaches_nextest('test(=registered) | test(=unregistered)',
                                                   crate=crate)
        with self.assertRaisesRegex(AssertionError,
                                    'covers these skip-line call sites.*unregistered'):
            self.assert_every_skip_line_reaches_nextest('test(=other)', crate=crate)
        # An exception that names a covering entry the filter does not hold.
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/sample.rs::unregistered': ('helper', ('missing_entry',))}):
            with self.assertRaisesRegex(AssertionError, 'does not name that test'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=registered) | test(=unregistered)', crate=crate)
        # An exception with no call site behind it would excuse nothing.
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/sample.rs::deleted_test': ('gone', ())}):
            with self.assertRaisesRegex(AssertionError,
                                        'no longer match a stderr_skip_line call site'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=registered) | test(=unregistered)', crate=crate)
        # Every exception has to say why; an empty reason is not a reason.
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/sample.rs::unregistered': ('', ())}):
            with self.assertRaisesRegex(AssertionError, 'states why'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=registered) | test(=unregistered)', crate=crate)
        # A covered entry that merely exists in the filter text is not enough:
        # a renamed test would leave the exception excusing nothing. #2115
        # (sibling #1): the claim is now checked against the token records, so
        # the failure message names the stronger condition — no record of this
        # file is nextest-visible under that name (a file-local `fn ghost_test`
        # would pass where a foreign same-leaf fn used to).
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/sample.rs::unregistered': ('helper', ('ghost_test',))}):
            with self.assertRaisesRegex(
                    AssertionError,
                    'claims ghost_test covers it, but no record of tests/sample.rs is '
                    'nextest-visible under that name'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=registered) | test(=unregistered) | test(=ghost_test)', crate=crate)
        # #2115 (sibling #1), the discriminative shape: the covered leaf
        # EXISTS in the file, but nested — its nextest-visible name is
        # `deep::ghost_test`, so the claimed bare `ghost_test` selects
        # nothing. Mere existence (the old check) would accept it and the
        # note would still be captured.
        spelling = self.root/'spelling-crate'
        (spelling/'tests').mkdir(parents=True)
        (spelling/'tests'/'sample.rs').write_text(
            'fn covered() {\n'
            '    skip_line::stderr_skip_line("skip: covered\\n");\n'
            '}\n'
            '\n'
            'mod deep {\n'
            '    pub fn ghost_test() {}\n'
            '}\n')
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/sample.rs::covered': ('helper', ('ghost_test',))}):
            with self.assertRaisesRegex(
                    AssertionError,
                    'claims ghost_test covers it, but no record of tests/sample.rs is '
                    'nextest-visible under that name'):
                self.assert_every_skip_line_reaches_nextest('test(=ghost_test)',
                                                            crate=spelling)
        # #1900 fix-r1 codex P2: the key is `file::leaf`, so a same-named
        # *test* in another `mod` of the same file would inherit the exception
        # without a register entry of its own (codex measured the guard green
        # for exactly that duplicate; a non-test would instead be caught by the
        # relay walk, which is why the two functions here carry `#[test]`).
        ambiguous = self.root/'ambiguous-crate'
        (ambiguous/'tests').mkdir(parents=True)
        (ambiguous/'tests'/'dup.rs').write_text(
            'mod first {\n'
            '    #[test]\n'
            '    fn duplicated() {\n'
            '        skip_line::stderr_skip_line("skip: first\\n");\n'
            '    }\n'
            '}\n'
            'mod second {\n'
            '    #[test]\n'
            '    fn duplicated() {\n'
            '        skip_line::stderr_skip_line("skip: second\\n");\n'
            '    }\n'
            '}\n')
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/dup.rs::duplicated': ('one call site is excused', ())}):
            with self.assertRaisesRegex(AssertionError,
                                        r'occurs 2 times.*inherit the exception'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=duplicated)', crate=ambiguous)
        # Only the one shared writer is exempt, not every file called
        # `skip_line.rs`: a copy that prints a skip line is still a call site.
        (crate/'tests'/'other').mkdir(parents=True)
        (crate/'tests'/'other'/'skip_line.rs').write_text(
            'pub fn stderr_skip_line(_note: &str) {}\n'
            '\n'
            'fn hidden_in_a_copy() {\n'
            '    skip_line::stderr_skip_line("skip: copied\\n");\n'
            '}\n')
        with self.assertRaisesRegex(AssertionError,
                                    'covers these skip-line call sites.*hidden_in_a_copy'):
            self.assert_every_skip_line_reaches_nextest(
                'test(=registered) | test(=unregistered)', crate=crate)
        # A call whose name and `(` straddle a line break must still be seen;
        # the line-by-line scan was fail-open (#1900 concurrency P3).
        multiline = self.root/'multiline-crate'
        (multiline/'tests').mkdir(parents=True)
        (multiline/'tests'/'split.rs').write_text(
            'fn split_call() {\n'
            '    skip_line::stderr_skip_line\n'
            '        ("skip: split\\n");\n'
            '}\n')
        with self.assertRaisesRegex(AssertionError,
                                    'covers these skip-line call sites.*split_call'):
            self.assert_every_skip_line_reaches_nextest('test(=other)', crate=multiline)
        self.assert_every_skip_line_reaches_nextest('test(=split_call)', crate=multiline)
        # A helper's `covered` list is a claim about the callers the author
        # thought of; the declared relays turn it into a check. A relay caller
        # that is not registered must fail even though the covered entry is
        # (#1900 boundary P2).
        relay_crate = self.root/'relay-crate'
        (relay_crate/'tests').mkdir(parents=True)
        (relay_crate/'tests'/'relay.rs').write_text(
            'fn helper_note() {\n'
            '    skip_line::stderr_skip_line("skip: helper\\n");\n'
            '}\n'
            '\n'
            'fn relay() {\n'
            '    helper_note();\n'
            '}\n'
            '\n'
            'fn covered_test() {\n'
            '    relay();\n'
            '}\n'
            '\n'
            'fn unregistered_test() {\n'
            '    relay();\n'
            '}\n')
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/relay.rs::helper_note': ('helper', ('covered_test',),
                                                           ('relay',))}):
            with self.assertRaisesRegex(AssertionError,
                                        'every caller is registered too.*unregistered_test'):
                self.assert_every_skip_line_reaches_nextest('test(=covered_test)',
                                                            crate=relay_crate)
            self.assert_every_skip_line_reaches_nextest(
                'test(=covered_test) | test(=unregistered_test)', crate=relay_crate)
            # A relay nobody calls any more is stale data, not a pass.
            with self.assertRaisesRegex(AssertionError,
                                        'expects the note to travel through .relay.., but '
                                        'nothing in .* calls it'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=covered_test) | test(=unregistered_test)',
                    crate=relay_crate,
                    sources={relay_crate/'tests'/'relay.rs':
                             (relay_crate/'tests'/'relay.rs').read_text()
                             .replace('    relay();\n', '')})
        # A caller that reaches the helper without going through a *declared*
        # relay is exactly as unregistered, and the old walk (declared relays
        # only) never looked at it: with no relays declared the check was a
        # no-op (#1900 review, three reviewers). The walk now starts at the
        # helper itself.
        direct_crate = self.root/'direct-crate'
        (direct_crate/'tests').mkdir(parents=True)
        (direct_crate/'tests'/'direct.rs').write_text(
            'fn helper_note() {\n'
            '    skip_line::stderr_skip_line("skip: helper\\n");\n'
            '}\n'
            '\n'
            'fn covered_test() {\n'
            '    helper_note();\n'
            '}\n'
            '\n'
            'fn direct_caller() {\n'
            '    helper_note();\n'
            '}\n')
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/direct.rs::helper_note': ('helper', ('covered_test',))}):
            with self.assertRaisesRegex(AssertionError,
                                        'every caller is registered too.*direct_caller'):
                self.assert_every_skip_line_reaches_nextest('test(=covered_test)',
                                                            crate=direct_crate)
            self.assert_every_skip_line_reaches_nextest(
                'test(=covered_test) | test(=direct_caller)', crate=direct_crate)
        # Only the shared writer's definition line is exempt, not everything in
        # that file: a second note-printing function written next to the writer
        # is still a call site (#1900 simplify P3).
        writer_crate = self.root/'writer-crate'
        (writer_crate/'tests'/'common').mkdir(parents=True)
        (writer_crate/'tests'/'common'/'skip_line.rs').write_text(
            'pub fn stderr_skip_line(_note: &str) {}\n'
            '\n'
            'fn helper_note() {\n'
            '    stderr_skip_line("skip: from the writer file\\n");\n'
            '}\n'
            '\n'
            'fn covered_test() {\n'
            '    helper_note();\n'
            '}\n')
        with self.assertRaisesRegex(AssertionError,
                                    'covers these skip-line call sites.*helper_note'):
            self.assert_every_skip_line_reaches_nextest('test(=covered_test)',
                                                        crate=writer_crate)
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/common/skip_line.rs::helper_note':
                           ('helper', ('covered_test',))}):
            self.assert_every_skip_line_reaches_nextest('test(=covered_test)',
                                                        crate=writer_crate)
        # A nested item is part of the test that declares it, not an entry
        # point of its own: `impl Drop for Restore { fn drop(&mut self) { … } }`
        # inside a test body is how src/skills/tests.rs prints the note on the
        # privileged-host path. Attributing that to `drop` would demand a filter
        # entry nextest can never name; attributing it to the enclosing test
        # keeps the check true (the note really is printed by that test). The
        # green case is the control: `drop` itself is never registered.
        nested_crate = self.root/'nested-crate'
        (nested_crate/'tests').mkdir(parents=True)
        (nested_crate/'tests'/'nested.rs').write_text(
            'fn helper_note() {\n'
            '    skip_line::stderr_skip_line("skip: helper\\n");\n'
            '}\n'
            '\n'
            '#[test]\n'
            'fn covered_test() {\n'
            '    struct Guard;\n'
            '    impl Drop for Guard {\n'
            '        fn drop(&mut self) {\n'
            '            helper_note();\n'
            '        }\n'
            '    }\n'
            '    let _guard = Guard;\n'
            '}\n'
            '\n'
            'fn unregistered_direct() {\n'
            '    helper_note();\n'
            '}\n')
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/nested.rs::helper_note': ('helper', ('covered_test',))}):
            with self.assertRaisesRegex(AssertionError,
                                        'every caller is registered too.*'
                                        'unregistered_direct'):
                self.assert_every_skip_line_reaches_nextest('test(=covered_test)',
                                                            crate=nested_crate)
            self.assert_every_skip_line_reaches_nextest(
                'test(=covered_test) | test(=unregistered_direct)', crate=nested_crate)
        # A call site above no `fn` at all must not be silently skipped.
        (crate/'tests'/'other'/'skip_line.rs').unlink()
        (crate/'tests'/'sample.rs').write_text(
            'skip_line::stderr_skip_line("skip: orphan\\n");\n')
        with self.assertRaisesRegex(AssertionError, 'cannot map the stderr_skip_line call'):
            self.assert_every_skip_line_reaches_nextest(
                'test(=registered) | test(=unregistered)', crate=crate)

    def test_skip_line_exception_needs_a_lexically_unique_owner(self):
        # #2115: the exception is keyed by `file::leaf`, so a second fn of the
        # same name in the same file would inherit it unregistered. The count
        # comes from the proc-macro2 item records, so every shape the old
        # literal regex misread now counts: a comment between `fn` and the
        # name, an `r#` prefix, a definition inside a `macro_rules!` body —
        # and, fail-closed, a file the lexer refuses or that splices items in
        # via include! must not be provable at all. The green case at the end
        # is the mirror: comments and string literals do not inflate the
        # count, where the old regex saw a second definition in both.
        base = ('fn helper_note() {\n'
                '    skip_line::stderr_skip_line("skip: helper\\n");\n'
                '}\n'
                '\n'
                '#[test]\n'
                'fn covered_test() {\n'
                '    helper_note();\n'
                '}\n')

        def owner_crate(name, extra):
            crate = self.root/name
            (crate/'tests').mkdir(parents=True)
            (crate/'tests'/'dup.rs').write_text(base + extra)
            return crate

        def duplicated_by(extra, crate_name):
            crate = owner_crate(crate_name, extra)
            with patch.object(self, 'skip_line_exceptions',
                              {'tests/dup.rs::helper_note': ('helper', ())}):
                with self.assertRaisesRegex(AssertionError, 'occurs 2 times'):
                    self.assert_every_skip_line_reaches_nextest(
                        'test(=covered_test)', crate=crate)

        # A comment between `fn` and the name hides the duplicate from the
        # literal regex `\bfn\s+<name>\s*[(<]`; the lexer sees one token
        # stream and one more record.
        duplicated_by('fn /* deliberately */ helper_note() {}\n',
                      'lex-dup-comment-crate')
        # `fn r#helper_note` is the same fn as `fn helper_note`; the raw
        # prefix is stripped by the lexer before the name is compared.
        duplicated_by('fn r#helper_note() {}\n', 'lex-dup-raw-crate')
        # A named fn inside a macro body only exists after expansion, but it
        # exists: it is reported with the macro flag, and the count includes
        # it.
        duplicated_by('macro_rules! gen { () => { fn helper_note() {} } }\n',
                      'lex-dup-macro-crate')
        # A metavariable-fn macro invoked with the owner's name can stamp out
        # a second same-named fn the register entry never covers.
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/dup.rs::helper_note': ('helper', ())}):
            with self.assertRaisesRegex(
                    AssertionError, 'a metavariable-fn macro can instantiate'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=covered_test)',
                    crate=owner_crate(
                        'lex-dup-metavar-crate',
                        'macro_rules! stamp { ($name:ident) => { fn $name() {} } }\n'
                        'stamp!(helper_note);\n'))
            # A real `include!` token splices another file's items in, so the
            # fn set of this file is not enumerable.
            with self.assertRaisesRegex(
                    AssertionError, 'splices other items in via include!'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=covered_test)',
                    crate=owner_crate('lex-dup-include-crate',
                                      'include!("spliced.rs");\n'))
            # A file that does not lex reports no items at all — existence
            # and uniqueness are not provable there, so the exception is
            # refused rather than excused.
            with self.assertRaisesRegex(
                    AssertionError, 'keyed inside a file the lexer refuses'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=covered_test)',
                    crate=owner_crate('lex-dup-broken-crate', 'fn broken( {\n'))
        # The false-positive side: an `fn helper_note(` inside a comment and
        # inside a string literal holds no tokens, so the owner stays unique
        # and the exception stays green.
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/dup.rs::helper_note': ('helper', ())}):
            self.assert_every_skip_line_reaches_nextest(
                'test(=covered_test)',
                crate=owner_crate(
                    'lex-dup-text-crate',
                    '// fn helper_note() {}\n'
                    'const NOTE: &str = "fn helper_note(";\n'))

    def test_relay_walk_crosses_files_and_resolves_identity(self):
        # #1943: the walk used to stop at the helper's own file, so a caller
        # in another file was invisible. The controls below prove the walk
        # crosses files AND that it demands registration only of callers that
        # provably reach *this* relay — a same-named local fn, a wildcard
        # import and a foreign qualified call are all green without being
        # registered, while an explicit binding and a `crate::…` path call
        # are demanded.
        crate = self.root/'cross-file-crate'
        (crate/'src').mkdir(parents=True)
        (crate/'src'/'relay_home.rs').write_text(
            'pub fn helper_note() {\n'
            '    skip_line::stderr_skip_line("skip: helper\\n");\n'
            '}\n'
            '\n'
            'pub fn replay() {\n'
            '    helper_note();\n'
            '}\n')
        # A cross-file caller through an explicit binding: demanded.
        (crate/'src'/'caller.rs').write_text(
            'use crate::relay_home::replay;\n'
            '\n'
            '#[test]\n'
            'fn cross_file_test() {\n'
            '    replay();\n'
            '}\n')
        # A cross-file caller through the full path: demanded too.
        (crate/'src'/'explicit.rs').write_text(
            '#[test]\n'
            'fn explicit_test() {\n'
            '    crate::relay_home::replay();\n'
            '}\n')
        # A local `fn replay` answers this file's bare sites: NOT a caller
        # of the home relay, so nothing here is demanded.
        (crate/'src'/'local.rs').write_text(
            'fn replay() {}\n'
            '\n'
            '#[test]\n'
            'fn local_test() {\n'
            '    replay();\n'
            '}\n')
        # A wildcard import is collected but cannot spell the item: not a
        # caller (registered limitation, mirrored by the control staying
        # green without an entry).
        (crate/'src'/'wild.rs').write_text(
            'use crate::relay_home::*;\n'
            '\n'
            '#[test]\n'
            'fn wild_test() {\n'
            '    replay();\n'
            '}\n')
        # A qualified call resolving through a module alias to a DIFFERENT
        # home: not a caller of this relay.
        (crate/'src'/'other_home.rs').write_text('pub fn replay() {}\n')
        (crate/'src'/'qual.rs').write_text(
            'use crate::other_home as fixture;\n'
            '\n'
            '#[test]\n'
            'fn qual_test() {\n'
            '    fixture::replay();\n'
            '}\n')
        with patch.object(self, 'skip_line_exceptions',
                          {'src/relay_home.rs::helper_note': ('helper', (), ('replay',))}):
            # The binding caller is the unregistered one; the path caller is
            # named, so the red names the file the note is reached from.
            with self.assertRaisesRegex(
                    AssertionError,
                    'cross_file_test in src/caller.rs reaches the note through replay'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=explicit::explicit_test)', crate=crate)
            # Local definition, wildcard import and foreign qualified call are
            # all green without an entry — the walk demanded only the two
            # provable cross-file callers.
            self.assert_every_skip_line_reaches_nextest(
                'test(=caller::cross_file_test) | test(=explicit::explicit_test)',
                crate=crate)
        # A relay only called from another file is still called: the refusal
        # for an uncalled relay must not fire just because the caller sits
        # outside the home file (the home file itself has no caller of
        # `replay` at all in this crate).
        # A call the walker cannot attribute (no enclosing fn) is refused
        # rather than guessed — it could be any test's note.
        unattributed = self.root/'unattributed-crate'
        (unattributed/'src').mkdir(parents=True)
        (unattributed/'src'/'relay_home.rs').write_text(
            (crate/'src'/'relay_home.rs').read_text())
        (unattributed/'src'/'cold.rs').write_text(
            'use crate::relay_home::replay;\n'
            '\n'
            'static PRELUDE: () = replay();\n')
        with patch.object(self, 'skip_line_exceptions',
                          {'src/relay_home.rs::helper_note': ('helper', (), ('replay',))}):
            with self.assertRaisesRegex(
                    AssertionError, 'cannot attribute a caller'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=cold::anything)', crate=unattributed)
        # #1943 (the `:5294` fix): the definition-line skip used to fire on
        # ANY line `defining_function` matched, so a single-line
        # `fn caller() { helper_note(); }` was skipped as if it were the
        # helper's own definition. It must instead be walked and demanded.
        inline = self.root/'inline-crate'
        (inline/'tests').mkdir(parents=True)
        (inline/'tests'/'inline.rs').write_text(
            'fn helper_note() {\n'
            '    skip_line::stderr_skip_line("skip: helper\\n");\n'
            '}\n'
            '\n'
            'fn inline_caller() { helper_note(); }\n')
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/inline.rs::helper_note': ('helper', ())}):
            with self.assertRaisesRegex(
                    AssertionError, 'every caller is registered too.*inline_caller'):
                self.assert_every_skip_line_reaches_nextest('test(=other)', crate=inline)
            self.assert_every_skip_line_reaches_nextest('test(=inline_caller)',
                                                        crate=inline)
        # #2115 (sibling #3): same-named callers in two inner mods are
        # indistinguishable by the walk's caller name, so BOTH nextest-visible
        # names are demanded — naming one leaves the other unregistered.
        twins = self.root/'twin-caller-crate'
        (twins/'tests').mkdir(parents=True)
        (twins/'tests'/'twins.rs').write_text(
            'fn helper_note() {\n'
            '    skip_line::stderr_skip_line("skip: helper\\n");\n'
            '}\n'
            '\n'
            'mod alpha {\n'
            '    #[test]\n'
            '    pub fn twin_caller() { super::helper_note(); }\n'
            '}\n'
            '\n'
            'mod beta {\n'
            '    #[test]\n'
            '    pub fn twin_caller() { super::helper_note(); }\n'
            '}\n')
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/twins.rs::helper_note': ('helper', ())}):
            with self.assertRaisesRegex(
                    AssertionError,
                    'twin_caller reaches the note directly, but .beta::twin_caller. is '
                    'not in the success-output filter'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=alpha::twin_caller)', crate=twins)
            self.assert_every_skip_line_reaches_nextest(
                'test(=alpha::twin_caller) | test(=beta::twin_caller)', crate=twins)
        # #1937 (the walk side of the consolidation): the shared pairing
        # helper's own shape — one file, pulled in under several module
        # names. The lib root includes it as `test_pairing`, an integration
        # crate includes it as `pairing`; a caller through either spelling
        # is a caller of the ONE helper, and the flat module-name call
        # resolves through the file's own `mod name;` declaration, not a
        # use binding (the resolution shape the pre-#1937 walk could not
        # see at all).
        shared = self.root/'shared-helper-crate'
        (shared/'src').mkdir(parents=True)
        (shared/'src'/'lib.rs').write_text(
            '#[path = "../tests/common/pairing.rs"]\n'
            'mod test_pairing;\n'
            'mod arm;\n')
        (shared/'src'/'arm.rs').write_text(
            'fn unregistered_through_crate_path() {\n'
            '    crate::test_pairing::pair_note();\n'
            '}\n')
        (shared/'tests'/'common').mkdir(parents=True)
        (shared/'tests'/'common'/'skip_line.rs').write_text(
            'pub fn stderr_skip_line(_text: &str) {}\n')
        (shared/'tests'/'common'/'pairing.rs').write_text(
            '#[path = "skip_line.rs"]\n'
            'mod skip_line;\n'
            '\n'
            'pub fn pair_note() {\n'
            '    skip_line::stderr_skip_line("skip: paired\\n");\n'
            '}\n')
        (shared/'tests'/'integration.rs').write_text(
            '#[path = "common/pairing.rs"]\n'
            'mod pairing;\n'
            '\n'
            'fn unregistered_flat_module_call() {\n'
            '    pairing::pair_note();\n'
            '}\n')
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/common/pairing.rs::pair_note': ('helper', ())}):
            with self.assertRaisesRegex(
                    AssertionError,
                    'unregistered_through_crate_path in src/arm.rs reaches the note '
                    'directly'):
                self.assert_every_skip_line_reaches_nextest('test(=nothing)', crate=shared)
            # The two resolutions are independent: with the crate-path caller
            # named, the next red is the flat module-name one.
            with self.assertRaisesRegex(
                    AssertionError,
                    'unregistered_flat_module_call in tests/integration.rs reaches the '
                    'note directly'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=arm::unregistered_through_crate_path)', crate=shared)
            self.assert_every_skip_line_reaches_nextest(
                'test(=arm::unregistered_through_crate_path) | '
                'test(=unregistered_flat_module_call)', crate=shared)
        # #1937 (the fix the consolidation forced): a qualified call's caller
        # set is the inventory's own answer for that spelling — the pre-fix
        # walk looked the spelling up among the BARE-call tables, found
        # nothing, and a full-path caller was invisible. The path caller is
        # the unregistered one here; the binding caller is named.
        with patch.object(self, 'skip_line_exceptions',
                          {'src/relay_home.rs::helper_note': ('helper', (), ('replay',))}):
            with self.assertRaisesRegex(
                    AssertionError,
                    'explicit_test in src/explicit.rs reaches the note through replay'):
                self.assert_every_skip_line_reaches_nextest(
                    'test(=caller::cross_file_test)', crate=crate)
        # #1937 fail-closed: a `#[path]` the lexer cannot read as a plain
        # string literal (a `cfg_attr` that can attach one) must refuse the
        # walk, not fall back to the convention — the convention points the
        # module at a different file, and a caller routed through such a
        # declaration would be counted from the WRONG home.
        unreadable = self.root/'unreadable-path-crate'
        (unreadable/'tests').mkdir(parents=True)
        (unreadable/'tests'/'common').mkdir(parents=True)
        (unreadable/'tests'/'common'/'skip_line.rs').write_text(
            'pub fn stderr_skip_line(_text: &str) {}\n')
        (unreadable/'tests'/'common'/'pairing.rs').write_text(
            'pub fn pair_note() {\n'
            '    skip_line::stderr_skip_line("skip: paired\\n");\n'
            '}\n')
        (unreadable/'tests'/'caller.rs').write_text(
            '#[cfg_attr(unix, path = "common/pairing.rs")]\n'
            'mod pairing;\n'
            '\n'
            'fn cfg_attr_routed_caller() {\n'
            '    pairing::pair_note();\n'
            '}\n')
        with patch.object(self, 'skip_line_exceptions',
                          {'tests/common/pairing.rs::pair_note': ('helper', ())}):
            with self.assertRaisesRegex(
                    AssertionError,
                    'declares a module whose file the walker cannot resolve'):
                self.assert_every_skip_line_reaches_nextest('test(=nothing)',
                                                            crate=unreadable)

    def test_skip_lines_pair_a_requirement(self):
        # #1937: the consolidation leaves exactly one hand-written
        # skip/require pair, and the two drift shapes the issue names must
        # both go red — a skip with no require (a privileged host skips it
        # silently green) and a require with no skip (the reason is lost).
        # Both controls are permanent, per the issue's acceptance list.
        crate = self.root/'pairing-crate'
        (crate/'tests'/'common').mkdir(parents=True)
        (crate/'tests'/'common'/'skip_line.rs').write_text(
            'pub fn stderr_skip_line(_text: &str) {}\n')
        (crate/'tests'/'common'/'permission_faults.rs').write_text(
            '#[path = "skip_line.rs"]\n'
            'mod skip_line;\n'
            '\n'
            'pub fn skip_or_require(note: &str, requirement: &str) {\n'
            '    skip_line::stderr_skip_line(note);\n'
            '    assert!(!require_permission_faults(), "{requirement}");\n'
            '}\n'
            '\n'
            'pub fn require_permission_faults() -> bool { false }\n')
        (crate/'tests'/'arm.rs').write_text(
            '#[path = "common/skip_line.rs"]\n'
            'mod skip_line;\n'
            '#[path = "common/permission_faults.rs"]\n'
            'mod permission_faults;\n'
            '\n'
            'fn skip_only_arm() {\n'
            '    skip_line::stderr_skip_line("skip: half a pair\\n");\n'
            '}\n')
        with patch.object(self, 'permission_fault_pair_exceptions', {}):
            with self.assertRaisesRegex(
                    AssertionError,
                    'skips them silently green.*tests/arm.rs::skip_only_arm'):
                self.assert_skip_lines_pair_their_requirement(crate=crate)
        # The other drift shape, on its own so its red is not shadowed by
        # the skip-side refusal above: the requirement is asserted but no
        # note is printed, so the reason is lost.
        (crate/'tests'/'arm.rs').write_text(
            '#[path = "common/permission_faults.rs"]\n'
            'mod permission_faults;\n'
            '\n'
            'fn require_only_arm() {\n'
            '    assert!(!permission_faults::require_permission_faults(), "#1525 arm");\n'
            '}\n')
        with patch.object(self, 'permission_fault_pair_exceptions', {}):
            with self.assertRaisesRegex(
                    AssertionError,
                    'the reason is lost.*tests/arm.rs::require_only_arm'):
                self.assert_skip_lines_pair_their_requirement(crate=crate)
        # The consolidated shape stays green: the pair is written once, in
        # the helper, and an arm routed through it touches neither half
        # directly. A probe note that is not a permission-fault contract at
        # all is registered, which is the reviewed decision the registry
        # exists to hold.
        (crate/'tests'/'arm.rs').write_text(
            '#[path = "common/permission_faults.rs"]\n'
            'mod permission_faults;\n'
            '\n'
            'fn paired_arm() {\n'
            '    permission_faults::skip_or_require("skip: the pair\\n", "#1525 arm");\n'
            '}\n')
        with patch.object(self, 'permission_fault_pair_exceptions', {}):
            self.assert_skip_lines_pair_their_requirement(crate=crate)
        (crate/'tests'/'probe.rs').write_text(
            '#[path = "common/skip_line.rs"]\n'
            'mod skip_line;\n'
            '\n'
            'fn environment_probe_arm() {\n'
            '    skip_line::stderr_skip_line("skip: environment probe\\n");\n'
            '}\n')
        with patch.object(self, 'permission_fault_pair_exceptions',
                          {'tests/probe.rs::environment_probe_arm':
                           ('an environment probe, not a permission-fault contract',)}):
            self.assert_skip_lines_pair_their_requirement(crate=crate)
        with self.assertRaisesRegex(
                AssertionError,
                'skips them silently green.*tests/probe.rs::environment_probe_arm'):
            self.assert_skip_lines_pair_their_requirement(crate=crate)

    def test_pairing_guard_refuses_a_call_it_cannot_attribute(self):
        # fix-r1 (DS boundary #2): a half-pair written inside a macro body
        # lands in the lexer's `unattributed_calls` bucket, and the pairing
        # guard used to read only `calls` / `qualified_calls` — the macro
        # shape hid "print the skip, never require" from BOTH halves of the
        # check (and the text-based reverse guard maps such a site to
        # whatever fn happens to sit above the macro). The refusal must
        # name the file and the spelling; the same call spelled inside its
        # fn stays visible through the ordinary unpaired path, so the
        # location, not the call, was the blind spot. A macro that only
        # routes the pair keeps the consolidated shape green.
        crate = self.root/'pairing-macro-crate'
        (crate/'tests'/'common').mkdir(parents=True)
        (crate/'tests'/'common'/'skip_line.rs').write_text(
            'pub fn stderr_skip_line(_text: &str) {}\n')
        (crate/'tests'/'common'/'permission_faults.rs').write_text(
            '#[path = "skip_line.rs"]\n'
            'mod skip_line;\n'
            '\n'
            'pub fn skip_or_require(note: &str, requirement: &str) {\n'
            '    skip_line::stderr_skip_line(note);\n'
            '    assert!(!require_permission_faults(), "{requirement}");\n'
            '}\n'
            '\n'
            'pub fn require_permission_faults() -> bool { false }\n')
        (crate/'tests'/'arm.rs').write_text(
            '#[path = "common/skip_line.rs"]\n'
            'mod skip_line;\n'
            '\n'
            'macro_rules! note {\n'
            '    ($m:literal) => { skip_line::stderr_skip_line($m); };\n'
            '}\n'
            '\n'
            'fn lazy_arm() {\n'
            '    note!("skip: half a pair, hidden in a macro\\n");\n'
            '}\n')
        with patch.object(self, 'permission_fault_pair_exceptions', {}):
            with self.assertRaisesRegex(
                    AssertionError,
                    r'pairing walk refuses tests/arm.rs calling '
                    r'`skip_line::stderr_skip_line` where the lexer cannot '
                    r'attribute a caller'):
                self.assert_skip_lines_pair_their_requirement(crate=crate)
        (crate/'tests'/'arm.rs').write_text(
            '#[path = "common/skip_line.rs"]\n'
            'mod skip_line;\n'
            '\n'
            'fn plain_arm() {\n'
            '    skip_line::stderr_skip_line("skip: half a pair\\n");\n'
            '}\n')
        with patch.object(self, 'permission_fault_pair_exceptions', {}):
            with self.assertRaisesRegex(
                    AssertionError,
                    'skips them silently green.*tests/arm.rs::plain_arm'):
                self.assert_skip_lines_pair_their_requirement(crate=crate)
        (crate/'tests'/'arm.rs').write_text(
            '#[path = "common/permission_faults.rs"]\n'
            'mod permission_faults;\n'
            '\n'
            'macro_rules! pair {\n'
            '    ($m:literal, $r:literal) => '
            '{ permission_faults::skip_or_require($m, $r) };\n'
            '}\n'
            '\n'
            'fn paired_arm() {\n'
            '    pair!("skip: the pair\\n", "#1525 arm");\n'
            '}\n')
        with patch.object(self, 'permission_fault_pair_exceptions', {}):
            self.assert_skip_lines_pair_their_requirement(crate=crate)

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
                self.nextest_visible_name(crate, crate / crates_root, [], 'leaf')
        # `src/mod.rs` pops to an empty module list; it used to raise
        # IndexError from `parts[-1]`, which reads like a bug rather than a
        # refusal. It must now refuse with the same message.
        with self.assertRaisesRegex(AssertionError,
                                    'cannot derive the nextest-visible name'):
            self.nextest_visible_name(crate, crate / 'src/mod.rs', [], 'leaf')
        # The fix must not swallow the normal cases.
        self.assertEqual(
            self.nextest_visible_name(crate, crate / 'src/skills/tests.rs', [], 'leaf'),
            'skills::tests::leaf')
        # #1461 r2/r3: this mapping used to be "proven" by scanning the file
        # for `mod tests {` — the old example was a hypothetical `src/grep/
        # mod.rs` that does not exist, and `src/channel/mod.rs` carries the
        # semicolon form (`mod tests;`), which does NOT nest this file's own
        # fns; the inline-brace carrier is `src/handle_path.rs:128` (measured,
        # `grep -n 'mod tests {'`). #2115: the nesting is now read from the
        # lexical record of the declaring file, so the proof runs on a real
        # test of the real carrier end-to-end instead of a hypothetical one.
        record = self._lex_fn_items(self.read_crate_sources(crate))[crate / 'src/handle_path.rs']
        self.assertEqual(
            [item['mod'] for item in record['fns']
             if item['name'] == 'handle_path_rejects_a_non_utf8_name'],
            [['tests']])
        self.assertEqual(
            self.nextest_visible_name(crate, crate / 'src/handle_path.rs', ['tests'], 'leaf'),
            'handle_path::tests::leaf')
        # An integration-test file is its own crate: no stem, only the
        # lexical nesting (measured, `--list` on tests/live_e2e.rs).
        self.assertEqual(
            self.nextest_visible_name(crate, crate / 'tests/live_e2e.rs', [], 'leaf'),
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
                      'timeout-minutes: 1', 'working-directory: .')
            if title != '测试':
                fields += ('env: {NEXTEST_RETRIES: 2}',)
            for field in fields:
                change(f'{title}-{field}', raw, raw + f'        {field}\n',
                       f'#922 {title} cannot override execution or ignore failure')
            # #1809 r7：`shell` 已是登记键，注入重复键只会红在 #919——变异改成
            # 「换值」：换成自定义模板 `bash {0}` 后失去 fail-fast，仍必须被拒。
            change(f'{title}-shell-swapped', raw,
                   raw.replace('        shell: bash\n', '        shell: bash {0}\n', 1),
                   f'#922 {title} pins the fail-fast shell `bash`')
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
                ('needs: [scope, metadata-observer, rust, a51-coverage, negative-controls, '
                 'retirement-outcome-parity]',
                 'needs: [scope, metadata-observer, a51-coverage, negative-controls, '
                 'retirement-outcome-parity]',
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
        # #1809 r9：变异对象是 .config/nextest.toml（read_bytes 按文件名路由，见
        # check_ci_mutations 的注释）。
        self.check_ci_mutations(cases, 'test_nextest_ci_profile_has_no_retries_filters_or_kill_timeout',
                                '#922 profile', reader='read_bytes', mutated='nextest.toml')

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
            'python3 -I scripts/test-check-frozen-delivery.py',
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
            # #1809 r7：`shell: bash` 成为登记键（step 级 shell 优先于任何
            # defaults.run.shell，值恰为 `bash` 时 GitHub 以 fail-fast 语义执行；
            # 自定义模板如 `bash {0}` 会丢掉 fail-fast）。判定从「不许有 shell」改成
            # 「恰一条、值恰为 bash」；job 级前缀（含 defaults）仍然一个 shell 模板
            # 都不许有。
            self.assertEqual(re.findall(r"""(?m)^        ['"]?shell['"]?\s*:\s*(.+?)\s*$""",
                                        step), ['bash'],
                             f'#918 {label} pins the fail-fast shell `bash`')
            self.assertNotRegex(prefix, shell_key,
                                f'#918 {label} job carries no shell template '
                                '(defaults.run.shell)')
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
                # #1809 r7：shell 已是登记键，注入重复键只会红在 #919——变异改成
                # 「换值 / 换键拼写」，让失败落在 shell 值钉死那条判定上。
                change(f'{label}-step-{key}', owner, raw,
                       raw.replace('        shell: bash\n', f'        {key}: bash {{0}}\n', 1),
                       f'#918 {label} pins the fail-fast shell `bash`')
                if owner == 'rust':
                    old = '    defaults:\n      run:\n'
                    new = old + f'        {key}: bash {{0}}\n'
                else:
                    old = '    steps:\n'
                    new = f'    defaults:\n      run:\n        {key}: bash {{0}}\n' + old
                change(f'{label}-job-default-{key}', owner, old, new,
                       f'#918 {label} job carries no shell template')
        for defaults in ('defaults:\n  run:\n    shell: bash {0}\n',
                         'defaults: {run: {shell: "bash {0}"}}\n',
                         "defaults:\n  run:\n    'shell': bash {0}\n"):
            cases.append((f'workflow-{defaults!r}', defaults + workflow,
                          '#918 workflow preserves default fail-fast shell'))
        return cases

    def test_ci_rejects_b2_and_smoke_placement_drift(self):
        self.check_ci_mutations(self.ci_b2_smoke_mutations(),
                                'test_b2_and_smoke_keep_commands_in_their_own_jobs', '#918')

    def check_ci_mutations(self, cases, target, issue, reader='read_text',
                           mutated='ci.yml'):
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
                # #1809 r9：按文件名路由——变异文本只喂给被变异的 workflow，其余
                # 路径回真实文件。旧的全局 return_value 会把 ci.yml 文本喂给任何
                # read_text：pr-gate.yml 的镜像守卫（r9）读它时拿到的是 ci.yml 的
                # 内容，变异根本没被测到。普通函数存进类属性仍走描述符协议，实例
                # 调用照常拿到 path。
                real = getattr(Path, reader)

                def routed(path, *args, **kwargs):
                    return (workflow if path.name == mutated
                            else real(path, *args, **kwargs))
                with patch.object(Path, reader, routed):
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

    def assert_ci_rejects(self, workflow, test_name, message, mutated='ci.yml'):
        # Invoke the real CI check on an in-memory workflow mutation, never edit ci.yml.
        # subTest callers continue through every case even if one mutation survives.
        # #1809 r9：与 check_ci_mutations 同口径按文件名路由（见其注释）。
        real = Path.read_text

        def routed(path, *args, **kwargs):
            return workflow if path.name == mutated else real(path, *args, **kwargs)
        with patch.object(Path, 'read_text', routed):
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
        # #1809 r7：run step 现登记 `shell: bash`（变异时整步替换，含 shell 行）。
        anchor = '      - shell: bash\n        run: rustup show active-toolchain\n'
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
        # #1809 r7：run step 现登记 `shell: bash`（变异时整步替换，含 shell 行）。
        anchor = '      - shell: bash\n        run: rustup show active-toolchain\n'
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
        # #1809 r7：run step 现登记 `shell: bash`（变异时整步替换，含 shell 行）。
        anchor = '      - shell: bash\n        run: rustup show active-toolchain\n'
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
        # #1809 r7：run step 现登记 `shell: bash`（变异时整步替换，含 shell 行）。
        anchor = '      - shell: bash\n        run: rustup show active-toolchain\n'
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
        # #1809 r7：run step 现登记 `shell: bash`（变异时整步替换，含 shell 行）。
        anchor = '      - shell: bash\n        run: rustup show active-toolchain\n'
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
                # #2743 (DS r3 sibling #1): every sentinel in this probe must
                # record, not raise -- main()'s blanket `except Exception`
                # swallows an AssertionError and resurfaces it as exactly the
                # INVALID/BLOCKED shape this probe already asserts, so a
                # raise can never turn the probe red. The empty-list
                # assertions after main() are what make a busy-lock bypass
                # visible on every write/launch path at once.
                source_text_writes = []
                source_byte_writes = []
                run_calls = []

                def evidence_only(path, *a, **k):
                    resolved = Path(path)
                    if not resolved.is_relative_to(Path(temp).resolve()):
                        source_text_writes.append((resolved, a[0] if a else k.get('data')))
                        return None
                    return write_text(path, *a, **k)

                def record_bytes(path, value):
                    source_byte_writes.append(Path(path))
                    return len(value)

                def record_run(*args, **kwargs):
                    from types import SimpleNamespace
                    run_calls.append(args)
                    return SimpleNamespace(returncode=1, stdout='', stderr='')

                atomic_calls = []
                guard_atomic_calls = []

                def record_guard_atomic(root, path, data, *, mode=None):
                    # #2743 (DS r3 sibling #1): the guard-level global gets
                    # the same record-then-assert treatment -- a raise here
                    # is swallowed into INVALID just like the others.
                    guard_atomic_calls.append(Path(path))
                    return None

                def record_fixture_atomic(root, path, data, *, mode=None):
                    # #2743 (DS r2 concurrency #2 / boundary #1): fixtures
                    # migrated to write_source_atomic bind their own global at
                    # import, so patching the guard's attribute never
                    # intercepts their publish/restore. Record rather than
                    # raise -- main()'s blanket `except Exception` would
                    # swallow an AssertionError and come back out as exactly
                    # the BLOCKED/INVALID this probe already asserts, so a
                    # raise cannot turn the probe red. The empty-list
                    # assertion below is what makes a busy-lock bypass
                    # visible; the guard-level `guard_atomic_calls` assert
                    # stays the guard-attribute mirror of the same claim.
                    atomic_calls.append(Path(path))
                    return None

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
                        patch.object(Path, 'write_bytes', record_bytes), \
                        patch.object(guard, 'write_source_atomic', record_guard_atomic), \
                        patch.object(module.sys, 'argv', argv), \
                        patch.object(module, 'run_owned', record_run):
                    if hasattr(module, 'pgrep_launcher'):
                        module.pgrep_launcher = lambda: ''
                    if hasattr(module, 'write_source_atomic'):
                        module.write_source_atomic = record_fixture_atomic
                    else:
                        self.assertIn(folder, ('gateway-batch', 'gateway-entry-guard'),
                                      '#166 only the pre-migration fixtures '
                                      'lack the atomic publish global')
                    code = module.main()
                self.assertEqual(atomic_calls, [],
                                 '#166 busy lock must not publish source '
                                 'bytes through the fixture global')
                self.assertEqual(source_text_writes, [],
                                 '#166 busy lock must never write source through Path.write_text')
                self.assertEqual(source_byte_writes, [],
                                 '#166 busy lock must never write source through Path.write_bytes')
                self.assertEqual(run_calls, [],
                                 '#166 busy lock must not launch a test through run_owned')
                # #1903：还原改走 `write_source_atomic`（临时文件 + `os.replace`），不再经过
                # `Path.write_text/bytes`；只钉那两个入口，这条「拒锁期间不得写源码」的探针
                # 对还原路径就变成空转（r1 DeepSeek sibling / zai sibling P2）。
                # 接线自证见 `assert_restore_reads_the_patched_global`（r2 zai boundary P1：
                # 第一版 assertIs 两边是同一次字典查找，恒真）。
                self.assertEqual(restore_wiring_problems(guard), [],
                                 f'#166 busy lock {folder}: the restore must resolve '
                                 'write_source_atomic through the patched module global')
                self.assertEqual(guard_atomic_calls, [],
                                 '#166 busy lock must not publish source bytes')
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

                # #2743 (DS r3 sibling #1): same record-then-assert fix as
                # the busy-lock probe -- every raise sentinel here is
                # swallowed by main()'s blanket `except Exception`, so each
                # write/launch path records and the empty-list assertions
                # below are what actually turn a bypass red.
                source_text_writes = []
                source_byte_writes = []
                run_calls = []

                def evidence_only(path, *args, **kwargs):
                    resolved = Path(path)
                    if not resolved.is_relative_to(Path(temp).resolve()):
                        source_text_writes.append((resolved, args[0] if args else kwargs.get('data')))
                        return None
                    return write_text(path, *args, **kwargs)

                def record_bytes(path, value):
                    source_byte_writes.append(Path(path))
                    return len(value)

                def record_run(*args, **kwargs):
                    from types import SimpleNamespace
                    run_calls.append(args)
                    return SimpleNamespace(returncode=1, stdout='', stderr='')

                atomic_calls = []
                guard_atomic_calls = []

                def record_guard_atomic(root, path, data, *, mode=None):
                    guard_atomic_calls.append(Path(path))
                    return None

                def record_fixture_atomic(root, path, data, *, mode=None):
                    # #2743 (DS r2 concurrency #2 / boundary #1): same blind
                    # spot as the busy-lock probe -- the fixture's own
                    # write_source_atomic global is invisible to the
                    # guard-level patch above, and an AssertionError here is
                    # swallowed into the BLOCKED shape this probe already
                    # expects. Record and assert empty after main() instead.
                    atomic_calls.append(Path(path))
                    return None

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
                            patch.object(Path, 'write_bytes', record_bytes), \
                            patch.object(guard, 'write_source_atomic', record_guard_atomic), \
                            patch.object(module, 'run_owned', record_run), \
                            patch.object(module.sys, 'argv', argv):
                        if hasattr(module, 'pgrep_launcher'):
                            module.pgrep_launcher = lambda: ''
                        if hasattr(module, 'write_source_atomic'):
                            module.write_source_atomic = record_fixture_atomic
                        else:
                            self.assertIn(folder, ('gateway-batch', 'gateway-entry-guard'),
                                          '#166 only the pre-migration fixtures '
                                          'lack the atomic publish global')
                        try:
                            code = module.main()
                        except Exception as error:
                            caught = error
                    self.assertEqual(atomic_calls, [],
                                     '#166 dirty guard must not publish source '
                                     'bytes through the fixture global')
                    self.assertEqual(source_text_writes, [],
                                     '#166 dirty guard must never write source through Path.write_text')
                    self.assertEqual(source_byte_writes, [],
                                     '#166 dirty guard must never write source through Path.write_bytes')
                    self.assertEqual(run_calls, [],
                                     '#166 dirty guard must not launch a child through run_owned')
                    # #1903：同 busy-lock 探针 —— 还原走 `write_source_atomic`，不经过
                    # `Path.write_bytes`，只钉旧入口等于没钉还原路径。
                    self.assertEqual(restore_wiring_problems(guard), [],
                                     f'#166 dirty {folder}: the restore must resolve '
                                     'write_source_atomic through the patched module global')
                    self.assertEqual(guard_atomic_calls, [],
                                     '#166 dirty guard must not publish source bytes')
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
            # #2743 (DS r3 sibling #2): record instead of raise -- main()'s
            # blanket `except Exception` swallows an AssertionError and
            # resurfaces it as the INVALID verdict this test already expects,
            # so a pre-validation publish through the legacy write paths
            # would stay invisible. The post-main assertions make any such
            # write red.
            text_writes = []
            byte_writes = []

            def evidence_only(path, *a, **k):
                resolved = Path(path)
                if not resolved.is_relative_to(output):
                    text_writes.append((resolved, a[0] if a else k.get('data')))
                    return None
                return write_text(path, *a, **k)

            def record_bytes(path, value):
                byte_writes.append((Path(path), bytes(value)))
                return len(value)

            restored = SimpleNamespace(returncode=0, stdout='', stderr='')
            summary = {'status': 'PASS', 'results': [
                {'mode': 'wiring', 'passed': True, 'status': 'PASS_WITH_CLEANUP_UNVERIFIED'}]}
            # #2743: the fixture publishes and restores its guarded sources
            # through negative_process.write_source_atomic now, so the
            # Path.write_text canary above no longer intercepts the real
            # source-write path. Record the atomic writer's calls instead and
            # prove the #166 invariant on them directly: an invalid mutant
            # must never put mutated bytes on a source path -- only the
            # per-case and backstop restores may write, and they must carry
            # the pristine snapshot bytes.
            atomic_writes = []

            def recording_atomic(root, path, data, *, mode=None):
                atomic_writes.append((Path(path), bytes(data)))

            with patch.object(module, 'mutating_sources', guard), \
                    patch.object(module, 'pgrep_launcher', return_value=''), \
                    patch.object(module, 'run_check', return_value=restored) as check, \
                    patch.object(module, 'read_summary', return_value=(summary, None)), \
                    patch.object(module, 'write_source_atomic', recording_atomic), \
                    patch.object(module.sys, 'argv', ['negative_controls.py', '--output', str(output)]), \
                    patch.object(module.sys, 'platform', 'darwin'), \
                    patch.object(Path, 'write_text', evidence_only), \
                    patch.object(Path, 'write_bytes', record_bytes):
                code = module.main()
            self.assertEqual(len(atomic_writes), len(module.MUTATIONS) + len(originals),
                             '#2743 invalid mutants must restore every source through the '
                             'atomic writer (one per case plus one backstop per source, '
                             'never a publish)')
            self.assertTrue(all(path in originals and data == originals[path]
                                for path, data in atomic_writes),
                            '#2743 the atomic writer must never carry mutated bytes for an '
                            'invalid mutant')
            self.assertEqual(text_writes, [],
                             '#166 invalid mutants must never write outside the '
                             'output dir through Path.write_text')
            self.assertTrue(all(path in originals and data == originals[path]
                                for path, data in byte_writes),
                            '#166 Path.write_bytes may only ever carry pristine '
                            'source bytes for these invalid mutants')
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
