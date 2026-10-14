#!/usr/bin/env python3
"""Scan five CJK blocks without downloading data or adding dependencies.
Usage: python3 tests/scan_cjk_collation.py NEW_OUTPUT_DIRECTORY
Builds locked/offline, then extracts the production node_locale/node_collator
functions into a temporary Rust probe with the exact Cargo build artifacts.
All Node processes use isolated HOME/XDG/TMPDIR and SKIP_BROWSER_OPEN=1.
Counts output-index reversals, not every possible pairwise comparison.
"""
import json, os, subprocess, sys, shutil, hashlib
from pathlib import Path
r=Path(sys.argv[1]).resolve()
r.mkdir(parents=True, exist_ok=False)
for directory in ('home','xdg','tmp'):
 (r/'isolation'/directory).mkdir(parents=True)
workspace=Path(__file__).resolve().parents[3]
source=(workspace/'src/pilotdeck-gateway/src/project_files.rs').read_text()
locale=source[source.index('pub(crate) fn node_locale('):source.index('fn lowercase(')]
collator=source[source.index('pub(crate) fn node_collator('):source.index('pub(crate) fn native_entries(')]
(r/'probe.rs').write_text("use icu_collator::{Collator,CollatorBorrowed,CollatorPreferences};\nuse icu_locale_core::Locale;\ntype Result<T> = std::result::Result<T, (&'static str,String)>;\n"+locale+collator+"""fn main() {
 let locale=node_locale(|k|std::env::var(k).ok());
 let collator=node_collator(&locale).unwrap();
 let input:Vec<String>=std::io::stdin().lines().map(|s|s.unwrap()).collect();
 let mut order=input.clone();order.sort_by(|a,b|collator.compare(a,b));
 println!("{}",serde_json::json!({"locale":locale.to_string(),"order":order,"anchorSigns":input.iter().map(|s|match collator.compare(s,"中"){std::cmp::Ordering::Less=>-1,std::cmp::Ordering::Equal=>0,_=>1}).collect::<Vec<_>>()}));
}
""")
build=subprocess.run(['cargo','build','--manifest-path',str(workspace/'src/Cargo.toml'),'-p','pilotdeck-gateway','--locked','--offline','--message-format=json'],capture_output=True,text=True,check=True)
artifacts={}
for line in build.stdout.splitlines():
 event=json.loads(line)
 if event.get('reason')=='compiler-artifact':
  for filename in event['filenames']:
   if filename.endswith('.rlib'):artifacts[event['target']['name']]=filename
cmd=['rustc','--edition=2024',str(r/'probe.rs'),'-L','dependency='+str(Path(artifacts['icu_collator']).parent),'-o',str(r/'rust-probe')]
for crate in ('icu_collator','icu_locale_core','serde_json'):
 cmd+=['--extern',crate+'='+artifacts[crate]]
subprocess.run(cmd,check=True)
env={k:v for k,v in os.environ.items() if not k.startswith('LC_') and k!='LANG'}
env.update(HOME=str(r/'isolation/home'),XDG_CONFIG_HOME=str(r/'isolation/xdg'),XDG_CACHE_HOME=str(r/'isolation/xdg'),XDG_DATA_HOME=str(r/'isolation/xdg'),TMPDIR=str(r/'isolation/tmp'),SKIP_BROWSER_OPEN='1',PILOTDECK_SKIP_BROWSER_OPEN='1')
ranges={'Ext-A':(0x3400,0x4dbf),'Basic':(0x4e00,0x9fff),'Ext-B':(0x20000,0x2a6df),'Compatibility':(0xf900,0xfaff),'Compatibility-Supplement':(0x2f800,0x2fa1f)}
inputs=[chr(cp) for a,b in ranges.values() for cp in range(a,b+1)]
(r/'scan-input.json').write_text(json.dumps(inputs,ensure_ascii=False))
(r/'scan.mjs').write_text("import fs from 'node:fs';const x=JSON.parse(fs.readFileSync(process.argv[2],'utf8'));console.log(JSON.stringify({versions:process.versions,resolved:new Intl.Collator().resolvedOptions(),order:x.slice().sort((a,b)=>a.localeCompare(b)),anchorSigns:x.map(s=>Math.sign(s.localeCompare('中')))}));")
def compress(cps):
 out=[]
 for cp in cps:
  if out and cp==out[-1][1]+1:out[-1][1]=cp
  else:out.append([cp,cp])
 return [[f'U+{a:04X}',f'U+{b:04X}'] for a,b in out]
rows=[]
for lang in ['C','en_US.UTF-8','zh_CN.UTF-8','zh_TW','ja_JP']:
 e={**env,'LANG':lang}
 node=json.loads(subprocess.check_output([shutil.which('node'),str(r/'scan.mjs'),str(r/'scan-input.json')],env=e,text=True))
 rust=json.loads(subprocess.check_output([str(r/'rust-probe')],env=e,text=True,input='\n'.join(inputs)+'\n'))
 node_rank={s:i for i,s in enumerate(node['order'])}
 inversions=[(ord(a),ord(b)) for a,b in zip(rust['order'],rust['order'][1:]) if node_rank[a]>node_rank[b]]
 summary={'env':{'LANG':lang},'locale':node['resolved']['locale'],'corpusSize':len(inputs),'rustAdjacentPairsReversedInNode':len(inversions),'firstInversions':[[f'U+{a:04X}',f'U+{b:04X}'] for a,b in inversions[:20]],'blocks':{}}
 for name,(a,b) in ranges.items():
  block=[(i,ord(c)) for i,c in enumerate(inputs) if a<=ord(c)<=b]
  different=[cp for i,cp in block if node['anchorSigns'][i]!=rust['anchorSigns'][i]]
  summary['blocks'][name]={'range':[f'U+{a:04X}',f'U+{b:04X}'],'scanned':len(block),'compareToZhongSignDifferences':len(different),'differentRanges':compress(different)}
 rows.append(summary)
 def block(c):
  return next(k for k,(a,b) in ranges.items() if a<=ord(c)<=b)
 by_block={};examples={}
 for a,b in zip(rust['order'],rust['order'][1:]):
  if node_rank[a]>node_rank[b]:
   key=block(a)+' / '+block(b);by_block[key]=by_block.get(key,0)+1
   if len(examples.get(key,[]))<2:examples.setdefault(key,[]).append([a,b])
 summary['outputAdjacentIndexReversalsByBlock']=by_block;summary['examplesByBlock']=examples
 print(lang,summary['rustAdjacentPairsReversedInNode'],{k:v['compareToZhongSignDifferences'] for k,v in summary['blocks'].items()})
 (r/f'scan-{lang}.json').write_text(json.dumps({'node':node,'rust':rust},ensure_ascii=False))
(r/'scope.json').write_text(json.dumps({'node': node['versions']['node'], 'icu': node['versions']['icu'], 'cldr': node['versions']['cldr'], 'rustCollator': '2.1.1', 'rustCompiledData': '2.1.1; CLDR 48.0.0; ICU release-78.1rc', 'productionSourceSha256': hashlib.sha256(source.encode()).hexdigest(), 'note':'Every scalar in these blocks, including unassigned positions; differences are signs vs U+4E2D, not an exhaustive pairwise classification.', 'rows':rows},ensure_ascii=False,indent=2)+'\n')
