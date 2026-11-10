#!/usr/bin/env python3
"""Exercise owner write failure and pre-existing lease using isolated local mocks."""
import os, pathlib, shutil, subprocess, sys, tempfile, time
script=pathlib.Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix='pdrs-asr-owner-') as tmp:
 root=pathlib.Path(tmp).resolve(); home=root/'home'; source=root/'source'; llama=root/'llama'; models=root/'models'; bins=root/'bin'
 for path in (home,source/'runtime/llama.cpp',llama,models,bins):path.mkdir(parents=True,exist_ok=True)
 (source/'runtime/llama.cpp/CMakeLists.txt').write_text('# fixture\n')
 for directory in (source,llama):(directory/'LICENSE').write_text('mock license\n')
 for name in ('sensevoice-small-q8.gguf','fsmn-vad.gguf'):(models/name).write_text('mock model')
 (bins/'git').write_text('''#!/bin/sh
if [ "$3" = rev-parse ]; then
 if [ "$2" = "$PILOTDECK_FUNASR_SOURCE" ]; then echo 500956bc331bb7edbaac58d8f84a84f28bd3d29f; else echo 803b7fcae893e9caaee3921779628fef83ac0965; fi
fi
''')
 (bins/'date').write_text('''#!/bin/sh
: > "$FIXTURE_GATE/owner-write-reached"
/bin/ln -s /dev/full "$PILOT_HOME/funasr/.source-install.lock/owner.txt"
exec /bin/date "$@"
''')
 (bins/'cmake').write_text('#!/bin/sh\necho unexpected_builder >&2\nexit 9\n')
 for file in bins.iterdir():file.chmod(0o700)
 env={'PATH':str(bins)+':/usr/bin:/bin','PILOT_HOME':str(home),'PILOTDECK_FUNASR_SOURCE':str(source),'PILOTDECK_LLAMA_SOURCE':str(llama),'PILOTDECK_FUNASR_MODELS_DIR':str(models),'FIXTURE_GATE':str(root)}
 result=subprocess.run(['/bin/sh',str(script)],env=env,capture_output=True,timeout=15)
 lock=home/'funasr/.source-install.lock'
 assert result.returncode!=0,(result.returncode,result.stdout,result.stderr)
 assert (root/'owner-write-reached').is_file(), ('owner write was never reached',result.stderr)
 assert b'I/O error' in result.stderr or b'No space left on device' in result.stderr,result.stderr
 assert b'unexpected_builder' not in result.stderr,result.stderr
 assert not lock.exists() and not lock.is_symlink(), ('failed owner write left lease',result.stderr)
 assert not list((home/'funasr').glob('.source-build.*'))
 # The next attempt must never remove a lease that predates it.
 lock.mkdir();(lock/'owner.txt').write_text('other owner\n')
 result=subprocess.run(['/bin/sh',str(script)],env=env,capture_output=True,timeout=15)
 assert result.returncode!=0 and (lock/'owner.txt').read_text()=='other owner\n'
 assert b'Existing installation lease:' in result.stderr,result.stderr
 # Recreate a private lease only inside this fixture; old process must not remove it.
 shutil.rmtree(lock)
 (bins/'date').write_text('#!/bin/sh\nexec /bin/date "$@"\n'); (bins/'date').chmod(0o700)
 (bins/'cmake').write_text('#!/bin/sh\ntouch "$FIXTURE_GATE/ready"\nwhile [ ! -f "$FIXTURE_GATE/release" ]; do /bin/sleep 0.02; done\nexit 9\n'); (bins/'cmake').chmod(0o700)
 env['FIXTURE_GATE']=str(root)
 proc=subprocess.Popen(['/bin/sh',str(script)],env=env,stdout=subprocess.PIPE,stderr=subprocess.PIPE)
 try:
  end=time.monotonic()+10
  while not (root/'ready').exists() and proc.poll() is None and time.monotonic()<end:time.sleep(0.02)
  assert (root/'ready').exists(), ('mock builder did not start',proc.poll())
  shutil.rmtree(lock);lock.mkdir();(lock/'owner.txt').write_text('replacement owner\n')
  (root/'release').touch()
  stdout,stderr=proc.communicate(timeout=10)
  assert proc.returncode!=0
  assert (lock/'owner.txt').read_text()=='replacement owner\n', (stdout,stderr)
 finally:
  if proc.poll() is None:proc.kill();proc.communicate(timeout=5)
 print('owner-write failure cleaned own lease; pre-existing and replacement leases preserved')
