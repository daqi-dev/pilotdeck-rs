#!/usr/bin/env python3
"""Default installed CLI, packaged 32 skills, real sidecar; local model only."""
import argparse, hashlib, http.server, json, os, pathlib, re, signal, subprocess, threading, time
P=pathlib.Path
STANDALONE_NAMES = {'skill-creator', 'apple-reminders', 'gog', 'frontend-slides', '1password', 'web-design-guidelines', 'bear-notes', 'blogwatcher', 'apple-notes', 'trello', 'diagram-maker', 'audio-transcription', 'himalaya', 'notion', 'powershell', 'obsidian', 'spreadsheets', 'summarize', 'pdf', 'weather', 'docx', 'github', 'frontend-design', 'pilotdeck-skills-migration', 'tmux', 'karpathy-guidelines', 'meeting-recorder-assistant', 'pptx', 'find-skills', 'browser-use', 'react-next-best-practices', 'spike'}
class ExclusiveServer(http.server.ThreadingHTTPServer):
    allow_reuse_address = False

def write(p,v):
    with p.open('x') as f: json.dump(v,f,ensure_ascii=False,indent=2)
def table():
    rows={}
    for line in subprocess.check_output(['/bin/ps','-axo','pid=,ppid=,lstart=,comm='],text=True,timeout=5).splitlines():
        a=line.split(None,7)
        if len(a)==8: rows[int(a[0])]=(int(a[1]),a[2:7],a[7])
    return rows

def main():
    a=argparse.ArgumentParser(); a.add_argument('--install',type=P,required=True); a.add_argument('--out',type=P,required=True); a.add_argument('--expected-skills',type=int,choices=[32,33],default=33); x=a.parse_args()
    assert x.out.absolute().parent.is_dir() and not x.out.exists()
    assert not x.out.is_symlink()
    x.out.mkdir(mode=0o700); out=x.out.resolve(); root=x.install.resolve(strict=True)
    os.umask(0o077)
    for d in ['home','project','tmp']: (out/d).mkdir()
    records=[]; catalog=[]; errors=[]; child=None; known={}; sidecar_proofs={}; servers=[]
    result={'defaultSidecar':'NOT_RUN','skills':'NOT_RUN','pass':False}
    class Guard(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args): pass
        def do_GET(self): self.send_response(503); self.send_header('Content-Length','0'); self.end_headers()
    class Model(http.server.BaseHTTPRequestHandler):
        def log_message(self,*args): pass
        def do_POST(self):
            try:
                n=int(self.headers.get('Content-Length','0')); assert 0<n<8*1024*1024
                body=json.loads(self.rfile.read(n)); records.append(body); assert len(records)<=6
                text='\n'.join(str(m.get('content','')) for m in body.get('messages',[]) if m.get('role')=='system')
                if body.get('stream') and not catalog:
                    block=text.split('<available-skills>')[-1].split('</available-skills>')[0]
                    catalog.extend(re.findall(r'^- (\S+).*?\(file: (.*?)\)$',block,re.M))
                    assert len(catalog)==x.expected_skills, f'expected {x.expected_skills} default skills, got {len(catalog)}'
                    delta={'role':'assistant','tool_calls':[{'index':i,'id':f'skill-{i}','type':'function','function':{'name':'read_skill','arguments':json.dumps({'skillName':name})}} for i,(name,path) in enumerate(catalog)]}; reason='tool_calls'
                else: delta={'role':'assistant','content':'DEFAULT_CONSUMER_DONE'}; reason='stop'
                if body.get('stream'):
                    payload=''.join('data: '+json.dumps(v)+'\n\n' for v in [{'choices':[{'index':0,'delta':delta,'finish_reason':None}]},{'choices':[{'index':0,'delta':{},'finish_reason':reason}]}])+'data: [DONE]\n\n'; mime='text/event-stream'
                else:
                    payload=json.dumps({'choices':[{'message':{'role':'assistant','content':'{"title":"Default consumer"}'},'finish_reason':'stop'}]}); mime='application/json'
                b=payload.encode(); self.send_response(200); self.send_header('Content-Type',mime); self.send_header('Content-Length',str(len(b))); self.end_headers(); self.wfile.write(b)
            except Exception as e:
                errors.append(str(e)); self.send_error(500)
    expected_sidecar=(root/'current/bin/pilotdeck-agent-loop-sidecar').resolve(strict=True)
    sidecar_sha=json.loads((root/'current/manifest.json').read_text())['binHashes']['pilotdeck-agent-loop-sidecar']
    def sample():
        t=table(); active={pid for pid,v in known.items() if pid in t and t[pid][1]==v[1]}
        while True:
            fresh={pid for pid,v in t.items() if v[0] in active}-active
            fresh={pid for pid in fresh if pid not in known}
            if not fresh: break
            for pid in fresh: known[pid]=t[pid]
            active.update(fresh)
        # Linux comm is truncated to 15 bytes. Prove the actual owned executable
        # instead of accepting that truncated label as an executable identity.
        if P('/proc/self/stat').exists():
            for pid,identity in known.items():
                if pid not in t or t[pid][1]!=identity[1]: continue
                proc=P('/proc')/str(pid)
                try:
                    before=(proc/'stat').read_text().rpartition(') ')[2].split()
                    exe=(proc/'exe').resolve(strict=True)
                    if exe!=expected_sidecar: continue
                    digest=hashlib.sha256(exe.read_bytes()).hexdigest()
                    argv=(proc/'cmdline').read_bytes().split(b'\0')
                    after=(proc/'stat').read_text().rpartition(') ')[2].split()
                    if before[19]!=after[19] or int(before[1])!=t[pid][0]: continue
                    if digest!=sidecar_sha: raise AssertionError('default sidecar executable SHA mismatch')
                    if not argv or P(os.fsdecode(argv[0])).name!='pilotdeck-agent-loop-sidecar': continue
                    sidecar_proofs[pid]={'pid':pid,'identity':identity,'startTicks':before[19],'exe':str(exe),'sha256':digest,'argv':[os.fsdecode(x) for x in argv if x]}
                except (OSError,ProcessLookupError):
                    continue
        return t
    try:
        # Own the legacy discovery endpoint before launch; never contact an existing service.
        guard=ExclusiveServer(('127.0.0.1',18789),Guard); servers.append(guard)
        model=ExclusiveServer(('127.0.0.1',0),Model); servers.append(model)
        for s in servers: threading.Thread(target=s.serve_forever,daemon=True).start()
        (out/'home/pilotdeck.yaml').write_text(f'schemaVersion: 1\nagent:\n  model: test/test\nmodel:\n  providers:\n    test:\n      protocol: openai\n      url: http://127.0.0.1:{model.server_port}\n      apiKey: local-test-only\n      models:\n        test: {{}}\n')
        env={'HOME':str(out/'home'),'PILOT_HOME':str(out/'home'),'TMPDIR':str(out/'tmp'),'PATH':os.environ['PATH'],'LANG':'en_US.UTF-8'}
        write(out/'command.json',{'argv':[str(root/'bin/pilotdeck'),'Read the available bundled skills for this isolated acceptance.'],'env':env,'deadlineSeconds':180,'modelRequestLimit':6})
        with (out/'stdout').open('xb') as stdout,(out/'stderr').open('xb') as stderr:
            child=subprocess.Popen([root/'bin/pilotdeck','Read the available bundled skills for this isolated acceptance.'],cwd=out/'project',env=env,stdin=subprocess.DEVNULL,stdout=stdout,stderr=stderr,start_new_session=True)
            t=table()
            if child.pid in t: known[child.pid]=t[child.pid]
            write(out/'pid.json',{'pid':child.pid,'identity':known.get(child.pid)})
            start=time.monotonic()
            while child.poll() is None:
                sample()
                if time.monotonic()-start>180: raise TimeoutError('consumer deadline')
                time.sleep(.025)
        assert child.returncode==0, f'CLI exit {child.returncode}'
        assert not errors, errors
        assert 'DEFAULT_CONSUMER_DONE' in (out/'stdout').read_text()
        assert len(catalog)==x.expected_skills
        expected_names = STANDALONE_NAMES | ({'windows-skills:powershell-safe-invocation'} if x.expected_skills == 33 else set())
        assert {name for name,path in catalog} == expected_names, 'default skill name set mismatch'
        tools=[m for b in records for m in b.get('messages',[]) if m.get('role')=='tool']
        answers={m.get('tool_call_id'):m.get('content') for m in tools}
        assert len(answers)==x.expected_skills, f'expected {x.expected_skills} read_skill results, got {len(answers)}'
        assets=json.loads((root/'current/source/skills/upstream-assets.json').read_text())
        assert len(assets['files']) == 109, 'expected 109 bundled assets'
        for f in assets['files']:
            p=root/'current/source/skills'/f['path']; assert hashlib.sha256(p.read_bytes()).hexdigest()==f['sha256']
        for i,(name,path) in enumerate(catalog):
            p=P(path).resolve(strict=True)
            if name == 'windows-skills:powershell-safe-invocation':
                assert p == (root/'current/skills/.builtin-plugins/windows-skills/skills/powershell-safe-invocation/SKILL.md').resolve(strict=True), 'wrong installed Windows skill asset path'
                assert hashlib.sha256(p.read_bytes()).hexdigest() == '6748cbc46061399ad2ce3270cfd7ecd7ac37fc11aef18a38183bd6bdf0eb3d2a'
            else:
                assert p.is_relative_to((root/'current/source/skills').resolve())
            assert str(path) in str(answers[f'skill-{i}']), f'missing returned skill path: {name}'
            assert str(answers[f'skill-{i}']).startswith('<skill>\n<name>'), f'missing successful skill envelope: {name}'
        sidecars=list(sidecar_proofs.values()) if P('/proc/self/stat').exists() else [{'pid':pid,'identity':v} for pid,v in known.items() if 'pilotdeck-agent-loop-sidecar' in v[2]]
        assert sidecars, 'no actual default sidecar process observed'
        result.update(defaultSidecar='PASS',skills='PASS',pass_=True,skillCount=x.expected_skills,sidecars=sidecars); result['pass']=True
    except Exception as e:
        import traceback
        result['error']=repr(e); result['traceback']=traceback.format_exc()
    finally:
        if child:
            for sig in [signal.SIGTERM,signal.SIGKILL]:
                t=sample()
                for pid,v in known.items():
                    if pid in t and t[pid][1]==v[1]:
                        try: os.kill(pid,sig)
                        except ProcessLookupError: pass
                try: child.wait(timeout=5)
                except subprocess.TimeoutExpired: pass
                time.sleep(.1)
        final = sample()
        write(out/'sidecar-proofs.json',list(sidecar_proofs.values()))
        remaining = [pid for pid,v in known.items() if pid in final and final[pid][1] == v[1]]
        result['remainingOwnedProcesses'] = remaining
        if remaining:
            result['pass'] = False
            result['cleanupError'] = 'observed owned identities survived'
        for s in servers: s.shutdown(); s.server_close()
        write(out/'model-requests.json',records); write(out/'catalog.json',catalog); write(out/'processes.json',known); write(out/'result.json',result)
    return 0 if result['pass'] else 1
if __name__=='__main__': raise SystemExit(main())
