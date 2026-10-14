#!/usr/bin/env python3
"""Read every pair's differences; cluster by full field path × rule, no verdict edits."""
import argparse
from collections import Counter
import json
from pathlib import Path
import re


def summarize(before, after):
    clusters = {}
    aliases = {}
    totals = []
    for version, report in (('before', before), ('after', after)):
        if len(report['pairs']) != 10:
            raise ValueError('expected all ten TS pairs')
        for pair in report['pairs']:
            name = '-'.join(map(str, pair['pair']))
            snapshot = json.loads((Path(pair['left']) / 'snapshot.json').read_text())
            totals.append({'version': version, 'pair': name, 'verdict': pair['verdict'],
                           'count': len(pair['differences']), 'rules': dict(Counter(d['rule'] for d in pair['differences']))})
            for difference in pair['differences']:
                location = difference['location']
                match = re.match(r'disk\[(\d+)\]', location)
                if match:
                    physical = snapshot['entries'][int(match[1])]['path']
                    alias = physical
                    if physical == 'home/runs/registry.json':
                        alias = '@registry'
                    for index in (1, 2):
                        stem = f'home/projects/exp008-fixture/chats/web:33333333-3333-4333-8333-00000000000{index}.jsonl'
                        if physical == stem:
                            alias = f'@chat:{index}'
                        elif physical == stem + '.projections.json':
                            alias = f'@projection:{index}'
                    aliases[alias] = physical
                    location = 'disk[' + alias + ']' + location[match.end():]
                # Only numeric indices become '*'; quoted object keys and
                # connection/case identities stay intact.
                location = re.sub(r'\[\d+\]', '[*]', location)
                key = difference['rule'], location
                if key not in clusters:
                    clusters[key] = {'rule': key[0], 'path': key[1], 'before': {}, 'after': {}, 'examples': {}}
                cluster = clusters[key]
                cluster[version][name] = cluster[version].get(name, 0) + 1
                cluster['examples'].setdefault(version, {'pair': name, **difference})
    return {'pairs': totals, 'disk_aliases': aliases, 'clusters': [clusters[k] for k in sorted(clusters)]}


def markdown(summary, label='第 2 轮'):
    lines = ['### 十对逐对结果', '', f'| TS 对 | 原 differences | {label} differences | {label}结果 |', '|---|---:|---:|---|']
    old = {r['pair']: r for r in summary['pairs'] if r['version'] == 'before'}
    for row in summary['pairs']:
        if row['version'] == 'after':
            lines.append(f"| {row['pair']} | {old[row['pair']]['count']} | {row['count']} | {row['verdict']} |")
    lines += ['', '### 字段路径 × 比较规则：完整聚类表', '',
              '逐对遍历原报告和重放报告的全部 `differences`，仅将数组数字下标换为 `[*]`；字段缺失、长度和错位产生的重复诊断也计入，不把条数当成独立缺陷数。WS 全序与 by_type 是两道独立门，因此表中保留两套位置。磁盘槽位解析为以下文件别名：', '']
    for alias, physical in sorted(summary['disk_aliases'].items()):
        lines.append(f'- `{alias}` = `{physical}`')
    lines += ['', '表中“原→新”是十对累计数；“涉及 TS 对”取前后并集。每簇逐对计数与原始完整 location/值例子另存同名 JSON，便于复核。没有过滤掉任何一对或规则。', '',
              '| 规则 | 完整字段路径（数组下标归一） | 原→新 | 涉及 TS 对 |', '|---|---|---:|---|']
    for c in summary['clusters']:
        pairs = sorted(c['before'].keys() | c['after'].keys())
        lines.append(f"| `{c['rule']}` | `{c['path']}` | {sum(c['before'].values())}→{sum(c['after'].values())} | {', '.join(pairs)} |")
    return '\n'.join(lines) + '\n'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--before', type=Path, required=True)
    parser.add_argument('--after', type=Path, required=True)
    parser.add_argument('--out', type=Path, required=True)
    parser.add_argument('--label', default='第 2 轮')
    args = parser.parse_args()
    summary = summarize(json.loads(args.before.read_text()), json.loads(args.after.read_text()))
    args.out.write_text(json.dumps(summary, ensure_ascii=False, indent=2) + '\n')
    args.out.with_suffix('.md').write_text(markdown(summary, args.label))
    print(f"{len(summary['clusters'])} complete path/rule clusters; all ten pairs included")


if __name__ == '__main__':
    main()
