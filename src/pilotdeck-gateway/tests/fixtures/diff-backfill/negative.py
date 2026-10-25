#!/usr/bin/env python3
"""Temporarily break each #502 behavior; demand a test failure, always restore."""
import argparse
import hashlib
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned

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
    # Reintroduce the actual pre-fix formatter (the body replaced in
    # 025b31f8), including its missing extended-year sign. HEAD already carries
    # the fix, so reading HEAD here made this control a no-op: attempt 1 logged
    # "extended-year: mutation survived" while the test still passed.
    name = 'runtime/context/prompt.rs'
    old_body = '''    let ms = timestamp_ms as i64;
    let days = ms.div_euclid(86_400_000);
    let day_ms = ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        day_ms / 3_600_000,
        day_ms / 60_000 % 60,
        day_ms / 1_000 % 60,
        day_ms % 1_000
    )'''
    cases = CASES + [('extended-year', name,
                      '    pilotdeck_transport::system_clock::format_epoch_iso(timestamp_ms as i64)',
                      old_body, 'extended_year_timestamps_match_real_ts')]
    for label, name, before, after, test in cases:
        path = SRC / name
        with mutating_sources(ROOT, [path]) as snapshots:
            saved = snapshots[path]
            text = saved.decode()
            assert text.count(before) == 1, (label, 'mutation anchor')
            slots = subprocess.run([str(ROOT / 'scripts/cargo-slots.sh')], cwd=ROOT, capture_output=True, text=True)
            # Exit 1 means no slot. Do not mutate or launch cargo while full.
            assert slots.returncode == 0, slots.stdout + slots.stderr
            try:
                path.write_text(text.replace(before, after))
                suite = ['--test', 'diff_backfill'] if label.endswith('-syntax') else ['--lib']
                cmd = ['cargo', 'test', '-p', 'pilotdeck-gateway', *suite, test]
                run = run_owned(cmd, cwd=ROOT / 'src', timeout=300)
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
