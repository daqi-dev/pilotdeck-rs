#!/usr/bin/env python3
"""Temporarily break each #502 behavior; demand a test failure, always restore."""
import argparse
import hashlib
from pathlib import Path
import re
import subprocess

ROOT = Path(__file__).resolve().parents[5]
SRC = ROOT / 'src/pilotdeck-gateway/src'
CASES = [
    ('count-url', 'runtime/model/count_body.rs',
     '    let value = field(provider, "url");\n',
     '    let value = field(provider, "url");\n'
     '    if value.as_string().is_none() { return Ok(text(value)); }\n',
     'count_urls_match_real_ts_including_error_precedence'),
    ('managed-env', 'runtime/context/instructions.rs',
     '            managed_dir: ManagedDirectory::Environment,',
     '            managed_dir: ManagedDirectory::Fixed(std::env::var_os("PILOTDECK_MANAGED_CONFIG").filter(|v| !v.is_empty()).map(PathBuf::from)),',
     'managed_environment_is_read_on_each_discovery_like_real_ts'),
    ('transcript-syntax', 'storage/transcript.rs',
     'pilotdeck_protocol::json_syntax::parse_error(line)',
     'None::<String>', 'transcript_syntax_diagnostics_match_real_ts'),
    ('registry-syntax', 'storage/run_registry.rs',
     'pilotdeck_protocol::json_syntax::parse_error(&source)',
     'None::<String>', 'run_registry_syntax_errors_match_real_ts'),
]


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('output', type=Path)
    args = parser.parse_args()
    args.output.mkdir(parents=True, exist_ok=True)
    # Reintroduce the actual pre-fix formatter, including its missing year sign.
    name = 'runtime/context/prompt.rs'
    previous = subprocess.check_output(['git', 'show', f'HEAD:src/pilotdeck-gateway/src/{name}'], cwd=ROOT, text=True)
    match = re.search(r'pub\(crate\) fn iso_timestamp\(timestamp_ms: f64\) -> String \{\n(.*?)\n\}', previous, re.S)
    assert match
    cases = CASES + [('extended-year', name,
                      '    pilotdeck_transport::system_clock::format_epoch_iso(timestamp_ms as i64)',
                      match[1], 'extended_year_timestamps_match_real_ts')]
    for label, name, before, after, test in cases:
        path = SRC / name
        saved = path.read_bytes()
        text = saved.decode()
        assert text.count(before) == 1, (label, 'mutation anchor')
        slots = subprocess.run([str(ROOT / 'scripts/cargo-slots.sh')], cwd=ROOT, capture_output=True, text=True)
        # Exit 1 means no slot. Do not mutate or launch cargo while full.
        assert slots.returncode == 0, slots.stdout + slots.stderr
        try:
            path.write_text(text.replace(before, after))
            suite = ['--test', 'diff_backfill'] if label.endswith('-syntax') else ['--lib']
            cmd = ['cargo', 'test', '-p', 'pilotdeck-gateway', *suite, test]
            run = subprocess.run(cmd, cwd=ROOT / 'src', capture_output=True, text=True)
            log = slots.stdout + slots.stderr + '$ ' + ' '.join(cmd) + '\n' + run.stdout + run.stderr
            (args.output / f'negative-{label}.log').write_text(log)
            assert run.returncode != 0, f'{label}: mutation survived'
            assert f'{test} ... FAILED' in log and 'test result: FAILED' in log, f'{label}: not a runtime assertion failure'
            print(f'{label}: KILLED (exit {run.returncode}); {test}')
        finally:
            path.write_bytes(saved)
            assert hashlib.sha256(path.read_bytes()).digest() == hashlib.sha256(saved).digest()


if __name__ == '__main__':
    main()
