#!/usr/bin/env python3
"""Record fixed TS setup and actual weixin-ilink callbacks without real network."""
import argparse,hashlib,json,os,selectors,subprocess,time,resource,signal
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--baseline',required=True,type=Path);p.add_argument('--node',required=True,type=Path);p.add_argument('--output',required=True,type=Path);p.add_argument('--all-only',action='store_true');p.add_argument('--write-failure',action='store_true');a=p.parse_args();base=a.baseline.resolve();node=a.node.resolve();out=a.output.resolve();out.mkdir(parents=True,exist_ok=False)
sha='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';source='src/cli/commands/gatewaySetup.ts';original=subprocess.check_output(['git','-C',str(base),'show',sha+':'+source])
if subprocess.check_output(['git','-C',str(base),'rev-parse','HEAD'],text=True).strip()!=sha or original!=(base/source).read_bytes():raise SystemExit('baseline mismatch')
v=json.loads(subprocess.check_output([str(node),'-p','JSON.stringify({node:process.version,icu:process.versions.icu})'],text=True))
if v!={'node':'v22.22.0','icu':'78.2'}:raise SystemExit('Node/ICU mismatch')
if json.loads((base/'node_modules/weixin-ilink/package.json').read_text())['version']!='0.1.0':raise SystemExit('SDK mismatch')
start='继续扫码登录？ [Y/n]: ';relogin='重新登录？ [y/N]: '
qr={'json':{'qrcode':'a/b 中','qrcode_img_content':'https://fixture.invalid/qr'}}
confirmed={'json':{'status':'confirmed','ilink_bot_id':'fixture-account','bot_token':'fixture-token','baseurl':'https://fixture.invalid/api','ilink_user_id':'not-saved'}}
old='{"baseUrl":"https://fixture.invalid/old","botToken":"old-token","accountId":"old-account","keep":"untouched"}'
cases=[
 {'id':'cancel','argv':['weixin'],'answers':[[start,'no']]},
 {'id':'reuse','argv':['wechat'],'credentials':old,'initial':'keep: old\n','answers':[[relogin,'']]},
 {'id':'login-success','argv':['weixin'],'initial':'keep: old\n','answers':[[start,'']],'responses':[qr,confirmed]},
 {'id':'relogin-success','argv':['weixin'],'credentials':old,'answers':[[relogin,'yes'],[start,'yes']],'responses':[qr,confirmed]},
 {'id':'scanned-refresh-success','argv':['weixin'],'answers':[[start,'']],'responses':[qr,{'json':{'status':'scaned'}},{'json':{'status':'expired'}},qr,confirmed]},
 {'id':'expired-too-many','argv':['weixin'],'answers':[[start,'']],'responses':[qr,{'json':{'status':'expired'}},qr,{'json':{'status':'expired'}},qr,{'json':{'status':'expired'}}]},
 {'id':'confirmed-missing-credentials','argv':['weixin'],'answers':[[start,'']],'responses':[qr,{'json':{'status':'confirmed','ilink_bot_id':'missing-token'}}]},
 {'id':'initial-http-error','argv':['weixin'],'answers':[[start,'']],'responses':[{'status':503,'raw':'{bad'}]},
 {'id':'poll-http-error','argv':['weixin'],'answers':[[start,'']],'responses':[qr,{'status':502,'raw':'{bad'}]},
 {'id':'abort-then-success','argv':['weixin'],'answers':[[start,'']],'responses':[qr,{'error':'This operation was aborted','errorName':'AbortError'},confirmed]},
 {'id':'surrogate-qrcode','argv':['weixin'],'answers':[[start,'']],'responses':[{'raw':r'{"qrcode":"\ud800","qrcode_img_content":"https://fixture.invalid/qr"}'}]},
 {'id':'invalid-old-credentials','argv':['weixin'],'credentials':'{bad','answers':[[start,'']],'responses':[qr,confirmed]},
 {'id':'missing-custom-pilot-parent','argv':['weixin'],'createPilot':False,'answers':[[start,'']],'responses':[qr,confirmed]},
 {'id':'menu-cancel','argv':[],'answers':[['请选择 [1/2/3/4/q]: ','2'],[start,'n']]},
]
if a.all_only:cases=[{'id':'all-platforms-cancel','argv':[],'answers':[['请选择 [1/2/3/4/q]: ','4'],['使用哪个平台？ [feishu/lark] (当前: feishu): ',''],['App ID: ',''],['App Secret: ',''],[start,'n'],['是否扫码自动创建/获取 Bot ID 和 Secret？ [Y/n]: ','n'],['Bot ID: ',''],['Secret: ','']]}]
if a.write_failure:cases=[{'id':'credential-write-efbig','argv':['weixin'],'credentials':old,'initial':'keep: original\n','answers':[[relogin,'yes'],[start,'yes']],'responses':[qr,confirmed]}]
def restrict_write():
 signal.signal(signal.SIGXFSZ,signal.SIG_IGN)
 resource.setrlimit(resource.RLIMIT_FSIZE,(0,0))
results=[]
for case in cases:
 home=out/case['id'];home.mkdir();pilot=home/'pilot'
 if case.get('createPilot',True):pilot.mkdir()
 if 'initial'in case:(pilot/'pilotdeck.yaml').write_text(case['initial'])
 if 'credentials'in case:(pilot/'weixin-credentials.json').write_text(case['credentials'])
 data=home/'case.json';data.write_text(json.dumps(case,ensure_ascii=False));tmp=home/'tmp';tmp.mkdir();events=home/'events.json';env={**os.environ,'HOME':'home','PILOT_HOME':'pilot','PILOTDECK_CONFIG_PATH':'pilot/pilotdeck.yaml','TMPDIR':str(tmp),'SETUP_EVENTS':str(events)};env.pop('NODE_OPTIONS',None)
 if a.write_failure:env.update({'TSX_DISABLE_CACHE':'1','SETUP_EVENTS_STDOUT':'1'})
 child=subprocess.Popen([str(node),'--import',str(base/'node_modules/tsx/dist/loader.mjs'),str(Path(__file__).with_name('weixin-driver.mjs')),str(data),str(base/source)],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=subprocess.PIPE,env=env,cwd=home,preexec_fn=restrict_write if a.write_failure else None)
 sel=selectors.DefaultSelector();sel.register(child.stdout,selectors.EVENT_READ);stdout=b'';index=0;deadline=time.monotonic()+8
 while child.poll() is None and time.monotonic()<deadline:
  for key,_ in sel.select(.03):
   stdout+=os.read(key.fd,65536)
   if index<len(case['answers']) and case['answers'][index][0].encode() in stdout:child.stdin.write((case['answers'][index][1]+'\n').encode());child.stdin.flush();index+=1
 timed=child.poll()is None
 if timed:child.kill()
 rest,stderr=child.communicate();stdout+=rest
 result={**case,'stdout':stdout.decode(),'stderr':stderr.decode(),'code':child.returncode,'timeout':timed,'answered':index,'after':(pilot/'pilotdeck.yaml').read_text()if(pilot/'pilotdeck.yaml').exists()else None,'afterCredentials':(pilot/'weixin-credentials.json').read_text()if(pilot/'weixin-credentials.json').exists()else None,'homePilotCreated':(home/'home/.pilotdeck').exists(),**(json.loads(events.read_text())if events.exists()else{})}
 with(out/(case['id']+'.raw.json')).open('x')as f:json.dump(result,f,ensure_ascii=False,indent=2)
 if timed or child.returncode or index!=len(case['answers']):raise SystemExit('failed case '+case['id'])
 results.append(result)
with(out/('weixin-write-failure.json'if a.write_failure else'weixin-all.json'if a.all_only else'weixin-oracle.json')).open('x')as f:json.dump({'baseline':sha,'sourceSha256':hashlib.sha256(original).hexdigest(),**v,'sdkVersion':'0.1.0','cases':results},f,ensure_ascii=False,indent=2);f.write('\n')
print('recorded',len(results),'Weixin cases')
