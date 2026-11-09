#!/usr/bin/env python3
"""Re-run controls, validate self-comparison evidence, and report review pending."""
import sys

# -I keeps PYTHONPATH, user site and the script directory out of module
# resolution; without it, exit before loading any apparatus module.
if not sys.flags.isolated:
    raise SystemExit(f'run with -I: python3 -I {__file__}')

if sys.version_info < (3, 11) or 'os' not in sys.modules:
    raise SystemExit('os must be preloaded from CPython 3.11+ frozen modules')
import os

_boot_dir = os.path.dirname(os.path.realpath(__file__))
_boot_dir = os.path.dirname(_boot_dir) if os.path.basename(_boot_dir) == 'controls' else _boot_dir
# Identity, not spelling: a case-insensitive alias, a symlinked spelling or a
# mixed ``//`` form names this very directory while the string differs
# (#1033). Nonexistent sys.path entries skip; an empty entry means cwd.
if any(
        os.path.isdir(entry or os.curdir)
        and any(os.path.isdir(root)
                and os.path.samefile(entry or os.curdir, root)
                for root in (_boot_dir, os.path.join(_boot_dir, 'controls')))
        for entry in sys.path):
    raise SystemExit('apparatus directory on sys.path; run the entry file directly with -I')

import tempfile
sys.pycache_prefix = tempfile.mkdtemp(prefix='pdrs-parity-pyc-')
sys.dont_write_bytecode = True

import argparse
import importlib.util
import json
import re
from pathlib import Path

_HERE = Path(__file__).resolve().parent


def _load(name, path):
    """Load an apparatus module by explicit path; never resolve via sys.path."""
    path = Path(path).resolve()
    existing = sys.modules.get(name)
    existing_file = getattr(existing, '__file__', None)
    if existing is not None and existing_file and Path(existing_file).resolve() == path:
        return existing
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    if Path(module.__file__).resolve() != path:
        raise RuntimeError(f'{name}: loaded {module.__file__}, expected {path}')
    return module


G = _load('g03_compare', _HERE / 'g03_compare.py')
_controls = _load('controls.run_all', _HERE / 'controls/run_all.py')
run_controls, DEFAULT_SOURCE = _controls.run_controls, _controls.DEFAULT_SOURCE
check_frozen = _load('check_frozen', _HERE / 'check_frozen.py').check


# #2245: the conclusion sentence alone let a record pass while its table still
# carried an un-disposed P0/P1/P2 row. A handling cell closes a finding only
# when it names a closure and is not still pending; anything else stays open.
# fix-r1 (r1 reviews): closure/pending were bare substrings, so "修复中" or
# "not fixed" read as closed (ruler false-PASS) while a stray 未/待 anywhere
# poisoned a closed row ("已修，未引入回归" read open, false-FAIL). Pending now
# binds to a disposal verb (待X修 / 未修 / 计划修复 / 修复中 / not fixed …),
# closure covers the CLAUDE.md disposal kinds (合入/并入/降级/升级), and both
# level scanning and negation are case-insensitive and share one token shape.
_NEGATED_LEVELS = re.compile(r'(?:无|没有|未发现|不含|未见)\s*P[012]'
                             r'(?:\s*[/／、]\s*P[012])*', re.IGNORECASE)
_LEVEL_TOKEN = re.compile(r'(?<![0-9A-Za-z])p[012](?![0-9A-Za-z])', re.IGNORECASE)
_CLOSED_HANDLING = ('修', '解决', '关闭', '拒', '保留', '无需', '完成', '合入',
                    '并入', '降级', '升级', 'done', 'fixed', 'resolved', 'closed',
                    'wontfix')
_IN_PROGRESS_HANDLING = re.compile(
    r'(?:待|计划|拟|暂不?|未|后续|尚未|正在)[^，,。;；）)]{0,8}'
    r'(?:修|解决|关闭|处置|处理)'
    r'|修复中|进行中|观察中|保留观察|继续观察|待定|待办'
    r'|\bin[ -]progress\b|\bnot[ -]?fixed\b|\bun[- ]?resolved\b|\bun[- ]?done\b'
    r'|\bpending\b|\btodo\b|\btbd\b|\bwip\b', re.IGNORECASE)


def _row_cells(stripped):
    r"""Split a table row on unescaped pipes; `\|` is cell content, not a fence.

    A bare split('|') shifted every later column whenever a cell quoted
    `a \| b`, so the 结论 column could be read from the wrong cell (#2245 r1
    boundary, both review families). Escaped pipes are unescaped after split.
    """
    cells = [cell.strip() for cell in re.split(r'(?<!\\)\|', stripped)]
    if cells and not cells[0]:
        cells = cells[1:]
    if cells and not cells[-1]:
        cells = cells[:-1]
    return [cell.replace('\\|', '|') for cell in cells]


def _open_review_rows(text):
    """Review-table rows whose P0/P1/P2 finding the 处理 cell leaves open (#2245).

    Returns those rows, or None when the record has no readable 结论/处理
    table: unverifiable is not clean. Lines come from the shared LF-only
    splitter, so a U+2028 inside a cell cannot hide half of a finding (CR/CRLF
    are normalised first). Fenced code blocks are skipped. Each table is
    classified by its own header: one with both 结论 and 处理 is a review
    table (rows read per its columns); one with exactly one of them is a
    findings table whose P-level rows cannot show a disposition and are
    therefore open. A review-table row whose cell count no longer matches
    its header is unreadable and therefore open.
    """
    open_rows, review_table = [], False
    fence, table = False, None  # table: None | 'review' | 'finding' | 'other'
    columns = None
    for line in G.jsonl_lines(text.replace('\r\n', '\n').replace('\r', '\n')):
        stripped = line.strip()
        if stripped.startswith('```'):
            fence = not fence
            table, columns = None, None  # a table cannot span a fence
            continue
        if fence:
            continue
        if not stripped.startswith('|'):
            table, columns = None, None  # prose or blank line ends the table
            continue
        cells = _row_cells(stripped)
        if all(not cell.strip('-: ') for cell in cells):
            continue  # |---|---| delimiter, not a review round
        if table is None:
            has_finding_col, has_handling_col = '结论' in cells, '处理' in cells
            if has_finding_col and has_handling_col:
                table, review_table = 'review', True
                columns = (cells.index('结论'), cells.index('处理'), len(cells))
            elif has_finding_col or has_handling_col:
                table = 'finding'
            else:
                table = 'other'
            continue  # the header row itself carries no finding
        if table == 'review':
            finding_at, handling_at, width = columns
            if len(cells) != width:
                open_rows.append(cells)  # misaligned: unreadable, fail-closed
                continue
            finding = _NEGATED_LEVELS.sub('', cells[finding_at])
            handling = cells[handling_at].replace('*', '').lower()
            if _LEVEL_TOKEN.search(finding) and (
                    not any(closed in handling for closed in _CLOSED_HANDLING)
                    or _IN_PROGRESS_HANDLING.search(handling)):
                open_rows.append(cells)
        elif table == 'finding':
            if any(_LEVEL_TOKEN.search(_NEGATED_LEVELS.sub('', cell))
                   for cell in cells):
                open_rows.append(cells)  # no 处理 column: unverifiable
    return open_rows if review_table else None


def independent_review():
    """独立审查记录存在、表内 P0/P1/P2 行均已处置、结论为「无 P0/P1/P2」才算过（#2245）。"""
    record = G.WORKTREE / 'docs/reviews/001_ruler-recheck.md'
    try:
        text = record.read_text(encoding='utf-8')
    except (OSError, UnicodeError):
        return False
    if '结论：**无 P0/P1/P2。**' not in text:
        return False
    return _open_review_rows(text) == []


def check_self_comparison(path):
    self_result, self_error, size = {}, None, 0
    try:
        self_result = json.loads(path.read_text())
        for group in ('input_sha256', 'code_sha256', 'original_sha256'):
            if not isinstance(self_result[group], dict) or not self_result[group]:
                raise ValueError(f'self comparison has no {group} provenance')
        for filename, expected in self_result['original_sha256'].items():
            if G.sha256(G.TOOLS / filename) != expected:
                raise ValueError(f'Original input changed: {filename}')
        for group in ('input_sha256', 'code_sha256'):
            for path, expected in self_result[group].items():
                if G.sha256(Path(path)) != expected:
                    raise ValueError(f'stale self comparison: {path}')
        size = len(self_result.get('S', []))
        expected_ids = set(G.scenario_catalog())
        expected_pairs = {f'r{i}-r{j}' for i in range(1, 6) for j in range(i+1, 6)}
        pair_data = self_result.get('pairs', {})
        if set(pair_data) != expected_pairs or any(set(p.get('per_scenario', {})) != expected_ids for p in pair_data.values()):
            self_error = 'self comparison missing pair/scenario results'
        else:
            recomputed_s = {sid for sid in expected_ids if all(p['per_scenario'][sid]['verdict'] == 'PASS' for p in pair_data.values())}
            if recomputed_s != set(self_result.get('S', [])):
                self_error = 'self comparison S inconsistent with pair verdicts'
        self_ok = self_error is None and self_result.get('valid_calibration') is True and self_result.get('k') == 5 and len(self_result.get('pairs', {})) == 10 and size >= 45
    except (OSError, ValueError, TypeError, AttributeError, KeyError) as error:
        self_error = str(error)
        self_ok = False
    return {'ok': bool(self_ok), 'S_size': size, 'error': self_error}


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument('--self-compare', type=Path, default=G.WORKTREE / '_tmp/self-compare.json')
    ap.add_argument('--control-source', type=Path, default=DEFAULT_SOURCE)
    ap.add_argument('--json', type=Path, default=G.WORKTREE / '_tmp/ruler-check.json')
    args = ap.parse_args()
    try:
        controls = run_controls(args.control_source)
    except (OSError, ValueError, TypeError, AttributeError, KeyError) as error:
        controls = {'ok': False, 'negative_controls': False, 'both_broken': False, 'error': str(error)}
    self_status = check_self_comparison(args.self_compare)
    frozen = check_frozen()
    result = {'self_compare': self_status['ok'], 'negative_controls': controls['negative_controls'],
              'both_broken': controls['both_broken'], 'independent_review': independent_review(), 'S_size': self_status['S_size'],
              'positive_controls': controls.get('positive_controls', False),
              'frozen_hashes': frozen['ok'], 'fully_frozen': frozen['fully_registered'],
              'self_compare_error': self_status['error'], 'controls': controls,
              'evidence': str(args.self_compare), 'review_notice': '独立评审由协调者填写，当前未判定。'}
    result['machine_checks_pass'] = all(result[k] for k in ('self_compare', 'negative_controls', 'both_broken', 'positive_controls', 'frozen_hashes', 'fully_frozen', 'independent_review'))
    G.write_result_exclusive(args.json, json.dumps(result, ensure_ascii=False, indent=2) + '\n')
    print(json.dumps(result, ensure_ascii=False))
    # Pending independent review never becomes an overall acceptance PASS.
    return 2 if result['machine_checks_pass'] else 1


if __name__ == '__main__':
    sys.exit(main())
