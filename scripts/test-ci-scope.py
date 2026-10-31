#!/usr/bin/env python3
"""test-ci-scope.py — scripts/ci_scope.py 的自测（#1199）：真实 git 正负例 + 内存内变异负控。

用法：python3 -I scripts/test-ci-scope.py   退出码 0 全过 · 1 有失败 · 2 锚点过期
不修改任何产品源码：变异体只 exec 改写后的源码文本（与 test_metadata_io_driver.py
对 CI YAML 的内存变异同模式），逐项记 KILLED / SURVIVED / INVALID，全部跑完再汇总。

fix-r1 收窄：白名单只剩 state/board.md 一个精确路径（state/ .42cog/ docs/ 的其余文件是
重 job 尺子的输入），并新增 board_literal_violations 机检——扫描 scripts/ 与 src/**/tests
下引用 state/board.md 字面量的 .py / .mjs / .sh，未在允许清单登记即违例。
"""
from __future__ import annotations

import contextlib
import io
import subprocess
import sys
import tempfile
import types
from pathlib import Path
from unittest import mock

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / 'scripts' / 'ci_scope.py'


def check(condition, tag):
    """每个断言都带 #1199 专属消息：变异体绑定到具体样例靠它。"""
    if not condition:
        raise AssertionError(tag)


def run(args, cwd):
    result = subprocess.run(args, cwd=cwd, capture_output=True, text=True, timeout=30)
    if result.returncode != 0:
        raise RuntimeError(f'{" ".join(args)} 失败（{result.returncode}）：{result.stderr.strip()}')
    return result


def head(repo):
    return run(['git', 'rev-parse', 'HEAD'], repo).stdout.strip()


def repo_with_changes(temp, changes=(), rename=None, delete=None, board_in_base=True):
    """建真实 git 仓库：base 提交含 src/main.rs（board_in_base 时含 state/board.md）。"""
    repo = Path(temp) / 'repo'
    repo.mkdir()
    run(['git', 'init', '-q'], repo)
    run(['git', 'config', 'user.email', 'ci-scope@invalid'], repo)
    run(['git', 'config', 'user.name', 'ci-scope'], repo)
    (repo / 'src').mkdir()
    (repo / 'src' / 'main.rs').write_text('fn main() {}\n', encoding='utf-8')
    (repo / 'state').mkdir()
    if board_in_base:
        (repo / 'state' / 'board.md').write_text('updated: 2026-10-31 00:00 · by base\n', encoding='utf-8')
    run(['git', 'add', '-A'], repo)
    run(['git', 'commit', '-qm', 'base'], repo)
    base = head(repo)
    for path, text in changes:
        target = repo / path
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding='utf-8')
    if rename is not None:
        old, new = rename
        (repo / new).parent.mkdir(parents=True, exist_ok=True)
        run(['git', 'mv', old, new], repo)
    if delete is not None:
        run(['git', 'rm', '-q', delete], repo)
    if not changes and rename is None and delete is None:
        return repo, base, base  # 空 diff：head 就是 base
    run(['git', 'add', '-A'], repo)
    run(['git', 'commit', '-qm', 'change'], repo)
    return repo, base, head(repo)


# ---------- 正负例（对被测模块对象跑；真实模块与变异模块都跑同一组） ----------

def example_docs_only_paths(module):
    check(module.paths_are_docs_only(['state/board.md']),
          '#1199 board-only paths classify true')


def example_board_plus_any_other(module):
    check(not module.paths_are_docs_only(['state/board.md', 'docs/a.md']),
          '#1199 board plus any other file is not docs-only')


def example_mixed_src_paths(module):
    check(not module.paths_are_docs_only(['state/board.md', 'src/main.rs']),
          '#1199 mixed src is not docs-only')


def example_github_paths(module):
    check(not module.paths_are_docs_only(['.github/workflows/ci.yml']),
          '#1199 .github is not docs-only')


def example_empty_paths(module):
    check(not module.paths_are_docs_only([]), '#1199 empty path list is not docs-only')


def example_rename_sides(module):
    check(not module.paths_are_docs_only(['docs/main.md', 'src/main.rs']),
          '#1199 rename old side is not docs-only')


def example_ruler_input_research(module):
    check(not module.paths_are_docs_only(['docs/research/008_phase-a-session-domain/gap.md']),
          '#1199 docs/research file is not docs-only')


def example_ruler_input_plan(module):
    check(not module.paths_are_docs_only(['docs/plan/005_phase-a-session-domain.md']),
          '#1199 docs/plan file is not docs-only')


def example_ruler_input_decisions(module):
    check(not module.paths_are_docs_only(['state/decisions-20261031-x.md']),
          '#1199 state/decisions file is not docs-only')


def example_42cog_file(module):
    check(not module.paths_are_docs_only(['.42cog/real.md']),
          '#1199 .42cog file is not docs-only')


def example_git_docs_only(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-pos-') as temp:
        repo, base, tip = repo_with_changes(temp, changes=[('state/board.md', 'x')])
        check(module.docs_only_for_range(repo, base, tip), '#1199 git: docs-only range is true')


def example_git_board_plus_docs(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-bpd-') as temp:
        repo, base, tip = repo_with_changes(
            temp, changes=[('state/board.md', 'x'), ('docs/a.md', 'y')])
        check(not module.docs_only_for_range(repo, base, tip),
              '#1199 git: board plus docs file is not docs-only')


def example_git_mixed(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-mix-') as temp:
        repo, base, tip = repo_with_changes(
            temp, changes=[('state/board.md', 'x'), ('src/main.rs', 'fn f() {}\n')])
        check(not module.docs_only_for_range(repo, base, tip), '#1199 git: mixed src is not docs-only')


def example_git_github(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-gh-') as temp:
        repo, base, tip = repo_with_changes(
            temp, changes=[('.github/workflows/other.yml', 'on: push\n')])
        check(not module.docs_only_for_range(repo, base, tip), '#1199 git: .github is not docs-only')


def example_git_rename(module):
    # rename 进白名单路径：改名检测会藏掉旧侧 src/main.rs，diff 就「看起来」只碰 board。
    with tempfile.TemporaryDirectory(prefix='ci-scope-rn-') as temp:
        repo, base, tip = repo_with_changes(temp, rename=('src/main.rs', 'state/board.md'),
                                            board_in_base=False)
        check(not module.docs_only_for_range(repo, base, tip), '#1199 git: rename keeps both sides')


def example_git_board_rename(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-brn-') as temp:
        repo, base, tip = repo_with_changes(temp, rename=('state/board.md', 'docs/board.md'))
        check(not module.docs_only_for_range(repo, base, tip),
              '#1199 git: renaming the board keeps both sides')


def example_git_delete_board(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-del-') as temp:
        repo, base, tip = repo_with_changes(temp, delete='state/board.md')
        check(module.docs_only_for_range(repo, base, tip),
              '#1199 git: deleting only the board stays docs-only')


def example_git_empty(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-emp-') as temp:
        repo, base, tip = repo_with_changes(temp)
        check(not module.docs_only_for_range(repo, base, tip), '#1199 git: empty diff is not docs-only')


def example_git_failure(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-err-') as temp:
        repo, base, tip = repo_with_changes(temp, changes=[('state/board.md', 'x')])
        check(not module.docs_only_for_range(repo, '0' * 40, tip), '#1199 git: failure is fail-closed')


def example_classify_push(module):
    check(module.classify({'EVENT_NAME': 'push'}) is False, '#1199 push to main stays full')


def example_classify_unknown_event(module):
    check(module.classify({'EVENT_NAME': 'schedule'}) is False, '#1199 unknown event is fail-closed')


def example_classify_missing_sha(module):
    check(module.classify({'EVENT_NAME': 'pull_request', 'PR_HEAD': 'a' * 40}) is False,
          '#1199 missing event SHA is fail-closed')


def example_classify_pull_request(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-pr-') as temp:
        repo, base, tip = repo_with_changes(temp, changes=[('state/board.md', 'x')])
        check(module.classify({'EVENT_NAME': 'pull_request', 'PR_BASE': base, 'PR_HEAD': tip}, str(repo)),
              '#1199 pull_request range reaches git')


def example_classify_merge_group(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-mg-') as temp:
        repo, base, tip = repo_with_changes(temp, changes=[('docs/a.md', 'y'), ('src/main.rs', 'fn g() {}\n')])
        check(not module.classify({'EVENT_NAME': 'merge_group', 'MG_BASE': base, 'MG_HEAD': tip}, str(repo)),
              '#1199 merge_group range reaches git')


def example_emit(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-out-') as temp:
        output = Path(temp) / 'github-output'
        with mock.patch.dict(module.os.environ, {'GITHUB_OUTPUT': str(output)}), \
                contextlib.redirect_stdout(io.StringIO()) as captured:
            module.emit(True)
        check(output.read_text(encoding='utf-8') == 'docs_only=true\n'
              and captured.getvalue() == 'docs_only=true\n',
              '#1199 emit appends to GITHUB_OUTPUT')


def example_main_fail_closed(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-main-') as temp:
        output = Path(temp) / 'github-output'

        def boom():
            raise RuntimeError('controlled classifier failure')

        with mock.patch.dict(module.os.environ, {'GITHUB_OUTPUT': str(output)}), \
                contextlib.redirect_stdout(io.StringIO()):
            original = module.classify
            module.classify = boom
            try:
                code = module.main()
            finally:
                module.classify = original
        check(code == 0 and output.read_text(encoding='utf-8') == 'docs_only=false\n',
              '#1199 classifier failure still emits false')


# ---------- 机检正负例（board_literal_violations：看守「没有尺子读 state/board.md」） ----------

def make_registered_tree(temp, module):
    """造一棵「允许清单全部登记在案」的最小合成树，返回树根。"""
    root = Path(temp) / 'root'
    for rel in module.BOARD_LITERAL_ALLOWLIST:
        target = root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text('BOARD = "state/board.md"  # 登记在案的引用\n', encoding='utf-8')
    return root


def example_scan_real_repo(module):
    check(module.board_literal_violations(ROOT) == [],
          '#1199 scan: real repo registers every board ruler')


def example_scan_allows_registered(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-reg-') as temp:
        root = make_registered_tree(temp, module)
        check(module.board_literal_violations(root) == [],
              '#1199 scan: registered board references pass')


def example_scan_flags_unregistered(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-rog-') as temp:
        root = make_registered_tree(temp, module)
        rogue = root / 'scripts' / 'rogue-check.py'
        rogue.write_text("BOARD = 'state/board.md'\n", encoding='utf-8')
        check(module.board_literal_violations(root) == ['scripts/rogue-check.py'],
              '#1199 scan: unregistered board ruler is flagged')


def example_scan_flags_src_test_ruler(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-src-') as temp:
        root = make_registered_tree(temp, module)
        rogue = root / 'src' / 'pilotdeck-gateway' / 'tests' / 'rogue.mjs'
        rogue.parent.mkdir(parents=True, exist_ok=True)
        rogue.write_text("const board = 'state/board.md';\n", encoding='utf-8')
        check(module.board_literal_violations(root) == ['src/pilotdeck-gateway/tests/rogue.mjs'],
              '#1199 scan: src tests ruler is flagged')


def example_scan_ignores_out_of_scope(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-out-') as temp:
        root = make_registered_tree(temp, module)
        for rel in ('docs/foo.md', 'scripts/notes.txt', 'src/main.rs', 'src/rogue.py'):
            target = root / rel
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text('see state/board.md\n', encoding='utf-8')
        check(module.board_literal_violations(root) == [],
              '#1199 scan: out-of-scope files are ignored')


def example_scan_flags_stale_registration(module):
    with tempfile.TemporaryDirectory(prefix='ci-scope-stl-') as temp:
        root = make_registered_tree(temp, module)
        (root / 'scripts' / 'next-task.sh').write_text('# 不再引用 board\n', encoding='utf-8')
        check(module.board_literal_violations(root) == ['scripts/next-task.sh'],
              '#1199 scan: stale registration is flagged')


EXAMPLES = (
    ('#1199 board-only paths classify true', example_docs_only_paths),
    ('#1199 board plus any other file is not docs-only', example_board_plus_any_other),
    ('#1199 mixed src is not docs-only', example_mixed_src_paths),
    ('#1199 .github is not docs-only', example_github_paths),
    ('#1199 empty path list is not docs-only', example_empty_paths),
    ('#1199 rename old side is not docs-only', example_rename_sides),
    ('#1199 docs/research file is not docs-only', example_ruler_input_research),
    ('#1199 docs/plan file is not docs-only', example_ruler_input_plan),
    ('#1199 state/decisions file is not docs-only', example_ruler_input_decisions),
    ('#1199 .42cog file is not docs-only', example_42cog_file),
    ('#1199 git: docs-only range is true', example_git_docs_only),
    ('#1199 git: board plus docs file is not docs-only', example_git_board_plus_docs),
    ('#1199 git: mixed src is not docs-only', example_git_mixed),
    ('#1199 git: .github is not docs-only', example_git_github),
    ('#1199 git: rename keeps both sides', example_git_rename),
    ('#1199 git: renaming the board keeps both sides', example_git_board_rename),
    ('#1199 git: deleting only the board stays docs-only', example_git_delete_board),
    ('#1199 git: empty diff is not docs-only', example_git_empty),
    ('#1199 git: failure is fail-closed', example_git_failure),
    ('#1199 push to main stays full', example_classify_push),
    ('#1199 unknown event is fail-closed', example_classify_unknown_event),
    ('#1199 missing event SHA is fail-closed', example_classify_missing_sha),
    ('#1199 pull_request range reaches git', example_classify_pull_request),
    ('#1199 merge_group range reaches git', example_classify_merge_group),
    ('#1199 emit appends to GITHUB_OUTPUT', example_emit),
    ('#1199 classifier failure still emits false', example_main_fail_closed),
    ('#1199 scan: real repo registers every board ruler', example_scan_real_repo),
    ('#1199 scan: registered board references pass', example_scan_allows_registered),
    ('#1199 scan: unregistered board ruler is flagged', example_scan_flags_unregistered),
    ('#1199 scan: src tests ruler is flagged', example_scan_flags_src_test_ruler),
    ('#1199 scan: out-of-scope files are ignored', example_scan_ignores_out_of_scope),
    ('#1199 scan: stale registration is flagged', example_scan_flags_stale_registration),
)


def run_examples(module):
    """跑全部样例，返回失败的 tag 列表（非 AssertionError 异常直接抛出）。"""
    failed = []
    for tag, example in EXAMPLES:
        try:
            example(module)
        except AssertionError as error:
            failed.append(str(error) or tag)
    return failed


def load(source):
    code = compile(source, str(SOURCE), 'exec')
    module = types.ModuleType('ci_scope_under_test')
    module.__dict__['__name__'] = 'ci_scope_under_test'
    exec(code, module.__dict__)  # noqa: S102 —— 变异体只进隔离命名空间，__name__ 不是 __main__
    return module


# ---------- 变异负控（全部内存内 exec，不改源码文件） ----------
# (名字, 旧锚点, 新文本, 必须命中的样例 tag)。锚点在源码中必须恰好出现一次，过期即退出 2。
MUTATIONS = (
    ('whitelist-widens-to-state-dir',
     'return bool(paths) and all(path == DOCS_ONLY_PATH for path in paths)',
     'return bool(paths) and all(path.startswith("state/") for path in paths)',
     '#1199 state/decisions file is not docs-only'),
    ('whitelist-widens-to-docs-dir',
     'return bool(paths) and all(path == DOCS_ONLY_PATH for path in paths)',
     'return bool(paths) and all(path.startswith("docs/") for path in paths)',
     '#1199 docs/research file is not docs-only'),
    ('whitelist-widens-to-42cog-dir',
     'return bool(paths) and all(path == DOCS_ONLY_PATH for path in paths)',
     'return bool(paths) and all(path.startswith(".42cog/") for path in paths)',
     '#1199 .42cog file is not docs-only'),
    ('whitelist-value-drifts',
     "DOCS_ONLY_PATH = 'state/board.md'",
     "DOCS_ONLY_PATH = 'state/board.mdx'",
     '#1199 board-only paths classify true'),
    ('empty-diff-accepted',
     'return bool(paths) and all(path == DOCS_ONLY_PATH for path in paths)',
     'return all(path == DOCS_ONLY_PATH for path in paths)',
     '#1199 git: empty diff is not docs-only'),
    ('rename-sides-hidden',
     "['git', 'diff', '--name-only', '--no-renames', '-z', base, head]",
     "['git', 'diff', '--name-only', '-z', base, head]",
     '#1199 git: rename keeps both sides'),
    ('git-failure-accepted',
     'return False  # #1199 git 出错 fail-closed',
     'return True  # #1199 git 出错 fail-closed',
     '#1199 git: failure is fail-closed'),
    ('push-counts-as-docs-only',
     'return False  # #1199 push 到 main 恒为 full',
     'return True  # #1199 push 到 main 恒为 full',
     '#1199 push to main stays full'),
    ('missing-sha-accepted',
     'return False  # #1199 事件缺 SHA fail-closed',
     'return True  # #1199 事件缺 SHA fail-closed',
     '#1199 missing event SHA is fail-closed'),
    ('scan-literal-ignored',
     'if DOCS_ONLY_PATH in _read(root, rel) and rel not in BOARD_LITERAL_ALLOWLIST:',
     'if False and rel not in BOARD_LITERAL_ALLOWLIST:',
     '#1199 scan: unregistered board ruler is flagged'),
    ('scan-allowlist-bypassed',
     'if DOCS_ONLY_PATH in _read(root, rel) and rel not in BOARD_LITERAL_ALLOWLIST:',
     'if DOCS_ONLY_PATH in _read(root, rel):',
     '#1199 scan: registered board references pass'),
    ('scan-stale-registration-ignored',
     'if DOCS_ONLY_PATH not in _read(root, rel):',
     'if False:',
     '#1199 scan: stale registration is flagged'),
    ('scan-drops-src-tests',
     "if directory == 'src' and 'tests' not in rel.parts:",
     "if directory == 'src':",
     '#1199 scan: src tests ruler is flagged'),
    ('scan-drops-mjs-extension',
     "RULER_SCAN_EXTENSIONS = ('.py', '.mjs', '.sh')",
     "RULER_SCAN_EXTENSIONS = ('.py', '.sh')",
     '#1199 scan: src tests ruler is flagged'),
)


def main() -> int:
    source = SOURCE.read_text(encoding='utf-8')
    module = load(source)
    failed = run_examples(module)
    if failed:
        print('#1199 正控失败（真源码上就有样例挂了）：', flush=True)
        for tag in failed:
            print(f'  FAIL {tag}', flush=True)
        return 1
    # 先整批验锚点：任何锚点过期都报错退出（不能记成 SURVIVED）。
    for name, old, _new, _bound in MUTATIONS:
        count = source.count(old)
        if count != 1:
            print(f'ANCHOR EXPIRED: {name}: 锚点在源码中出现 {count} 次（应为 1）：{old!r}',
                  file=sys.stderr, flush=True)
            return 2
    rows = []
    for name, old, new, bound in MUTATIONS:
        mutated = source.replace(old, new, 1)
        try:
            mutant = load(mutated)
            failed = run_examples(mutant)
        except Exception as error:  # noqa: BLE001 —— 记 INVALID，继续跑完其余变异
            rows.append((name, 'INVALID', f'变异体执行异常：{type(error).__name__}: {error}'))
            continue
        if bound in failed:
            extra = [tag for tag in failed if tag != bound]
            rows.append((name, 'KILLED', f'命中 {bound}' + (f'（另杀 {extra}）' if extra else '')))
        elif failed:
            rows.append((name, 'INVALID', f'只被未绑定样例杀死 {failed}，期望 {bound}'))
        else:
            rows.append((name, 'SURVIVED', f'无样例失败，期望命中 {bound}'))
    for name, status, reason in rows:
        print(f'{name}: {status}: {reason}', flush=True)
    counts = {status: sum(row[1] == status for row in rows) for status in ('KILLED', 'SURVIVED', 'INVALID')}
    print(f'#1199 negative summary: {counts}', flush=True)
    return 0 if rows and all(row[1] == 'KILLED' for row in rows) else 1


if __name__ == '__main__':
    raise SystemExit(main())
