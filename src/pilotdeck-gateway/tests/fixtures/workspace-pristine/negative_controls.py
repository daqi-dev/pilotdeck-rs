#!/usr/bin/env python3
"""#1166 Python/Rust controls; commit first, never bypass the real HEAD guard.

--check validates anchors and compiles Python candidates (Rust builds need HEAD).
Shared admission bootstraps a missing workspace_inputs before its lease. Normal mode compiles each mutant, runs its bound unittest,
restores, reruns, and accounts for every mutation before a nonzero summary.
"""
import argparse
import importlib.util
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot, tree_lock
from negative_process import exclusive_write, new_output_directory, _scanner_environment, _ensure_workspace_scanner
from negative_suite import invalidate_completed, publish_summary

ROOT = Path(__file__).resolve().parents[5]
FIXTURES = ROOT / 'src/pilotdeck-gateway/tests/fixtures'
SOURCE = FIXTURES / 'negative_process.py'
TESTS = Path(__file__).with_name('test_controls.py')
SCANNER = ROOT / 'src/pilotdeck-gateway/tests/common/source_inputs.rs'
DISCOVERY = ROOT / 'src/pilotdeck-gateway/examples/workspace_inputs.rs'
YAML = ROOT / 'src/pilotdeck-yaml/tests/run_resource_scaling.py'
ROUND6 = FIXTURES / 'edgeclaw-entries/negative_round6.py'
JUDGE = FIXTURES / 'negative-output-directory/negative.py'
spec = importlib.util.spec_from_file_location('workspace_python_judge', JUDGE)
judge = importlib.util.module_from_spec(spec)
spec.loader.exec_module(judge)

cases = [
    ('skip-attribute-inputs', SCANNER,
     'if self.workspace {\n                    self.walk(attribute_tokens, false, module_dirs.clone());\n                }',
     'if false && self.workspace {\n                    self.walk(attribute_tokens, false, module_dirs.clone());\n                }',
     'WorkspacePristine.test_attribute_data_inputs_are_guarded', '#1166 attribute data inputs remain guarded'),
    ('gateway-only', SOURCE, "areas = ('src', '.cargo', ':(top,glob)rust-toolchain*')",
     "areas = ('src/pilotdeck-gateway/src', '.cargo', ':(top,glob)rust-toolchain*')",
     'WorkspacePristine.test_clean_and_all_input_categories', '#1166 all workspace inputs reject residue'),
    ('skip-root-config', SOURCE, "areas = ('src', '.cargo', ':(top,glob)rust-toolchain*')",
     "areas = ('src', ':(top,glob)rust-toolchain*')",
     'WorkspacePristine.test_clean_and_all_input_categories', '#1166 all workspace inputs reject residue'),
    ('skip-toolchain', SOURCE, "areas = ('src', '.cargo', ':(top,glob)rust-toolchain*')",
     "areas = ('src', '.cargo')",
     'WorkspacePristine.test_clean_and_all_input_categories', '#1166 all workspace inputs reject residue'),
    ('no-staged-diff', SOURCE, "for options in ((), ('--cached',)):",
     'for options in ((),):',
     'WorkspacePristine.test_staged_cancellation_is_rejected', '#1166 staged drift cannot hide behind HEAD worktree'),
    ('index-as-baseline', SOURCE, "for options in ((), ('--cached',)):\n        changed = _git(root, 'diff', *options, '--name-only', '-z', baseline,",
     "for options in ((),):\n        changed = _git(root, 'diff', *options, '--name-only', '-z',",
     'WorkspacePristine.test_staged_deletion_is_rejected', '#1166 staged deletion must remain guarded'),
    ('allow-hidden-index', SOURCE, "if entry and (entry[:1].islower() or entry[:1] == b'S'):",
     "if False and entry and (entry[:1].islower() or entry[:1] == b'S'):",
     'WorkspacePristine.test_hidden_index_flags_are_rejected', '#1166 hidden index flags must fail closed'),
    ('ignore-untracked', SOURCE, 'if untracked[0]:',
     'if False and untracked[0]:',
     'WorkspacePristine.test_untracked_and_ignored_inputs_are_rejected', '#1166 ignored and untracked inputs must fail closed'),
    ('ignore-ignored', SOURCE, "'ls-files', '--others', '-z', '--', *areas,",
     "'ls-files', '--others', '--exclude-standard', '-z', '--', *areas,",
     'WorkspacePristine.test_untracked_and_ignored_inputs_are_rejected', '#1166 ignored and untracked inputs must fail closed'),
    ('killed-other-crate', SOURCE, "areas = ('src', '.cargo', ':(top,glob)rust-toolchain*')",
     "areas = ('src/pilotdeck-gateway/src', '.cargo', ':(top,glob)rust-toolchain*')",
     'WorkspacePristine.test_sigkill_predecessor_other_crate_is_rejected', '#1166 next holder must reject killed other-crate residue'),
    ('skip-build-clean', SOURCE, "        _assert_product_pristine(root, 'HEAD', env=env)\n",
     '        # missing pristine verification\n',
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock', '#1166 clean slot build copy must stay inside tree lease'),
    ('skip-build-slot', SOURCE, "slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15)",
     "slot = subprocess.CompletedProcess([], 0, '', '')",
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock', '#1166 clean slot build copy must stay inside tree lease'),
    ('skip-build-lock', SOURCE, 'with tree_lock(root, timeout=900):',
     'with nullcontext():',
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock', '#1166 clean slot build copy must stay inside tree lease'),
    ('return-shared-binary', SOURCE, "binary = copy_binary(executables.pop(), output / 'executable')",
     'binary = executables.pop()',
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock', '#1166 clean slot build copy must stay inside tree lease'),
    ('compile-runs-measurements', SOURCE, "command.append('--no-run')",
     'pass  # intentionally omit compile-only option',
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock', '#1166 measurement test build must not execute tests'),
    ('bad-build-accepted', SOURCE, '        result.check_returncode()\n        executables',
     '        # missing build status\n        executables',
     'SnapshotControls.test_failed_or_missing_build_never_copies', '#1166 bad build must not create measurement snapshot'),
    ('missing-executable-accepted', SOURCE, 'if len(executables) != 1:',
     'if len(executables) > 1:',
     'SnapshotControls.test_missing_build_is_attributable', '#1166 missing artifact must fail explicitly'),
    ('unpin-binary', SOURCE, 'if expected_sha256 is not None and digest != expected_sha256:',
     'if False and expected_sha256 is not None and digest != expected_sha256:',
     'SnapshotControls.test_pinned_copy_is_verified_and_exclusive', '#1166 pinned digest must match copied bytes'),
    ('overwrite-binary', SOURCE, "destination.open('xb')",
     "destination.open('wb')",
     'SnapshotControls.test_pinned_copy_is_verified_and_exclusive', '#1166 binary copy cannot overwrite evidence'),
    ('reuse-output', SOURCE, 'output.mkdir(parents=True, exist_ok=False)',
     'output.mkdir(parents=True, exist_ok=True)',
     'SnapshotControls.test_existing_empty_run_is_refused', '#1166 existing empty run must be refused'),
    ('missing-target-accepted', SOURCE, "if not env.get('CARGO_TARGET_DIR'):\n        raise RuntimeError('#1166 CARGO_TARGET_DIR must be supplied by the caller')",
     "env.setdefault('CARGO_TARGET_DIR', str(root / 'target'))",
     'SnapshotControls.test_no_target_no_build', '#1166 absent target must fail before build'),
    ('yaml-missing-pin-accepted', YAML, 'if sha256 is None:',
     'if False and sha256 is None:',
     'SnapshotControls.test_yaml_pinned_snapshot_does_not_wait_for_parent', '#1166 explicit binary requires pinned digest'),
    ('artifact-kind-filter', SOURCE, "and row['target'].get('kind') == [kind] and row.get('executable')):",
     "and row.get('executable')):",
     'SnapshotControls.test_artifact_kind_filter', '#1166 select only requested executable kind'),
    ('build-progress-deadline', SOURCE, 'run_owned(command, cwd=root, env=env, timeout=1800, idle_timeout=600)',
     'run_owned(command, cwd=root, env=env, timeout=1800, idle_timeout=60)',
     'SnapshotControls.test_build_copy_and_slot_are_inside_lock', '#1166 cargo build must retain bounded progress deadlines'),
    ('missing-digest-record', SOURCE, "(output / 'sha256').write_text(hashlib.sha256(Path(binary).read_bytes()).hexdigest() + '\\n')",
     "(output / 'wrong-digest-path').write_text(hashlib.sha256(Path(binary).read_bytes()).hexdigest() + '\\n')",
     'SnapshotControls.test_digest_metadata', '#1166 snapshot digest is recorded'),
    ('overwrite-measurement-output', YAML, "with output.open('x') as stream:",
     "with output.open('w') as stream:",
     'SnapshotControls.test_yaml_output_is_exclusive_and_default_is_unique', '#1166 explicit measurement output is exclusive'),
    ('copy-drops-mode', SOURCE, 'os.fchmod(writer.fileno(), stat.S_IMODE(os.fstat(reader.fileno()).st_mode))',
     'os.fchmod(writer.fileno(), 0o600)',
     'SnapshotControls.test_copy_preserves_mode', '#1166 snapshot must retain executable mode'),
    ('yaml-stray-digest', YAML, 'if sha256 is not None:',
     'if False and sha256 is not None:',
     'SnapshotControls.test_yaml_digest_without_binary_is_rejected', '#1166 digest requires an explicit binary'),
    ('runner-candidate-not-loaded', ROUND6, "patch.object(io, 'open_code', side_effect=code_reader):",
     'contextlib.nullcontext():',
     'SnapshotControls.test_runner_code_stream_keeps_real_sources_pristine', '#1166 runner meta-control must execute candidate code stream'),
    ('skip-literal-inputs', SOURCE, 'literal_inputs = _workspace_literal_inputs(root, baseline, env=env)',
     'literal_inputs = ()',
     'WorkspacePristine.test_new_external_literal_inputs_are_automatically_guarded', '#1166 new external literal inputs must be guarded'),
    ('skip-module-recursion', DISCOVERY, 'if edge.rust_source {',
     'if edge.rust_source && edge.path.extension().is_some_and(|ext| ext == "rs") {',
     'WorkspacePristine.test_new_external_literal_inputs_are_automatically_guarded', '#1166 new external literal inputs must be guarded'),
    ('skip-default-module', SCANNER, 'if !explicit_module {',
     'if false && !explicit_module {',
     'WorkspacePristine.test_external_default_modules_are_guarded', '#1166 external default modules remain guarded'),
    ('ignore-parse-error', SOURCE, 'if result.returncode != 0:\n            raise ValueError',
     'if result.returncode != 0:\n            return ()\n            raise ValueError',
     'WorkspacePristine.test_nonliteral_and_unparseable_inputs_fail_closed', '#1166 unresolved Rust inputs must fail closed'),
    ('cache-input-exempt', SOURCE, '_assert_input_areas(root, baseline, batch, exclude_caches=False)',
     '_assert_input_areas(root, baseline, batch, exclude_caches=True)',
     'WorkspacePristine.test_explicit_cache_input_is_not_exempt', '#1166 included cache names remain build inputs'),
    ('cache-names-rejected', SOURCE, "caches = ('__pycache__/**', '*.pyc', '.DS_Store', '.pytest_cache/**')",
     "caches = ('__pycache__/**', '*.pyc')",
     'WorkspacePristine.test_only_untracked_caches_are_excluded', '#1166 disposable untracked caches are not build inputs'),
    ('reject-external-target', SOURCE, "    env = _scanner_environment(env)\n    if kind not in",
     "    env = _scanner_environment(env)\n    raise RuntimeError('foreign target rejected')\n    if kind not in",
     'SnapshotControls.test_external_target_is_rebuilt_and_copied', '#1166 external target must build and snapshot successfully'),
    ('cwd-relative-target', SOURCE, "str((scanner_root / target).resolve())",
     "str(Path(target).resolve())",
     'SnapshotControls.test_relative_target_is_independent_of_process_cwd', '#1166 target resolves relative to repository root'),
    ('no-default-scanner-target', SOURCE, "or str(scanner_root / 'src/target')",
     "or str(scanner_root / 'wrong-target')",
     'SnapshotControls.test_relative_target_is_independent_of_process_cwd', '#1166 scanner has a default target without caller setup'),
    ('skip-scanner-bootstrap', SOURCE, '    _ensure_workspace_scanner()\n', '    # bootstrap omitted\n',
     'ScannerBootstrap.test_fresh_target_first_mutation_bootstraps_before_lock', '#1166 cold first mutation prepares before lease and warm reuses helper'),
    ('bootstrap-no-slot', SOURCE, "    wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=900, interval=15).check_returncode()",
     '    pass # no slot',
     'ScannerBootstrap.test_fresh_target_first_mutation_bootstraps_before_lock', '#1166 cold first mutation prepares before lease and warm reuses helper'),
    ('bootstrap-status-ignored', SOURCE, "    result.check_returncode()\n    if not executable.is_file():",
     "    return\n    if not executable.is_file():",
     'ScannerBootstrap.test_bootstrap_failure_never_enters_mutation', '#1166 failed scanner bootstrap must fail closed'),
    ('deep-target-exempt', SOURCE, "':(exclude,glob)src/*/target/**'",
     "':(exclude,glob)src/**/target/**'",
     'WorkspacePristine.test_deep_target_is_not_a_cache', '#1166 nested target inputs cannot be exempt'),
    ('unbounded-pathspecs', SOURCE, 'if batch and size + length > 32768:',
     'if False and batch and size + length > 32768:',
     'WorkspacePristine.test_literal_batches_are_bounded_and_complete', '#1166 literal argv batches stay bounded'),
    ('drop-final-pathspec-batch', SOURCE, '    if batch:\n        yield tuple(batch)',
     '    if False and batch:\n        yield tuple(batch)',
     'WorkspacePristine.test_literal_batches_are_bounded_and_complete', '#1166 every literal survives batching'),
    ('drop-final-real-literal-batch', SOURCE, '    if batch:\n        yield tuple(batch)',
     '    if False and batch:\n        yield tuple(batch)',
     'WorkspacePristine.test_last_real_literal_batch_is_guarded', '#1166 final real literal batch remains guarded'),
    ('skip-literal-batch-checks', SOURCE, '_assert_input_areas(root, baseline, batch, exclude_caches=False)',
     'pass # dropped literal batch',
     'WorkspacePristine.test_literal_batches_are_bounded_and_complete', '#1166 every literal batch is checked without cache exemptions'),
    ('omit-crate-root-context', DISCOVERY, 'path.parent().expect("#1166 seed parent").to_path_buf(),',
     'source_inputs::source_module_dir(&path),',
     'WorkspacePristine.test_crate_root_and_module_seed_contexts', '#1166 crate roots and module seeds retain both real inputs'),
    ('ordinary-module-treated-as-root', SCANNER, 'Some("mod.rs")',
     'Some("lib.rs" | "main.rs" | "mod.rs")',
     'WorkspacePristine.test_ordinary_lib_and_main_modules_use_stem_directories', '#1166 ordinary lib and main modules use stem directories'),
    ('drop-second-seed-candidate', SCANNER, 'for candidate in resolved {',
     'for candidate in resolved.into_iter().take(1) {',
     'WorkspacePristine.test_seed_union_does_not_stop_at_first_existing_module', '#1166 seed ownership union cannot drop a candidate'),
    ('ambiguous-seed-accepted', SCANNER, 'existing.len() <= 1,',
     'true || existing.len() <= 1,',
     'WorkspacePristine.test_ambiguous_seed_module_fails_closed', '#1166 each ownership context rejects ambiguous modules'),
    ('runner-target-env-omitted', Path(__file__).resolve(),
     "run_owned(command, cwd=ROOT / 'src', env=env, "
     "timeout=1800, idle_timeout=600)",
     "run_owned(command, cwd=ROOT / 'src', timeout=1800, idle_timeout=600)",
     'SnapshotControls.test_runner_build_uses_normalized_environment', '#1166 runner Cargo uses normalized caller target'),
    ('runner-build-budget-shortened', Path(__file__).resolve(),
     "run_owned(command, cwd=ROOT / 'src', env=env, "
     "timeout=1800, idle_timeout=600)",
     "run_owned(command, cwd=ROOT / 'src', env=env, timeout=900, idle_timeout=600)",
     'SnapshotControls.test_runner_build_uses_normalized_environment', '#1166 runner helper keeps bounded progress deadlines'),
    ('accept-input-symlink', SCANNER, '!candidate.ancestors().any(Path::is_symlink),',
     'true || !candidate.ancestors().any(Path::is_symlink),',
     'WorkspacePristine.test_internal_symlink_input_is_rejected', '#1166 input symlink identities cannot be hidden'),
    ('accept-source-symlink', SCANNER, '!candidate.ancestors().any(Path::is_symlink),',
     'true || !candidate.ancestors().any(Path::is_symlink),',
     'WorkspacePristine.test_symlink_workspace_source_is_rejected', '#1166 source symlink identities cannot be hidden'),
    ('raw-literal-misparsed', SCANNER, "let rest = literal.strip_prefix('r')?;",
     'let rest = literal.strip_prefix("r#r")?;',
     'WorkspacePristine.test_new_external_literal_inputs_are_automatically_guarded', '#1166 literal discovery accepts valid Rust paths'),
    ('skip-include-recursion', SCANNER, 'kind == "include");',
     'false);',
     'WorkspacePristine.test_external_default_modules_are_guarded', '#1166 external default modules remain guarded'),
    ('ambiguous-macros-accepted', SOURCE, 'if result.returncode != 0:\n            raise ValueError',
     'if result.returncode != 0:\n            return ()\n            raise ValueError',
     'WorkspacePristine.test_macro_expansion_and_aliases_fail_closed', '#1166 ambiguous macro input locations must fail closed'),
    ('scanner-nonzero-accepted', SOURCE, "if result.returncode != 0:\n            raise ValueError('Rust scanner failed: '",
     "if False and result.returncode != 0:\n            raise ValueError('Rust scanner failed: '",
     'WorkspacePristine.test_scanner_failure_is_rejected', '#1166 scanner nonzero must fail closed'),
    ('scanner-identity-ignored', SOURCE, "raise ValueError('Rust scanner identity mismatch; prebuild the current example offline')",
     'return ()',
     'WorkspacePristine.test_scanner_identity_is_rejected', '#1166 stale scanner identity must fail closed'),
    ('scanner-json-error-ignored', SOURCE, 'except (ValueError, OSError, subprocess.SubprocessError) as error:\n        raise SourceBaselineError',
     'except (ValueError, OSError, subprocess.SubprocessError) as error:\n        return ()\n        raise SourceBaselineError',
     'WorkspacePristine.test_scanner_malformed_json_is_rejected', '#1166 malformed scanner output must fail closed'),
    ('scanner-list-type-ignored', SOURCE, "raise ValueError('Rust scanner input list is malformed')",
     'return ()',
     'WorkspacePristine.test_scanner_input_schema_is_rejected', '#1166 malformed scanner paths must fail closed'),
    ('scanner-path-shape-ignored', SOURCE, "raise ValueError('Rust scanner input path is malformed')",
     'return ()',
     'WorkspacePristine.test_scanner_input_schema_is_rejected', '#1166 malformed scanner paths must fail closed'),
    ('scanner-duplicates-ignored', SOURCE, 'if len(set(inputs)) != len(inputs):',
     'if False and len(set(inputs)) != len(inputs):',
     'WorkspacePristine.test_scanner_input_schema_is_rejected', '#1166 malformed scanner paths must fail closed'),
    ('scanner-unavailable-accepted', SOURCE, 'except (ValueError, OSError, subprocess.SubprocessError) as error:\n        raise SourceBaselineError',
     'except (ValueError, OSError, subprocess.SubprocessError) as error:\n        return ()\n        raise SourceBaselineError',
     'WorkspacePristine.test_scanner_absent_or_timeout_is_rejected', '#1166 unavailable scanner must fail closed'),
    ('escaped-data-literal-accepted', SCANNER, '(!value.contains(\'\\\\\') && !value.contains(\'"\')).then(|| value.to_owned())',
     'Some(value.replace("\\\\u{70}", "p").replace("\\\\x2e", "."))',
     'WorkspacePristine.test_escaped_and_nonstring_literals_are_rejected', '#1166 data includes require an unescaped string literal'),
    ('conditional-path-accepted', SCANNER, 'matches!(tokens.first(), Some(SourceToken::Ident(name)) if name == "path"),',
     'true,',
     'WorkspacePristine.test_conditional_and_nested_paths_are_rejected', '#1166 ambiguous path attributes must fail closed'),
    ('nested-path-accepted', SCANNER, '                top_level,',
     '                top_level || self.workspace,',
     'WorkspacePristine.test_conditional_and_nested_paths_are_rejected', '#1166 ambiguous path attributes must fail closed'),
    ('path-on-nonmodule-accepted', SCANNER, '!explicit_module || word == "pub",',
     'true || !explicit_module || word == "pub",',
     'WorkspacePristine.test_conditional_and_nested_paths_are_rejected', '#1166 ambiguous path attributes must fail closed'),
    ('indirect-include-accepted', SCANNER, '!names_indirect_include(inner),',
     'true || !names_indirect_include(inner),',
     'WorkspacePristine.test_macro_expansion_and_aliases_fail_closed', '#1166 ambiguous macro input locations must fail closed'),
    ('source-escape-accepted', SOURCE, 'if result.returncode != 0:\n            raise ValueError',
     'if result.returncode != 0:\n            return ()\n            raise ValueError',
     'WorkspacePristine.test_external_path_escape_is_rejected', '#1166 literal inputs cannot escape the repository'),
]


def changed(original, name, old, new):
    count = original.count(old)
    if count != 1:
        raise ValueError(f'#1166 stale anchor {name}: count={count}')
    return original.replace(old, new, 1)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path, nargs='?')
    parser.add_argument('--check', action='store_true')
    parser.add_argument('--case', choices=[row[0] for row in cases], help='run one bounded sample')
    args = parser.parse_args()
    if not os.environ.get('CARGO_TARGET_DIR'):
        parser.error('CARGO_TARGET_DIR must be supplied by the caller')
    if args.check:
        failures = []
        for name, path, old, new, _test, _marker in cases:
            try:
                candidate = changed(path.read_text(), name, old, new)
                if path.suffix == '.py':
                    compile(candidate, str(path), 'exec')
                    print(f'{name}: anchor unique; Python candidate compile PASS (not executed)')
                else:
                    print(f'{name}: anchor unique; Rust compile requires committed HEAD on host')
            except Exception as error:
                failures.append(name)
                print(f'{name}: INVALID: {error}')
        return int(bool(failures))
    if args.output is None:
        parser.error('OUTPUT is required without --check')
    env = _scanner_environment()
    out = new_output_directory(args.output)
    records = []
    guarded = [SOURCE, TESTS, YAML, ROUND6, JUDGE, SCANNER, DISCOVERY, Path(__file__).resolve(),
               FIXTURES / 'negative_suite.py', FIXTURES / 'negative_panic.py',
               FIXTURES / 'pending_reanchor.py']

    def run(label, test):
        result = run_owned([sys.executable, '-I', '-B', '-X',
                            'pycache_prefix=' + str(out / (label + '.pycache')),
                            str(TESTS), test], cwd=ROOT, env=env, timeout=90)
        exclusive_write(out / (label + '.log'), result.stdout + result.stderr)
        return result

    def build_helper(label):
        for index, command in enumerate((
                ['cargo', 'test', '--offline', '-p', 'pilotdeck-gateway', '--example', 'workspace_inputs', '--no-run'],
                ['cargo', 'build', '--offline', '-p', 'pilotdeck-gateway', '--example', 'workspace_inputs'])):
            wait_for_slot(ROOT / 'scripts/cargo-slots.sh', timeout=900).check_returncode()
            result = run_owned(command, cwd=ROOT / 'src', env=env, timeout=1800, idle_timeout=600)
            exclusive_write(out / (label + f'-{index}.log'), result.stdout + result.stderr)
            result.check_returncode()

    selected = [row for row in cases if args.case is None or row[0] == args.case]
    for name, path, old, new, test, marker in selected:
        status, reason, compiled = 'INVALID', 'not run', False
        written = False
        try:
            # Prepare before the outer lease as well; inner admissions reuse
            # the helper while the lease covers mutation and restored rebuild.
            _ensure_workspace_scanner(env)
            with tree_lock(ROOT):
                try:
                    with mutating_sources(ROOT, guarded) as originals:
                        candidate = changed(originals[path].decode(), name, old, new)
                        if not judge.passed(run(name + '-positive', test), test):
                            raise RuntimeError('#1166 bound positive did not pass')
                        path.write_text(candidate)
                        written = True
                        if path.suffix == '.rs':
                            build_helper(name + '-compile')
                        else:
                            compile(path.read_bytes(), str(path), 'exec')
                            exclusive_write(out / (name + '-compile.log'), '#1166 on-disk Python compile PASS\n')
                        compiled = True
                        status, reason = judge.classify(run(name + '-negative', test), test, marker)
                finally:
                    if written and path.suffix == '.rs':
                        build_helper(name + '-restore-compile')
                with mutating_sources(ROOT, guarded):
                    if not judge.passed(run(name + '-restored', test), test):
                        raise RuntimeError('#1166 bound restored did not pass')
        except Exception as error:
            status, reason, compiled = 'INVALID', f'{type(error).__name__}: {error}', False
            invalidate_completed(records, '#1166 control/guard failure: ' + reason)
        records.append(dict(mutation=name, test=test, assertion=marker,
                            status=status, reason=reason, compiled=compiled))
        print(f'{name}: {status}: {reason}', flush=True)
    publish_summary(out, records)
    counts = {state: sum(row['status'] == state for row in records)
              for state in ('KILLED', 'SURVIVED', 'INVALID')}
    print('summary: ' + ', '.join(f'{state}={count}' for state, count in counts.items()))
    return int(not records or any(row['status'] != 'KILLED' for row in records))


if __name__ == '__main__':
    raise SystemExit(main())
