// PilotDeck-RS：把 PilotDeck 后端从 TypeScript 重写为 Rust · 多视角自检
//
// 动态编排最典型的用法。**它真正管用的不是「自检」，是「多视角」。**
// 自检不是再读一遍——再读一遍，AI 只会再一次确认自己是对的。
// 有效的自检，是把它分裂成几个互不相同、互相对抗的视角同时审。
// **视角越分化，越能逼出单一视角视而不见的问题。**
//
// ⚠️ 这是 Claude Code 的写法（Dynamic workflows）。换一个框架，位置和语法就变——
// 要记的是这套分工：脚本管分几段、每段派几条、什么时候收；模型管每一条具体怎么干。

export const meta = {
  name: 'pdrs-review',
  description: '多视角自检：几个互相对抗的视角同时审，再按 P0–P3 收成一张修复清单',
  phases: [
    { title: '分头审', detail: '每个视角只挑自己那一类问题' },
    { title: '收清单', detail: '合并去重，按等级排序' },
  ],
}

// 换成你这个领域的视角。要点：**彼此越不像越好**。
// 三个「都懂行的人」审出来的东西高度重合，等于只审了一遍。
const LENSES = [
  { key: '外行', prompt: '你完全不懂这个领域。只挑看不懂的地方：术语没解释、跳步骤、默认我知道的前提。' },
  { key: '老手', prompt: '你是这行做了十年的人。只挑结构与体例的问题：该有的没有、顺序不对、前后不一致。' },
  { key: '挑刺', prompt: '你的任务是找出这份东西最站不住的一处。只报一条，但要能一击致命。' },
]

const TARGET = args?.target || 'src/'

const FINDINGS = {
  type: 'object',
  properties: {
    findings: {
      type: 'array',
      items: {
        type: 'object',
        properties: {
          level: { type: 'string', enum: ['P0', 'P1', 'P2', 'P3'] },
          where: { type: 'string', description: '文件与行号，不要贴全文' },
          what: { type: 'string' },
          why: { type: 'string', description: '为什么这是问题，不是口味' },
        },
        required: ['level', 'where', 'what', 'why'],
      },
    },
  },
  required: ['findings'],
}

phase('分头审')
const rounds = await parallel(
  LENSES.map((lens) => () =>
    agent(
      `${lens.prompt}\n\n审这里：${TARGET}\n\n` +
        `逐条给：等级（P0 最高 / P3 最低）· 在哪（文件与行号，**不要贴全文**）· 是什么 · 为什么是问题。\n` +
        `只报你这个视角该管的，别越界。**找不到就说找不到**，不要凑数。`,
      { label: `审:${lens.key}`, phase: '分头审', schema: FINDINGS },
    ),
  ),
)

phase('收清单')
const all = rounds.filter(Boolean).flatMap((r) => r.findings)
const order = { P0: 0, P1: 1, P2: 2, P3: 3 }
all.sort((a, b) => order[a.level] - order[b.level])

log(`${all.length} 条：P0 ${all.filter((f) => f.level === 'P0').length} · ` +
    `P1 ${all.filter((f) => f.level === 'P1').length} · ` +
    `P2 ${all.filter((f) => f.level === 'P2').length} · ` +
    `P3 ${all.filter((f) => f.level === 'P3').length}`)

// 分级修复：给「无限评审」一个出口。
// 多视角一开，它能一轮轮挑下去——什么时候算完？这条判据说了算。
return {
  findings: all,
  收工判据: 'P0 / P1 / P2 修复为零，P3 酌情处理。到这条线就停，不再开下一轮。',
}
