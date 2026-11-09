#!/usr/bin/env python3
"""Three product mutants in an explicitly isolated workspace; never edits fixtures.
Caller supplies separate build and runtime commands as JSON argv. Compilation
failure is ERROR, never KILLED. No command is run by merely importing this file.
"""
import argparse,json,pathlib,subprocess

def main():
    p=argparse.ArgumentParser();p.add_argument('--workspace',required=True);p.add_argument('--build-command-json',required=True);p.add_argument('--test-command-json',required=True);a=p.parse_args()
    root=pathlib.Path(a.workspace).resolve(); module=root/'src/pilotdeck-gateway/src/channel/qq'
    if not (root/'.git').exists():p.error('explicit isolated git workspace required')
    build=json.loads(a.build_command_json);test=json.loads(a.test_command_json)
    if not all(isinstance(x,str) for x in build+test) or not build or not test:p.error('commands must be nonempty JSON string arrays')
    controls=[('group-scope','mapper.rs','"qq:group="','"foreign:group="'),('gateway-intents','gateway.rs','JsValue::Number(33_554_432.0)','JsValue::Number(0.0)'),('passive-sequence','gateway.rs','msg_seq.unwrap_or(1) as f64','(msg_seq.unwrap_or(1)+1) as f64')]
    # A failing original must never make every mutant look killed.
    subprocess.run(build,cwd=root,check=True)
    subprocess.run(test,cwd=root,check=True)
    results=[]
    for name,file,old,new in controls:
        path=module/file;original=path.read_bytes();source=original.decode()
        if source.count(old)!=1:raise RuntimeError(f'{name}: exact production anchor count {source.count(old)}')
        try:
            path.write_text(source.replace(old,new))
            built=subprocess.run(build,cwd=root,check=False)
            if built.returncode:results.append({'name':name,'status':'ERROR','buildExit':built.returncode});continue
            ran=subprocess.run(test,cwd=root,check=False)
            results.append({'name':name,'status':'KILLED' if ran.returncode else 'SURVIVED','testExit':ran.returncode})
        finally:path.write_bytes(original)
    # Rebuild the restored original so no mutated binary is left as evidence.
    subprocess.run(build,cwd=root,check=True)
    subprocess.run(test,cwd=root,check=True)
    print(json.dumps(results,indent=2))
    return 0 if all(r['status']=='KILLED' for r in results) else 1
if __name__=='__main__':raise SystemExit(main())
