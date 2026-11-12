# anchor-selfcheck: unsupported isolated runtime path and mount layout, no product source mutations
"""Same logical runtime paths on every side; no business path rewriting."""
from pathlib import Path
RUNTIME={'source':'/app/source','bin':'/app/source/bin','home':'/app/home','project':'/app/project','tmp':'/app/tmp','workspaces':'/app/workspaces','controlPort':28610,'providerPort':28690}
def prepare_run(run):
 run=Path(run).resolve();run.mkdir(mode=0o700)
 for name in ['home','project','tmp','workspaces']:(run/name).mkdir(mode=0o700)
 (run/'observation').mkdir(mode=0o700)
 return run
def app_mounts(run,baseline,package,engine):
 run=Path(run).resolve();baseline=Path(baseline).resolve();package=Path(package).resolve()
 rows=[{'source':str(baseline),'destination':'/app/source','readonly':True},{'source':str(package/'bin'),'destination':'/app/source/bin','readonly':True}]
 if engine=='native':rows.append({'source':str(package/'source/skills'),'destination':'/app/source/skills','readonly':True})
 rows.extend({'source':str(run/name),'destination':RUNTIME[name],'readonly':False} for name in ['home','project','tmp','workspaces'])
 return rows
def docker_mount_args(rows):
 args=[]
 for row in rows:args+=['--mount','type=bind,src='+row['source']+',dst='+row['destination']+(',readonly' if row['readonly'] else '')]
 return args
# Frozen compare.disk is applied only to run/home and run/project. Keep config,
# skills and all business bytes; package/bin/source are delivery evidence, not disk output.
