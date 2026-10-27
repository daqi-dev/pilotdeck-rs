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
from negative_process import mutating_sources, run_owned, wait_for_slot

source = root / 'src/pilotdeck-gateway/src/runtime/plugins/catalog.rs'
bundle = root / 'src/pilotdeck-gateway/src/runtime/plugins.rs'
tests = root / 'src/pilotdeck-gateway/src/runtime/plugins/catalog_tests.rs'
# Explicit tables are also checked by anchor_selfcheck.py without executing us.
mutations = [
    ('locale-guard', tests, 'node_locale(|key| std::env::var(key).ok()).to_string()', 'node_locale(|_| Some("en_US.UTF-8".into())).to_string()', 'catalog_locale_contract'),
    ('oracle-cardinality', tests, 'let results = case["results"].as_array().expect(MESSAGE);', 'let results = &case["results"].as_array().expect(MESSAGE)[..requests.len() - 1];', 'catalog_empty'),
    ('selected-cycle', source, '.to_js_value()\n                .map_err(|e| PluginRuntimeError::new(e.message.to_utf8().unwrap_or_default()))?', '.to_js_value().unwrap_or_else(|_| object([]))', 'catalog_selected_cycle_boundary'),
    ('metadata-projection', source, 'set(item, "metadata", value);', 'set(item, "metadata", object([])); let _ = value;', 'catalog_metadata'),
    # Skipping this join's normalization is equivalent: commands/skills normalize
    # again. Instead corrupt ParentDir before that later join can recover it.
    ('path-normalization', source, 'super::loader::join_directory(Path::new(&input.project_key), ".pilotdeck")', 'super::loader::join_directory(Path::new(&input.project_key.replace("/../", "/")), ".pilotdeck")', 'catalog_normalized_paths'),
    ('yaml-collections', source, '*node = pilotdeck_yaml::value::Container::Object(vec![]);', '*node = pilotdeck_yaml::value::Container::Array(vec![]);', 'catalog_metadata'),
    ('sort-rank', source, '"project" => 1,', '"project" => 3,', 'catalog_sorting'),
    ('sort-key', source, '&text(a, "name"), &text(b, "name")', '&text(b, "name"), &text(a, "name")', 'catalog_sorting'),
    ('first-wins', source, 'all.retain(|item| seen.insert(text(item, "name")));', 'all.reverse();\n    all.retain(|item| seen.insert(text(item, "name")));', 'catalog_precedence'),
    ('release', bundle, 'self.release_lease(&mut lease).await;', 'if result.is_ok() { self.release_lease(&mut lease).await; }', 'catalog_lease_finally'),
    ('pinned-order', source, '["/skill_install", "/projects", "/switch-project"]', '["/projects", "/skill_install", "/switch-project"]', 'catalog_empty'),
    ('command-extension', source, 'name.ends_with(".md")', 'name.to_lowercase().ends_with(".md")', 'catalog_disk'),
    ('skill-symlink', source, 'kind.is_dir() || kind.is_symlink()', 'kind.is_dir()', 'catalog_disk'),
    ('empty-skill', source, 'if skills && source.is_empty()', 'if false && skills && source.is_empty()', 'catalog_disk'),
    ('yaml-type', source, 'if !matches!(graph.root, pilotdeck_yaml::value::Value::Ref(_))', 'if false && !matches!(graph.root, pilotdeck_yaml::value::Value::Ref(_))', 'catalog_metadata'),
    ('argument-hint', source, 'if !skills && let Some(hint)', 'if skills && let Some(hint)', 'catalog_metadata'),
    ('description', source, 'first_line(&source).into()', '"incorrect description".into()', 'catalog_metadata'),
    ('extension-trim', source, 'let name = js_trim(&c.name);', 'let name = c.name.as_str();', 'catalog_extensions'),
    ('extension-default', source, '"extension"\n        } else', '"plugin"\n        } else', 'catalog_extensions'),
    ('query-alias', source, 'field(field(item, "metadata"), "aliases")', 'field(field(item, "metadata"), "ignored")', 'catalog_query'),
    ('query-overlap', source, 'from = start + query.len_utf16();', 'from = start + 1;', 'catalog_query'),
    ('page-limit', source, '.skip(offset).take(limit)', '.skip(offset).take(limit + 1)', 'catalog_pagination'),
    ('cursor-signature', source, '|| !text(&value, "signature").equals_str(signature)', '|| false && !text(&value, "signature").equals_str(signature)', 'catalog_pagination'),
    ('query-limit', source, 'query.len_utf16() > 256', 'query.len_utf16() > 257', 'catalog_validation'),
    ('limit-validation', source, '!(1..=100).contains(&limit)', '!(0..=101).contains(&limit)', 'catalog_validation'),
    ('project-required', source, 'if input.project_key.is_empty()', 'if false && input.project_key.is_empty()', 'catalog_validation'),
    ('read-failure', source, 'if skills && source.is_empty()', 'if source.is_empty()', 'catalog_read_errors'),
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
    for name, path, before, after, short_test in mutations:
        if selected and name not in selected:
            continue
        entry = dict(name=name, verdict='INVALID', reason='not run')
        module = 'lease_tests' if short_test == 'catalog_lease_finally' else 'tests'
        test = f'runtime::plugins::catalog::{module}::{short_test}'
        command = ['cargo', 'test', '-q', '--manifest-path', 'src/Cargo.toml', '-p', 'pilotdeck-gateway', '--lib', test]
        marker = '#887 catalog lease finally' if short_test == 'catalog_lease_finally' else '#887 commands catalog oracle'
        if short_test == 'catalog_normalized_paths':
            marker = '#887 commands catalog oracle: normalized-paths'
        if short_test == 'catalog_locale_contract':
            marker = '#887 catalog locale guard bypassed: zh_CN.UTF-8'
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
            # This guard intentionally refuses all new/uncommitted product files.
            with mutating_sources(root, [path]) as originals:
                original = originals[path].decode()
                if original.count(before) != 1:
                    raise ValueError(f'stale anchor: expected 1, got {original.count(before)}')
                path.write_text(original.replace(before, after, 1))
                rc, log = run('compile', command + ['--no-run'])
                if rc != 0:
                    raise ValueError(f'mutant failed compilation rc={rc}')
                rc, log = run('mutant', command + ['--', '--exact', '--format', 'pretty', '--nocapture'])
                if rc == 0:
                    entry.update(verdict='SURVIVED', reason='named test passed')
                elif rc == 101 and f'{test} ... FAILED' in log and 'running 1 test' in log and marker in log and 'test result: FAILED' in log:
                    entry.update(verdict='KILLED', reason=f'named assertion: {marker}')
                else:
                    entry.update(reason=f'not the named assertion rc={rc}')
            rc, log = run('restored', command + ['--', '--exact', '--format', 'pretty'])
            if rc != 0 or '1 passed' not in log:
                entry.update(verdict='INVALID', reason=f'restored positive failed rc={rc}')
        except (Exception, subprocess.TimeoutExpired) as error:
            entry.update(verdict='INVALID', reason=str(error))
        summary.append(entry)
        print(f"{name}: {entry['verdict']} {entry['reason']}", flush=True)
        (out / 'summary.json').write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    counts = {status: sum(e['verdict'] == status for e in summary) for status in ['KILLED', 'SURVIVED', 'INVALID']}
    print(json.dumps(counts), flush=True)
    return int(not summary or any(e['verdict'] != 'KILLED' for e in summary))


if __name__ == '__main__':
    sys.exit(main())
