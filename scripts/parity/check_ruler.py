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
# 20261109-2245.md through 20261110-2245-r12.md). Five patch rounds proved
# lexical enumeration does not converge, so verdicts now derive structurally:
#   1. every 处理/处置 cell is judged on its own copy, never concatenated —
#      a row closes only when each cell names a closure and none is pending;
#   2. the judgement copy is normalised first (apostrophe fold, THEN NFKC —
#      the order matters: U+00B4 decomposes under NFKC, r9 DS#3) and never
#      written back to the record bytes;
#   3. word/token edges: <X>中 is in-progress on ANY unknown successor and
#      only attested compound continuations close it (r10 both lineages —
#      the r9 enumerated whitelist was fail-open), bare 需/会 intent markers
#      carry morpheme guards (需求方 / 会议 / 机会, r6 MM#3), explicit pending
#      words take right edges, and closure verbs block known right
#      extensions that form a different lexeme (拒绝 / 保留意见, r6 MM#2;
#      passed to / accepted as a risk, r9–r10);
#   4. clause separators cover the CJK+ASCII punctuation set, '.' included
#      with digit protection so v1.2 stays one token (r6 MM#5);
#   5. level negation is grammar, not a modifier whitelist (r6 DS#4), in
#      two tiers since r9: an existence negation (未发现/没发现/没有发现)
#      folds the level across a bounded attributive gap, every other
#      negation head folds only across attested modifier characters — a gap
#      carrying a verb or unknown character keeps the level a real finding
#      (无法复现 P1 / 未见修复 P1 / 没有处理 P1 / 无关 P1 / 尚未定位 P1);
#   6. unknown shapes stay open, never guessed (fail-closed). One attested
#      exception, r9 anthropic boundary#6: an apostrophe-free contraction
#      OUTSIDE the closed whitelist reads closed (aint-family stems beyond
#      the attested set) — enumerated rather than guessed because
#      `\w+n'?t\b` swallows real words (want/count/plant) into negation.
# Vocabulary that cannot be derived structurally stays in named closed lists
# below; each residual entry is paired with a control in
# controls/review_r1.py (memory: ruler-real-data).
# r8 (closure-round DS review): 修改/维修/检修 join the single word source —
# an unknown disposal verb made 修改中 / 维修中 read as closed (boundary#2),
# and the English table gains accepted/migrated/passed/completed so an
# English disposal cell does not read open (sibling#5). r9 (DS r8 review,
# sibling#6): modified/upgraded/retested/rechecked join so the English
# disposal verbs stay paired with the Chinese side (降级/升级/复检/修改).
# r10 (DS boundary#6): downgraded/handled join (降级 / 处置·处理) — the
# pairing is checked by a live cross-walk in review_r1.py.
_ENGLISH_VERBS = ('done', 'fixed', 'resolved', 'closed', 'merged',
                  'verified', 'confirmed', 'reviewed', 'tested', 'checked',
                  'accepted', 'migrated', 'passed', 'completed',
                  'modified', 'upgraded', 'retested', 'rechecked',
                  'downgraded', 'handled')
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
# r9 (DS boundary#1): passed/accepted carry attested right extensions that
# read as deferral or risk-acceptance prose, not closure — the guarded form
# does not close, the bare form still does (each with a row control).
# r10 (DS sibling#1 / boundary#7, MM boundary#6): the guard shape is the
# whole extension family, not the two attested literals — accepted takes any
# determiner/preposition before risk(s) (the risk / as a risk / with risk /
# comma-separated), passed takes on to / onto / to (incl. a comma before
# them), and accepted for now is a deferral. Each shape has a row control.
# r11 (DS boundary#4 / MM boundary#4, both lineages, P0): the r10 prefix was
# ONE optional slot (`as a | the | with`) — two-slot shapes (accepted with
# the risk / as this risk / in the risk) and single-slot ones outside the
# three hard-coded heads (a / this / all risks) escaped the guard and read
# closed. The prefix is now a STACKED shape family: any run of prepositions,
# then any run of determiners, before risk(s) — each part independently
# optional (a preposition without a determiner still blocks: accepted as
# risk stayed open under r9/r10 and a first-round realdata recompute caught
# the draft stacked form requiring one), and a bare risk after a comma or a
# determiner alone blocks too.
_EN_CLOSURE_GUARDS = {'passed': r'(?![\s,]*(?:on(?:\s+to)?|onto|to)\b)',
                      'accepted': r'(?![\s,]*(?:(?:(?:as|in|on|with|'
                                  r'under|despite|after)\s+)*'
                                  r'(?:(?:a|an|the|this|that|these|'
                                  r'those|all|any|no)\s+)*'
                                  r'risks?\b|for\s+now\b))'}
_EN_CLOSURE_RE = re.compile(r'\b(?:' + '|'.join(
    word + _EN_CLOSURE_GUARDS.get(word, '') for word in _ENGLISH_VERBS)
    + r'|wontfix)\b')
_CLOSURE_PHRASES = ('无需', "won't fix")
_CJK_FAILING_RE = re.compile(
    '(?:' + '|'.join(_longest_first(_CJK_LONG_VERBS)) + ')失败')
# r9 (DS boundary#2 / boundary#4 / sibling#1 / sibling#4): <X>中 is judged
# structurally, not from the verb table — ANY CJK character before 中 makes it
# an in-progress form (回归中 / 排查中 / 定位中 were table-blind, a false PASS).
# r10 (DS concurrency#1 + boundary#1 / MM boundary#1, both lineages, P0): the
# r9 right edge was an ENUMERATED whitelist (end of cell, whitespace, clause
# punctuation, closing bracket, 的/了) — every unlisted successor (opening
# bracket 修复中（待回归）, slash 修复中/待跟进, 【, quote, digit, latin, an
# unknown CJK continuation like 修复中发现新问题) read CLOSED, a fail-open
# regression from r8. The polarity is now fail-closed: 中 is in-progress
# UNLESS the next character continues an attested noun compound. Each
# blacklisted character keeps a closed row control and each unknown-successor
# shape keeps a pending row control (review_r1.py).
# r11 (DS boundary#1 / sibling#2, MM boundary#3, both lineages, P0): the r10
# blacklist judged by the SINGLE character after 中 — 修复中等待回归 (等
# blacklisted) and 修复中间发现新问题 (间) read CLOSED, a fail-open hole. The
# entries are now WORD-level: an unambiguous noun-second character stays
# bare, and characters that also start a verb phrase carry a continuation
# guard — 等 only in the attested 中等X severities (等(?=[级严优]), NOT
# 等待), 间 only in the attested compounds 中间件/中间层/中间态 (NOT
# 修复中间发现新问题), 期 only before 指标 (评审中期指标 the mid-review METRIC
# closes, while 修复中期评估 — the
# mid-fix evaluation — is the in-progress work itself, DS sibling#2 P1). New
# attested noun seconds join bare: 台 控 南 西 (中台服务 / 中控服务 /
# 中南区网关 / 中西部节点, DS boundary#5 P1 false FAILs).
# Known boundary (fail-closed direction, DS sibling#3 / boundary#4, P2): a
# word-final 中 that is NOT progressive (缺陷集中 / 缓存命中 / 其中的) reads
# pending — the progressive/resultative distinction is not shape-derivable,
# and a false open costs a reviewer's glance while a false close hides a P1.
# Known boundary (r11): an unattested 中X noun compound (中间层 / 中期报告 /
# 中等X outside 级严优) reads pending; 修复中文档未更新 reads closed (中文
# the noun is indistinguishable from 中+文档) — both directions documented
# here, each with a row control pinning the attested side.
_CJK_COMPOUND_RULES = ('断', '心', '文', '层', '期(?=指标)', '端', '部', '档',
                       '国', '华', '台', '控', '南', '西', '间(?=件|层|态)',
                       '等(?=[级严优])')
_CJK_IN_PROGRESS_RE = re.compile(
    r'\w[ \t]{0,2}中(?!' + '|'.join(_CJK_COMPOUND_RULES) + ')')
# 拒绝 keeps matching inside longer phrases (拒绝修复 / 已拒绝合入). r9 (DS
# boundary#5): the explicit pending words carry attested right extensions —
# 不通过的用例 / 待办事项 / 保留意见变更 are noun phrases inside a closed cell,
# not pending markers; each guard keeps its row case in review_r1.py.
_PENDING_GUARDS = {'不通过': '(?![率的])', '待办': '(?![事])',
                   '保留意见': '(?!变)'}
_EXPLICIT_PENDING_RE = re.compile('|'.join(
    word + _PENDING_GUARDS.get(word, '')
    for word in _longest_first(('保留观察', '继续观察', '待观察', '待定',
                                '待办', '不通过', '拒绝', '保留意见'))))
# r8 (DS boundary#2): partial-completion residuals — a cell that names a
# partial disposal (部分修复 / 修复了一半 / 阶段性修复) is still open. r9 (DS
# sibling#2): 局部 / 过半 / 未完全 join the attested partials; 部分 carries a
# left guard so 全部分支 (已合并全部分支) is a closed compound, not a partial
# (anthropic boundary#9).
# r11 (DS sibling#4 P1 / MM boundary#6 P2): 部分 and 局部 were unconditional —
# 影响部分用户 / 剩余风险影响部分功能 and 局部地区 are closed-scope prose,
# not a partial disposal (false FAIL). 部分 gains the 影响 left guard (the
# attested 全 guard stays), 局部 gains the 地区 right guard; the English
# members gain the same treatment — almost no impact and half-open (a
# hyphenated derivative, not the fraction half) stop reading partial, and
# the dead duplicate 'half-way' is gone ('halfway' already covers it, with
# its own row control keeping it attested).
_PARTIAL_WORDS = ('部分', '一半', '阶段性', '局部', '过半', '未完全')
# Guards split by side: LEFT guards prepend ((?<!全)部分 must block the
# compound 全部分支, not the word itself), RIGHT guards append (局部(?!地区)
# — 局部地区 is a closed-scope note, not a partial disposal).
_PARTIAL_LEFT_GUARDS = {'部分': '(?<!全)(?<!影响)'}
_PARTIAL_RIGHT_GUARDS = {'局部': '(?!地区)'}
_PARTIAL_RE = re.compile('|'.join(
    _PARTIAL_LEFT_GUARDS.get(word, '') + word + _PARTIAL_RIGHT_GUARDS.get(word, '')
    for word in _longest_first(_PARTIAL_WORDS)))
# r9 (DS boundary#1 / sibling#6): the English partial family mirrors the
# Chinese partials — a modifier of partial completion keeps the row open.
# r10 (MM sibling#3): the members live in a named tuple so the control can
# walk a FIXED list (a live traversal of the module's own tuple cannot see a
# deletion — the same self-source blindness _NEGATION_WORDS hit in r9) and
# every member carries a row control.
_EN_PARTIAL_GUARDS = {'almost': r'(?!\s*no\b)', 'half': r'(?!-)'}
_EN_PARTIAL_WORDS = ('partially', 'partial', 'partly', 'almost', 'nearly',
                     'halfway', 'half', 'incomplete', 'unfinished')
_EN_PARTIAL_RE = re.compile('|'.join(
    r'\b' + re.escape(word) + _EN_PARTIAL_GUARDS.get(word, '') + r'\b'
    for word in _longest_first(_EN_PARTIAL_WORDS)),
    re.IGNORECASE)
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
# r9 (anthropic boundary#6): the apostrophe-free contraction stems are an
# attested closed whitelist — `\w+n'?t\b` would swallow real words (want /
# count / plant) into negation, a false FAIL, so an unknown stem without its
# apostrophe instead reads closed (known boundary, fail-open only there).
# r10 (DS boundary#2 / MM sibling#2 + boundary#5): the attested family
# extends to havent/dont/cant/mustnt/neednt/shant/aint and the real word
# cannot joins the negation; won't stays special — it negates EXCEPT in the
# wontfix closure phrase, so the r6 `(?!won)` hole is replaced by a
# right-extension guard: "won't fix" closes, "won't be fixed" stays open;
# the apostrophe-free 'wont' joins the whitelist for "wont be fixed" — bare
# "wont fix" (no apostrophe) then reads open, a fail-closed known boundary
# (the closure phrase keeps its apostrophe and wontfix its one word; only
# those two attested spellings close).
# r11 (MM boundary#8 / DS simplify#2): the r10 guard exempted EVERY
# \w+n't before fix/fixing, not just won't — didn't fix, can't fix read as
# affirmations, so didn't fix the leak, verified closed its cell (false
# PASS). Only won't carries the fix exemption now; every other contraction
# negates unconditionally.
_EN_CONTRACTIONS = ('hasnt', 'isnt', 'wasnt', 'doesnt', 'didnt', 'arent',
                    'werent', 'couldnt', 'wouldnt', 'shouldnt',
                    'havent', 'dont', 'cant', 'mustnt', 'neednt', 'shant',
                    'aint', 'wont')
_EN_NEGATION = re.compile(
    r"\bnot\b|\bnever\b|\bcannot\b"
    r"|\bwon't\b(?!\s*(?:fix|fixing)\b)"
    r"|\b(?!won't\b)\w+n't\b"
    r"|\b(?:" + '|'.join(_EN_CONTRACTIONS) + r")\b", re.IGNORECASE)
_EN_INTENT = re.compile(r'\bwill\s+be\b|\bto\s+be\b|\bplanned\b|\bdeferred\b'
                        r'|\bpostponed\b|\bscheduled\b', re.IGNORECASE)
# r11 (MM boundary#8): the verb after didn't / can't is the BARE form (fix,
# not fixed), which no table carries. Deriving lemmas morphologically is
# unreliable (fixed→fix, merged→merge, verified→verify), so the negation
# binding accepts a word that a closure verb STARTS WITH (fix ⊂ fixed,
# merge ⊂ merged, do ⊂ done); a word no closure verb starts with (want,
# plant, happen) binds nothing.
_EN_NEG_INFINITIVE = re.compile(r'\s*([A-Za-z]{2,})')
_ANY_VERB = re.compile('|'.join(_longest_first(_CJK_VERBS + _ENGLISH_VERBS)))
# Intention binds to gate verbs only: 观察 follows a completed disposal
# (已修复，后续观察) instead of reopening it, while 验收 stays a gate
# (已修，待人工验收 stays pending).
_GATE_VERB = re.compile('|'.join(
    _longest_first(_CJK_GATE_VERBS + _ENGLISH_VERBS)))
_CLAUSE_SPLIT = re.compile(r'[，、。；：！？,;:!?]|(?<!\d)\.(?!\d)')
# Level negation by grammar (invariant 5), two tiers as of r9 (DS boundary#2 /
# boundary#6 / sibling#3 / sibling#5 — the r8 open-set exemption let unknown
# verbs 尚未定位 / 尚未核对 / 未覆盖 P1 fold the level away, a false PASS):
#   strong — an existence-negation head (未发现 / 没发现 / 没有发现) folds the
#     level across a bounded attributive gap (up to 8 chars, no clause
#     punctuation, '.' only between digits so 未发现 v1.2 的 P1 folds while
#     未发现缓存泄漏.P1 never does);
#   weak — every other negation head (未见 / 没有 / 不含 / 未 / 无(?!法)) folds
#     only across attested modifier characters (_NEG_GAP_INNOCUOUS + spaces),
#     so a gap carrying any verb or unknown character keeps the level a real
#     finding (fail-closed: 尚未排除 / 未见修复 / 无关 / 尚未定位 P1); r10
#     (MM boundary#4) 有 joins the modifier set so 未有任何 P1 folds like
#     无任何 P1 — the same semantics no longer split on the head.
# Known boundary: a strong head with a verb inside the gap (未发现修复P1的根因)
# still folds — the attributive/relative distinction is not shape-derivable.
_NEG_GAP_INNOCUOUS = '新的增严重级别其他它任何有'
_NEGATED_LEVELS = re.compile(
    '(?:(?:没有发现|未发现|没发现)'
    r'(?:[^，、。；：！？,;:!?./／]|(?<=\d)\.(?=\d)){0,8}'
    '|(?:未见|没有|不含|未|无(?!法))[' + _NEG_GAP_INNOCUOUS + r' \t]{0,8})'
    r'(?<![0-9A-Za-z_])P[012](?![0-9A-Za-z_])'
    r'(?:\s*[/／、]\s*P[012](?![0-9A-Za-z_]))*', re.IGNORECASE)
# Word boundaries keep identifiers (`case_p1`) from reading as a level.
_LEVEL_TOKEN = re.compile(r'(?<![0-9A-Za-z_])p[012](?![0-9A-Za-z_])', re.IGNORECASE)
# r8 (DS boundary#4) / r9 (DS boundary#3 + sibling#8): every attested
# typographic apostrophe code point folds to ASCII ' on the judgement copy —
# NFKC folds none of them (verified live: U+00B4 even decomposes to space +
# combining acute), and hasn‘t / hasnʼt / hasn′t / hasnʹt / hasn´t / hasnʻt /
# hasn‚t / hasnꞌt otherwise dodge the English negation while the closure
# verb still matches (false PASS). This is the attested code-point set, not
# the whole Unicode family; each member has a control in review_r1.py, and
# U+FF07 is covered by NFKC itself.
_APOSTROPHES = '’‘‛ʼ′ʹ´ʻ‚ꞌ'
# r10 (DS concurrency#2 + boundary#3 + sibling#5, MM boundary#2 + #7): the
# r9 percentage check judged EVERY bare sub-100 percentage in the cell — a
# pass rate (回归通过率 98%), a coverage metric (覆盖率 95%), a leftover count
# (遗留 0%) and an impact note (影响 30% 用户) all reopened a closed cell
# (false FAIL), while 100.0% matched its trailing 0 and 50 % / 50 percent
# escaped entirely (false PASS). A percentage is now partial only inside a
# COMPLETION context: a completion verb within a bounded gap (no clause
# punctuation, no bracket — so 已修复，影响 30% cannot anchor and 已修复
# （影响 30% neither — no % digits and the bracket blocks the gap — but
# digits inside the gap are now fine, see r11), no 率 and not crossing the
# word "rate" (a rate metric is never a completion fraction), then a
# decimal-capable number compared as a VALUE (<100 partial, 100 / 100.0 /
# 1000 closed) and % / percent / pct with optional space.
# r11 (DS boundary#1+concurrency#2 / MM boundary#1+boundary#2, both
# lineages, P0): the verb list was a THIRD hand-copied table — the Chinese
# side carried only 8 of the 27 closure verbs (已合并 50% / 已解决 30% had no
# anchor and read closed), and the gap banned digits so 已修复 3 个用例 50%
# / completed 3 of 5 (60%) / 已完成（80%） never anchored (false PASS both
# ways). The completion verbs are now DERIVED from the single closure word
# source (gate verbs + the two attested progress nouns 进度/完成度 + the
# full English table — a cross-walk equivalence assertion pins all three in
# review_r1.py, and the derivation ends the hand-copy drift for good); the
# gap allows digits and brackets while still refusing clause punctuation,
# %, / and a second 率/rate/影响. The captured number must be a COMPLETE
# token — (?<![\d.]) stops the engine from sliding the gap into the digits
# and capturing a suffix (verified 100% must not anchor as gap ' 10' +
# number 0).
_CJK_PCT_EXTRA = ('进度', '完成度')
_PCT_COMPLETION_VERBS = (_CJK_GATE_VERBS + _CJK_PCT_EXTRA + _ENGLISH_VERBS)
_PCT_CJK_RE = '(?:' + '|'.join(_longest_first(
    _CJK_GATE_VERBS + _CJK_PCT_EXTRA)) + ')'
_PCT_EN_RE = (r'\b(?:' + '|'.join(_longest_first(_ENGLISH_VERBS)) + r')\b')
_PCT_RE = re.compile(
    '(?:' + _PCT_CJK_RE + '|' + _PCT_EN_RE + ')'
    r'(?:(?!\brate\b|impact\b|率|影响)'
    r'[^，。；：！？,;:!?%/／]){0,12}'
    r'(?<![\d.])(\d{1,4}(?:\.\d+)?)\s*(?:%|percent\b|pct\b)', re.IGNORECASE)
# r10 (MM boundary#3): a residual-quantity phrase keeps the row open even
# inside an otherwise closed cell — 已修复不通过的用例，还有 3 个 still has
# 3 undisposed findings. 无剩余 / 没有剩余 do NOT count (negated residual).
# r11 (MM boundary#5 + sibling#2 / DS sibling#5 + boundary#6, both
# lineages, P1): the r10 family was four BARE markers — an enumerated
# vocabulary again, the shape the coordinator ruling bans. The family is now
# COUNT-ANCHORED: a residual quantifier only counts when a count word or a
# known residual noun follows (还有 3 个 / 尚余 2 个 / 遗留 2 项 / fixed, 3
# remaining), so 还有 0 个 (zero) and 另有说明 (a notice, not a count) stop
# reading pending; 遗留 joins with its own noun set (遗留 2 项 / 遗留问题 /
# 遗留 P1) plus a 无/没 left guard (无遗留 closes), and the English side
# (remaining / outstanding / left / still N) finally mirrors the Chinese
# side instead of splitting by language. Known boundary (fail-closed): a
# bare 还有 without a count after it no longer reads pending — the count
# anchor is what separates the residual meaning from the discourse marker,
# so an unanchored shape costs a reviewer's glance, not a hidden finding.
# 待 rides along with the counts so 还有待回归 keeps reading residual — the
# r10 bare 还有 matched it, and dropping it would be a fail-open regression.
_CJK_COUNT_WORDS = '1-9一二三四五六七八九十百几待'
_CJK_RESIDUAL_NOUNS = 'P[012]|问题|风险|隐患|缺陷|事项'
_RESIDUAL_RE = re.compile(
    '(?<![无没])遗留(?=\\s*(?:[' + _CJK_COUNT_WORDS + ']|' + _CJK_RESIDUAL_NOUNS + '))'
    '|(?:还有|还剩|仍有|剩余|尚余|余下|另有)(?=\\s*[' + _CJK_COUNT_WORDS + '])'
    '|\\b(?:\\d+\\s*(?:remaining|outstanding|left\\s+over|left|open)\\b'
    '|still\\s+\\d+\\b)', re.IGNORECASE)


def _normalized_handling(cell):
    """Judgement copy of a handling cell (#2245 r7): the apostrophe family
    already folded on the text copy (see _open_review_rows); strip bold
    markers and case."""
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
    if _PARTIAL_RE.search(handling):
        return True  # a partial disposal (部分修复/修复了一半) is still open (r8)
    if _EN_PARTIAL_RE.search(handling):
        return True  # partially fixed / almost completed / half done (r9)
    if _RESIDUAL_RE.search(handling):
        return True  # 还有 3 个 / 尚余 2 个 / fixed, 3 remaining (r10 MM b#3,
        #             count-anchored r11 MM b#5 — see _RESIDUAL_RE)
    for m in _PCT_RE.finditer(handling):
        if float(m.group(1)) < 100:
            return True  # completed 50% / fixed 70% / 修复了 70% is partial (r9)
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
            word = _EN_NEG_INFINITIVE.match(clause, marker.end())
            if (word and marker.group(0).lower() != 'wont'
                    and any(v.startswith(word.group(1))
                            for v in _ENGLISH_VERBS)):
                # didn't fix / can't merge (r11, bare form). The
                # apostrophe-free wont + fix keeps its r10 known boundary:
                # it reads open (not the closure spelling, not pending).
                return True
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
    finding (CR/CRLF are normalised first). Every attested apostrophe code
    point (see _APOSTROPHES — not the whole Unicode family) folds to ' on
    this judgement copy — never on the record bytes — before NFKC, and the
    text must already be fence-free (see _visible_text). Full-width Ｐ１
    folds to P1 under NFKC. Each run of consecutive pipe lines is one table
    block classified by its own header.
    """
    # The apostrophe family folds BEFORE NFKC: U+00B4 decomposes to space +
    # combining acute under NFKC, so a fold after it would never fire (r9 DS
    # boundary#3); every other member is NFKC-stable, so the order is safe.
    for curly in _APOSTROPHES:
        text = text.replace(curly, "'")
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
