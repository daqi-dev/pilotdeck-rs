#!/usr/bin/env python3
"""Real fixed-baseline setup function, scripted answers and fake network only."""
import argparse,hashlib,json,os,selectors,subprocess,time
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--baseline',type=Path,required=True);p.add_argument('--node',type=Path,required=True);p.add_argument('--output',type=Path,required=True);p.add_argument('--boundaries',action='store_true');a=p.parse_args()
base=a.baseline.resolve();node=a.node.resolve();out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
sha='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';source='src/cli/commands/gatewaySetup.ts'
if subprocess.check_output(['git','-C',str(base),'rev-parse','HEAD'],text=True).strip()!=sha:raise SystemExit('baseline HEAD mismatch')
original=subprocess.check_output(['git','-C',str(base),'show',sha+':'+source])
if (base/source).read_bytes()!=original:raise SystemExit('modified baseline source')
versions=json.loads(subprocess.check_output([str(node),'-p','JSON.stringify({node:process.version,icu:process.versions.icu})'],text=True))
if versions!={'node':'v22.22.0','icu':'78.2'}:raise SystemExit('Node/ICU mismatch')
generate={'json':{'data':{'scode':'a/b 中','auth_url':'https://fixture.invalid/scan'}}}
success={'json':{'data':{'status':'SUCCESS','bot_info':{'bot_id':' qr-bot ','secret':' qr-secret '}}}}
seed="keep: &group\n  value: old\nadapters:\n  other:\n    enabled: true\n  wecom:\n    enabled: false\n    token: old-bot-id\n    extra:\n      secret: old-secret\n      websocketUrl: wss://old.example\n      dm_policy: ' ALLOWLIST '\n      allowFrom: [a, ' b ', '', 7]\n      group_policy: invalid\n      groupAllowFrom: 'g1, g2'\n      groups: *group\n"
scan='是否扫码自动创建/获取 Bot ID 和 Secret？ [Y/n]: '
ws='WebSocket URL (默认: wss://openws.work.weixin.qq.com): '
dm='私聊访问策略 [1=open 推荐, 2=allowlist, 3=disabled] (默认 1): '
group='群聊访问策略 [1=disabled 推荐, 2=open, 3=allowlist] (默认 1): '
cases=[
 {'id':'manual','argv':['wecom'],'answers':[[scan,'n'],['Bot ID: ','fixture-bot'],['Secret: ','fixture-secret'],[ws,''],[dm,'2'],['允许私聊的企业微信用户 ID（逗号分隔，留空则暂时禁用私聊）: ',' a, b ,, '],[group,'3'],['允许响应的群聊 chat_id（逗号分隔）: ',' grp-1, grp-2 ']]},
 {'id':'reuse','argv':['work-weixin'],'initial':seed,'answers':[['重新配置？ [y/N]: ','no']]},
 {'id':'skip','argv':['workwechat'],'answers':[[scan,'no'],['Bot ID: ',''],['Secret: ','']]},
 {'id':'qr-retry-success','argv':['wecom'],'answers':[[scan,''],[ws,''],[dm,''],[group,'']],'responses':[generate,{'error':'fixture query failed'},success]},
 {'id':'qr-malformed','argv':['wecom'],'answers':[[scan,''],['Bot ID: ',''],['Secret: ','']],'responses':[{'json':{'data':{}}}]},
 {'id':'qr-http-error','argv':['wecom'],'answers':[[scan,''],['Bot ID: ',''],['Secret: ','']],'responses':[{'status':503,'raw':'{bad'}]},
 {'id':'qr-missing-creds','argv':['wecom'],'answers':[[scan,''],['Bot ID: ',''],['Secret: ','']],'responses':[generate,{'json':{'data':{'status':'success','bot_info':{'botid':'only-id'}}}}]},
 {'id':'qr-timeout','argv':['wecom'],'answers':[[scan,''],['Bot ID: ',''],['Secret: ','']],'responses':[generate]+[{'json':{'data':{'status':'waiting'}}} for _ in range(100)]},
 {'id':'reconfigure','argv':['wecom'],'initial':seed,'answers':[['重新配置？ [y/N]: ','yes'],[scan,'n'],['Bot ID (当前: old-…t-id): ',''],['Secret (回车保留当前值): ',''],[ws,''],[dm,'3'],[group,'2']]},
 {'id':'menu','argv':[],'answers':[['请选择 [1/2/3/4/q]: ','3'],[scan,'n'],['Bot ID: ',''],['Secret: ','']]},
]
if a.boundaries:
 deep="shared: &external {policy: open}\nadapters:\n  wecom:\n    token: old-bot-id\n    extra:\n      secret: old-secret\n      groups: &groups\n        first: &entry {allow: [one, two]}\n        second: *entry\n        self: *groups\n        external: *external\n"
 cases=[
  {'id':'surrogate-scode','argv':['wecom'],'answers':[[scan,''],['Bot ID: ',''],['Secret: ','']],'responses':[{'raw':r'{"data":{"scode":"\ud800","auth_url":"https://fixture.invalid/scan"}}'}]},
  {'id':'deep-shared-cycle-groups','argv':['wecom'],'initial':deep,'answers':[['重新配置？ [y/N]: ','no']]},
 ]
results=[]
for case in cases:
 home=out/case['id'];home.mkdir();pilot=home/'pilot';pilot.mkdir();config=pilot/'pilotdeck.yaml'
 if 'initial'in case:config.write_text(case['initial'])
 data=home/'case.json';data.write_text(json.dumps(case,ensure_ascii=False))
 tmp=home/'tmp';tmp.mkdir();events=home/'events.json';env={**os.environ,'HOME':str(home),'PILOT_HOME':str(pilot),'PILOTDECK_CONFIG_PATH':str(config),'TMPDIR':str(tmp),'SETUP_EVENTS':str(events)};env.pop('NODE_OPTIONS',None)
 driver=Path(__file__).with_name('wecom-driver.mjs')
 child=subprocess.Popen([str(node),'--import',str(base/'node_modules/tsx/dist/loader.mjs'),str(driver),str(data),str(base/source)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env,cwd=home)
 selector=selectors.DefaultSelector();selector.register(child.stdout,selectors.EVENT_READ);stdout=b'';index=0;deadline=time.monotonic()+8
 while child.poll() is None and time.monotonic()<deadline:
  for key,_ in selector.select(.03):
   stdout+=os.read(key.fd,65536)
   if index<len(case['answers']) and case['answers'][index][0].encode() in stdout:
    child.stdin.write((case['answers'][index][1]+'\n').encode());child.stdin.flush();index+=1
 timed=child.poll() is None
 if timed:child.kill()
 rest,stderr=child.communicate();stdout+=rest
 result={**case,'stdout':stdout.decode(),'stderr':stderr.decode(),'code':child.returncode,'timeout':timed,'answered':index,'after':config.read_text() if config.exists() else None,**(json.loads(events.read_text()) if events.exists() else {})}
 with (out/(case['id']+'.raw.json')).open('x')as f:json.dump(result,f,ensure_ascii=False,indent=2)
 if timed or child.returncode or index!=len(case['answers']):raise SystemExit('failed case '+case['id'])
 results.append(result)
with (out/('wecom-boundaries.json' if a.boundaries else 'wecom-oracle.json')).open('x')as f:json.dump({'baseline':sha,'sourceSha256':hashlib.sha256(original).hexdigest(),**versions,'cases':results},f,ensure_ascii=False,indent=2);f.write('\n')
print('recorded',len(results),'WeCom cases')
