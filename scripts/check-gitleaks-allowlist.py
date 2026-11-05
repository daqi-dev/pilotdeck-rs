#!/usr/bin/env python3
"""check-gitleaks-allowlist.py — `.gitleaks.toml` 值级放行的机检落点（#1595 / #1594）。

守两件事：
1. 静态口径（`--static`，不需要 gitleaks，CI 可跑；站点检查仍会起 `git grep` 子进程）：
   - **分桶不看值的形状，只看它是不是私钥 DER 片段**：正则去锚点后能精确解码成一个干净字面量
     （没有未转义元字符、没有 `\\d` 这类转义）、且不是私钥 DER 开头 → **一律算值级**，
     必须 `regexTarget = "secret"` + `^…$` 两端锚定。这样大写 hex、sha512 的 128-hex、无填充长
     base64、带前缀的 `sha256:…` 都跑不掉（早先按「64-hex / 带 `=` 填充短 base64」两种形状分桶，
     落不进这两种形状的哈希类字面量会被归到片段桶，而片段桶允许 `match` 子串语义——那正是本单
     要消灭的东西）。
   - 片段条目（私钥 PEM 正文/前缀）是唯一例外，判定不是「看着像 base64 头」，而是**解码后逐段核结构**：
     SEQUENCE 头 `0x30` → INTEGER 长度 1 → 版本号 0（PKCS#8，后跟 AlgorithmIdentifier `0x30`）
     或 1（SEC1 ECPrivateKey，后跟 OCTET STRING `0x04`）。只看前两个字符不够——任何 `MH…` 都解出
     `0x30 0x7x`、`MI…` 都解出 `0x30 0x8x`，随便一个长 base64 值就能冒充片段拿到子串放行。
     hex 字面量本来也进不来（`M`/`H`/`I` 都不是 hex 字符）。
     片段条目必须 `regexTarget = "match"`（片段只能是子串语义；写成 "secret" 而正则又不锚定，
     就退化成子串放行），description 必须写明「按 match 子串语义放行」与理由，且片段有长度下界
     ——把 PEM 前缀缩成 `MIGH` 等于放行一整族私钥。长度按**解码后**的值量（转义形态先还原）。
   - 条目里出现 `paths` / `stopwords` / `commits` 等本检查器不认识的键一律判红：`paths` 会整目录跳过扫描，
     顶层出现 `rules`（`[[rules.allowlists]]`）同样判红——那是另一型放行，必须连本检查器一起扩展。
     `[extend]` 也只允许 `useDefault`：8.30.1 实测 `disabledRules = ["generic-api-key"]` 会把规则整条禁掉，
     全仓扫描恒「no leaks found」而本闸仍绿。
   - 空 regex 一律判红：gitleaks 里 `regexes = [""]` 匹配一切，等于把整个闸关掉
     （实测见 `_tmp/20261106-gitleaks-tighten/`）。
   - **条目 ↔ 站点一致性**：每条放行的值必须仍能在仓内某个跟踪文件里原样找到（配置文件自身与
     本检查器不计）。值级字面量要求整 token；片段条目要求**整条片段正则**逐字匹配在一个 PEM 私钥块里、
     落在 description 点名的那个仓内相对路径上（逐字相等，不按后缀）、且兑现它声明的放行范围
     （「完整 PEM 正文」= 与 PEM 块正文归一后逐字相等；「前缀」= 落在正文开头）。夹具用 openssl
     重新生成、基线源文件换版之后条目会与站点脱钩——放行静默失效、命中被报、推送被拒，而形态检查照绿。
     站点查询会起 `git grep` 子进程（没有 `.git` 时退回全树遍历），所以 `--static` 是「不需要 gitleaks」，
     不是「不起子进程」。
2. 行为负控（默认模式，需要本机有 gitleaks）：用配置里的字面量现造样本——「Secret 恰等于该值」
   必须放行，「含该值但不等」（前后缀各一种）必须命中；样本先用「放行全关」配置验证真的会触发
   规则，并核对 gitleaks **实际抽出的 Secret 恰是预期文本**、命中的**是同一条 RuleID**（只看文件名
   会把「规则切在边界外、抽出的 Secret 其实等于放行值」这种假绿放过去）。没有 gitleaks 时打印 SKIP
   并**退 3**（不与「跑过且通过」同码），CI 里只跑 `--static`。

用法：
  python3 -I scripts/check-gitleaks-allowlist.py                        # 静态 + 行为（无 gitleaks 则退 3）
  python3 -I scripts/check-gitleaks-allowlist.py --static               # 只静态（CI 用）
  python3 -I scripts/check-gitleaks-allowlist.py --static --self-check  # 只静态 + 变异负控（CI 用）
  python3 -I scripts/check-gitleaks-allowlist.py --self-check           # 附加变异负控；行为负控仍会跑

退出码：0 静态口径通过（行为负控跑了且通过时也是 0）· 1 判红 · 2 前置错误
（缺配置 / 解析失败 / gitleaks 没写出报告且非零退出）· 3 行为负控 SKIP（本机没有 gitleaks，
静态口径已通过——「没跑」与「跑过」不同码，闸门才能要求「必须跑过」）。

覆盖范围：只守 `.gitleaks.toml` 的顶层 allowlists 与 extend。行内 `gitleaks:allow` 注释（行级
跳过）、仓根 `.gitleaksignore`（按指纹跳过，出现即判红）、`[[rules.allowlists]]`（判红）
各按上述口径处理；前者不在本检查器范围内——它跳过的是整行，值仍是公开示例 nonce。
"""

from __future__ import annotations

import argparse
import base64
import binascii
import json
import re
import shutil
import subprocess
import sys
import tempfile
from collections import Counter as _Counter
from pathlib import Path
from typing import NamedTuple

ROOT = Path(__file__).resolve().parent.parent
CONFIG = ROOT / ".gitleaks.toml"

# 值的形状只用于**取样**（行为负控挑一个能触发 generic-api-key 的模板），不用于分桶：
# 分桶只认「是不是 DER 私钥片段」，见 is_der_fragment / is_value_level。
HASH_RE = re.compile(r"^[0-9a-f]{64}$")
# 私钥 DER 的 base64 形态一定以 ASN.1 SEQUENCE 头开头：首字节 0x30 → 'M'，次字节决定第二个字符——
# 短形式长度 0x77 → 'H'（`MHcCAQ…`），长形式 0x81/0x82 → 'I'（`MIGH…` / `MIIE…`）。
# hex 字面量永远进不来：'M' / 'H' / 'I' 都不是 hex 字符，所以哈希族一个也落不进片段桶。
DER_B64_PREFIXES = ("MH", "MI")
# base64 字符集（含填充）：片段解码前先确认整串是 base64，避免把带杂字符的字面量硬解成半个 DER。
BASE64_CHARS = set("ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/=")
# 正则元字符：出现在值里必须转义，否则放行的是一族值（`+` 是量词、`.` 是任意字符）。
METACHARS = set(".^$*+?()[]{}|")
ESCAPABLE = METACHARS | {"\\", "/", "-"}  # Go RE2 里 `\-` 是合法字面量转义
# 非值级条目（PEM 正文/前缀等片段）必须在 description 里写明这句 + 理由，否则判红。
SUBSTRING_OPT_OUT = "按 match 子串语义放行"
# 片段条目还必须在 description 里**声明放行范围**，二选一、且只能写一个：
#   「完整 PEM 正文」——放行的是整把私钥的正文，站点检查要求归一后与 PEM 块正文逐字相等；
#   「前缀」      ——有意只放行 DER 的 base64 前缀（锚定整把私钥不现实），站点检查要求它落在正文开头。
# 少了这条声明，把一条「完整 PEM 正文」截成 64 字符前缀后 check_config / check_sites 全绿（codex 评审实测：
# 条目 #9 从 164 截到 64，两条检查都是 0 错误）——放行从「这一把私钥」扩成「同头同尾的一族私钥」。
FRAGMENT_SCOPE_FULL = "完整 PEM 正文"
FRAGMENT_SCOPE_PREFIX = "前缀"
# 片段条目的长度下界：P-256 PKCS#8 私钥的固定 DER 头是 36 字节，base64 后正好 48 字符、全族恒定。
# 实测（仓内 MIGH 片段解码）：30 81 87 | 02 01 00 | 30 13 06 07 2a8648ce3d0201 |
# 06 08 2a8648ce3d030107 | 04 6d | 30 6b | 02 01 01 | 04 20 —— 第 37 字节起才是 32 字节密钥材料。
# 所以截到 48 字符就是「零密钥字节」的族级前缀，等于放行所有 P-256 私钥；下界必须明显高于它。
# 64 字符 = 48 + 16（12 组 → 12 字节密钥材料），也正好是仓内现有三个 MIGH 片段的最短长度。
MIN_FRAGMENT_LEN = 64
# 条目里只允许这三个键；`paths` / `stopwords` / `commits` 会整类跳过或整类放行，必须连本检查器一起扩展。
ALLOWED_ENTRY_KEYS = {"description", "regexTarget", "regexes"}
# 顶层只允许这两张表；多出 `rules`（规则级放行）或 `[allowlist]` 单数表都是另一型放行/跳过，一律判红。
ALLOWED_TOP_KEYS = {"extend", "allowlists"}
# `[extend]` 只允许 useDefault：8.30.1 实测 `disabledRules = ["generic-api-key"]` 会整条禁掉规则，
# 全仓扫描恒为「no leaks found」而本检查器仍绿（`useDefault` 仍是 true，光查它挡不住）。
ALLOWED_EXTEND_KEYS = {"useDefault"}
# gitleaks 的按指纹忽略通道：仓根一出现就是「整条命中被静默压掉」，本检查器看不见，必须判红。
IGNORE_FILES = (".gitleaksignore", ".gitleaks-ignore")
MIN_VALUE_ENTRIES = 8  # 当前 7 条哈希 + 1 条 RFC 6455 nonce；只防「删条后静默变绿」，挡不住新增不合规条目（那靠逐条规则）
MIN_LITERALS = 20       # 7 条哈希共 19 个字面量 + 1 个 nonce
# 「条目 ↔ 站点」一致性：每条放行必须在仓内还能找到它放行的那个值（配置自身不算站点）。
# 夹具重新生成（openssl 换密钥 / 换基线源文件）后条目会与站点脱钩——放行静默失效、私钥被报，
# 而形态检查照绿。搜索时跳过这些目录与配置文件本身。
SITE_SKIP_DIRS = {".git", "_build", "_tmp", "node_modules", "target", ".venv", "__pycache__"}
SITE_MAX_BYTES = 2_000_000
SITE_SELF_EXCLUDE = {".gitleaks.toml", "scripts/check-gitleaks-allowlist.py"}
# 值级字面量的「整 token」边界：前后都不能接 hex / base64 字符。少了这条，把 64-hex 截成前
# 16 字符仍能在 fixture 里找到（完整哈希的前缀），全套检查一起变绿而真实 Secret 不再被放行。
SITE_TOKEN_CHARS = "0-9A-Za-z+/=_-"
_SITE_CANDIDATE_CACHE: dict[tuple[str, str], list[Path]] = {}


def _token_pattern(literal: str) -> "re.Pattern[str]":
    return re.compile(rf"(?<![{SITE_TOKEN_CHARS}]){re.escape(literal)}(?![{SITE_TOKEN_CHARS}])")


def die(msg: str):
    """前置错误统一走退出码 2（`sys.exit("…")` 会退 1，与「判红」混淆）。"""
    print(f"check-gitleaks-allowlist: {msg}", file=sys.stderr)
    sys.exit(2)


def load_config(path: Path) -> dict:
    try:
        import tomllib
    except ModuleNotFoundError:  # pragma: no cover - CI 与本机都是 3.11+
        die("需要 Python 3.11+（tomllib）")
    try:
        with path.open("rb") as fh:
            return tomllib.load(fh)
    except FileNotFoundError:
        die(f"缺 {path}")
    except tomllib.TOMLDecodeError as exc:
        die(f"{path} 解析失败：{exc}")


def strip_anchors(regex: str) -> str:
    """脱掉两端锚点（连写多个锚点也脱干净，`^^x$$` 与 `^x$` 等价）。"""
    body = regex
    while body.startswith("^"):
        body = body[1:]
    while body.endswith("$"):
        body = body[:-1]
    return body


def decode_exact(regex: str) -> str | None:
    """`regex` 若整个是「转义过的字面量」（去掉两端锚点后），返回还原值；否则 None。

    未转义的元字符（`+ . *` 等）返回 None——它们让正则匹配一族值而不是一个值；
    `\\n` / `\\d` 这类非元字符转义同样返回 None（那是正则片段，不是值）。
    脱完锚点后是空串（`^$`）也返回 None：空值不是字面量。
    """
    body = strip_anchors(regex)
    out: list[str] = []
    i = 0
    while i < len(body):
        c = body[i]
        if c == "\\":
            if i + 1 >= len(body) or body[i + 1] not in ESCAPABLE:
                return None
            out.append(body[i + 1])
            i += 2
            continue
        if c in METACHARS:
            return None
        out.append(c)
        i += 1
    return "".join(out) or None


def der_private_key_prefix(raw: bytes) -> bool:
    """解码后的字节是不是私钥 DER 的开头（PKCS#8 版本 0 或 SEC1 ECPrivateKey 版本 1）。

    只校验「像 ASN.1 SEQUENCE」不够：base64 里任何 `MH…` 都解出 `0x30 0x7x`、`MI…` 解出
    `0x30 0x8x`，于是任意以 MH/MI 开头的长 base64 值都能冒充片段、拿到 match 子串放行。
    这里按真实结构逐段核：SEQUENCE 头 → INTEGER 长度 1 → 版本号 0 或 1 → 下一个元素
    （PKCS#8 是 AlgorithmIdentifier `0x30`，SEC1 是私钥 OCTET STRING `0x04`）。
    """
    if len(raw) < 8 or raw[0] != 0x30:
        return False
    i = 1
    first = raw[i]
    i += 1
    if first & 0x80:                      # 长形式长度：只认 1–2 字节（P-256/RSA 私钥都在这个区间）
        n = first & 0x7F
        if not 1 <= n <= 2 or i + n >= len(raw):
            return False
        i += n
    if i + 3 > len(raw) or raw[i] != 0x02 or raw[i + 1] != 0x01:
        return False                      # 必须是 INTEGER，长度 1
    version = raw[i + 2]
    i += 3
    if i >= len(raw):
        return False
    if version == 0:                      # PKCS#8 PrivateKeyInfo：版本 0 后跟 AlgorithmIdentifier
        return raw[i] == 0x30
    if version == 1:                      # SEC1 ECPrivateKey：版本 1 后跟 privateKey OCTET STRING
        return raw[i] == 0x04
    return False


def normalized_literal(regex: str) -> str | None:
    """解码后的字面量，再去掉 PEM 正文里的字面 `\\n` / `\\r`（JSON 转义换行）。

    片段判定与片段长度**共用这一份归一**：两处各写各的，就会出现「一边按去换行的值解码、
    一边按没去换行的值量长度」——48 个 base64 字符加几个 `\\n` 转义就能把长度撑过 64 的下界。
    """
    plain = decode_exact(regex)
    if plain is None:
        return None
    return plain.replace("\\n", "").replace("\\r", "")


def is_der_fragment(regex: str) -> bool:
    """是不是私钥 DER/PEM 的 base64 片段（片段桶的唯一入口）。

    判定：去锚点后以 `MH`/`MI` 开头（ASN.1 SEQUENCE 的 base64 头）**且解码后真是私钥 DER 的开头**
    （见 `der_private_key_prefix`）。PEM 正文里的转义换行（JSON 写法的字面 `\\n` / `\\r`）先去掉
    再解码——`#551` / `#676` 两条实测形态都走这条路径，不能因为带反斜杠就绕过结构校验。
    其余一切「能精确解码成干净字面量」的正则都是值级，必须 secret + 锚定：不按值的形状分桶，
    是为了让大写 hex / 128-hex / 无填充长 base64 / `sha256:…` 这类哈希族字面量无处可躲。
    """
    if not strip_anchors(regex).startswith(DER_B64_PREFIXES):
        return False
    b64 = normalized_literal(regex)
    if b64 is None:
        return False                      # 含 `\\d` 这类非字面量转义：不是片段
    if not b64 or any(c not in BASE64_CHARS for c in b64):
        return False
    try:
        raw = base64.b64decode(b64 + "=" * (-len(b64) % 4), validate=True)
    except (binascii.Error, ValueError):
        return False
    return der_private_key_prefix(raw)


def is_value_level(regex: str) -> bool:
    """能精确解码成一个干净字面量、且不是 DER 私钥片段 → 值级（必须 secret + `^…$`）。"""
    return decode_exact(regex) is not None and not is_der_fragment(regex)


def fragment_len(regex: str) -> int:
    """片段长度按**归一后**的值量：`\\+` / `\\\\` 转义不虚增，PEM 正文的字面 `\\n` / `\\r` 也不算长度。

    与 `is_der_fragment` 共用 `normalized_literal`：换行转义只是 JSON 写法的排版，不是密钥字节；
    按含转义的串量长度，48 个 base64 字符（P-256 共享 DER 头、零密钥字节）加 8 个 `\\n` 就有 64，
    直接把下界绕过去、放行整族私钥。
    """
    v = normalized_literal(regex)
    return len(v) if v is not None else len(strip_anchors(regex))


def escape_literal(value: str) -> str:
    """把还原值写回正则字面量形态（片段自检里按解码值截断用）。"""
    return "".join("\\" + c if c in ESCAPABLE else c for c in value)


def fragment_scope(description: str) -> str:
    """片段条目声明的放行范围：\"full\"（整段 PEM 正文）/ \"prefix\"（DER 的 base64 前缀）/ \"\"（没声明或有歧义）。

    两种声明只能出现一种：都写时无法判断条目到底承诺了什么，按「没声明」判红（fail-closed）。
    """
    full = FRAGMENT_SCOPE_FULL in description
    prefix = FRAGMENT_SCOPE_PREFIX in description
    if full == prefix:
        return ""
    return "full" if full else "prefix"


def has_regex_semantics(regex: str) -> bool:
    """去掉两端锚点后，是否带「一族值」语义：未转义元字符，或 `\\d` / `\\w` / `\\s` 这类字符类转义。

    与 `decode_exact` 共用同一套合法转义定义（`ESCAPABLE`）：`\\.` `\\+` `\\\\` 这种转义过的字面量不算。
    """
    body = strip_anchors(regex)
    i = 0
    while i < len(body):
        c = body[i]
        if c == "\\":
            if i + 1 >= len(body) or body[i + 1] not in ESCAPABLE:
                return True
            i += 2
            continue
        if c in METACHARS:
            return True
        i += 1
    return False


def value_entries(cfg: dict) -> list[dict]:
    """值级放行条目（全部正则都是值级字面量）。形状不对时返回空，由 check_config 报出来。"""
    allowlists = cfg.get("allowlists")
    if not isinstance(allowlists, list):
        return []          # `[allowlists]` 单数表 / 字符串等：不是条目列表，交给 check_config 判红
    out = []
    for entry in allowlists:
        if not isinstance(entry, dict):
            continue
        regexes = entry.get("regexes") or []
        if not isinstance(regexes, list):
            continue
        if regexes and all(isinstance(r, str) and is_value_level(r) for r in regexes):
            out.append(entry)
    return out


def check_config(cfg: dict, root: Path = ROOT) -> list[str]:
    """返回违规说明列表；空列表 = 通过。"""
    bad: list[str] = []
    # 顶层键白名单：`rules` / `[allowlist]` 单数表都是另一型放行，必须连本检查器一起扩展。
    extra_top = sorted(set(cfg) - ALLOWED_TOP_KEYS)
    if extra_top:
        bad.append(
            f"顶层出现本检查器不认识的键 {extra_top}（rules / [allowlist] 单数表等是另一型放行或整类跳过，"
            f"必须连本检查器一起扩展）"
        )
    # 零规则边界：`useDefault = false`（或误删 [extend] 表）会让全仓扫描恒为「no leaks found」，而本闸仍绿。
    extend = cfg.get("extend")
    if not isinstance(extend, dict) or extend.get("useDefault") is not True:
        bad.append("缺 [extend] useDefault = true：规则集被掏空，全仓扫描会恒为「no leaks found」而本闸假通过")
    elif set(extend) - ALLOWED_EXTEND_KEYS:
        extra = sorted(set(extend) - ALLOWED_EXTEND_KEYS)
        bad.append(
            f"[extend] 出现本检查器不认识的键 {extra}：8.30.1 实测 disabledRules 会整条禁掉规则"
            f"（全仓恒「no leaks found」而本闸绿），新增键必须连本检查器一起扩展"
        )
    for name in IGNORE_FILES:
        if (root / name).exists():
            bad.append(f"仓根出现 {name}：gitleaks 的按指纹忽略通道会静默压掉命中，本检查器看不见，必须判红")
    entries = value_entries(cfg)
    if len(entries) < MIN_VALUE_ENTRIES:
        bad.append(f"值级放行条目只剩 {len(entries)} 条（应 ≥ {MIN_VALUE_ENTRIES}）：配置被删条或改成了正则片段")
    literals: set[str] = set()   # 按去重后的值计，复制一条真放行不能补上被删的那条
    for idx, entry in enumerate(cfg.get("allowlists", []) or [], start=1):
        if not isinstance(entry, dict):
            bad.append(f"条目 #{idx}: 不是表（TOML 形状不对），本检查器无法判定")
            continue
        extra = sorted(set(entry) - ALLOWED_ENTRY_KEYS)
        if extra:
            bad.append(
                f"条目 #{idx}: 出现本检查器不认识的键 {extra}——paths 会整目录跳过扫描、stopwords/commits 会整类放行，"
                f"新增这类键必须连本检查器一起扩展"
            )
        regexes = entry.get("regexes") or []
        desc = str(entry.get("description") or "")
        target = entry.get("regexTarget")
        if not isinstance(regexes, list) or not all(isinstance(r, str) for r in regexes):
            bad.append(f"条目 #{idx}: regexes 不是字符串列表（TOML 形状不对），本检查器无法判定")
            continue
        if not regexes:
            # 本仓库只用 regexes 型；paths / stopwords 型放行等于整类跳过，必须连同本检查器一起扩展。
            bad.append(f"条目 #{idx}: 没有 regexes（本检查器只认 regexes 型；新增其它型请同时扩展本检查器）")
            continue
        if not desc:
            bad.append(f"条目 #{idx}: 缺 description")
        if any(not str(r).strip() for r in regexes):
            bad.append(f"条目 #{idx}: 有空 regex——gitleaks 里空正则匹配一切，等于关掉整个闸")
            continue
        has_fragment = False
        for r in regexes:
            if is_value_level(r):
                literals.add(decode_exact(r) or "")
                if target != "secret":
                    bad.append(
                        f'条目 #{idx}: 值级字面量在 regexTarget = {target!r} 下放行'
                        f'（必须是 "secret"；"match" 是子串语义）'
                    )
                if not (r.startswith("^") and r.endswith("$")):
                    bad.append(f"条目 #{idx}: 值级字面量未两端锚定：{r[:24]}…")
                continue
            has_fragment = True
            # 片段桶只有 DER 私钥片段这一个入口；不在这里的形态（正则片段、新族字面量）一律判红，
            # 免得又冒出「落不进值级桶所以掉进片段桶」的逃逸。
            if not is_der_fragment(r):
                bad.append(
                    f"条目 #{idx}: 正则既不能精确解码成字面量、也不是 DER 私钥片段"
                    f"（本检查器只认这两类放行，新增类型必须连本检查器一起扩展）：{r[:32]}…"
                )
            # 片段条目绝不能锚定：regexTarget="match" 匹配的是整段命中（PEM 里含
            # `-----BEGIN … PRIVATE KEY-----` 头），`^片段$` 永不命中——放行静默失效，
            # 夹具私钥会在推送链上被报出，而本检查器照绿。8.30.1 实测见 evidence。
            if r.startswith("^") or r.endswith("$"):
                bad.append(
                    f"条目 #{idx}: 片段正则加了锚点（{r[:24]}…）——match 语义比的是整段命中、"
                    f"含 PEM 头，锚定后永不命中，放行会静默失效；片段条目必须不锚定"
                )
            if has_regex_semantics(r):
                bad.append(
                    f"条目 #{idx}: 正则含未转义元字符或字符类转义（`+`/`.`/`\\d` 这类是量词/一族值，"
                    f"不是精确值）：{r[:32]}…"
                )
            flen = fragment_len(r)
            if flen < MIN_FRAGMENT_LEN:
                bad.append(
                    f"条目 #{idx}: 片段只有 {flen} 字符（应 ≥ {MIN_FRAGMENT_LEN}；"
                    f"过短前缀等于放行一整族）：{r[:32]}…"
                )
        if has_fragment:
            if target != "match":
                bad.append(
                    f'条目 #{idx}: 含片段正则的条目 regexTarget = {target!r}'
                    f'（片段条目必须是 "match"；"secret" 配未锚定正则会退化成子串语义）'
                )
            if SUBSTRING_OPT_OUT not in desc:
                bad.append(
                    f'条目 #{idx}: 片段放行没在 description 写「{SUBSTRING_OPT_OUT}」+ 理由'
                )
            hint = _site_hint(desc)
            if "/" not in hint:
                bad.append(
                    f"条目 #{idx}: 片段条目的 description 没有点名仓内**相对路径**"
                    f"（现在写的是 {hint or '（无文件名）'!r}）——只写文件名时，"
                    f"同名夹具（多个 tls.json）可以互相冒充，站点一致性检查会假 PASS"
                )
            if not fragment_scope(desc):
                bad.append(
                    f"条目 #{idx}: 片段条目的 description 必须且只能声明一种放行范围："
                    f"「{FRAGMENT_SCOPE_FULL}」或「{FRAGMENT_SCOPE_PREFIX}」"
                    f"（现在两种都没写、或者两种都写了）——不声明范围时，"
                    f"把整段 PEM 正文截成前缀（放行一族私钥）也能通过静态检查与站点检查"
                )
    if len(literals) < MIN_LITERALS:
        bad.append(f"值级字面量只剩 {len(literals)} 个（应 ≥ {MIN_LITERALS}）")
    return bad


class SiteProbe(NamedTuple):
    """一条放行条目在仓内的落点探针。

    kind="value"：literal 是放行值本身，要求作为**整 token** 出现在某个跟踪文件里。
    kind="fragment"：literal 只用来缩小候选文件（最长的一段连续 base64）；真正的判据是
    pattern——**整条片段正则**必须能在候选文件里匹配到，且匹配位置落在 PEM 私钥块内，
    并（描述里点名了夹具文件时）落在那个文件里。
    """

    tag: str
    kind: str
    literal: str
    pattern: "re.Pattern[str] | None" = None
    site_hint: str = ""
    scope: str = ""   # 片段条目声明的放行范围："full" / "prefix"（见 `fragment_scope`）


SITE_HINT_RE = re.compile(r"[A-Za-z0-9_.-]+(?:/[A-Za-z0-9_.-]+)*\.(?:json|pem|txt|mts|js|rs)")
PEM_BEGIN_RE = re.compile(r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----")
PEM_END_RE = re.compile(r"-----END [A-Z0-9 ]*PRIVATE KEY-----")


def _site_hint(description: str) -> str:
    """description 里点名的夹具文件（`夹具 untrusted-tls.json：…`）→ 站点落点必须匹配它。

    没有这条，片段条目只要「仓内某处也有一段 64 字符以上的私钥 base64」就绿——把这把夹具私钥
    的前缀换成另一把夹具的前缀，静态形态检查与站点检查会一起放行，而真实夹具私钥在推送链上
    被报出（放行静默失效）。描述里没点名文件时返回空串，只做 PEM 上下文判据。
    """
    m = SITE_HINT_RE.search(description)
    return m.group(0) if m else ""


def site_probes(cfg: dict) -> list[SiteProbe]:
    """每条放行的站点探针。

    值级取整值且要求**整 token**：只做子串匹配时，把 64-hex 截成前 16 字符仍然能在 fixture 里
    找到（它是完整哈希的前缀），形态检查、行为负控、站点检查会一起变绿——真实站点的 64 字符
    Secret 不再被放行（推送期假 FAIL），任何恰为该前缀的 Secret 反而被放行（放行过宽）。
    片段不按「最长的一段连续 base64」判：那样只要仓内任何地方有长 base64 就绿；改成整条正则
    逐字匹配 + PEM 私钥块上下文 + 描述点名的夹具文件。
    """
    out: list[SiteProbe] = []
    allowlists = cfg.get("allowlists")
    if not isinstance(allowlists, list):
        return out
    for idx, entry in enumerate(allowlists, start=1):
        if not isinstance(entry, dict):
            continue
        regexes = entry.get("regexes")
        if not isinstance(regexes, list):
            continue
        desc = entry.get("description") if isinstance(entry.get("description"), str) else ""
        for r in regexes:
            if not isinstance(r, str):
                continue
            if is_value_level(r):
                out.append(SiteProbe(f"条目 #{idx} 的值级字面量", "value", decode_exact(r) or ""))
            elif is_der_fragment(r):
                plain = decode_exact(r) or ""
                segs = [s for s in re.split(r"\\[nr]", plain) if len(s) >= MIN_FRAGMENT_LEN // 2]
                seg = max(segs, key=len) if segs else ""
                try:
                    pattern = re.compile(r)
                except re.error as exc:
                    die(f"条目 #{idx} 的片段正则编译不了（检查器前置错误）：{exc}")
                out.append(SiteProbe(f"条目 #{idx} 的片段", "fragment", seg, pattern, _site_hint(desc),
                                     fragment_scope(desc)))
    return out


def _site_candidates(literal: str, root: Path) -> list[Path]:
    """可能含该字面量的文件。git 检出走 `git grep -l`（只搜跟踪文件），否则退回全树遍历。

    两条路径都排掉配置文件自身——否则「配置里写着这个值」就自我满足了。
    结果按 (字面量, 仓根) 缓存：一次 `--self-check` 里 check_sites 要跑好几遍，命中集合只取决于
    这两者，不缓存就是几十次全仓扫描（r13 concurrency P3）。
    """
    key = (literal, str(root))
    cached = _SITE_CANDIDATE_CACHE.get(key)
    if cached is not None:
        return cached
    found = _site_candidates_uncached(literal, root)
    _SITE_CANDIDATE_CACHE[key] = found
    return found


def _site_candidates_uncached(literal: str, root: Path) -> list[Path]:
    if (root / ".git").exists():
        excludes = [f":(exclude){p}" for p in SITE_SELF_EXCLUDE]
        try:
            proc = subprocess.run(
                ["git", "-C", str(root), "grep", "-l", "-F", "-e", literal, "--", ".", *excludes],
                capture_output=True, timeout=120,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            die(f"站点一致性检查跑不动（git grep：{exc}）——按前置错误退 2，不与「判红」同码")
        if proc.returncode not in (0, 1):  # 1 = 没有命中，0 = 有命中，其余都是真错误
            detail = proc.stderr.decode("utf-8", "replace").strip()[:200]
            die(f"git grep 失败（rc={proc.returncode}）：{detail}")
        return [root / line for line in proc.stdout.decode("utf-8", "replace").splitlines() if line]
    out: list[Path] = []
    for path in root.rglob("*"):
        if any(part in SITE_SKIP_DIRS for part in path.parts):
            continue
        if path.is_file() and path.name not in SITE_SELF_EXCLUDE:
            out.append(path)
    return out


def _read_site(path: Path) -> str | None:
    try:
        if path.stat().st_size > SITE_MAX_BYTES:
            return None
        return path.read_text(encoding="utf-8", errors="ignore")
    except OSError:
        return None


def _found_in_tree(literal: str, root: Path, whole_token: bool) -> bool:
    if not literal:
        return False
    token = _token_pattern(literal) if whole_token else None
    for path in _site_candidates(literal, root):
        text = _read_site(path)
        if text is None or literal not in text:
            continue
        if token is None or token.search(text):
            return True
    return False


def _in_pem_block(text: str, start: int, end: int) -> bool:
    """匹配位置必须落在某个 PEM 私钥块内部（不是仓内任意一段长 base64）。"""
    last_begin = None
    for m in PEM_BEGIN_RE.finditer(text, 0, start):
        last_begin = m.end()
    if last_begin is None:
        return False
    for m in PEM_END_RE.finditer(text, 0, start):
        if m.start() > last_begin:
            return False  # 上一个块已经结束：这段 base64 不在任何私钥块里
    return PEM_END_RE.search(text, end) is not None


def _path_matches_hint(path: Path, root: Path, hint: str, exact: bool = False) -> bool:
    """站点文件是否就是描述里点名的那一个。

    片段条目按**仓内相对路径逐字相等**（`exact=True`）：只比后缀时，描述写 `tests/fixtures/tls.json`
    而实际落在 `src/other/tests/fixtures/tls.json` 也会被认作同一把夹具（codex 评审实测
    `_path_matches_hint(Path("/repo/other/tests/fixtures/tls.json"), …, "tests/fixtures/tls.json")`
    → True）——同名同后缀的两把私钥可以互相冒充。
    值级条目仍允许只写文件名（描述里通常只点 basename）。
    """
    if not hint:
        return True
    try:
        rel = path.relative_to(root).as_posix()
    except ValueError:
        rel = path.as_posix()
    if exact:
        return rel == hint
    return rel == hint or rel.endswith("/" + hint)


def _pem_normalize(text: str) -> str:
    """PEM 正文归一：去掉排版空白与 JSON 里的 `\\n` / `\\r` 两字符转义（它们不是密钥字节）。"""
    return re.sub(r"\\[nr]", "", "".join(ch for ch in text if not ch.isspace()))


def _pem_body_span(text: str, start: int, end: int) -> "tuple[int, int] | None":
    """包含匹配位置的那个 PEM 私钥块的正文区间（BEGIN 行末 → END 行首）；不在块内返回 None。"""
    if not _in_pem_block(text, start, end):
        return None
    begin = None
    for m in PEM_BEGIN_RE.finditer(text, 0, start):
        begin = m
    if begin is None:
        return None
    endm = PEM_END_RE.search(text, end)
    if endm is None:
        return None
    return begin.end(), endm.start()


def _fragment_scope_ok(text: str, start: int, end: int, scope: str) -> bool:
    """片段是否兑现了它声明的放行范围。

    full：归一后必须与 PEM 块正文**逐字相等**——否则从 164 截到 64 也能过检查，放行范围从
    「这一把私钥」扩成「同头的所有私钥」（codex 评审实测的 P1）。
    prefix：归一后必须落在正文**开头**（有意只放行 DER 前缀的条目）。
    没声明范围（scope 为空）时静态检查已经判红，这里按不兑现处理（fail-closed）。
    """
    span = _pem_body_span(text, start, end)
    if span is None:
        return False
    body_start, body_end = span
    matched = _pem_normalize(text[start:end])
    body = _pem_normalize(text[body_start:body_end])
    if scope == "full":
        return matched == body
    if scope == "prefix":
        return _pem_normalize(text[body_start:start]) == "" and body.startswith(matched)
    return False


def _fragment_found(probe: SiteProbe, root: Path) -> bool:
    """片段条目：整条正则逐字匹配 + PEM 私钥块上下文 + 描述点名的夹具文件 + 兑现声明的范围。"""
    if probe.pattern is None or not probe.literal:
        return False
    for path in _site_candidates(probe.literal, root):
        if not _path_matches_hint(path, root, probe.site_hint, exact=True):
            continue
        text = _read_site(path)
        if text is None:
            continue
        for m in probe.pattern.finditer(text):
            if _fragment_scope_ok(text, m.start(), m.end(), probe.scope):
                return True
    return False


def check_sites(cfg: dict, root: Path = ROOT) -> list[str]:
    """每条放行都必须在仓内还能找到它放行的那个值（配置自身与检查器不计）。

    形态检查只能证明「条目写法自洽」，证明不了「这条放行还对应一个真实站点」：夹具用 openssl
    重新生成、基线源文件换版后，条目会与站点脱钩——放行静默失效、命中被报、推送被拒，而形态
    检查照绿。真实数据复算（全仓「放行全关」扫描）证明的是同一件事的另一半：站点上的命中确实
    被这些字面量覆盖。

    值级字面量按**整 token** 判（前后不接 hex/base64 字符）：只做子串匹配时，把 64-hex 截成前
    16 字符——形态检查、行为负控、本检查会一起变绿，而真实站点的 Secret 不再被放行。
    片段按**整条正则**判、要求落在 PEM 私钥块里、且（描述点名了夹具文件时）落在那个文件里：
    只验「最长的一段连续 base64 在仓内出现」时，把条目换成仓内另一把夹具私钥的前缀照样全绿。
    """
    bad: list[str] = []
    for probe in site_probes(cfg):
        if probe.kind == "value":
            ok = _found_in_tree(probe.literal, root, True)
        else:
            ok = _fragment_found(probe, root)
        if ok:
            continue
        if probe.kind == "value":
            detail = f"{probe.literal[:16]}…（要求整 token，前后不能接 hex/base64 字符）"
        else:
            hint = f"，且须在描述点名的 {probe.site_hint} 里" if probe.site_hint else ""
            scope = (
                "、与 PEM 块正文逐字相等（声明的范围是「完整 PEM 正文」）" if probe.scope == "full"
                else "、落在正文开头（声明的范围是「前缀」）" if probe.scope == "prefix"
                else "、且必须声明放行范围（完整 PEM 正文 / 前缀）"
            )
            detail = f"{probe.literal[:16]}…（要求整条片段逐字匹配、落在 PEM 私钥块内{hint}{scope}）"
        bad.append(
            f"{probe.tag}在仓内找不到真实站点（配置自身不计）：{detail}"
            f"——夹具重新生成、字面量打错或被换成别的夹具的值后，这条放行会静默失效"
        )
    return bad


def sample_lines(cfg: dict) -> list[tuple[str, str, str]]:
    """(slug, 放行值, 触发模板)。模板照抄仓里真实命中形态。"""
    out = []
    for entry in value_entries(cfg):
        for r in entry.get("regexes") or []:
            v = decode_exact(r)
            if v is None:
                continue
            if HASH_RE.match(v):
                out.append((v[:8], v, '{{"outputTokenRetry.ts": "{}"}}'))
            else:
                # 真实形态：native-ws-upgrade-budget/oracle.json 的 request 行（JSON 转义的 \r\n）。
                # 裸头行与 JSON 转义行被 generic-api-key 切出的 Secret 边界不同，必须按真实形态验。
                out.append((v[:8], v,
                            '{{"request": "GET /ws HTTP/1.1\\r\\nHost: localhost\\r\\n'
                            'Sec-WebSocket-Key: {}\\r\\nx-p: aaaa"}}'))
    return out


def run_gitleaks(cases: Path, config: Path, report: Path) -> list[dict]:
    """跑一次 `gitleaks dir` 并返回报告条目。

    判「跑成了」只认报告文件 + JSON 可解析（命中/未命中两种输出文案随版本变，不能耦合）；
    报告缺失且进程 rc=0 = 零命中（gitleaks 在零命中时未必落盘）。
    子进程有超时、独立进程组、stdin 接 /dev/null：gitleaks 停滞时不拖死调用方，也不读终端。
    """
    try:
        cp = subprocess.run(
            ["gitleaks", "dir", str(cases), "--config", str(config), "--no-banner",
             "--report-format", "json", "--report-path", str(report), "--exit-code", "0"],
            capture_output=True, text=True, timeout=180, stdin=subprocess.DEVNULL,
            start_new_session=True,
        )
    except subprocess.TimeoutExpired:
        die("gitleaks 180 s 未返回（配置或 IO 停滞），已放弃本次行为负控")
    tail = (cp.stdout + cp.stderr).strip()
    if not report.exists():
        if cp.returncode == 0:
            return []
        die(f"gitleaks 没写报告 {report}（rc={cp.returncode}）：{tail[-300:]}")
    try:
        data = json.loads(report.read_text() or "[]")
    except json.JSONDecodeError as exc:
        die(f"报告不是 JSON：{exc}")
    if not isinstance(data, list):
        die(f"报告不是条目数组：{type(data).__name__}")
    return data


def hit_map(entries: list[dict]) -> dict[str, list[tuple[str, str]]]:
    """报告条目按文件名聚合出 (RuleID, Secret)。

    只看文件名会把「规则捕获组切在边界外、实际抽出的 Secret 恰等于放行值」这种假绿放过去，
    所以命中判定同时用 RuleID 与 Secret（字段名大小写随 gitleaks 版本变，都兜底）。
    """
    out: dict[str, list[tuple[str, str]]] = {}
    for x in entries:
        if not isinstance(x, dict):
            continue
        name = x.get("File") or x.get("file") or ""
        if not name:
            continue
        rule = str(x.get("RuleID") or x.get("RuleId") or x.get("ruleID") or x.get("ruleId") or "")
        secret = str(x.get("Secret") or x.get("secret") or "")
        out.setdefault(Path(name).name, []).append((rule, secret))
    return out


def check_behavior(cfg: dict) -> tuple[list[str], bool]:
    """返回 (违规说明, 是否真跑了)。没有 gitleaks 时返回 ([], False)，由 main 打印 SKIP。"""
    if not shutil.which("gitleaks"):
        return [], False
    bad: list[str] = []
    samples = sample_lines(cfg)
    if len(samples) < MIN_LITERALS:
        return [f"可取样字面量只剩 {len(samples)} 个（应 ≥ {MIN_LITERALS}）"], True
    with tempfile.TemporaryDirectory(prefix="pdrs-gitleaks-negctl-") as td:
        root = Path(td)
        cases = root / "cases"          # 只放样本；报告写在 root，避免被下一次扫描当成输入
        cases.mkdir()
        expect: dict[str, tuple[str, str]] = {}   # 文件名 -> (种类, 期望 gitleaks 抽出的 Secret)
        for idx, (slug, value, tmpl) in enumerate(samples, start=1):
            variants = [("exact", value), ("suffix", value + "X"), ("prefix", "X" + value)]
            if HASH_RE.match(value):
                # 捕获宽度负控（#1595 的立身前提）：generic-api-key 的 Secret 捕获组若在 64 字符处截断，
                # 那么「哈希 + 尾巴」会被切成恰好等于哈希的 Secret，`^哈希$` 就会把真密钥尾部放行——
                # 即本 PR 的收紧在边界上失效。这里实际喂一个 70 字符的更长值，要求 gitleaks 抽出的
                # Secret 就是这 70 字符（步骤 1 核对）、且真配置下必须命中（步骤 2）。
                variants.append(("wider", value + "abcdef"))
            for kind, val in variants:
                name = f"{idx:02d}_{slug}_{kind}.txt"
                (cases / name).write_text(tmpl.format(val) + "\n", encoding="utf-8")
                expect[name] = (kind, val)
        # 1) 样本本身必须真的触发规则（否则「放行」是假绿）：拿掉全部放行再扫一遍。
        bare = root / "no-allowlist.toml"
        bare.write_text("[extend]\nuseDefault = true\n", encoding="utf-8")
        bare_hits = hit_map(run_gitleaks(cases, bare, root / "bare.json"))
        hits = hit_map(run_gitleaks(cases, CONFIG, root / "repo.json"))
        bad.extend(assert_behavior(expect, bare_hits, hits))
    return bad, True


def assert_behavior(
    expect: dict[str, tuple[str, str]],
    bare_hits: dict[str, list[tuple[str, str]]],
    hits: dict[str, list[tuple[str, str]]],
) -> list[str]:
    """按两份报告判定行为负控（纯函数，可拿构造出来的报告直接验，不依赖 gitleaks）。

    断言强度是这里的关键：命中判定必须钉在**规则**上，不能只钉在文件上。
    反例（P1，假 PASS）：同一文件上，某条规则的 Secret 捕获组在 64 字符处截断 → 抽出的恰好是
    哈希本身 → 被 `^哈希$` 放行、不再上报；同文件上另一条规则抽出完整值仍被上报。此时
    「文件名在命中里」「期望 Secret 在命中 Secret 里」「两份报告的规则集有交集」三条全成立，
    旧断言判绿——放行口径被截断这件事完全没被验到（正是 #1595 要消灭的形态）。
    所以：**无放行时报出这条样本的每条规则，在有放行时都必须仍然报出它**；
    并且**抽出期望完整值的那条规则**必须仍然是抽出该值的那条。
    """
    bad: list[str] = []
    # 步骤 1：样本本身必须真的触发规则，且抽出的 Secret 就是文件里那段文本。
    usable: set[str] = set()
    for name in sorted(expect):
        _kind, want = expect[name]
        if name not in bare_hits:
            bad.append(f"样本不触发规则（控制失效，检查会假绿）：{name}")
            continue
        secrets = {s for _, s in bare_hits[name]}
        if want not in secrets:
            bad.append(
                f"样本抽出的 Secret 不是预期文本（规则捕获组切在边界外，行为负控在验错对象）："
                f"{name} 期望 {want!r}，实得 {sorted(secrets)!r}"
            )
            continue
        usable.add(name)
    # 步骤 2：真配置下逐条判定。
    #   exact：恰等于放行值 → 一条都不该再报。
    #   suffix / prefix / wider：含该值但不等 → 必须命中，且**规则维度**与无放行时一致。
    for name in sorted(usable):
        kind, want = expect[name]
        if kind == "exact":
            if name in hits:
                bad.append(f"恰等于放行值的 Secret 仍被报（放行失效）：{name}")
            continue
        if name not in hits:
            bad.append(f"「含该值但不等」的 Secret 被顺带放行（放行过宽）：{name}")
            continue
        hit_secrets = {s for _, s in hits[name]}
        if want not in hit_secrets:
            bad.append(
                f"命中的 Secret 不是预期的更长值（放行口径与样本对不上）："
                f"{name} 期望 {want!r}，实得 {sorted(hit_secrets)!r}"
            )
            continue
        hit_rules = {r for r, _ in hits[name] if r}
        if not hit_rules:
            bad.append(f"命中条目缺 RuleID（无法核对「同一条规则」）：{name}")
            continue
        # 规则集只能增不能减：任何在无放行时报出这条样本的规则，在有放行时都必须仍然报。
        # 少了就说明放行把某条规则的命中整条吞掉了（截断后的 Secret 恰等于放行值即此形态）。
        # 按 (规则, Secret) **多重集**比，不只比规则集合（codex r16 P2）：同一文件、同一 RuleID 有两条
        # 命中时，规则集合不变也可能已经丢了一条（截断的那条被放行吞掉，完整的那条还在）。
        bare_pairs = _Counter((r, s) for r, s in bare_hits[name] if r)
        hit_pairs = _Counter((r, s) for r, s in hits[name] if r)
        missing = bare_pairs - hit_pairs
        if missing:
            lost = sorted(missing)
            bad.append(
                f"有命中在真配置下消失（放行吞掉了命中，多半是 Secret 被截成恰好等于放行值）："
                f"{name} 缺 {len(lost)} 条，例如 规则 {lost[0][0]!r} / Secret {lost[0][1][:24]!r}…"
            )
            continue
        # 抽出期望完整值的那条规则必须仍是同一批规则：规则还在、但捕获边界变了同样要判红。
        want_rules = {r for r, s in bare_hits[name] if s == want}
        if want_rules and not (want_rules & {r for r, s in hits[name] if s == want}):
            bad.append(
                f"抽出完整值的规则换了（捕获边界变化后由别条规则兜住，放行口径没被验到）："
                f"{name} 无放行时 {sorted(want_rules)}，真配置抽出该值的是 "
                f"{sorted({r for r, s in hits[name] if s == want}) or '无'}"
            )
    return bad


def self_check(cfg: dict) -> list[str]:
    """对比检查器自身的变异负控：值级族与片段族都要能判红。"""
    import copy

    bad: list[str] = []
    entries = value_entries(cfg)
    if not entries:
        return ["self-check: 配置里没有值级条目，变异负控无从谈起（本来就该判红）"]
    allowlists = cfg.get("allowlists") or []
    first = next(i for i, e in enumerate(allowlists) if any(e is x for x in entries))
    frag = next(
        (i for i, e in enumerate(allowlists)
         if e.get("regexTarget") == "match" and e.get("regexes")),
        None,
    )
    m1 = copy.deepcopy(cfg)
    m1["allowlists"][first]["regexTarget"] = "match"
    if not check_config(m1):
        bad.append("self-check: 把值级条目 regexTarget 改回 match 后检查仍绿")
    m2 = copy.deepcopy(cfg)
    m2["allowlists"][first]["regexes"] = [strip_anchors(m2["allowlists"][first]["regexes"][0])]
    if not check_config(m2):
        bad.append("self-check: 去掉 ^…$ 锚点后检查仍绿")
    m3 = copy.deepcopy(cfg)
    m3["allowlists"] = m3["allowlists"][:1]
    if not check_config(m3):
        bad.append("self-check: 删掉大部分放行条目后检查仍绿")
    m4 = copy.deepcopy(cfg)
    m4["allowlists"].append({"description": "self-check 空正则", "regexTarget": "match", "regexes": [""]})
    if not check_config(m4):
        bad.append("self-check: 追加 regexes=[''] 的 match-all 条目后检查仍绿")
    m5 = copy.deepcopy(cfg)
    v = entries[0]["regexes"][0]
    body = strip_anchors(v)
    m5["allowlists"][first]["regexes"] = ["^" + body[:4] + "+" + body[4:] + "$"]
    if not check_config(m5):
        bad.append("self-check: 值里塞进未转义量词（一族值）后检查仍绿")
    m8 = copy.deepcopy(cfg)
    m8["allowlists"][first]["paths"] = ["src/pilotdeck-cli/tests/fixtures/**"]
    if not check_config(m8):
        bad.append("self-check: 值级条目上挂 paths（整目录跳过扫描）后检查仍绿")
    m9 = copy.deepcopy(cfg)
    m9["rules"] = {"allowlists": [{"regexes": [".*"]}]}
    if not check_config(m9):
        bad.append("self-check: 顶层出现 rules（[[rules.allowlists]]）后检查仍绿")
    if frag is None:
        # 有意为之的 tripwire（与 MIN_VALUE_ENTRIES 同性质）：片段族全没了说明有人把 PEM 条目改成值级，
        # 那时必须同时重写片段族变异负控——判红逼一次人工确认，不静默跳过。
        bad.append("self-check: 配置里没有片段条目，片段族的变异负控需要重写（有意为之的 tripwire）")
        return bad
    m6 = copy.deepcopy(cfg)
    m6["allowlists"][frag]["regexes"] = ["MIGH"]
    if not check_config(m6):
        bad.append("self-check: 把 PEM 前缀缩成 4 字符（放行一族私钥）后检查仍绿")
    m7 = copy.deepcopy(cfg)
    m7["allowlists"][frag]["regexTarget"] = "secret"
    if not check_config(m7):
        bad.append("self-check: 把片段条目改成 regexTarget='secret'（未锚定=子串语义）后检查仍绿")
    # 反过来也不行：片段条目加锚点后，gitleaks 拿 `^…$` 去比整段命中（含 PEM 头）永不命中，
    # 放行静默失效而检查器照绿——8.30.1 实测：同一份 PEM，未锚定放行 → 0 命中，加 `^…$` → 仍 1 条命中。
    frag_regex = cfg["allowlists"][frag]["regexes"][0]
    for tag, mutated in (
        ("两端 `^…$`", f"^{strip_anchors(frag_regex)}$"),
        ("只加 `^`", f"^{strip_anchors(frag_regex)}"),
        ("只加 `$`", f"{strip_anchors(frag_regex)}$"),
    ):
        manchor = copy.deepcopy(cfg)
        manchor["allowlists"][frag]["regexes"] = [mutated]
        if not check_config(manchor):
            bad.append(f"self-check: 片段条目{tag}锚定（match 语义下永不命中，放行静默失效）后检查仍绿")
    # 片段族边界：按**归一后的值**截到 P-256 共享头长度（48，零密钥字节）与下界差一（63）都必须判红。
    frag_value = normalized_literal(cfg["allowlists"][frag]["regexes"][0]) or strip_anchors(
        cfg["allowlists"][frag]["regexes"][0])
    for cut, tag in ((48, "P-256 共享头 48 字符（零密钥字节）"),
                     (MIN_FRAGMENT_LEN - 1, f"下界差一（{MIN_FRAGMENT_LEN - 1}）")):
        mcut = copy.deepcopy(cfg)
        mcut["allowlists"][frag]["regexes"] = [escape_literal(frag_value[:cut])]
        if not check_config(mcut):
            bad.append(f"self-check: 把片段截到{tag}后检查仍绿")
    # 换行转义不能撑长度：48 个 base64 字符按 PEM 排版拆成 4 字一行（正则里的 `\\n` 转义），
    # 含转义量长度是 70、归一后仍是 48——长度与片段判定必须共用同一份归一，否则下界被绕开。
    mcut = copy.deepcopy(cfg)
    mcut["allowlists"][frag]["regexes"] = [
        "\\\\n".join(frag_value[i:i + 4] for i in range(0, 48, 4))
    ]
    if not check_config(mcut):
        bad.append("self-check: 把 48 字符片段用字面 `\\n` 转义拆行撑过 64 下界后检查仍绿")
    m14 = copy.deepcopy(cfg)
    m14.setdefault("extend", {})["useDefault"] = False
    if not check_config(m14):
        bad.append("self-check: 把 [extend] useDefault 改成 false（零规则）后检查仍绿")
    m18 = copy.deepcopy(cfg)
    m18.setdefault("extend", {})["disabledRules"] = ["generic-api-key", "private-key"]
    if not check_config(m18):
        bad.append("self-check: [extend] 下挂 disabledRules（整条禁规则，8.30.1 实测）后检查仍绿")
    m15 = copy.deepcopy(cfg)
    m15["allowlist"] = {"regexes": [".*"]}
    if not check_config(m15):
        bad.append("self-check: 顶层出现 [allowlist] 单数表后检查仍绿")
    m10 = copy.deepcopy(cfg)
    m10["allowlists"][frag]["regexes"] = [strip_anchors(m10["allowlists"][frag]["regexes"][0])[:-1] + "\\d"]
    if not check_config(m10):
        bad.append("self-check: 片段里塞进字符类转义（\\d，一族值）后检查仍绿")
    m11 = copy.deepcopy(cfg)
    m11["allowlists"].append({"description": f"self-check {SUBSTRING_OPT_OUT}：测试", "regexTarget": "secret",
                              "regexes": ["dGhlIHNhbXBsZSBub25jZQ"]})
    if not check_config(m11):
        bad.append("self-check: 追加「未锚定 + secret + 短字面量」条目后检查仍绿")
    # 分桶逃逸（MiniMax r5 P0）：不按值的形状分桶后，各种哈希族字面量都必须留在值级桶里；
    # 这里逐形状确认「挂 match + 写好子串例外话术」也照样判红。
    # 断言钉在**分桶那一条错误**上，并给变异条目一个合法相对路径（codex r16 P2）：原来只要
    # `check_config` 非空就算杀死，描述里没路径这一条无关错误会把「分桶其实坏了」掩盖掉。
    for tag, lit in (
        ("sha512 的 128-hex", "a" * 128),
        ("大写 hex", "A1B2C3D4E5F60718" * 4),
        ("带前缀的 sha256:…", "sha256:" + "b" * 64),
        ("无填充长 base64", "QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVphYmNkZWZnaGlqa2xtbm9wcXJzdHV2d3h5ejAxMjM0NTY3ODk="),
    ):
        mshape = copy.deepcopy(cfg)
        mshape["allowlists"].append({
            "description": f"self-check 分桶逃逸（{tag}）夹具 src/self-check/fixture.json：测试 {SUBSTRING_OPT_OUT}",
            "regexTarget": "match",
            "regexes": [lit],
        })
        errs = check_config(mshape)
        if not any("值级字面量在 regexTarget" in e for e in errs):
            bad.append(
                f"self-check: 追加「{tag}」字面量并按片段放行（match 子串语义）后，"
                f"检查器没有按「值级字面量不许挂 match」判红"
            )
    # 片段桶的边界：以 MH/MI 开头但解码后不是私钥 DER（SEQUENCE + INTEGER 版本号 + 算法/私钥元素）
    # 的长字面量，必须落回值级桶（挂 match 就判红），否则「MH/MI 开头」就是新的子串放行后门。
    # 第三条带 `\+` 转义：转义形态也要过结构校验，不能靠反斜杠绕过。
    for tag, lit in (
        ("MI 开头 + 第三字符使长度形式非法", "MID" + "A" * 69),
        ("MH 开头但解码不是私钥 DER", "MH" + "A" * 62),
        ("MI 开头 + 转义的非 DER 字面量", "MI" + "A" * 20 + "\\+" + "A" * 40),
    ):
        mshape = copy.deepcopy(cfg)
        mshape["allowlists"].append({
            "description": f"self-check {tag} 夹具 src/self-check/non-der.json：测试 {SUBSTRING_OPT_OUT}",
            "regexTarget": "match",
            "regexes": [lit],
        })
        errs = check_config(mshape)
        if not any("值级字面量在 regexTarget" in e for e in errs):
            bad.append(
                f"self-check: 追加「{tag}」的长 base64 按片段放行（match 子串语义）后，"
                f"检查器没有按「值级字面量不许挂 match」判红"
            )
    # 片段族的第二个入口条件：description 里没有「按 match 子串语义放行」+ 理由时判红。
    # 同样把断言钉在**这一条**错误上（codex r16 P2）：只写「测试夹具私钥」还会同时缺路径与范围声明，
    # 「有任意错误」证明不了例外话术检查本身有效。
    m17 = copy.deepcopy(cfg)
    m17["allowlists"][frag]["description"] = (
        f"self-check：夹具 {_site_hint(cfg['allowlists'][frag].get('description') or '')} "
        f"{FRAGMENT_SCOPE_PREFIX}，只写「测试夹具私钥」的片段条目"
    )
    if not any("片段放行没在 description 写" in e for e in check_config(m17)):
        bad.append("self-check: 片段条目 description 缺子串例外说明与理由后，检查器没有按这一条判红")
    # 按指纹忽略通道：换个 root 造一个 .gitleaksignore，必须判红。
    with tempfile.TemporaryDirectory(prefix="pdrs-gitleaks-ignore-") as td:
        (Path(td) / ".gitleaksignore").write_text("deadbeef\n", encoding="utf-8")
        if not check_config(cfg, root=Path(td)):
            bad.append("self-check: 仓根出现 .gitleaksignore（按指纹静默压掉命中）后检查仍绿")
    # 条目 ↔ 站点：把一个值级字面量换成仓内不存在的哈希，必须判红（夹具换版/打错字后放行会静默失效）。
    m_site = copy.deepcopy(cfg)
    m_site["allowlists"].append({
        "description": "self-check 站点缺失",
        "regexTarget": "secret",
        "regexes": ["^" + "0f" * 32 + "$"],
    })
    if not check_sites(m_site):
        bad.append("self-check: 值级字面量在仓内找不到真实站点后检查仍绿")
    # 截短逃逸：把一条值级字面量截成前 16 字符——它仍是完整哈希的前缀，子串匹配会照绿，
    # 而真实站点的 64 字符 Secret 不再被放行、任何恰为该前缀的 Secret 反而被放行。
    m_trunc = copy.deepcopy(cfg)
    first_value = next(
        (e for e in m_trunc["allowlists"]
         if e.get("regexTarget") == "secret" and any(is_value_level(r) for r in e.get("regexes") or [])),
        None,
    )
    if first_value is None:
        bad.append("self-check: 找不到值级条目，截短逃逸变异无从构造（本来就该判红）")
    else:
        bare = strip_anchors(next(r for r in first_value["regexes"] if is_value_level(r)))
        first_value["regexes"] = [
            ("^" + bare[:16] + "$") if is_value_level(r) else r for r in first_value["regexes"]
        ]
        if not check_sites(m_trunc):
            bad.append("self-check: 把值级字面量截成前 16 字符（仍是完整哈希的前缀）后站点检查仍绿")
    # 正控：现配置必须全部找得到站点（否则这条检查会一直红、被人顺手关掉）。
    res = check_sites(cfg)
    if res:
        bad.append(f"self-check: 现配置自身没通过站点一致性检查：{res[0]}")
    # 行为负控的断言强度：同一文件上「某条规则的 Secret 被截成恰好等于放行值 → 这条规则不再上报」，
    # 同文件另一条规则仍报出完整值。旧断言（文件名在命中里 + 期望 Secret 在命中 Secret 里 +
    # 两份报告的规则集有交集）三条全成立会判绿——这里给一份构造报告，必须判红。
    fake_expect = {"01_x.txt": ("prefix", "X" + "a" * 64)}
    fake_bare = {"01_x.txt": [("rule-trunc", "a" * 64), ("rule-full", "X" + "a" * 64)]}
    fake_hits = {"01_x.txt": [("rule-full", "X" + "a" * 64)]}
    if not assert_behavior(fake_expect, fake_bare, fake_hits):
        bad.append(
            "self-check: 行为负控在「某条规则的 Secret 被截成恰好等于放行值、另一条规则仍报完整值」"
            "时判绿（假 PASS，正是 #1595 要消灭的形态）"
        )
    # 片段条目的站点检查必须钉在「描述点名的那把夹具」上：把一条片段换成另一条真实存在的片段
    # （另一把夹具私钥的前缀），仓内仍能找到这段 base64，但放行已与真实夹具脱钩。
    frag_idxs = [
        i for i, e in enumerate(cfg.get("allowlists") or [])
        if isinstance(e, dict) and e.get("regexTarget") == "match"
        and isinstance(e.get("regexes"), list) and e["regexes"] and is_der_fragment(e["regexes"][0])
    ]
    # 描述必须点名**仓内相对路径**：只写 basename 时，同名夹具（仓里好几个 tls.json）可以互相冒充。
    path_frag = next(
        (i for i in frag_idxs if "/" in _site_hint(cfg["allowlists"][i].get("description") or "")),
        None,
    )
    if path_frag is None:
        bad.append("self-check: 没有点位相对路径的片段条目，路径退化变异无从构造（本来就该判红）")
    else:
        m_path = copy.deepcopy(cfg)
        desc = m_path["allowlists"][path_frag]["description"]
        hint = _site_hint(desc)
        m_path["allowlists"][path_frag]["description"] = desc.replace(hint, hint.rsplit("/", 1)[-1])
        if not check_config(m_path):
            bad.append("self-check: 片段条目 description 只留文件名（去掉目录）后检查仍绿")
    if len(frag_idxs) < 2:
        bad.append("self-check: 片段条目少于 2 条，「换成另一把夹具私钥前缀」的变异无从构造（本来就该判红）")
    else:
        m_swap = copy.deepcopy(cfg)
        m_swap["allowlists"][frag_idxs[0]]["regexes"] = [cfg["allowlists"][frag_idxs[1]]["regexes"][0]]
        if not check_sites(m_swap):
            bad.append("self-check: 把片段条目换成另一条真实存在的片段（另一把夹具私钥前缀）后站点检查仍绿")
    # 声明的放行范围必须兑现（codex r16 P1）：把「完整 PEM 正文」截成 64 字符前缀后，
    # check_config 与 check_sites 原来都是 0 错误——放行从「这一把私钥」扩成「同头的所有私钥」。
    # 断言钉在**这一条**的错误上（不是「有任意错误」），否则被别的条目的路径错误掩盖（codex r16 P2）。
    full_frag = next(
        (i for i in frag_idxs if fragment_scope(cfg["allowlists"][i].get("description") or "") == "full"),
        None,
    )
    if full_frag is None:
        bad.append("self-check: 没有声明「完整 PEM 正文」的片段条目，截短逃逸变异无从构造（本来就该判红）")
    else:
        m_trunc_frag = copy.deepcopy(cfg)
        frag_entry = m_trunc_frag["allowlists"][full_frag]
        frag_val = normalized_literal(frag_entry["regexes"][0]) or ""
        frag_entry["regexes"] = [escape_literal(frag_val[:MIN_FRAGMENT_LEN])]
        tag = f"条目 #{full_frag + 1} 的片段"
        if not any(tag in e for e in check_sites(m_trunc_frag)):
            bad.append(
                f"self-check: 把声明「{FRAGMENT_SCOPE_FULL}」的片段截成 {MIN_FRAGMENT_LEN} 字符前缀后，"
                f"站点检查没有报这一条（放行范围被悄悄扩大成一族私钥）"
            )
    # 放行范围声明本身：两种都不写（或两种都写）必须判红。
    if frag_idxs:
        m_scope = copy.deepcopy(cfg)
        i = frag_idxs[0]
        desc = m_scope["allowlists"][i]["description"]
        m_scope["allowlists"][i]["description"] = desc.replace(FRAGMENT_SCOPE_FULL, "整段").replace(
            FRAGMENT_SCOPE_PREFIX, "开头部分")
        if not any("放行范围" in e for e in check_config(m_scope)):
            bad.append("self-check: 片段条目 description 去掉放行范围声明（完整 PEM 正文 / 前缀）后检查仍绿")
    # 片段站点的路径判据按全路径逐字相等（codex r16 P2）：只比后缀时同名同后缀的夹具可以互相冒充。
    if _path_matches_hint(Path("/repo/other/tests/fixtures/tls.json"), Path("/repo"),
                          "tests/fixtures/tls.json", exact=True):
        bad.append("self-check: 片段站点路径判据在全路径模式下仍接受后缀匹配（同名夹具可以互相冒充）")
    return bad


def main() -> int:
    ap = argparse.ArgumentParser(description=".gitleaks.toml 值级放行口径与行为负控")
    ap.add_argument("--static", action="store_true", help="只跑静态口径检查（不需要 gitleaks）")
    ap.add_argument("--self-check", action="store_true", help="附带检查器自身的变异负控")
    a = ap.parse_args()

    cfg = load_config(CONFIG)
    bad = check_config(cfg)
    bad += check_sites(cfg)
    if a.self_check:
        bad += self_check(cfg)
    ran = False
    if not a.static:
        behavior_bad, ran = check_behavior(cfg)
        bad += behavior_bad
    for line in bad:
        print(f"FAIL {line}", file=sys.stderr)
    if bad:
        return 1
    n = len(value_entries(cfg))
    if a.static:
        print(f"check-gitleaks-allowlist: OK（值级条目 {n} 条，仅静态（--static））")
        return 0
    if ran:
        print(f"check-gitleaks-allowlist: OK（值级条目 {n} 条，静态 + 行为负控）")
        return 0
    # 「没跑」不能与「跑过且通过」同码：本机没有 gitleaks 时退 3，接闸门的人一眼能看出行为口径没验。
    print(f"check-gitleaks-allowlist: SKIP 行为负控（本机没有 gitleaks）；"
          f"静态口径 OK（值级条目 {n} 条）", file=sys.stderr)
    return 3


if __name__ == "__main__":
    sys.exit(main())
