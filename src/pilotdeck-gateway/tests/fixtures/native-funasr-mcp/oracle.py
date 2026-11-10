#!/usr/bin/env python3
# anchor-selfcheck: unsupported native FunASR protocol oracle; no mutation table.
import argparse,hashlib,json,os,platform,subprocess
from pathlib import Path
p=argparse.ArgumentParser();p.add_argument('--baseline',type=Path,required=True);p.add_argument('--node',type=Path,required=True);p.add_argument('--out',type=Path,required=True);a=p.parse_args();a.out.mkdir(parents=True,exist_ok=False)
assert subprocess.check_output(['git','-C',str(a.baseline),'rev-parse','HEAD'],text=True).strip()=='cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
versions=json.loads(subprocess.check_output([str(a.node),'-p','JSON.stringify(process.versions)'],text=True));assert(versions['node'],versions['icu'],versions['uv'])==('22.22.0','78.2','1.52.1')
project=a.out/'project';project.mkdir();(project/'audio.wav').write_bytes(b'mock-audio');(a.out/'outside.wav').write_bytes(b'mock-outside');(project/'escape.wav').symlink_to(a.out/'outside.wav')
runtime=a.out/'asr';binary=runtime/'runtime/v0.2.0'/('darwin-arm64'if platform.system()=='Darwin'else'linux-x64')/'llama-funasr-sensevoice';binary.parent.mkdir(parents=True);models=runtime/'models';models.mkdir()
for f in ['sensevoice-small-q8.gguf','fsmn-vad.gguf']:(models/f).write_bytes(b'mock-model')
source=a.baseline/'src/extension/plugins/builtin/funasr/funasr-local-mcp.mjs';rows=[]
def call(name,method,params=None,args=None,script="printf '1\\n00:00:01,250 --> 00:00:02,500\\nhello world\\n'"):
 binary.write_text('#!/bin/sh\n'+script+'\n');binary.chmod(0o700)
 command=[str(a.node),str(source)]+(['--project-root',str(project),'--runtime-root',str(runtime),'--install-command','mock install']if args is None else args)
 request={'jsonrpc':'2.0','id':1,'method':method}
 if params is not None:request['params']=params
 r=subprocess.run(command,input=json.dumps(request)+'\n',text=True,capture_output=True,timeout=8,env={'PATH':'/usr/bin:/bin','HOME':str(a.out)})
 assert r.returncode==0,(name,r.stderr);output=[json.loads(x)for x in r.stdout.splitlines()];assert len(output)==1,(name,output)
 rows.append({'case':name,'command':command,'request':request,'response':output[0],'stderr':r.stderr,'exitCode':r.returncode})
call('initialize','initialize',{'protocolVersion':'2025-03-26'})
call('tools-list','tools/list')
call('success','tools/call',{'name':'transcribe_audio','arguments':{'audio_path':'audio.wav'}})
call('language-before-path','tools/call',{'name':'transcribe_audio','arguments':{'language':None}})
call('escape','tools/call',{'name':'transcribe_audio','arguments':{'audio_path':'escape.wav'}})
call('missing-file','tools/call',{'name':'transcribe_audio','arguments':{'audio_path':'missing.wav'}})
call('url','tools/call',{'name':'transcribe_audio','arguments':{'audio_path':'https://invalid.example/audio.wav'}})
call('exit-error','tools/call',{'name':'transcribe_audio','arguments':{'audio_path':'audio.wav'}},script="printf 'real mock diagnostic' >&2; exit 9")
call('empty-srt','tools/call',{'name':'transcribe_audio','arguments':{'audio_path':'audio.wav'}},script="printf 'not an srt'; printf 'empty diagnostic' >&2")
call('no-runtime-precedes-language','tools/call',{'name':'transcribe_audio','arguments':{'language':'bad'}},args=['--install-command','mock install'])
call('unknown-tool','tools/call',{'name':'bad'})
call('unknown-method','ping')
(models/'fsmn-vad.gguf').unlink();call('missing-model','tools/call',{'name':'transcribe_audio','arguments':{'audio_path':'audio.wav'}})
(a.out/'oracle.json').write_text(json.dumps({'baseline':'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85','versions':versions,'node':str(a.node),'source':str(source),'sourceSha256':hashlib.sha256(source.read_bytes()).hexdigest(),'collectorSha256':hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),'network':'none; URL input rejected, never fetched','cases':rows},ensure_ascii=False,indent=2)+'\n')
print(json.dumps({'cases':len(rows),'output':str(a.out/'oracle.json')}))
