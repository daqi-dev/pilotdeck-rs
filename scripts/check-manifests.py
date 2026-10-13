#!/usr/bin/env python3
"""规则验证层：校验 plugin.json 与 42plugin.json。

  plugin.json    我是谁——本包身份，遵循 Agent Plugins 1.0.0（封闭 schema）
  42plugin.json  我带了谁——装了哪些外部扩展，本项目自定

用法：python3 scripts/check-manifests.py [项目根目录，默认 .]
退出码 0 = 全绿，1 = 有错。能用规则判的，绝不留给人判。
"""
import json
import re
import sys
from pathlib import Path

SCHEMA_ID = "https://agent-plugins.org/schemas/1.0.0/plugin.schema.json"
# §5.2 封闭 schema：只许这十个顶层字段
ALLOWED = {"$schema", "name", "version", "description", "author",
           "homepage", "repository", "license", "keywords", "extensions"}
REQUIRED = {"$schema", "name"}
AUTHOR_FIELDS = {"name", "email", "url"}
# §5.5 名字约束：1–64 字符，小写字母数字与 - . ，首尾必须字母数字，不许连续 -- 或 ..
NAME_RE = re.compile(r"^[a-z0-9](?:[a-z0-9.-]{0,62}[a-z0-9])?$")

errors: list[str] = []
notes: list[str] = []


def err(msg: str) -> None:
    errors.append(msg)


def load(path: Path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        err(f"{path.name}：不是合法 JSON —— 第 {e.lineno} 行第 {e.colno} 列，{e.msg}")
    except OSError as e:
        err(f"{path.name}：读不了 —— {e}")
    return None


def check_plugin(path: Path) -> None:
    data = load(path)
    if data is None:
        return
    if not isinstance(data, dict):
        err(f"{path.name}：顶层必须是对象（§5.2）")
        return

    for f in sorted(REQUIRED - data.keys()):
        err(f"{path.name}：缺必填字段 `{f}`（§5.3）")

    for f in sorted(data.keys() - ALLOWED):
        notes.append(f"{path.name}：未知顶层字段 `{f}` 会被客户端忽略——"
                     f"客户端专属数据应放进 `extensions`（§5.2、§8）")

    if data.get("$schema") not in (None, SCHEMA_ID):
        err(f"{path.name}：`$schema` 必须是 {SCHEMA_ID}（§5.2）")

    name = data.get("name")
    if name is not None:
        if not isinstance(name, str) or not name:
            err(f"{path.name}：`name` 必须是非空字符串（§5.3）")
        elif len(name) > 64:
            err(f"{path.name}：`name` 超过 64 字符（§5.5）")
        elif "--" in name or ".." in name:
            err(f"{path.name}：`name` 不许出现连续的 `--` 或 `..`（§5.5）")
        elif not NAME_RE.match(name):
            err(f"{path.name}：`name` 只许小写字母、数字、`-`、`.`，且首尾为字母或数字"
                f"——当前 `{name}`（§5.5）")

    author = data.get("author")
    if author is not None:
        if not isinstance(author, dict):
            err(f"{path.name}：`author` 必须是对象（§5.4）")
        else:
            for f in sorted(author.keys() - AUTHOR_FIELDS):
                err(f"{path.name}：`author` 只许 name/email/url，多了 `{f}`（§5.4）")
            for f, v in author.items():
                if f in AUTHOR_FIELDS and not isinstance(v, str):
                    err(f"{path.name}：`author.{f}` 必须是字符串（§5.4）")

    for f in ("version", "description", "homepage", "repository", "license"):
        if f in data and not isinstance(data[f], str):
            err(f"{path.name}：`{f}` 必须是字符串（§5.4）")
    kw = data.get("keywords")
    if kw is not None and (not isinstance(kw, list)
                           or any(not isinstance(k, str) for k in kw)):
        err(f"{path.name}：`keywords` 必须是字符串数组（§5.4）")
    ext = data.get("extensions")
    if ext is not None and not isinstance(ext, dict):
        err(f"{path.name}：`extensions` 必须是对象（§5.6）")


def check_42plugin(path: Path) -> None:
    data = load(path)
    if data is None:
        return
    if not isinstance(data, dict):
        err(f"{path.name}：顶层必须是对象")
        return
    plugins = data.get("plugins")
    if plugins is None:
        err(f"{path.name}：缺 `plugins` 字段")
        return
    if not isinstance(plugins, list):
        err(f"{path.name}：`plugins` 必须是数组")
        return
    seen = set()
    for i, item in enumerate(plugins):
        at = f"{path.name}：plugins[{i}]"
        if not isinstance(item, dict):
            err(f"{at} 必须是对象，形如 " '{"source": "...", "version": "..."}')
            continue
        src = item.get("source")
        if not isinstance(src, str) or not src:
            err(f"{at} 缺 `source`（从哪装）")
            continue
        if src in seen:
            err(f"{at} `source` 重复：{src}")
        seen.add(src)
        ver = item.get("version")
        if ver is not None and not isinstance(ver, str):
            err(f"{at} `version` 必须是字符串")


def check_layout(root: Path) -> None:
    """§6.1 固定位置：skills/ 下每个含 SKILL.md 的直接子目录算一个 skill。"""
    skills = root / "skills"
    if skills.exists() and not skills.is_dir():
        err("skills 存在但不是目录——该组件类型无效（§6.1）")
        return
    if not skills.is_dir():
        return
    found = [d.name for d in sorted(skills.iterdir())
             if d.is_dir() and (d / "SKILL.md").is_file()]
    stray = [d.name for d in sorted(skills.iterdir())
             if d.is_dir() and not (d / "SKILL.md").is_file()
             and not d.name.startswith(".")]
    for s in stray:
        notes.append(f"skills/{s}/ 里没有 SKILL.md，不会被识别为 skill（§7.1）")
    if found:
        notes.append(f"发现 {len(found)} 个 skill：{', '.join(found)}"
                     "——它们共用根目录那一份 plugin.json（§5.1）")


def main() -> int:
    root = Path(sys.argv[1] if len(sys.argv) > 1 else ".")
    checked = 0
    for name, fn in (("plugin.json", check_plugin), ("42plugin.json", check_42plugin)):
        p = root / name
        if p.exists():
            fn(p)
            checked += 1
        else:
            notes.append(f"{name} 不存在，跳过")
    check_layout(root)

    for n in notes:
        print(f"  · {n}")
    if errors:
        print()
        for e in errors:
            print(f"✗ {e}")
        print(f"\n{len(errors)} 个错误。")
        return 1
    print(f"\n✓ 全绿（校验 {checked} 份清单）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
