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
# 20261109-2245.md through 20261111-2245-r19.md). Six patch rounds proved
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
# r12 (DS boundary#4 P1): the determiner run was still an enumerated closed
# set — quantifier determiners escaped it (accepted every risk / each risk /
# several risks / 3 risks read CLOSED, a false PASS). The run now covers the
# quantifier family (every/each/several/many/few/both) and bare digits.
# r13 (DS r12 boundary#1 P0 / sibling#1 P1, full-diff review): the stacked
# runs were STILL enumerated closed sets in a fixed order — every shape
# outside the members escaped (accepted the remaining risks / all of the
# risks / three risks / most risks / as a residual risk / accepted; risk
# noted all read CLOSED). The guard became a bounded TOKEN WINDOW.
# r14 (DS r13 boundary#2 P0 / MM r13 boundary#1 P0 + sibling#2 P2): a
# bounded window only moved the hole — a risk noun past the 4th word unit
# (accepted with the caveat that the risk remains), one glued to punctuation
# (accepted (risk noted) / accepted/risk noted) and a full-width separator
# (accepted，risk noted) all escaped again, every one a false PASS. The
# window is now CLAUSE-SCOPE: any number of word units on the shared
# separator family (ASCII + CJK clause punctuation, slash, brackets) reach
# a risk noun and the row stays open — no length constant to step over. The
# word and separator classes are complements, so the window backtracks
# linearly (each position has exactly one parse). The separator run still
# prefixes BOTH alternatives — hoisting `for now` out of it re-closes it
# (r10 pin), and the attested closures (bare accepted / accepted the
# change) carry no risk noun and stay closed.
# r14 (MM r13 sibling#1 P1): 'passed' shares the separator family — its run
# stayed ASCII-comma-only, so passed; to next round read CLOSED while the
# same deferral after accepted did not.
# r15 (DS r14 boundary#2 P1 / sibling#2 P1 + MM r13 residue): the separator
# family takes ASCII sentence punctuation and the hyphen (accepted.risk
# noted / accepted the remaining-risk read CLOSED before — the hyphen glued
# risk into a single word unit the window could not see, the same glued-
# punctuation escape the r14 P0s banned), and 'passed' gains the for-now
# deferral branch (passed for now read CLOSED while accepted for now
# pends — MM r13 boundary#6 P2 confirmed the same deferral shape). The
# separator runs are POSSESSIVE (r15 DS r14 concurrency#3 P2, measured:
# 'accepted' + 20000 commas took 9.1 s — the outer separator run and the
# trailing separator run both consumed the same comma run through an empty
# word group, a quadratic split tree; the classes stay complements, so one
# parse exists and possessive loses nothing).
# r16 (DS r15 boundary#1 P0, measured live:
# _handling_closed('accepted【risk noted】') was True — the CJK brackets
# glued risk into a word unit the window could not see, the THIRD
# same-shape miss on this family in a row (r14 passed/accepted desync,
# r15 missed .!?/-, r16 missed 【】「」（）), so the class was consolidated
# into a single constant. The control side cross-walks it against the
# CJK clause-break family (review_r1.py, en-closure-sep-matches-cjk-breaks).
# r17 (DS r16 boundary#1 P0 / sibling#1 P1, both measured live in
# _tmp/20261110-issue-2245-closure/rev-r16-ds-probe/probe.out): the
# consolidation kept a HAND-ENUMERATED member list, and the FOURTH
# same-shape miss arrived on cue — quotes (“”"'‘’), book-title marks
# (《》〈〉『』〔〕〖〗) and dashes (—–) glued the risk window shut, so
# accepted "risk noted" / accepted—risk noted / passed "to next round"
# all read CLOSED (false PASS). Per the >2-round rule the enumeration is
# GONE: a separator is now ANY non-word character ([^\w] — whitespace,
# punctuation and symbols of every script, present and future; \w keeps
# letters/digits/underscore incl. CJK as word units). No member list
# remains to miss a family on; the cross-walk shrinks to checking that
# every CJK break character is indeed non-word. The r16 hyphen-placement
# trap (a bare '-' between members is a RANGE operator) is structurally
# retired with the list it lived in.
_EN_CLOSURE_SEP = r'[^\w]'
_EN_CLOSURE_NOTSEP = r'[\w]'
_EN_CLOSURE_GUARDS = {
    'passed': (r'(?!' + _EN_CLOSURE_SEP + r'*+'
               r'(?:(?:on(?:\s+to)?|onto|to)\b|for\s+now\b))'),
    'accepted': (r'(?!' + _EN_CLOSURE_SEP + r'*+(?:'
                 r'(?:' + _EN_CLOSURE_NOTSEP + r'+'
                 + _EN_CLOSURE_SEP + r'++)*'
                 + _EN_CLOSURE_SEP + r'*+risks?\b'
                 r'|for\s+now\b))')}
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
# r12 (DS sibling#3 P1, folds boundary#9 P2): 断 部 控 stayed BARE — the
# same single-character shape the 等/间/期 fix banned — so 修复中断 /
# 修复中部署 / 修复中控制 (verb phrases: interrupted / deploying /
# controlling) read CLOSED, a fail-open hole. They now carry attested
# continuation guards (断(?=分析|场景|的|$), 部(?=审计), 控(?=服务)) so the
# attested noun compounds (进行中断分析 / 已测试中断场景 / 已修复中断的会话 /
# 已关闭中断 at a word end / 已完成中部审计 / 已修复中控服务) close while the
# verb-phrase shapes pend.
# Known boundary (fail-closed direction, DS sibling#3 / boundary#4, P2): a
# word-final 中 that is NOT progressive (缺陷集中 / 缓存命中 / 其中的) reads
# pending — the progressive/resultative distinction is not shape-derivable,
# and a false open costs a reviewer's glance while a false close hides a P1.
# Known boundary (r11): an unattested 中X noun compound (中间环节 / 中期报告 /
# 中等X outside 级严优) reads pending; 修复中文档未更新 reads closed (中文
# the noun is indistinguishable from 中+文档) — both directions documented
# here, each with a row control pinning the attested side. r12 adds the
# sibling boundary: 迁移中断 / 测试中断 (word-final 中断 as an interruption
# REPORT) share the shape with 已关闭中断 (an interruption disposal) — the
# attested disposal closes, the report shapes stay closed with it.
# r13 (DS r12 sibling#6 P2): 端 and 层 stayed BARE — the exact single-char
# shape the 等/间/期 (r11) and 断/部/控 (r12) fixes banned — so verb-phrase
# seconds pended on paper only: 修复中端详日志 / 修复中端掉旧服务 /
# 修复中层叠缓存 read CLOSED (fail-open). 端 now closes only before the
# attested noun second (中端节点, 已升级中端节点) and 层 only before the
# genitive / word end — the attested 中间层 / 中间件 / 中间态 compounds ride
# the 间 guard and are untouched.
_CJK_COMPOUND_RULES = ('断(?=分析|场景|的|$)', '心(?=化|$)', '文(?=档)',
                       '层(?=的|$)', '期(?=指标)', '端(?=节)', '部(?=审计)',
                       '档(?=配置)', '国(?=区)', '华(?=分支)', '台(?=服务)',
                       '控(?=服务)', '南(?=区)', '西(?=部)',
                       '间(?=件|层|态)', '等(?=[级严优])')
# r15 (DS r14 boundary#9 P3): the r13 tightening of bare 心 (心(?=化|$))
# left 已修复中心节点 / 已修复中心，… reading pending — a NEW fail-closed
# boundary (closed only for 中心化 and word-final 中心), registered here
# because the shape is not attested in the real record.
# r12 (DS boundary#8 P2): the left blank run before 中 was capped at two
# ({0,2}) — a three-blank run read CLOSED. Whitespace quantity carries no
# judgement, so the run is unbounded (*); the double- and triple-space row
# controls both stay green.
# r13 (DS r12 boundary#4 P1): the r12 断/部/控 continuation guards flipped
# real closed records — 已修复中断，回归通过 / 已关闭中断，后续观察 read
# pending (false FAIL). The disposal shapes differ from the genuine
# in-progress ones (修复中断，等待回归 pends on purpose, r12 row control) in
# the completed marker glued to the verb AND in the successor class: a
# GUARDED second character whose continuation failed (断 at a clause
# boundary) after an 已-marked closure verb is the OBJECT of the disposal
# (已修复中断 = fixed the interruption), while an UNKNOWN second character
# keeps the fail-closed pending even after 已 (已修复中毒 stays open, r11
# walk). The distinction needs a variable-length verb check, so it is a
# match filter below, not a lookbehind.
_CJK_IN_PROGRESS_RE = re.compile(
    r'\w[ \t]*中(?!' + '|'.join(_CJK_COMPOUND_RULES) + ')')
_CJK_GUARDED_SECONDS = tuple(rule[0] for rule in _CJK_COMPOUND_RULES
                             if '(?=' in rule)
# a guarded second character only marks a DISPOSAL OBJECT when its own
# continuation broke at a clause boundary (已修复中断，回归通过); a live
# continuation (已修复中等待回归 — 等 keeps going with 待) is still the
# in-progress marker the r11 pin holds open. Since r18 the boundary is
# structural (see _WORD_CHAR_RE): a word character continues the compound
# (fail-closed), any non-word character breaks.
# r14 (DS r13 boundary#5 P1 / MM r13 boundary#3 P1): the family was a
# closed whitelist — an opening bracket, an ASCII period or a slash after
# the guarded second (已修复中断（回归通过） / 已修复中断. / 已修复中断/回归
# 通过) kept the hit alive and read pending (false FAIL). The bracket /
# period / slash family joins (post-NFKC the full-width forms are already
# folded, they ride along as defense).
# r15 (DS r14 boundary#2 P1): the family only carried ROUND brackets — an
# opening square bracket after the guarded second (已修复中断[回归通过] /
# 已修复中断【回归通过】) kept the hit alive and read pending (false FAIL)
# while the round-bracket twin closed. The square-bracket family joins
# ([]【】, and 「」 rides along — same clause-boundary role, defense only).
# r17 (DS r16 boundary#1 P0, reverse direction, measured live in
# rev-r16-ds-probe/probe.out): the quote/book-title/dash family was missing
# here too — 已修复中断“回归通过” / 已修复中断"回归通过" / 已修复中断'回归
# 通过' kept the hit alive and read pending (false FAIL) while the 【】 twin
# closed. The family joins, mirroring the r17 English side where the SAME
# family caused the false-PASS hole.
# r18 (DS r17 sibling#1 P1, measured live in fix-r18/raw/probe-r18-claims.out):
# the r17 join MISSED 『』 — the r17 comment (and the r18 event doc) claimed
# the whole 书名号 family while the constant carried only 《》〈〉〔〕〖〗, so
# 已修复中断『回归通过』 kept reading pending exactly like the “” twin.
# 『』 joins for real. The list no longer feeds the in-progress boundary at
# all (see _WORD_CHAR_RE below) — it remains the _DISPOSED_VERB_BEFORE run
# terminator and the cross-walk anchor; that walk (review_r1.py) asserts
# every char here is non-word (a one-way invariant: the EN separator is
# the structural complement [^\w] and ⊇ this family).
_CJK_CLAUSE_BREAKS = '，。、；：！？,;:!? \t.()（）/[]【】「」“”‘’"\'《》〈〉『』〔〕〖〗—–'
# r18 (DS r17 boundary#1 P1, the FIFTH same-shape miss — r14/r15/r16/r17
# each left a family out of a hand-copied list): the character after a
# guarded second is now judged STRUCTURALLY, like the English side since
# r17 — a WORD character (\w: letters, digits, underscore of every script)
# continues the compound and keeps the hit pending (fail-closed), any
# NON-word character is a clause break. No membership list remains to miss
# a quote family on: 已修复中断『回归通过』 / ‹回归通过› / «回归通过» close
# via the boundary itself (measured live — all six unlisted shapes read
# pending before the fix).
_WORD_CHAR_RE = re.compile(r'\w')
# r14 (DS r13 boundary#7 P2 / MM sibling#5 P2 / DS sibling#3 P2, one code
# site): the disposal verb check walked _CJK_LONG_VERBS with an IMMEDIATE
# 已+verb adjacency — the single-char verbs (已修中断), a 了 after the verb
# (已修复了中断) and an adverbial between 已 and the verb (已成功修复中断)
# all read pending (false FAIL). The verb now comes from the full gate-verb
# table via a clause-scope prefix: 已, then any break-free run, then a gate
# verb with an optional 了, ending exactly at the hit start — the same
# 已[^breaks]*verb$ derivation the quantifier windows took (no adjacency
# constant), and the r11 pins (已修复中毒, 已修复中等待回归) ride the
# successor/second checks, not this prefix.
# r15 (DS r14 boundary#1 P0, measured live): the r14 prefix had no LEFT
# bound on the run between 已 and the verb — a 已-marked verb ANYWHERE
# before the hit dropped it, so 已修复A但复检中断，需要继续 (the clause
# names an unfinished re-check after a completed fix) read CLOSED. The run
# between 已 and the final verb now refuses any position where a gate verb
# STARTS: 已修复A但复检 fails (修 starts 修复 inside the run), while the
# attested disposal shapes keep their single-verb parse (已修复中断 /
# 已修中断 / 已成功修复了中断 — 成功 is no verb start).
_DISPOSED_VERB_BEFORE = re.compile(
    '已(?:(?!' + '|'.join(_longest_first(_CJK_GATE_VERBS)) + r')[^'
    + _CJK_CLAUSE_BREAKS.replace('\\', '\\\\').replace(']', '\\]')
        .replace('-', '\\-').replace('^', '\\^')
    + r'])*(?:'
    + '|'.join(_longest_first(_CJK_GATE_VERBS)) + r')了?$')
# (the break family's ] is escaped for the character class — r15 added it
# and an unescaped ] would close the class early; r17 (DS r16 sibling#3)
# also escapes '-' and '^': a '-' landing mid-class is the RANGE operator
# that silently swallows every letter between its neighbours — the same
# trap the r16 EN class comment records catching live — and '^' must not
# ever reach the class head. No member of the current family triggers
# either escape; they are hardening for the next family that joins.)


def _cjk_in_progress(handling):
    """True in-progress hits: _CJK_IN_PROGRESS_RE matches minus the
    disposal-object false hits (see the r13 comment above — the verb ending
    at the match start is checked against the 已-marked closure verbs, and
    the guarded second must sit at a clause break)."""
    hits = []
    for m in _CJK_IN_PROGRESS_RE.finditer(handling):
        succ = handling[m.end()] if m.end() < len(handling) else ''
        after = (handling[m.end() + 1] if m.end() + 1 < len(handling)
                 else '')
        if (succ in _CJK_GUARDED_SECONDS
                and (not after or not _WORD_CHAR_RE.match(after))):
            if _DISPOSED_VERB_BEFORE.search(handling[:m.start() + 1]):
                continue  # 已修复中断 / 已修中断 / 已成功修复了中断 — object
            hits.append(m)
        else:
            hits.append(m)
    return hits
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
# hyphenated derivative, not the fraction half) stop reading partial.
# r12 (DS boundary#3 P0 / sibling#1 P1 / simplify#1 P2 — one lineage, three
# roles): the r11 half guard was a blanket (?!-) and the 'half-way' member
# was deleted claiming 'halfway already covers it' — but halfway and
# half-way are DIFFERENT strings, so every hyphenated half-X (half-done /
# half-fixed / half-completed / half-way done) read CLOSED, a fail-open
# hole. The guard now blocks only the attested closed compound (half-open);
# every other hyphenated form pends again.
_PARTIAL_WORDS = ('部分', '一半', '阶段性', '局部', '过半', '未完全')
# Guards split by side: LEFT guards prepend ((?<!全)部分 must block the
# compound 全部分支, not the word itself), RIGHT guards append (局部(?!地区)
# — 局部地区 is a closed-scope note, not a partial disposal).
# r13 (DS r12 sibling#2 P1): the scope guard rode 部分 alone — 影响局部用户 /
# 影响过半用户 / 影响一半用户 are the same closed-scope prose but read pending
# (false FAIL). The 影响-prefix left guard now covers every quantity member
# of the family, not one member of it.
# r14 (DS r13 boundary#6 P2 / MM r13 sibling#3 P2 — the r13 note claimed
# "every member" while covering 4 of 6): the non-quantity members 阶段性 /
# 未完全 join the family — 影响阶段性目标 / 影响未完全覆盖的用例 are the
# same closed-scope prose and read pending (false FAIL).
_PARTIAL_LEFT_GUARDS = {'部分': '(?<!全)(?<!影响)', '局部': '(?<!影响)',
                        '过半': '(?<!影响)', '一半': '(?<!影响)',
                        '阶段性': '(?<!影响)', '未完全': '(?<!影响)'}
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
# r12 (DS sibling#7 P1 / boundary#13 P3): nearly carries the same
# no-exclusion as almost — nearly no impact is scope prose, not a partial.
# r14 (DS r13 sibling#4② P2): the English side had no scope guard at all —
# fixed; impact on half of the users read pending while the Chinese mirror
# 影响一半用户 closed (r13). 'half' gains the attested impact/affect left
# guards. They live in a LEFT-guard dict prepended before the word — the
# right-guard dict is appended after it, where a lookbehind can never see
# past the word it follows (the first attempt placed them there and the
# guard silently matched nothing).
# r15 (DS r14 sibling#3 P1 / boundary#4 P2): the r14 guard covered 'half'
# ALONE and missed the participle inflections — fixed; impact on partial
# rollout and fixed; the change affected half of the users read pending
# (false FAIL) while the Chinese mirror 影响部分用户 closed (r13). The
# inflected family (impacted / affected / impacts on) joins, and the guard
# now covers every non-exclusion member of the family. almost / nearly keep
# their no-exclusion right guard only: 'impact on almost everything' reads
# pending — the known conservative boundary (fail-closed), mirrored by the
# Chinese 几乎, which is not a partial-left-guard member either.
_EN_SCOPE_LEFT_GUARD = (r'(?<!impact\son\s)(?<!impacts\son\s)'
                        r'(?<!impact\s)(?<!impacts\s)(?<!impacting\s)'
                        r'(?<!impacted\s)(?<!affect\s)(?<!affects\s)'
                        r'(?<!affecting\s)(?<!affected\s)')
# r16 (DS r15 sibling#7 P3 + simplify#5 P3): the guarded membership is
# DERIVED from the frozen word tuple, not a hand copy — 'almost' /
# 'nearly' keep only their no-exclusion right guard (impact on almost
# everything reads pending — the known conservative boundary, mirrored by
# 几乎), and a future word joins the guard set by joining the tuple. The
# control side cross-walks the derivation (review_r1.py,
# en-partial-left-guard-derivation) and every guarded member carries an
# open polarity row (r16 rows E).
_EN_PARTIAL_WORDS = ('partially', 'partial', 'partly', 'almost', 'nearly',
                     'halfway', 'half', 'incomplete', 'unfinished')
_EN_PARTIAL_LEFT_GUARDS = {
    word: _EN_SCOPE_LEFT_GUARD for word in _EN_PARTIAL_WORDS
    if word not in ('almost', 'nearly')}
_EN_PARTIAL_GUARDS = {'almost': r'(?!\s*no\b)', 'nearly': r'(?!\s*no\b)',
                      'half': r'(?!-open\b)'}
_EN_PARTIAL_RE = re.compile('|'.join(
    _EN_PARTIAL_LEFT_GUARDS.get(word, '') + r'\b' + re.escape(word)
    + _EN_PARTIAL_GUARDS.get(word, '') + r'\b'
    for word in _longest_first(_EN_PARTIAL_WORDS)),
    re.IGNORECASE)
# r15 (DS r14 sibling#1 P1): awaiting / waiting for|on were in no family —
# fixed, awaiting retest / fixed, waiting for retest read CLOSED (false
# PASS) while the Chinese mirror 等待回归 pends. They join the explicit
# pending words: the object names an outstanding action regardless of any
# gate verb, exactly like 待's await construction.
# r16 (DS r15 sibling#1 P1, measured live: fixed for now / verified for
# now read CLOSED — 'for now' pended only after the closure verbs r15 had
# happened to guard one at a time): the deferral is an explicit pending
# word here — ANY closure verb plus the deferral pends, one place, no
# verb list to keep in step. (r17 note, DS r16 simplify#5: this regex is
# the FINAL verdict — :824 runs it before _handling_closed, so any cell
# carrying 'for now' never reaches the closure read. The for-now branches
# inside the passed/accepted guards are deliberate DUAL coverage: they
# serve the direct _handling_closed calls and the r10/r16 frozen guard
# literals, and deleting them must sync review_r1.py's guard pins.)
# The awaiting / waiting family has NO closed
# polarity anywhere by design — no attested closed shape carries the
# words (the Chinese mirror 等待回归 alike), so every occurrence pends:
# the known fail-closed boundary.
# r16 (DS r15 boundary#3 P3): the await construction takes a space too
# (待 办事项 reads pending) — a typo'd spelling of the r9 待办 noun pin
# on the conservative side; the r16 row control
# open-已修复，待 办事项 pins the current value.
_EN_PENDING_RE = re.compile(r'\bin[ -]?progress\b|\bpending\b|\btodo\b|\btbd\b'
                            r'|\bwip\b|\bawaiting\b|\bwaiting\s+(?:for|on)\b'
                            r'|\bfor\s+now\b',
                            re.IGNORECASE)
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
# r14 (DS r13 boundary#4 P0): the r5 rule "intention binds to gate verbs"
# had no shape for the AWAIT construction — 等待回归 / 待回归 name an
# outstanding action whose object is not a gate verb (回归 is not one), so
# after the r13 disposal filter dropped the 中 hit, 已修复中断，等待回归 read
# CLOSED (false PASS) while the same sentence without 已 pends on purpose
# (r12 pin). 待 now ALSO binds to the word right behind it (any CJK word —
# the await construction), with the attested closed compounds guarded on
# the right (待办事项 via the r9 待办 pin, 待遇). No left guard: 期待人工验收
# is the SAME outstanding action as 待人工验收 and must pend, and 期待/接待/
#看待 + a CJK word reading pending is the conservative direction (no
# attested closed shape carries them — verified against the row controls
# and the real record).
_MARKER_GUARDS = {
    '需': r'(?<![无不])需(?!求)',
    '会': r'(?<!不)(?<![机社体])会(?!议|话)',
    '待': r'待(?!办|遇)',
}
# r15 (DS r14 sibling#4 P1): the awaited word was CJK-only and glued —
# 已修复，等待 CI / 等待ci read CLOSED (false PASS): a Latin-named
# outstanding action escaped the construction. An optional space joins (待
# 回归 was equally dead), Latin letters and digits too (等待 3 天).
_AWAITED_WORD = re.compile(r'\s*[一-鿿A-Za-z0-9]')
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
# r12 (DS sibling#8 P1 / boundary#7 P2): the startswith rule is blind to the
# consonant+y family — verified/modify diverge from verify/modify at the
# y→i fold, so didn't verify / didn't modify bound nothing and read CLOSED.
# The stem set now carries BOTH spellings of a final-consonant+y bare form
# (verify→verifi, modify→modifi) — standard English orthography, not another
# word list; study/buy still bind nothing (no closure verb starts with them).
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
# r12 (DS boundary#10 P2): the English members of the gap exclusion were
# WHOLE WORDS (\brate\b|impact\b) while the Chinese side matches substrings
# (率|影响) — inflected forms escaped (fixed impacted 30% of users anchored a
# partial from an impact note, a false FAIL). The English exclusion now
# covers the inflected family (rates? / impacts? / impacted / impacting).
_PCT_RE = re.compile(
    '(?:' + _PCT_CJK_RE + '|' + _PCT_EN_RE + ')'
    # r13 (DS r12 boundary#2 P0): the gap was a 12-CHARACTER cap shared by
    # both languages, so it coupled to digit count — completed 300 of 500
    # (60%) needed 13 characters and fixed after retesting 50% needed 17,
    # both anchored nothing and read CLOSED (false PASS). The gap became
    # unit-based: a whole digit RUN or a whole letter RUN counts as ONE unit
    # (any length), every other character (CJK, space, bracket) counts
    # alone. The exclusion lookahead still guards the START of every unit
    # (rates / impacted / 率 / 影响).
    # r14 (DS r13 concurrency#1 + MM r13 concurrency#1, both lineages, the
    # SAME finding; measured live): the unit alternation OVERLAPPED — the
    # fallback branch also matched digits and letters, and a non-possessive
    # \d+ / [A-Za-z]+ can split one run into several units, so a long alnum
    # token with no % forced the engine through every split tree
    # (measured: a 32-char hex token in a handling cell took 4.89 s in
    # _handling_pending, exponential in run length — a full sha256 pasted
    # into a cell hangs the ruler = P0 hang). The units are now POSSESSIVE
    # (\d++ / [A-Za-z]++ — CPython 3.11+, already required by this file) and
    # the fallback class is mutually exclusive with them, so each position
    # has exactly one parse and the gap backtracks linearly. The bound
    # {0,12} is GONE (MM r13 boundary#4 P1): the cap only moved the r12
    # hole — a 13-unit tail (fixed after retesting the sharded cluster in
    # the eu west region today 50%) anchored nothing and read CLOSED. The
    # gap now runs to the next clause boundary — its class already refuses
    # clause punctuation (、 joins the family, DS simplify#1: 、 is a clause
    # separator everywhere else in this file), %, / and ／, so an unbounded
    # run cannot leave the clause or slide into the number (the
    # (?<![\d.]) tail anchor keeps whole-token capture, r11).
    # r15 (DS r14 boundary#3 P2, measured live): \d in CPython is Unicode
    # Nd, but the fallback class only refused ASCII 0-9 — an Arabic-Indic
    # digit (٣) matched BOTH \d++ and the fallback, a two-branch position
    # that replays the r14 exponential on a different alphabet (measured:
    # 0.004 s at L=28, 0.018 s at L=32, doubling per 4 chars; extrapoled
    # ~20 minutes at L=64). The fallback class now refuses \d itself, so
    # every digit position has the single \d++ parse again.
    r'(?:(?!\brates?\b|impact(?:s|ed|ing)?\b|率|影响)'
    r'(?:\d++|[A-Za-z]++|[^，。、；：！？,;:!?%/／0-9A-Za-z\d]))*'
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
# side instead of splitting by language.
# r12 (DS boundary#1 + #2 P0 / sibling#2 + #5 + #6 P1 / boundary#5 P1):
# three anchors were still narrow. (1) The residual NOUN set rode the 遗留
# branch alone — 还有问题 / 仍有风险 / 还有隐患 read CLOSED (false PASS); the
# noun set now rides the quantifier family that names a finding (还有/还剩/
# 仍有/另有), while 剩余/尚余/余下 stay count-only — 剩余风险已接受 is an
# accepted-risk closure and stays closed (boundary#1's split). 另有风险
# flips: the r11 row control pinned it closed, but it names an additional
# RISK — it pends now (sibling#5), and the flipped row control registers the
# direction. (2) The count-word class was a hand-copied subset missing
# 两/多/数/千/万 — 还有两处 / 还有两个 / 还有多个 read CLOSED (false PASS);
# the class is now the full attested numeral family. (3) 遗留's negation
# guard was single-character — 没有遗留问题 (no leftover issue = CLOSED)
# read pending (false FAIL); the guard covers the multi-char negation.
# The English mirror: zero is excluded on BOTH sides ([1-9], so fixed, 0
# remaining / still 0 open close — boundary#5), a bounded noun gap rides
# between the number and the family word (fixed, 3 issues remaining /
# 3 findings left — sibling#6), and the prepend form (remaining 3 /
# outstanding 2) joins (boundary#6 P2).
# Known boundary (fail-closed): a bare 还有 without a count after it no
# longer reads pending — the count anchor is what separates the residual
# meaning from the discourse marker, so an unanchored shape costs a
# reviewer's glance, not a hidden finding.
# 待 rides along with the counts so 还有待回归 keeps reading residual — the
# r10 bare 还有 matched it, and dropping it would be a fail-open regression.
# r13 (DS r12 boundary#3 P0 / sibling#4 P2): the count anchor was still an
# enumerated character class — the adjacent quantifier words escaped it
# (还有若干处 / 还有少量问题 / 还有较多未覆盖用例 / 还有不少问题, plus the
# colon form 还有：3 个, all read CLOSED, false PASS). 若干 joins the numeral
# family (it IS the numeral "several"); the quantifier branch now carries a
# bounded modifier window: a count word, a digit or a residual noun within
# four characters (no clause punctuation, one optional ：/: right after the
# quantifier) anchors — 较多 anchors at 多, 少量/不少 at the noun behind them.
# The English mirror (DS r12 sibling#3 P1): remain joins the family with its
# inflections (fixed, 3 issues remain / still issues remain read CLOSED
# before) and the still-branch takes the verb form — still 3 / still 0 keep
# their digit handling, the verb form only binds non-digit words.
# Known boundary (fail-closed): a bare quantifier with nothing anchored
# within the window still reads closed (还有别的说明) and a bare English
# noun + remain without a number or still (fixed, issues remain) reads
# closed — the count anchor stays the divider, same as the r12 boundary.
_CJK_COUNT_WORDS = '1-9一二三四五六七八九十百千万两几多数待若干'
# r14 (MM r13 boundary#2 P0): the noun set was a closed enumeration of
# finding words — 用例 / 场景 / 脚本 (the project's most common residual
# objects) were absent, so 已修复，还有少量未覆盖用例 read CLOSED (false
# PASS). The domain residual objects join the frozen set.
_CJK_RESIDUAL_NOUNS = 'P[012]|问题|风险|隐患|缺陷|事项|用例|场景|脚本'
# r14 (DS r13 boundary#1 P0 / sibling#1 P1 / boundary#2 P0, MM sibling#2):
# the r13 windows were bounded constants again — a modifier run one char
# past the cap (还有大量未覆盖问题), a quantifier noun outside the set
# (还有少量未覆盖用例) and the 遗留 branch still on the hard anchor
# (遗留少量问题 / 遗留：3 个) all read CLOSED. The quantifier family AND the
# 遗留 branch now anchor across the whole CLAUSE: any count word or residual
# noun between the quantifier and the next clause punctuation anchors (no
# length constant; the one optional ：/: right after the quantifier stays).
# Bare 还有别的说明 / 遗留说明 keep the closed known boundary — the count
# anchor is still the divider. r14 (DS r13 boundary#3 P0): the still-verb
# window was capped at 2 words — still a few issues remain read CLOSED; the
# window now runs over any number of words (spaces only, \w and \s are
# disjoint so it backtracks linearly).
# r15 (DS r14 concurrency#3 P2 + sibling#5 P1, measured live): the r14
# clause-scope lookaheads still carried TWO adjacent separator consumers
# (\s*…\s* before [^clause]*, which also matches spaces) — 已修复，还有 +
# 20000 spaces took >120 s, a quadratic split tree between the leading run
# and the character run. The leading space run is now POSSESSIVE and the
# redundant second \s* is gone ([^clause]* already matches spaces — one
# parse exists, possessive loses nothing). The English digit branch loses
# its {0,3} word cap (fixed, 3 of these lingering critical issues remain
# — a 4-word gap — read CLOSED): \w and \s are disjoint, the unbounded gap
# backtracks linearly. Known boundary (fail-closed, mirrors the Chinese
# side): fixed, 3 issues and closed the remaining follow-ups reads pending
# — the count anchor stays the divider, same as the r12/r13 boundaries.
_RESIDUAL_RE = re.compile(
    '(?<!没有)(?<!无)(?<!没)遗留(?=\\s*+[：:]?[^，。、；：！？,;:!?]*'
    '(?:[' + _CJK_COUNT_WORDS + ']|' + _CJK_RESIDUAL_NOUNS + '))'
    '|(?:还有|还剩|仍有|另有)(?=\\s*+[：:]?[^，。、；：！？,;:!?]*'
    '(?:[' + _CJK_COUNT_WORDS + ']|' + _CJK_RESIDUAL_NOUNS + '))'
    '|(?:剩余|尚余|余下)(?=\\s*[' + _CJK_COUNT_WORDS + '])'
    '|\\b[1-9]\\d*(?:\\s+\\w+)*\\s+(?:remaining|remains?|outstanding|left\\s+over|left|open)\\b'
    '|\\bstill\\s+[1-9]\\d*\\b'
    '|\\bstill\\s+(?!\\d)(?:\\w+\\s+)*remains?\\b'
    '|\\b(?:remaining|outstanding)\\s+[1-9]\\d*\\b', re.IGNORECASE)


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
    if _cjk_in_progress(handling):
        return True  # 修复中 / 处置中 / 验收中 …（升级中 ≠ 已升级中间件；
        #             已修复中断，回归通过 = closed disposal, r13 filter）
    if _EXPLICIT_PENDING_RE.search(handling):
        return True  # 进行中 / 保留观察 / 待观察 / 拒绝 / 保留意见 / 不通过 …
    if _EN_PENDING_RE.search(handling) or _EN_UNDONE_RE.search(handling):
        return True  # in progress / todo / tbd / wip / unmerged / un-fixed …
    clauses = _CLAUSE_SPLIT.split(handling)
    # gate_later[i] = any gate verb in clauses[i:]; built by one backward
    # pass so the marker loop below never rescans (see the r15 note inline).
    gate_later = [False] * (len(clauses) + 1)
    for i in range(len(clauses) - 1, -1, -1):
        gate_later[i] = gate_later[i + 1] or bool(
            _GATE_VERB.search(clauses[i]))
    for i, clause in enumerate(clauses):
        for marker in _NEGATION_MARKER.finditer(clause):
            if _ANY_VERB.search(clause, marker.start()):
                return True  # 未修 / 没合并 / 未保留 / not 修复 / hasn't 合入
        for marker in _EN_NEGATION.finditer(clause):
            if _ANY_VERB.search(clause, marker.start()):
                return True  # not yet fixed / never been fixed / not verified
            word = _EN_NEG_INFINITIVE.match(clause, marker.end())
            if word and marker.group(0).lower() != 'wont':
                bare = word.group(1)
                stems = ((bare, bare[:-1] + 'i')
                         if bare.endswith('y') and len(bare) > 2
                         and bare[-2] not in 'aeiou' else (bare,))
                if any(v.startswith(s) for s in stems
                       for v in _ENGLISH_VERBS):
                    # didn't fix / can't merge (r11, bare form) and the
                    # y→i family: didn't verify / didn't modify (r12). The
                    # apostrophe-free wont + fix keeps its r10 known
                    # boundary: it reads open (not the closure spelling,
                    # not pending).
                    return True
        # Intention binds to gate verbs in the clause or in later clauses; the
        # forward scan is a SUFFIX precompute (one _GATE_VERB pass over the
        # clauses, shared by every i) — r15 (DS r14 concurrency#3 P2, shape
        # 2): the per-clause any() re-scanned all later clauses, a Python-
        # level quadratic that 20000 commas in one cell turned into 7 s.
        forward = gate_later[i + 1]
        for marker in _INTENT_MARKER.finditer(clause):
            if _GATE_VERB.search(clause, marker.start()) or forward:
                return True  # 计划…修复 / 需人工验收 / 待人工验证
            # r14 (DS r13 boundary#4 P0): the await construction — 待 glued
            # to the word behind it (等待回归 / 待回归) is an outstanding
            # action even when that word is no gate verb.
            if (marker.group(0) == '待'
                    and _AWAITED_WORD.match(clause, marker.end())):
                return True  # 等待回归 / 待回归 / 等待复测
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
