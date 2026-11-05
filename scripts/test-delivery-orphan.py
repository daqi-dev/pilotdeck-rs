#!/usr/bin/env python3
"""delivery-orphan.py 自测：探测失败绝不被当成「没人」——A/B/C 三类里唯一能造成假 DEAD 的路径。

11-05 闸门 P1：`ssh host "pgrep … | wc -l"` 的管线退出码是 wc 的 0，远端没装 pgrep(127) / 报权限错(2)
都会打印 0，三台都这样就打出 ORPHAN-DEAD，而 worker 正跑着。本测把 run() 换成桩，逐条钉住
「rc=0 有匹配 / rc=1 没匹配 / 其余 = 没核成」，并留一条旧实现输出形态（裸 `0`）作负控。

    python3 -I scripts/test-delivery-orphan.py       # 0 全过，1 有失败
"""
import importlib.util
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))


def load(path=None):
    p = path or os.path.join(HERE, "delivery-orphan.py")
    spec = importlib.util.spec_from_file_location("delivery_orphan", p)
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


CASES = []


def case(name):
    def deco(fn):
        CASES.append((name, fn))
        return fn
    return deco


def stub_run(mod, table):
    """table: (cmd 判定函数) → (rc, out, err)；按顺序匹配，命中即返回。"""
    calls = []

    def fake(cmd, cwd=None, timeout=None):
        calls.append(cmd)
        for pred, ret in table:
            if pred(cmd):
                return ret
        raise AssertionError("没预期的命令：%r" % (cmd,))

    mod.run = fake
    return calls


def is_ssh(cmd):
    return cmd and cmd[0] == "ssh"


def is_pgrep(cmd):
    return cmd and cmd[0] == "pgrep"


@case("pgrep rc=1（没匹配）算核过，返回空")
def t_pgrep_rc1(_mod):
    mod = load()
    stub_run(mod, [(is_pgrep, (1, "", ""))])
    lines, ok = mod.pgrep_lines("wt-x")
    assert (lines, ok) == ([], True), (lines, ok)


@case("pgrep rc=124（超时）/ rc=2 / rc=127 都算没核成")
def t_pgrep_fail(_mod):
    for rc in (124, 2, 127):
        mod = load()
        stub_run(mod, [(is_pgrep, (rc, "", "boom"))])
        lines, ok = mod.pgrep_lines("wt-x")
        assert lines == [] and ok is False, (rc, lines, ok)


@case("pgrep rc=0 返回进程行")
def t_pgrep_hit(_mod):
    mod = load()
    stub_run(mod, [(is_pgrep, (0, "11 cargo build\n22 python3 x\n", ""))])
    lines, ok = mod.pgrep_lines("wt-x")
    assert ok is True and len(lines) == 2, (lines, ok)


@case("远端 rc=0 → 命中")
def t_remote_hit(_mod):
    mod = load()
    stub_run(mod, [(is_ssh, (0, "rc=0\n", ""))])
    hits, unchecked = mod.remote_activity("1473", hosts=["cb8"])
    assert hits == ["remote:cb8"] and unchecked == [], (hits, unchecked)


@case("远端 rc=1 → 核过、无人；不算未核")
def t_remote_nomatch(_mod):
    mod = load()
    stub_run(mod, [(is_ssh, (0, "rc=1\n", ""))])
    hits, unchecked = mod.remote_activity("1473", hosts=["cb8"])
    assert hits == [] and unchecked == [], (hits, unchecked)


@case("远端 pgrep 缺失(127)/报错(2) → 记未核，绝不判 DEAD（P1 负控）")
def t_remote_pgrep_missing(_mod):
    for prc in (127, 2, 3):
        mod = load()
        stub_run(mod, [(is_ssh, (0, "rc=%d\n" % prc, ""))])
        hits, unchecked = mod.remote_activity("1473", hosts=["cb8"])
        assert hits == [] and unchecked == ["cb8"], (prc, hits, unchecked)


@case("旧实现形态（裸计数 `0` / 空输出）→ 记未核（负控：修复前会被当成核过）")
def t_remote_old_shape(_mod):
    for out in ("0\n", "", "1\n", "  \n"):
        mod = load()
        stub_run(mod, [(is_ssh, (0, out, ""))])
        hits, unchecked = mod.remote_activity("1473", hosts=["cb8"])
        assert hits == [] and unchecked == ["cb8"], (out, hits, unchecked)


@case("ssh 本身失败(255)/超时(124) → 记未核")
def t_remote_ssh_fail(_mod):
    for rc in (255, 124):
        mod = load()
        stub_run(mod, [(is_ssh, (rc, "", "connect failed"))])
        hits, unchecked = mod.remote_activity("1473", hosts=["cb8"])
        assert hits == [] and unchecked == ["cb8"], (rc, hits, unchecked)


@case("定不出 issue 号 → 全部主机记未核")
def t_remote_no_tag(_mod):
    mod = load()
    stub_run(mod, [])
    hits, unchecked = mod.remote_activity("tools-x", hosts=["cb8", "mbp2"])
    assert hits == [] and unchecked == ["cb8", "mbp2"], (hits, unchecked)


@case("本机探针失败 → local_activity 的 ok=False（上层据此不判 DEAD）")
def t_local_unchecked(_mod):
    mod = load()
    stub_run(mod, [(is_pgrep, (124, "", "timeout"))])
    hits, ok = mod.local_activity("wt-99999", "99999", "/nonexistent")
    assert hits == [] and ok is False, (hits, ok)


@case("本机 rc=1 + 远端全核过 → ok=True（可判 DEAD）")
def t_local_ok(_mod):
    mod = load()
    stub_run(mod, [(is_pgrep, (1, "", ""))])
    hits, ok = mod.local_activity("wt-99999", "99999", "/nonexistent")
    assert hits == [] and ok is True, (hits, ok)


@case("远端探针命令里 pgrep 的 rc 必须自己出头（不许再走 `| wc -l`）")
def t_remote_cmd_shape(_mod):
    mod = load()
    calls = stub_run(mod, [(is_ssh, (0, "rc=1\n", ""))])
    mod.remote_activity("1473", hosts=["cb8"])
    cmd = " ".join(calls[0])
    assert "| wc -l" not in cmd, cmd
    assert "echo rc=$?" in cmd, cmd


@case("认领表按 TTL 重读：扫描期间别人 --claim 写的行能被看到（P2a）")
def t_ledger_refresh(_mod):
    mod = load()
    live = [{"k": (1.0, "claude-A", "claimed")}]
    mod.ledger_index = lambda root, rows: dict(live[0])
    mod.load_ledger = lambda path: []

    led = mod.Ledger("/r", "/r/claimed.tsv", ttl=0)   # ttl=0 → 每次都重读
    assert led.get("k") == (1.0, "claude-A", "claimed"), led.get("k")
    live[0] = {}                                     # 别人随后 --claim 的另一条 / 状态变了
    assert led.get("k") is None, "ttl=0 时仍读到旧快照"
    assert len(led) == 0, len(led)

    frozen = mod.Ledger("/r", "/r/claimed.tsv", ttl=9999)  # 大 TTL 不重读（避免每次取用都碰盘）
    live[0] = {"k": (1.0, "claude-A", "claimed")}
    assert frozen.get("k") == (1.0, "claude-A", "claimed")
    live[0] = {}
    assert frozen.get("k") == (1.0, "claude-A", "claimed"), "大 TTL 不该重读"


@case("PDRS_ORPHAN_HOSTS 空串 / 全逗号 / 全空白 → 回退默认表（否则零循环 = 假 DEAD）")
def t_parse_hosts(_mod):
    mod = load()
    default = mod.parse_hosts(None)
    assert default == ("cb8", "mbp2", "macmini-01"), default
    for raw in ("", "   ", ",", " , , "):
        assert mod.parse_hosts(raw) == default, (raw, mod.parse_hosts(raw))
    assert mod.parse_hosts("cb8") == ("cb8",)
    assert mod.parse_hosts(" cb8 , mbp2 ") == ("cb8", "mbp2")


@case("空主机表时 remote_activity 记未核，不判 DEAD")
def t_remote_no_hosts(_mod):
    mod = load()
    stub_run(mod, [])
    hits, unchecked = mod.remote_activity("1473", hosts=[])
    assert hits == [] and unchecked == ["未配置远端主机"], (hits, unchecked)


@case("pgrep -fl 的「PID 」前缀要剥掉，扫描类命令才不再算「有人在跑」（P1 负控）")
def t_signature_pid_prefix(_mod):
    mod = load()
    assert mod.signature_of("12345 find /Users/da/ws/pilotdeck-rs/_build/wt-1334 -name cargo") is None
    assert mod.signature_of("12345 grep -r remote-wait _build/wt-1334/") is None
    assert mod.signature_of("12345 /usr/bin/du -sh _build/wt-1334") is None
    # 真 runner 不能因为剥前缀被误杀
    assert mod.signature_of("98613 cargo test -p pilotdeck-gateway") == "cargo"
    assert mod.signature_of("66136 bash /Users/da/ws/pdrs-ops/scripts/remote-worker/remote-wait3.sh wt-1334") == "worker"
    assert mod.signature_of("85835 python3 run-review-roles.py --worktree wt-x") == "review"
    # 同族评审脚本逐个点名：漏登记会让在跑的评审被判「无人在跑」→ 假 ORPHAN-DEAD
    for prog in ("review-phase.py", "glm_review.py", "review-deepseek.py", "review-minimax.py", "grokbot-call.py"):
        assert mod.signature_of(f"85835 python3 scripts/{prog} --worktree wt-x") == "review", prog


@case("B 类静默：有单号按单号；无单号的工具树按树名归属，且允许 parked（P2a）")
def t_claim_covers_branch(_mod):
    mod = load()
    k = "_build/wt-1473/_tmp/20261105-issue-1473"
    assert mod.claim_covers_branch("1473", "wt-1473", k, "pr-opened") is True
    assert mod.claim_covers_branch("1473", "wt-1473", k, "claimed") is False
    assert mod.claim_covers_branch("1474", "wt-1474", k, "pr-opened") is False
    tool = "_build/wt-frozen-batch/_tmp/20261105-frozen"
    assert mod.claim_covers_branch(None, "wt-frozen-batch", tool, "parked") is True
    assert mod.claim_covers_branch(None, "wt-frozen-batch", tool, "claimed") is False
    assert mod.claim_covers_branch(None, "wt-other", tool, "parked") is False


@case("整树 git 不可用 → 计 skipped 并退出 2（P1 负控：不是「无孤儿」）")
def t_collect_git_broken(_mod):
    import subprocess
    import tempfile
    root = tempfile.mkdtemp(prefix="do-root-")
    os.makedirs(os.path.join(root, "_build", "wt-broken"), exist_ok=True)
    os.makedirs(os.path.join(root, "_tmp"), exist_ok=True)
    p = subprocess.run(
        [sys.executable, "-I", os.path.join(HERE, "delivery-orphan.py"),
         "--root", root, "--no-check-remote"],
        capture_output=True, text=True, timeout=120,
    )
    assert p.returncode == 2, (p.returncode, p.stdout, p.stderr[-400:])
    assert "没核成" in p.stderr, p.stderr[-400:]


@case("刚提交过的树不判 DEAD（P1 负控：worker 收尾只剩 git push 时不带树名）")
def t_fresh_commit(_mod):
    mod = load()
    now = 1000000.0
    assert mod.fresh_commit(now, now - 60) is True          # 1 分钟前
    assert mod.fresh_commit(now, now - mod.FRESH_MIN * 60) is True
    assert mod.fresh_commit(now, now - (mod.FRESH_MIN + 1) * 60) is False
    assert mod.fresh_commit(now, None) is False             # 取不到提交时间不能当「刚提交」


def main(argv):
    fails = 0
    for name, fn in CASES:
        try:
            fn(None)
            print("ok   %s" % name)
        except AssertionError as exc:
            fails += 1
            print("FAIL %s：%s" % (name, exc))
    print("%d/%d 通过" % (len(CASES) - fails, len(CASES)))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
