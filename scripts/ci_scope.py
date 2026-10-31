#!/usr/bin/env python3
"""ci_scope.py — CI 改动范围分类器（#1199）：改动只碰文档区才输出 docs_only=true。

用法（由 .github/workflows/ci.yml 的 scope job 调用，环境变量由该 job 注入）：
  python3 -I scripts/ci_scope.py    # 判定当前事件，写 $GITHUB_OUTPUT 并打印 docs_only=…

范围选择（#1199 规格）：
  pull_request → github.event.pull_request.base.sha..head.sha
  merge_group  → github.event.merge_group.base_sha..head_sha
  push         → 恒为 full（docs_only=false）

fail-closed：以下一律输出 false——任一路径（含 rename / delete 的两侧）不在白名单、
diff 为空、git 出错、事件缺 SHA、分类器自身异常。白名单只含 state/ .42cog/ docs/
三个前缀；放行的是「跳过重 job」，宁可多跑不可漏跑。
"""
from __future__ import annotations

import os
import subprocess
import sys

# #1199 白名单：只含这三个前缀。test_metadata_io_driver.py 按字面钉死，
# scripts/test-ci-scope.py 的行为正负例同层把关——加任何一项都会两处失败。
DOCS_PREFIXES = ('state/', '.42cog/', 'docs/')


def paths_are_docs_only(paths) -> bool:
    """纯路径分类：路径集为空，或任一路径不带白名单前缀，即 False。"""
    return bool(paths) and all(path.startswith(DOCS_PREFIXES) for path in paths)


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
