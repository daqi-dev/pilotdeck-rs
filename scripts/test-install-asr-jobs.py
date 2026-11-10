#!/usr/bin/env python3
"""Check installer fanout bounds without building or fetching any source."""
import pathlib,subprocess,sys,tempfile
script=pathlib.Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix='pdrs-asr-jobs-') as tmp:
 root=pathlib.Path(tmp);source=root/'source';llama=root/'llama';models=root/'models';bins=root/'bin'
 for p in (source/'runtime/llama.cpp',llama,models,bins):p.mkdir(parents=True,exist_ok=True)
 (source/'runtime/llama.cpp/CMakeLists.txt').write_text('# mock\n')
 for p in (source,llama):(p/'LICENSE').write_text('mock')
 for name in ('sensevoice-small-q8.gguf','fsmn-vad.gguf'):(models/name).write_text('mock')
 (bins/'git').write_text('''#!/bin/sh
if [ "$3" = rev-parse ]; then
 if [ "$2" = "$PILOTDECK_FUNASR_SOURCE" ]; then echo 500956bc331bb7edbaac58d8f84a84f28bd3d29f; else echo 803b7fcae893e9caaee3921779628fef83ac0965; fi
fi
''')
 (bins/'cmake').write_text('#!/bin/sh\necho fixture_cmake_reached >&2\nexit 9\n')
 for p in bins.iterdir():p.chmod(0o700)
 for i,(jobs,allowed) in enumerate([('1',True),('128',True),('129',False),('999999999999999999999999999',False),('0',False),('01',False),('x',False)]):
  home=root/('home-'+str(i));env={'PATH':str(bins)+':/usr/bin:/bin','PILOT_HOME':str(home),'PILOTDECK_FUNASR_SOURCE':str(source),'PILOTDECK_LLAMA_SOURCE':str(llama),'PILOTDECK_FUNASR_MODELS_DIR':str(models),'PILOTDECK_ASR_BUILD_JOBS':jobs}
  result=subprocess.run(['/bin/sh',str(script)],env=env,capture_output=True,timeout=15)
  assert result.returncode!=0
  assert (b'fixture_cmake_reached' in result.stderr)==allowed,(jobs,result.stderr)
  if not allowed:assert not (home/'funasr/.source-install.lock').exists(),jobs
 print('jobs 1/128 reach mock builder; 129/huge/0/leading-zero/non-digit rejected before lease')
