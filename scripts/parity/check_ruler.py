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
import unicodedata
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
# fix-r3 (r2 reviews): the level scan reads every cell of a review row (a
# 等级 column must not hide a finding), full-width Ｐ１ folds to P1 before
# matching, and the conclusion sentence obeys the same fence visibility as
# the tables — an unclosed fence makes the whole record unreadable.
# fix-r4 (r3 reviews): ~~~ fences hide content like ``` fences; a row that
# names no level is prose or quote noise, not an open finding (a pipe in
# prose no longer fails a clean record); a failed or still-expected disposal
# (修复失败 / 计划 2026 年 12 月 31 日修复 / 待下轮（r3）修复 / not yet
# fixed / never been fixed) reads as pending; merged closes like 合入, and
# 合并 joins both the closure and the pending verb families.
# fix-r5 (r4 reviews, both families): pending derives from the closure lists
# (un- / not / n't / X失败 / 无法X forms can no longer drift one-sidedly);
# Chinese negation markers bind within their clause while intention markers
# look forward across clauses, removing the character-count window; level
# negation tolerates short modifiers (无新增 P0/P1/P2); a backtick fence's
# info string may not contain backticks; a 处置 header column reads like
# 处理; 已验收/已复核/已评审 close; won't fix is the wontfix closure.
_NEGATED_LEVELS = re.compile(r'(?:无|没有|未发现|不含|未见)[一-龥]{0,4}\s*P[012]'
                             r'(?:\s*[/／、]\s*P[012])*', re.IGNORECASE)
# Word boundaries keep identifiers (`case_p1`) from reading as a level.
_LEVEL_TOKEN = re.compile(r'(?<![0-9A-Za-z_])p[012](?![0-9A-Za-z_])', re.IGNORECASE)
_CLOSED_HANDLING = ('修', '解决', '关闭', '拒', '保留', '无需', '完成', '合入',
                    '并入', '合并', '降级', '升级', '处置', '处理', '评审', '复核',
                    '验收', 'done', 'fixed', 'resolved', 'closed', 'merged',
                    'wontfix', "won't fix")
# The pending vocabulary derives from the closure lists, never hand-copied:
# every closure word keeps its negated form on the pending side, or the pair
# drifts one-sidedly and a ruler false-PASS slips in (r5 reviews, both
# families: unmerged / 合并失败 / 无法修复 read as closed). Chinese markers
# split by scope — negation markers (未/没/尚未…) bind within their clause so
# 「已修，未引入回归」 stays closed, while intention markers (计划/待/无法…)
# may look forward across clauses so 「计划 2026 年 12 月 31 日，修复」 stays
# pending; there is no character-count window left to fall off (r3/r5).
_ENGLISH_CLOSURE = ('done', 'fixed', 'resolved', 'closed', 'merged')
_CJK_VERBS = ('修', '解决', '关闭', '处置', '处理', '评审', '复核', '验收', '观察',
              '完成', '合入', '并入', '合并', '降级', '升级', '通过')
_CJK_FAILING = ('修复', '解决', '关闭', '完成', '合入', '并入', '合并', '降级',
                '升级', '处置', '处理', '迁移')
_NEGATION_MARKER = re.compile(r'未|没|尚未|暂不|正在')
_INTENT_MARKER = re.compile(r'待|计划|拟|暂缓|无法|不能|推迟|延后|后续')
_ANY_VERB = re.compile('|'.join(_CJK_VERBS + _ENGLISH_CLOSURE))


def _handling_pending(handling):
    """The 处理 cell still leaves its finding open (#2245 r5)."""
    if re.search('(?:' + '|'.join(v + '失败' for v in _CJK_FAILING) + ')', handling):
        return True  # a failed attempt (合并失败…) is pending, not closed
    if re.search(r'修复中|进行中|观察中|保留观察|继续观察|处置中|处理中|评审中'
                 r'|复核中|验收中|待定|待办', handling):
        return True
    if re.search(r'\bin[ -]progress\b|\bpending\b|\btodo\b|\btbd\b|\bwip\b',
                 handling, re.IGNORECASE):
        return True
    if re.search(r'\bun[- _]?(?:' + '|'.join(_ENGLISH_CLOSURE) + r')\b',
                 handling, re.IGNORECASE):
        return True  # unmerged / un-fixed / un closed
    english_verb = r'\b(?:' + '|'.join(_ENGLISH_CLOSURE) + r')\b'
    if re.search(r"\b(?!won)\w+n't\b[^，,。;]{0,24}" + english_verb,
                 handling, re.IGNORECASE):
        return True  # hasn't been fixed / wasn't merged (won't fix stays closed)
    if re.search(r'\b(?:not|never)\b[^，,。;]{0,24}' + english_verb,
                 handling, re.IGNORECASE):
        return True  # not yet fixed / never been fixed / not merged
    clauses = re.split(r'[，,。;；]', handling)
    for i, clause in enumerate(clauses):
        for marker in _NEGATION_MARKER.finditer(clause):
            if _ANY_VERB.search(clause, marker.start()):
                return True
        for marker in _INTENT_MARKER.finditer(clause):
            if _ANY_VERB.search(clause, marker.start()) or any(
                    _ANY_VERB.search(later) for later in clauses[i + 1:]):
                return True
    return False


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


def _is_delimiter(cells):
    """A `|---|---|` row between a table's header and its data rows."""
    return bool(cells) and all(not cell.strip('-: ') for cell in cells)


def _row_has_level(cells):
    """Any cell still names a P0/P1/P2 level after negation is stripped.

    Every cell is scanned, not just the 结论 column: a review table that
    carries the level in a separate 等级 column must not hide the finding
    (#2245 r2 boundary #2).
    """
    return any(_LEVEL_TOKEN.search(_NEGATED_LEVELS.sub('', cell))
               for cell in cells)


def _scan_table(run):
    """Rows one table leaves open, and whether it is a readable review table.

    A readable table is one header row, one delimiter row and then data rows
    (each with its leading pipe). Any other shape — headerless, a dropped
    leading pipe, or two tables run together with no blank line between
    them — cannot be read column-wise, so every row that names a level is
    open (#2245 r2 boundary #2). Only rows that name a level count as
    findings: a prose or quote line that happens to carry a pipe is not a
    review row (r4 review: such lines read as open). In a readable table
    with both 结论 and 处理 header cells, a row closes only when its 处理
    cell names a closure and is not still pending; misaligned rows are
    unreadable.
    """
    def levels(rows):
        return [cells for lead, cells in rows if _row_has_level(cells)]

    if not (len(run) >= 2 and run[0][0] and _is_delimiter(run[1][1])
            and not any(_is_delimiter(row[1]) for row in run[2:])):
        return levels(run), False  # unreadable shape: every level row is open
    header, data = run[0][1], run[2:]
    if '结论' not in header or not ({'处理', '处置'} & set(header)):
        return levels(data), False  # well-formed, but not a review table
    handling_at = (header.index('处置') if '处置' in header
                   else header.index('处理'))
    width = len(header)
    open_rows = []
    for lead, cells in data:
        if not _row_has_level(cells):
            continue  # no finding named: prose noise, not a review row
        if not lead:
            open_rows.append(cells)  # unreadable row, fail-closed
        elif len(cells) != width:
            open_rows.append(cells)  # misaligned: unreadable, fail-closed
        else:
            handling = cells[handling_at].replace('*', '').lower()
            if (not any(closed in handling for closed in _CLOSED_HANDLING)
                    or _handling_pending(handling)):
                open_rows.append(cells)
    return open_rows, True


def _visible_text(text):
    """Record lines outside fenced code blocks; None while a fence stays unclosed.

    The conclusion sentence and the tables obey one visibility rule: what a
    reader cannot see inside a fence cannot count, and a record whose fence
    is never closed is unreadable, not clean (#2245 r2 boundary #3). Both
    ``` and ~~~ fences count (r4 review: a tilde fence hid a finding from
    the ruler); a closing fence must repeat the opening character and be at
    least as long, per CommonMark. CR/CRLF are folded to LF first, so the
    splitter below sees only LF line breaks.
    """
    text = re.sub(r'\r\n?', '\n', text)
    fence = None  # None outside; (char, length) inside a fence
    visible = []
    for line in G.jsonl_lines(text):
        if fence is None:
            opened = re.match(r' {0,3}(`{3,}|~{3,})(.*)$', line)
            # CommonMark: a backtick fence's info string may not itself
            # contain backticks — such a line is prose, not a fence (r5).
            if opened and (opened.group(1)[0] == '~' or '`' not in opened.group(2)):
                fence = (opened.group(1)[0], len(opened.group(1)))
            else:
                visible.append(line)
        else:
            closed = re.match(r' {0,3}(`{3,}|~{3,})[ \t]*$', line)
            if closed and closed.group(1)[0] == fence[0] \
                    and len(closed.group(1)) >= fence[1]:
                fence = None
    return None if fence is not None else '\n'.join(visible)


def _open_review_rows(text):
    """Review-table rows whose P0/P1/P2 finding the 处理 cell leaves open (#2245).

    Returns those rows, or None when the record has no readable 结论/处理
    table: unverifiable is not clean. Lines come from the shared LF-only
    splitter, so a U+2028 inside a cell cannot hide half of a finding (CR/CRLF
    are normalised first). Full-width Ｐ１ folds to P1 before matching, and
    the text must already be fence-free (see _visible_text). Each run of
    consecutive pipe lines is one table block classified by its own header.
    """
    text = unicodedata.normalize('NFKC', text)
    open_rows, review_table, run = [], False, []
    for line in G.jsonl_lines(re.sub(r'\r\n?', '\n', text)):
        stripped = line.strip()
        if re.search(r'(?<!\\)\|', stripped):
            run.append((stripped.startswith('|'), _row_cells(stripped)))
        elif run:  # prose or blank line ends the table block
            rows, is_review = _scan_table(run)
            open_rows.extend(rows)
            review_table = review_table or is_review
            run = []
    if run:
        rows, is_review = _scan_table(run)
        open_rows.extend(rows)
        review_table = review_table or is_review
    return open_rows if review_table else None


def independent_review():
    """独立审查记录存在、表内 P0/P1/P2 行均已处置、结论为「无 P0/P1/P2」才算过（#2245）。"""
    record = G.WORKTREE / 'docs/reviews/001_ruler-recheck.md'
    try:
        text = record.read_text(encoding='utf-8')
    except (OSError, UnicodeError):
        return False
    visible = _visible_text(text)
    # The conclusion sentence is matched before NFKC folding: the record
    # contract fixes its exact bytes, and only fenced-off text is excluded.
    if visible is None or '结论：**无 P0/P1/P2。**' not in visible:
        return False
    return _open_review_rows(visible) == []


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
