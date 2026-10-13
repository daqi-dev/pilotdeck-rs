#!/usr/bin/env bash
# PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust · 工具就绪检查
#
# 只看不装：报告有什么、缺什么、缺的怎么装。**安装命令由你自己跑**——
# 让脚本替你往电脑上装东西，是把决定权交出去了。
#
#   bash scripts/check-tools.sh            检查
#   bash scripts/check-tools.sh --mirrors  顺带打印国内镜像配置
#
# ─────────────────────────────────────────────────────────────────────
# 装不上怎么办：三层降级，一层不成才进下一层
#
#   一 · 用哪个    本项目该用哪些 CLI 已经定死在下面。别自己挑，别现查。
#   二 · 换入口    同生态换另一个（npm↔bun、pip↔uv）；还不行换一条
#                  不依赖它的做法，并记一笔。
#   三 · 去哪装    先看本地已有什么，再走镜像（--mirrors），高校源优先。
#
#   ⚠️ 不许在第一层就开始换源、换写法、反复试——换个写法再试一次、
#      再换一个源、再试一次，时间和 token 就是这样烧掉的。
#      三层走完仍然不成，**停下来告诉人**，别自己绕。
#   两条底线：只从官方或可信源装；装之前核一眼包名与维护状态。
#
# ─────────────────────────────────────────────────────────────────────
# 换谱系评审：同一个会话里再问一遍没用
#
# 偏见有两个来源——上下文污染和模型权重。新开会话只洗掉前一半。
# 要换就换整套装置：**第三方的框架配第三方的模型**。装了哪些看下面。
#
#   codex exec -m <模型> -c model_reasoning_effort=medium < 提示词文件
#   cat 提示词文件 | opencode run --model <厂商/模型>
#
#   ⚠️ 提示词走文件（原则见 CLAUDE.md 铁律 12）。「读文件」要真的读——
#      用文件参数或标准输入，别用会先把内容展开再塞回命令的写法。
#   收敛线：P0 / P1 / P2 修复为零，P3 酌情。到这条线就停。
# ─────────────────────────────────────────────────────────────────────
set -uo pipefail

MIRRORS=0
[ "${1:-}" = "--mirrors" ] && MIRRORS=1

case "$(uname -s)" in
  Darwin) OS=mac ;;
  Linux)  OS=linux ;;
  *)      OS=win ;;   # Git Bash / WSL 也会落这里
esac

have() { command -v "$1" >/dev/null 2>&1; }
MISSING=""

# 名字 · 一句话 · 装法（按平台）
check() {
  local cmd="$1" what="$2" how_mac="$3" how_linux="$4" how_win="$5"
  if have "$cmd"; then
    printf '  ✓ %-10s %s\n' "$cmd" "$what"
  else
    local how
    case "$OS" in mac) how="$how_mac" ;; linux) how="$how_linux" ;; *) how="$how_win" ;; esac
    printf '  ✗ %-10s %-28s → %s\n' "$cmd" "$what" "$how"
    MISSING="$MISSING $cmd"
  fi
}

echo "▸ 分发市场（根系：任何具体需求都能在这三家里找到现成的）"
case "$OS" in
  mac)   check brew "Mac 的包管理器" '见 brew.sh' - - ;;
  linux) printf '  – %-10s %s\n' "系统自带" "apt / dnf / pacman，按发行版走" ;;
  win)   check scoop "Windows 上仿 Homebrew 的那个" - - '见 scoop.sh'
         check winget "微软官方包管理器" - - '新版 Windows 自带' ;;
esac
check node "JavaScript 运行时" 'brew install node' '包管理器装 nodejs' 'scoop install nodejs'
check npm  "JS 包索引入口" '随 node 一起' '随 node 一起' '随 node 一起'
check bun  "更快的那个 JS 入口" 'brew install oven-sh/bun/bun' 'curl -fsSL https://bun.sh/install | bash' 'scoop install bun'
check python3 "Python 运行时" 'brew install python' '多数发行版自带' 'scoop install python'
check pip3 "Python 包索引入口" '随 python 一起' '随 python 一起' '随 python 一起'
check uv   "更快的那个 Python 入口" 'brew install uv' 'curl -LsSf https://astral.sh/uv/install.sh | sh' 'scoop install uv'

echo
echo "▸ 常用命令行工具"
check git      "版本控制。没它，状态持久化无从谈起" 'brew install git' '包管理器装 git' 'scoop install git'
check rg       "全文检索，比 grep 快很多" 'brew install ripgrep' '包管理器装 ripgrep' 'scoop install ripgrep'
check jq       "命令行里处理 JSON" 'brew install jq' '包管理器装 jq' 'scoop install jq'
check gitleaks "提交前扫密钥——给新手保命的那一把" 'brew install gitleaks' '见项目 releases' 'scoop install gitleaks'
check gh       "GitHub 命令行入口" 'brew install gh' '见 cli.github.com' 'scoop install gh'

echo
echo "▸ 第二谱系（对抗性评审要用——现在装好，下一步分析时才用得上）"
echo "  换的不是模型，是**整套装置**：框架与模型要配套，别在这家的框架里塞那家的模型。"
have codex     && printf '  ✓ %-10s %s\n' codex     '配 GPT 系' || printf '  ✗ %-10s %-28s → %s\n' codex     '配 GPT 系' '见 OpenAI Codex 文档'
have opencode  && printf '  ✓ %-10s %s\n' opencode  '配 GLM 系' || printf '  ✗ %-10s %-28s → %s\n' opencode  '配 GLM 系' '见 OpenCode 文档'
have codewhale && printf '  ✓ %-10s %s\n' codewhale '配 DeepSeek 系' || printf '  – %-10s %s\n' codewhale '配 DeepSeek 系（可选）'
echo "  **一个都没有也能干活，但你就少了一双不同来路的眼睛。**"

# ══════════════════════════════════════════════════════════════════
# ▸ 本系统专属 —— 初始化第三步由 AI 按技术栈填，人过目
#   这一段空着，就说明第三步没做完。**写死的通用清单救不了你**：
#   上面那张单子里没有你这套技术栈的编译器/运行时，
#   就会从头到尾没人发现它压根没被检查过。
# ══════════════════════════════════════════════════════════════════
STACK_EMPTY=1   # AI 填入 check 行时，把这行改成 STACK_EMPTY=0
echo
echo "▸ 本系统专属"
  echo "  ⚠️ 空着——初始化第三步（配置与扩展）没做完。"
  echo "     按本系统技术栈补 check 行到 scripts/check-tools.sh，例（Rust）："
  echo "       check rustc \"Rust 编译器\"       'rustup 装，见 rustup.rs' '同左' '同左'"
  echo "       check cargo \"Rust 构建与包管理\" '随 rustup 一起'          '同左' '同左'"
  echo "     填完把上面的 STACK_EMPTY=1 改成 0。"

echo
if [ "$STACK_EMPTY" = 1 ]; then
  echo "▸ 未完成：第三步（配置与扩展）的专属清单还是空的。"
  echo "  通用底座齐不齐，回答不了「这套系统要用的每一件是不是真的装上了」——"
  echo "  所以下面不会说「都齐了」。"
elif [ -n "$MISSING" ]; then
  echo "▸ 缺这些：$MISSING"
  echo "  两条底线：**只从官方或可信源装**；装之前核一眼包名与维护状态。"
  echo "  同一生态的两套入口（npm 与 bun、pip 与 uv）**建议都装上**——"
  echo "  少数包只认老牌那个，都装着，AI 就不必现查版本管理、陷进无穷试错。"
else
  echo "▸ 都齐了。"
fi

if [ "$MIRRORS" = 1 ]; then
  cat <<'EOF'

▸ 国内镜像（装不上时的第三层退路）

  国内可用的大致两类：**高校维护的口碑最稳，作第一选择**（清华、南京大学这几家）；
  云厂商的质量参差，有的同步不够快，有的缺包。给三大市场各配两三个备选就够了。

  npm      npm config set registry https://registry.npmmirror.com
  PyPI     pip config set global.index-url https://pypi.tuna.tsinghua.edu.cn/simple
           （南大备选：https://mirror.nju.edu.cn/pypi/web/simple）
  Homebrew 见清华 / 中科大镜像站的 Homebrew 说明页，按它给的三条环境变量设

  三条纪律：**走加密连接**（只用 https）· **留一条退回官方源的路** ·
  **地址会变，用前核一眼镜像站的说明页**——别照抄过期的配置。
EOF
fi

# 专属清单没填 = 第三步没做完，非零退出。「一条命令能验的事，别靠印象」——
# 静默通过就等于把它留给了印象。（2026-08-28 codex 评审 P2-4）
[ "$STACK_EMPTY" = 1 ] && exit 1
exit 0
