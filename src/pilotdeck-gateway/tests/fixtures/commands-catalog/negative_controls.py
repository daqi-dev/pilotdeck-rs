#!/usr/bin/env python3
"""#887: commit first; never bypass negative_process's source == HEAD guard.

CARGO_TARGET_DIR=/absolute/tree/target python3 .../negative_controls.py NEW_OUT [names...]
Each mutant compiles, fails its named test with its dedicated marker, then the
restored source passes that same test. Every selected mutant gets a verdict.
"""
import json
import os
from pathlib import Path
import subprocess
import sys

root = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_panic import named_test_killed
from negative_selection import baseline_passed, verdict_exit
from negative_process import mutating_sources, node_version, run_owned, wait_for_slot

# generate.mts:11 pins the TS baseline: assert.equal(process.version,
# 'v22.22.0'). A host outside that contract records INVALID for the
# generator mutants instead of skipping them (#818 fix-r3).
NODE_CONTRACT = 'v22.22.0'

source = root / 'src/pilotdeck-gateway/src/runtime/plugins/catalog.rs'
bundle = root / 'src/pilotdeck-gateway/src/runtime/plugins.rs'
tests = root / 'src/pilotdeck-gateway/src/runtime/plugins/catalog_tests.rs'
locale = root / 'src/pilotdeck-gateway/src/node_locale.rs'
runtime_tests = root / 'src/pilotdeck-gateway/tests/plugin_runtime.rs'
baseline = root / 'src/pilotdeck-gateway/tests/common/baseline.rs'
# Explicit tables are also checked by anchor_selfcheck.py without executing us.
mutations = [
    ('locale-guard', tests, 'crate::test_oracle_locale::require_en_us();', 'if false { crate::test_oracle_locale::require_en_us(); }', '--lib', 'runtime::plugins::catalog::tests::catalog_locale_contract', '#887 catalog locale guard bypassed: invalid', False),
    ('oracle-cardinality', tests, 'let results = case["results"].as_array().expect(MESSAGE);', 'let results = &case["results"].as_array().expect(MESSAGE)[..requests.len() - 1];', '--lib', 'runtime::plugins::catalog::tests::catalog_empty', '#887 commands catalog oracle', False),
    ('selected-cycle', source, '.to_js_value()\n                .map_err(|e| PluginRuntimeError::new(e.message.to_utf8().unwrap_or_default()))?', '.to_js_value().unwrap_or_else(|_| object([]))', '--lib', 'runtime::plugins::catalog::tests::catalog_selected_cycle_boundary', '#887 commands catalog oracle', False),
    ('metadata-projection', source, 'set(item, "metadata", value);', 'set(item, "metadata", object([])); let _ = value;', '--lib', 'runtime::plugins::catalog::tests::catalog_metadata', '#887 commands catalog oracle', False),
    ('path-normalization', source, 'super::loader::join_directory(Path::new(&input.project_key), ".pilotdeck")', 'super::loader::join_directory(Path::new(&input.project_key.replace("/../", "/")), ".pilotdeck")', '--lib', 'runtime::plugins::catalog::tests::catalog_normalized_paths', '#887 commands catalog oracle: normalized-paths', False),
    ('yaml-collections', source, '*node = pilotdeck_yaml::value::Container::Object(vec![]);', '*node = pilotdeck_yaml::value::Container::Array(vec![]);', '--lib', 'runtime::plugins::catalog::tests::catalog_metadata', '#887 commands catalog oracle', False),
    ('sort-rank', source, '"project" => 1,', '"project" => 3,', '--lib', 'runtime::plugins::catalog::tests::catalog_sorting', '#887 commands catalog oracle', False),
    ('sort-key', source, '&text(a, "name"), &text(b, "name")', '&text(b, "name"), &text(a, "name")', '--lib', 'runtime::plugins::catalog::tests::catalog_sorting', '#887 commands catalog oracle', False),
    ('first-wins', source, 'all.retain(|item| seen.insert(text(item, "name")));', 'all.reverse();\n    all.retain(|item| seen.insert(text(item, "name")));', '--lib', 'runtime::plugins::catalog::tests::catalog_precedence', '#887 commands catalog oracle', False),
    ('release', bundle, 'self.release_lease(&mut lease).await;', 'if result.is_ok() { self.release_lease(&mut lease).await; }', '--lib', 'runtime::plugins::catalog::lease_tests::catalog_lease_finally', '#887 catalog lease finally', False),
    ('pinned-order', source, '["/skill_install", "/projects", "/switch-project"]', '["/projects", "/skill_install", "/switch-project"]', '--lib', 'runtime::plugins::catalog::tests::catalog_empty', '#887 commands catalog oracle', False),
    ('command-extension', source, 'name.ends_with(".md")', 'name.to_lowercase().ends_with(".md")', '--lib', 'runtime::plugins::catalog::tests::catalog_disk', '#887 commands catalog oracle', False),
    ('skill-symlink', source, 'kind.is_dir() || kind.is_symlink()', 'kind.is_dir()', '--lib', 'runtime::plugins::catalog::tests::catalog_disk', '#887 commands catalog oracle', False),
    ('empty-skill', source, 'if skills && source.is_empty()', 'if false && skills && source.is_empty()', '--lib', 'runtime::plugins::catalog::tests::catalog_disk', '#887 commands catalog oracle', False),
    ('yaml-type', source, 'if !matches!(graph.root, pilotdeck_yaml::value::Value::Ref(_))', 'if false && !matches!(graph.root, pilotdeck_yaml::value::Value::Ref(_))', '--lib', 'runtime::plugins::catalog::tests::catalog_metadata', '#887 commands catalog oracle', False),
    ('argument-hint', source, 'if !skills && let Some(hint)', 'if skills && let Some(hint)', '--lib', 'runtime::plugins::catalog::tests::catalog_metadata', '#887 commands catalog oracle', False),
    ('description', source, 'first_line(&source).into()', '"incorrect description".into()', '--lib', 'runtime::plugins::catalog::tests::catalog_metadata', '#887 commands catalog oracle', False),
    ('extension-trim', source, 'let name = js_trim(&c.name);', 'let name = c.name.as_str();', '--lib', 'runtime::plugins::catalog::tests::catalog_extensions', '#887 commands catalog oracle', False),
    ('extension-default', source, '"extension"\n        } else', '"plugin"\n        } else', '--lib', 'runtime::plugins::catalog::tests::catalog_extensions', '#887 commands catalog oracle', False),
    ('query-alias', source, 'field(field(item, "metadata"), "aliases")', 'field(field(item, "metadata"), "ignored")', '--lib', 'runtime::plugins::catalog::tests::catalog_query', '#887 commands catalog oracle', False),
    ('query-overlap', source, 'from = start + query.len_utf16();', 'from = start + 1;', '--lib', 'runtime::plugins::catalog::tests::catalog_query', '#887 commands catalog oracle', False),
    ('page-limit', source, '.skip(offset).take(limit)', '.skip(offset).take(limit + 1)', '--lib', 'runtime::plugins::catalog::tests::catalog_pagination', '#887 commands catalog oracle', False),
    ('cursor-signature', source, '|| !text(&value, "signature").equals_str(signature)', '|| false && !text(&value, "signature").equals_str(signature)', '--lib', 'runtime::plugins::catalog::tests::catalog_pagination', '#887 commands catalog oracle', False),
    ('query-limit', source, 'query.len_utf16() > 256', 'query.len_utf16() > 257', '--lib', 'runtime::plugins::catalog::tests::catalog_validation', '#887 commands catalog oracle', False),
    ('limit-validation', source, '!(1..=100).contains(&limit)', '!(0..=101).contains(&limit)', '--lib', 'runtime::plugins::catalog::tests::catalog_validation', '#887 commands catalog oracle', False),
    ('project-required', source, 'if input.project_key.is_empty()', 'if false && input.project_key.is_empty()', '--lib', 'runtime::plugins::catalog::tests::catalog_validation', '#887 commands catalog oracle', False),
    ('read-failure', source, 'if skills && source.is_empty()', 'if source.is_empty()', '--lib', 'runtime::plugins::catalog::tests::catalog_read_errors', '#887 commands catalog oracle', False),
    ('runtime-locale-guard', runtime_tests, 'oracle_locale::require_en_us();', 'if false { oracle_locale::require_en_us(); }', '--test=plugin_runtime', 'plugin_runtime_locale_contract', '#887 plugin-runtime locale guard bypassed: invalid', False),
    ('locale-no-env', locale, 'unwrap_or_else(|| "en-US".into())', 'unwrap_or_else(|| "zh-CN".into())', '--lib', 'project_files::tests::node_locale_contract', '#887 Node locale contract: mismatches=', False),
    ('locale-c-posix', locale, 'return "en-US".parse().expect("locale literal");', 'return "zh-CN".parse().expect("locale literal");', '--lib', 'project_files::tests::node_locale_contract', '#887 Node locale contract: mismatches=', False),
    ('locale-posix-components', locale, '.take_while(|part| part.len() != 1)', '.take_while(|_| true)', '--lib', 'project_files::tests::node_locale_contract', '#887 Node locale contract: mismatches=', False),
    ('locale-invalid-prefix', locale, 'prefix.pop();', 'prefix.clear();', '--lib', 'project_files::tests::node_locale_contract', '#887 Node locale contract: mismatches=', False),
    ('locale-invalid-language', locale, 'break Locale::UNKNOWN;', 'break "en-US".parse().expect("locale literal");', '--lib', 'project_files::tests::node_locale_contract', '#887 Node locale contract: mismatches=', False),
    ('locale-modifier', locale, 'value.split_once(\'@\').unwrap_or((&value, ""))', '(value.split(\'@\').next().unwrap_or(""), "")', '--lib', 'project_files::tests::node_locale_contract', '#887 Node locale contract: mismatches=', False),
    ('locale-short-modifier', locale, 'format!("{locale}-x-lvariant-{modifier}")', 'format!("{locale}")', '--lib', 'project_files::tests::node_locale_contract', '#887 Node locale contract: mismatches=', False),
    ('locale-priority', locale, '["LC_ALL", "LC_MESSAGES", "LANG"]', '["LANG", "LC_MESSAGES", "LC_ALL"]', '--lib', 'project_files::tests::node_locale_contract', '#887 Node locale contract: mismatches=', False),
    ('generator-lang', baseline, '.env("LANG", "en_US.UTF-8")', '.env("LANG", "zh_CN.UTF-8")', '--lib', 'runtime::plugins::catalog::tests::catalog_generator_locale_contract', '#887 generator LANG isolation', True),
    ('generator-lc-all', baseline, '.env_remove("LC_ALL")', '.env("LC_ALL", "zh_CN.UTF-8")', '--lib', 'runtime::plugins::catalog::tests::catalog_generator_locale_contract', '#887 generator LC_ALL isolation', True),
    ('generator-lc-messages', baseline, '.env_remove("LC_MESSAGES")', '.env("LC_MESSAGES", "zh_CN.UTF-8")', '--lib', 'runtime::plugins::catalog::tests::catalog_generator_locale_contract', '#887 generator LC_MESSAGES isolation', True),
]


def main():
    if not os.environ.get('CARGO_TARGET_DIR'):
        raise SystemExit('CARGO_TARGET_DIR required; no implicit/shared target')
    out = Path(sys.argv[1]).resolve()
    out.mkdir(parents=True, exist_ok=False)
    selected = sys.argv[2:]
    unknown = set(selected) - {m[0] for m in mutations}
    if unknown:
        raise SystemExit(f'unknown mutants: {sorted(unknown)}')
    env = os.environ.copy()
    env.update(CARGO_PROFILE_DEV_DEBUG='line-tables-only', CARGO_PROFILE_TEST_DEBUG='line-tables-only', LANG='en_US.UTF-8')
    env.pop('LC_ALL', None)
    env.pop('LC_MESSAGES', None)
    summary = []
    for name, path, before, after, target, test, marker, ignored in mutations:
        if selected and name not in selected:
            continue
        entry = dict(name=name, verdict='INVALID', reason='not run')
        blocker = None
        if ignored:
            version, error = node_version(env, timeout=60)
            if error:
                blocker = f'node prerequisite failed: {error}'
            elif version != NODE_CONTRACT:
                blocker = (
                    f'host node {version} != {NODE_CONTRACT} required by the '
                    f'generator contract (generate.mts:11); refusing to skip'
                )
        if blocker is not None:
            entry.update(reason=blocker)
        else:
            # No -q (#942): cargo forwards it to libtest as --quiet, which drops
            # the per-test lines this script's KILLED/restored criteria match.
            command = ['cargo', 'test', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', target, test]
            test_args = ['--', '--exact', '--format', 'pretty', '--nocapture']
            if ignored:
                test_args.append('--ignored')
            def run(label, args):
                slot = wait_for_slot(root / 'scripts/cargo-slots.sh', timeout=180)
                (out / f'{name}.{label}.slots.log').write_text(slot.stdout + slot.stderr)
                try:
                    result = run_owned(args, cwd=root, env=env, timeout=300)
                except subprocess.TimeoutExpired as error:
                    def text(value):
                        return value.decode('utf-8', 'replace') if isinstance(value, bytes) else value or ''
                    (out / f'{name}.{label}.log').write_text(text(error.output) + text(error.stderr))
                    raise
                log = result.stdout + result.stderr
                (out / f'{name}.{label}.log').write_text(log)
                return result.returncode, log
            try:
                rc, log = run('baseline', command + test_args)
                if rc != 0 or not baseline_passed(log, test):
                    raise ValueError(f'baseline failed rc={rc}')
                # This guard intentionally refuses all new/uncommitted product files.
                with mutating_sources(root, [path]) as originals:
                    original = originals[path].decode()
                    if original.count(before) != 1:
                        raise ValueError(f'stale anchor: expected 1, got {original.count(before)}')
                    path.write_text(original.replace(before, after, 1))
                    rc, log = run('compile', command + ['--no-run'])
                    if rc != 0:
                        raise ValueError(f'mutant failed compilation rc={rc}')
                    rc, log = run('mutant', command + test_args)
                    if rc == 0:
                        entry.update(verdict='SURVIVED', reason='named test passed')
                    elif rc == 101 and named_test_killed(log, test, marker) and 'running 1 test' in log and 'test result: FAILED' in log:
                        entry.update(verdict='KILLED', reason=f'named assertion: {marker}')
                    else:
                        entry.update(reason=f'not the named assertion rc={rc}')
                rc, log = run('restored', command + test_args)
                if rc != 0 or not baseline_passed(log, test):
                    entry.update(verdict='INVALID', reason=f'restored positive failed rc={rc}')
            except (Exception, subprocess.TimeoutExpired) as error:
                entry.update(verdict='INVALID', reason=str(error))
        summary.append(entry)
        print(f"{name}: {entry['verdict']} {entry['reason']}", flush=True)
        (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    counts = {status: sum(e['verdict'] == status for e in summary) for status in ['KILLED', 'SURVIVED', 'INVALID']}
    print(json.dumps(counts), flush=True)
    return verdict_exit(summary)


if __name__ == '__main__':
    sys.exit(main())
