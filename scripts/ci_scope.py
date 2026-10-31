#!/usr/bin/env python3
"""ci_scope.py — CI 改动范围分类器（#1199）：改动只碰白名单唯一路径才输出 docs_only=true。

用法（由 .github/workflows/ci.yml 的 scope job 调用，环境变量由该 job 注入）：
  python3 -I scripts/ci_scope.py    # 判定当前事件，写 $GITHUB_OUTPUT 并打印 docs_only=…

范围选择（#1199 规格）：
  pull_request → github.event.pull_request.base.sha..head.sha
  merge_group  → github.event.merge_group.base_sha..head_sha
  push         → 恒为 full（docs_only=false）

fail-closed：以下一律输出 false——任一路径（含 rename / delete 两侧）不等于白名单路径、
diff 为空、git 出错、事件缺 SHA、分类器自身异常。

白名单只有 state/board.md 一个精确路径（fix-r1 收窄）：state/ .42cog/ docs/ 的其余文件
是重 job 里尺子的输入——check_ledger / check_coverage_manifest 读 docs/research、docs/plan，
check_frozen / check-approval-provenance 读 state/decisions-*，product-coverage 读
docs/experiments——按目录放行会让纯文档 PR 跳过校验它们的尺子（假 PASS）。state/board.md
没有任何尺子读取（scripts/test-pdrs-loop.sh 用自造的临时测试 board），board_literal_violations
机检持续看守这条前提。放行的是「跳过重 job」，宁可多跑不可漏跑。
"""
from __future__ import annotations

import os
import subprocess
import sys

# #1199 白名单（fix-r1 收窄）：唯一精确路径，不用前缀匹配。test_metadata_io_driver.py
# 按字面钉死，scripts/test-ci-scope.py 的行为正负例同层把关——加宽或改回 startswith 都会两处失败。
DOCS_ONLY_PATH = 'state/board.md'


def paths_are_docs_only(paths) -> bool:
    """纯路径分类：路径集为空，或任一路径不等于白名单路径，即 False。"""
    return bool(paths) and all(path == DOCS_ONLY_PATH for path in paths)


def docs_only_for_range(root, base, head) -> bool:
    """base..head 两点 diff 是否只碰文档区。git 任何失败都按 False（fail-closed）。

    --no-renames 让 rename 两侧都列出（默认改名检测会藏掉旧路径），
    -z 按 NUL 切分，路径里带换行也不影响。
    """
    result = subprocess.run(
        ['git', 'diff', '--name-only', '--no-renames', '-z', base, head],
        cwd=root, stdout=subprocess.PIPE, stderr=subprocess.PIPE, check=False)
    if result.returncode != 0:
        return False  # #1199 git 出错 fail-closed
    paths = [chunk.decode() for chunk in result.stdout.split(b'\0') if chunk]
    return paths_are_docs_only(paths)


def classify(env=None, cwd='.') -> bool:
    """按事件选范围再分类。事件缺 SHA、未知事件都按 False（fail-closed）。"""
    if env is None:
        env = os.environ
    event = env.get('EVENT_NAME', '')
    if event == 'push':
        return False  # #1199 push 到 main 恒为 full
    if event == 'pull_request':
        base, head = env.get('PR_BASE', ''), env.get('PR_HEAD', '')
    elif event == 'merge_group':
        base, head = env.get('MG_BASE', ''), env.get('MG_HEAD', '')
    else:
        return False
    if not base or not head:
        return False  # #1199 事件缺 SHA fail-closed
    return docs_only_for_range(cwd, base, head)


# ---------- #1199 机检（fix-r1）：看守「没有尺子读 state/board.md」这条白名单前提 ----------
# scripts/test-ci-scope.py 在 scope job（无条件运行）里调用 board_literal_violations：扫描
# scripts/ 与 src/**/tests 下的 .py / .mjs / .sh，引用 state/board.md 字面量而不在允许清单
# 登记的都算违例；允许清单条目过期（文件不存在或不再引用字面量）也算违例，保证登记精确。
# 以后有人让尺子读 board，scope job 的自测会自动报警。允许清单逐条登记并写明为什么不危
# 险：本地 worker 工具不在 CI 任何 job 运行（ci.yml 只跑 test-pdrs-loop.sh），自造测试
# board 的用临时目录里自己那份。
BOARD_LITERAL_ALLOWLIST = {
    'scripts/ci_scope.py': '白名单常量与机检自身，只比较路径不读取文件',
    'scripts/test-ci-scope.py': '分类器自测的样例路径',
    'scripts/pdrs-loop.sh': '本地 worker 循环工具，CI 任何 job 都不运行',
    'scripts/next-task.sh': '本地 worker 工具，CI 任何 job 都不运行',
    'scripts/test-pdrs-loop.sh': '临时目录自造测试 board，不读仓库的 state/board.md',
    'src/pilotdeck-gateway/tests/test_metadata_io_driver.py': '按字面钉死白名单的守卫断言',
}
RULER_SCAN_EXTENSIONS = ('.py', '.mjs', '.sh')


def _read(root, rel):
    """读仓库相对路径的文本；文件缺失 / 不可读返回空串（按「不再引用」处理）。"""
    try:
        return (root / rel).read_text(encoding='utf-8', errors='replace')
    except OSError:
        return ''


def _ruler_files(root):
    """枚举 scripts/ 全部与 src/ 下 tests 目录内的 .py / .mjs / .sh，产出仓库相对路径。"""
    for directory in ('scripts', 'src'):
        base = root / directory
        if not base.is_dir():
            continue
        for path in sorted(base.rglob('*')):
            if not path.is_file() or path.suffix not in RULER_SCAN_EXTENSIONS:
                continue
            rel = path.relative_to(root)
            if directory == 'src' and 'tests' not in rel.parts:
                continue
            yield rel.as_posix()


def board_literal_violations(root):
    """返回违反「尺子不读 state/board.md」前提的文件（仓库相对路径，排序稳定）。

    两类违例：引用了字面量但不在允许清单（有人让尺子读 board）；允许清单条目过期
    （文件不存在或不再引用字面量，登记必须精确）。
    """
    violations = []
    for rel in _ruler_files(root):
        if DOCS_ONLY_PATH in _read(root, rel) and rel not in BOARD_LITERAL_ALLOWLIST:
            violations.append(rel)
    for rel in BOARD_LITERAL_ALLOWLIST:
        if DOCS_ONLY_PATH not in _read(root, rel):
            violations.append(rel)
    return sorted(set(violations))


def emit(value: bool) -> None:
    line = 'docs_only=' + ('true' if value else 'false') + '\n'
    output = os.environ.get('GITHUB_OUTPUT')
    if output:
        with open(output, 'a', encoding='utf-8') as handle:
            handle.write(line)
    sys.stdout.write(line)


def main() -> int:
    try:
        emit(classify())
    except Exception as error:  # noqa: BLE001 —— #1199 分类器异常一律 fail-closed 输出 false
        sys.stderr.write('ci_scope: fail-closed on ' + type(error).__name__ + ': ' + str(error) + '\n')
        emit(False)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
