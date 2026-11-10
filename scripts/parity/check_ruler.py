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


# #2245 fix-r7 (r6 reviews, both families — structural redesign per the
# 2026-11-10 coordinator ruling; round history: specs/parity/frozen/
# 20261109-2245.md through 20261110-2245-r8.md). Five patch rounds proved
# lexical enumeration does not converge, so verdicts now derive structurally:
#   1. every 处理/处置 cell is judged on its own copy, never concatenated —
#      a row closes only when each cell names a closure and none is pending;
#   2. the judgement copy is normalised first (NFKC + curly-apostrophe fold,
#      r6 MM#4) and never written back to the record bytes;
#   3. word/token edges: <verb>中 needs a right edge (升级中 must not match
#      已升级中间件, r6 DS#3), bare 需/会 intent markers carry morpheme
#      guards (需求方 / 会议 / 机会, r6 MM#3), explicit pending words take
#      right edges, and closure verbs block known right extensions that form
#      a different lexeme (拒绝 / 保留意见, r6 MM#2);
#   4. clause separators cover the CJK+ASCII punctuation set, '.' included
#      with digit protection so v1.2 stays one token (r6 MM#5);
#   5. level negation is grammar, not a modifier whitelist (r6 DS#4): an
#      existence negation + a bounded gap free of verb-initial characters +
#      the level token folds (未发现新 P1 / 无严重级别 P0), while a gap that
#      carries a verb-initial character marks a real finding (无法复现 P1 /
#      未见修复 P1 / 没有处理 P1 / 无关 P1 keep their level);
#   6. unknown shapes stay open, never guessed (fail-closed).
# Vocabulary that cannot be derived structurally stays in named closed lists
# below; each residual entry is paired with a control in
# controls/review_r1.py (memory: ruler-real-data).
# r8 (closure-round DS review): 修改/维修/检修 join the single word source —
# an unknown disposal verb made 修改中 / 维修中 read as closed (boundary#2),
# and the English table gains accepted/migrated/passed/completed so an
# English disposal cell does not read open (sibling#5).
_ENGLISH_VERBS = ('done', 'fixed', 'resolved', 'closed', 'merged',
                  'verified', 'confirmed', 'reviewed', 'tested', 'checked',
                  'accepted', 'migrated', 'passed', 'completed')
_CJK_OBSERVE_ONLY = ('观察',)  # 观察 never closes a row; 已修，后续观察 stays closed
_CJK_VERBS = ('修', '修复', '解决', '关闭', '拒', '保留', '完成', '合入', '并入', '合并',
              '降级', '升级', '处置', '处理', '评审', '复核', '验收', '通过',
              '验证', '确认', '检查', '测试', '复检', '迁移', '观察',
              '修改', '维修', '检修')
_CJK_GATE_VERBS = tuple(v for v in _CJK_VERBS if v not in _CJK_OBSERVE_ONLY)
_CJK_LONG_VERBS = tuple(v for v in _CJK_VERBS if len(v) >= 2)
# Right extensions that turn a closure verb into a different lexeme with a
# non-closure meaning (拒绝修复 / 已拒绝合入 / 保留意见 stay open, r6 MM#2).
_CJK_CLOSURE_GUARDS = {'拒': '绝', '保留': '意见'}


def _longest_first(words):
    return sorted(words, key=len, reverse=True)


_CJK_CLOSURE_RE = re.compile('|'.join(
    verb + ('(?!%s)' % _CJK_CLOSURE_GUARDS[verb]
            if verb in _CJK_CLOSURE_GUARDS else '')
    for verb in _longest_first(_CJK_GATE_VERBS)))
_EN_CLOSURE_RE = re.compile(r'\b(?:' + '|'.join(_ENGLISH_VERBS) + r'|wontfix)\b')
_CLOSURE_PHRASES = ('无需', "won't fix")
_CJK_FAILING_RE = re.compile(
    '(?:' + '|'.join(_longest_first(_CJK_LONG_VERBS)) + ')失败')
# r8 (DS boundary#1): the r7 blanket word edge made <动词>中 and the explicit
# pending words stop matching as soon as a CJK character — any character,
# including 的 — followed, so 修复中的问题已定位 / 进行中的评审 read as closed
# (false PASS). The edge now blocks only the attested noun compounds that
# swallow 中 as a morpheme (中间 / 中间层; 进行中断分析), each with a control in
# controls/review_r1.py.
_PENDING_RIGHT_GUARDS = {'进行中': '(?!断)'}
_CJK_IN_PROGRESS_RE = re.compile(
    '(?:' + '|'.join(_longest_first(_CJK_LONG_VERBS)) + ')中(?!间)')
# 拒绝 keeps matching inside longer phrases (拒绝修复 / 已拒绝合入).
_EXPLICIT_PENDING_RE = re.compile('|'.join(
    word + _PENDING_RIGHT_GUARDS.get(word, '')
    for word in _longest_first(('进行中', '保留观察', '继续观察', '待观察', '待定',
                                '待办', '不通过', '拒绝', '保留意见'))))
# r8 (DS boundary#2): partial-completion residuals — a cell that names a
# partial disposal (部分修复 / 修复了一半 / 阶段性修复) is still open.
_PARTIAL_WORDS = ('部分', '一半', '阶段性')
_EN_PENDING_RE = re.compile(r'\bin[ -]?progress\b|\bpending\b|\btodo\b|\btbd\b'
                            r'|\bwip\b', re.IGNORECASE)
_EN_UNDONE_RE = re.compile(r'\bun[- _]?(?:' + '|'.join(_ENGLISH_VERBS) + r')\b',
                           re.IGNORECASE)
# Negation markers bind within their clause; intention markers may look
# forward across clauses (#2245 r5). 裸「需/会」不与前接 无/不 组合
# （无需修复 closed；不会修复 is the wontfix closure）且排除名词复合
# （需求/需求方、会议/会话/机会/社会/体会，r6 MM#3）；「暂」拆成 暂不
# (negation) 与 暂缓/暂时/暂且 (intention)。
_NEGATION_WORDS = ('未', '没', '尚未', '暂不', '正在')
_INTENT_WORDS = ('待', '计划', '拟', '暂缓', '暂时', '暂且', '无法', '不能',
                 '推迟', '延后', '后续', '稍后', '下周', '本周', '需', '会', '将')
_MARKER_GUARDS = {
    '需': r'(?<![无不])需(?!求)',
    '会': r'(?<!不)(?<![机社体])会(?!议|话)',
}
_NEGATION_MARKER = re.compile('|'.join(_longest_first(_NEGATION_WORDS)))
_INTENT_MARKER = re.compile('|'.join(
    _MARKER_GUARDS.get(word, word) for word in _longest_first(_INTENT_WORDS)))
_EN_NEGATION = re.compile(r"\bnot\b|\bnever\b|\b(?!won)\w+n't\b", re.IGNORECASE)
_EN_INTENT = re.compile(r'\bwill\s+be\b|\bto\s+be\b|\bplanned\b|\bdeferred\b'
                        r'|\bpostponed\b|\bscheduled\b', re.IGNORECASE)
_ANY_VERB = re.compile('|'.join(_longest_first(list(_CJK_VERBS) + list(_ENGLISH_VERBS))))
# Intention binds to gate verbs only: 观察 follows a completed disposal
# (已修复，后续观察) instead of reopening it, while 验收 stays a gate
# (已修，待人工验收 stays pending).
_GATE_VERB = re.compile('|'.join(
    _longest_first(list(_CJK_GATE_VERBS) + list(_ENGLISH_VERBS))))
_CLAUSE_SPLIT = re.compile(r'[，、。；：！？,;:!?]|(?<!\d)\.(?!\d)')
# Level negation by grammar (invariant 5): the gap may span at most 8
# characters that start no verb (a verb-initial character in the gap marks a
# real finding, not a negated level) and carry no clause punctuation. r8:
# ASCII '.' joins the separator set (sibling#1 — _CLAUSE_SPLIT splits it, so
# 未发现缓存泄漏.P1 must not fold the level across the sentence boundary) and
# the heads of attested out-of-table statement verbs (排除 / 涉及 / 识别 /
# 触发 / 复现) join the exclusion set (boundary#3 — 尚未排除 P1 is a finding,
# not a negated level); both make the gap narrower, never wider.
_VERB_HEADS = ''.join(sorted({v[0] for v in _CJK_VERBS}))
_GAP_STATEMENT_HEADS = '涉排识触复'
_NEGATED_LEVELS = re.compile(
    '(?:没有|未发现|未见|不含|未|无(?!法))'
    '[^' + _VERB_HEADS + _GAP_STATEMENT_HEADS + '，、。；：！？,;:!?./／]{0,8}'
    r'(?<![0-9A-Za-z_])P[012](?![0-9A-Za-z_])'
    r'(?:\s*[/／、]\s*P[012](?![0-9A-Za-z_]))*', re.IGNORECASE)
# Word boundaries keep identifiers (`case_p1`) from reading as a level.
_LEVEL_TOKEN = re.compile(r'(?<![0-9A-Za-z_])p[012](?![0-9A-Za-z_])', re.IGNORECASE)
# r8 (DS boundary#4): every typographic apostrophe of the family folds to
# ASCII ' on the judgement copy — NFKC does not fold them, and hasn‘t /
# hasnʼt / hasn′t otherwise dodge the English negation while the closure
# verb still matches (false PASS). Each member has a control in
# controls/review_r1.py.
_APOSTROPHES = '’‘‛ʼ′'


def _normalized_handling(cell):
    """Judgement copy of a handling cell (#2245 r7): NFKC and the curly
    apostrophe already folded on the text copy; strip bold markers and case."""
    return cell.replace('*', '').lower()


def _handling_closed(handling):
    """The 处理/处置 cell names a disposal closure (#2245 r7).

    A gate verb matches as a substring — a verb with its object must still
    close (已升级中间件到 2.1) — except its known right extensions; English
    words match on \b edges; 无需 / won't fix are the wontfix closure phrases.
    """
    return bool(_CJK_CLOSURE_RE.search(handling)
                or _EN_CLOSURE_RE.search(handling)
                or any(phrase in handling for phrase in _CLOSURE_PHRASES))


def _handling_pending(handling):
    """The 处理/处置 cell still leaves its finding open (#2245 r7)."""
    if _CJK_FAILING_RE.search(handling):
        return True  # a failed attempt (合并失败/验收失败…) is pending
    if any(word in handling for word in _PARTIAL_WORDS):
        return True  # a partial disposal (部分修复/修复了一半) is still open (r8)
    if _CJK_IN_PROGRESS_RE.search(handling):
        return True  # 修复中 / 处置中 / 验收中 …（升级中 ≠ 已升级中间件）
    if _EXPLICIT_PENDING_RE.search(handling):
        return True  # 进行中 / 保留观察 / 待观察 / 拒绝 / 保留意见 / 不通过 …
    if _EN_PENDING_RE.search(handling) or _EN_UNDONE_RE.search(handling):
        return True  # in progress / todo / tbd / wip / unmerged / un-fixed …
    clauses = _CLAUSE_SPLIT.split(handling)
    for i, clause in enumerate(clauses):
        for marker in _NEGATION_MARKER.finditer(clause):
            if _ANY_VERB.search(clause, marker.start()):
                return True  # 未修 / 没合并 / 未保留 / not 修复 / hasn't 合入
        for marker in _EN_NEGATION.finditer(clause):
            if _ANY_VERB.search(clause, marker.start()):
                return True  # not yet fixed / never been fixed / not verified
        # Intention binds to gate verbs in the clause or in later clauses; the
        # forward scan is hoisted out of the marker loop (one pass per clause).
        forward = any(_GATE_VERB.search(later) for later in clauses[i + 1:])
        for marker in _INTENT_MARKER.finditer(clause):
            if _GATE_VERB.search(clause, marker.start()) or forward:
                return True  # 计划…修复 / 需人工验收 / 待人工验证
        for marker in _EN_INTENT.finditer(clause):
            if _GATE_VERB.search(clause, marker.start()) or forward:
                return True  # will be fixed / to be fixed / deferred, merged later
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
    with a 结论 header cell and one or more 处理/处置 header cells, a row
    closes only when every handling cell names a closure and none is still
    pending (a 处理 column beside a 处置 column is judged, not silently
    dropped — r6 review); misaligned rows are unreadable.
    """
    def levels(rows):
        return [cells for lead, cells in rows if _row_has_level(cells)]

    if not (len(run) >= 2 and run[0][0] and _is_delimiter(run[1][1])
            and not any(_is_delimiter(row[1]) for row in run[2:])):
        return levels(run), False  # unreadable shape: every level row is open
    header, data = run[0][1], run[2:]
    if '结论' not in header or not ({'处理', '处置'} & set(header)):
        return levels(data), False  # well-formed, but not a review table
    handling_cols = [i for i, cell in enumerate(header) if cell in ('处理', '处置')]
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
            # r7: every handling cell is judged on its own normalised copy —
            # cells are never concatenated, so an empty or pending cell
            # cannot hide behind another cell's closure (r6 DS#1/MM#1).
            judged = [_normalized_handling(cells[i]) for i in handling_cols]
            if any(_handling_pending(cell) for cell in judged) or not all(
                    _handling_closed(cell) for cell in judged):
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
    """Review-table rows whose P0/P1/P2 finding the 处理/处置 cell(s) leave open (#2245).

    Returns those rows, or None when the record has no readable
    结论/处理/处置 table: unverifiable is not clean. Lines come from the
    shared LF-only splitter, so a U+2028 inside a cell cannot hide half of a
    finding (CR/CRLF are normalised first). Full-width Ｐ１ folds to P1 and
    every typographic apostrophe (r8: ’ ‘ ‛ ʼ ′) folds to ' on this judgement
    copy — never on the record bytes — before matching, and the text must
    already be fence-free (see _visible_text). Each run of consecutive pipe
    lines is one table block classified by its own header.
    """
    text = unicodedata.normalize('NFKC', text)
    for curly in _APOSTROPHES:
        text = text.replace(curly, "'")
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
