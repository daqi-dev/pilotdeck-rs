#!/usr/bin/env python3
# anchor-selfcheck: unsupported Native business integration probe; no source mutation table.
"""Bounded product network increment reusing the real control-stream harness."""
import argparse,contextlib,hashlib,importlib.util,json,os,shutil,signal,sys,textwrap
from pathlib import Path
sys.dont_write_bytecode=True
HERE=Path(__file__).resolve().parent;ROOT=HERE.parents[4]
parser=argparse.ArgumentParser(add_help=False)
parser.add_argument('--baseline',type=Path,required=True)
parser.add_argument('--source-receipt',type=Path,required=True)
parser.add_argument('--provenance-label',required=True)
network,remaining_args=parser.parse_known_args()
sys.argv=[sys.argv[0],*remaining_args]
_network_here=HERE;_network_baseline=network.baseline.resolve()
_network_source_receipt=network.source_receipt.resolve()
_network_provenance_label=network.provenance_label
harness=ROOT/'src/pilotdeck-cli/tests/fixtures/control-stream/native.py'
source=harness.read_text()
EXPECTED_HARNESS_SHA='f54cf87382ad3688f026e10932983b873800ee2847e14ac5da859e7bca0dccb3'
assert hashlib.sha256(source.encode()).hexdigest()==EXPECTED_HARNESS_SHA,'existing harness source changed: review before reuse'
def replace_once(before,after):
 global source
 assert source.count(before)==1,('anchor changed',before)
 source=source.replace(before,after,1)
replace_once('deadline = time.monotonic() + 90','deadline = time.monotonic() + 120')
replace_once('agent: {{model: fixture/mock}}','agent: {{model: fixture/mock, maxOutputTokens: 256}}')
replace_once('        mock: {{}}','        mock: {{multimodal: {{input: [text, image]}}}}')
replace_once('        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))','        length=int(self.headers["Content-Length"]); assert 0 < length <= 2*1024*1024, "provider body cap"\n        assert len([r for r in rows if "provider" in r]) < 24, "provider request cap"\n        body = json.loads(self.rfile.read(length))')
snapshot=r'''
source_receipt_bytes=_network_source_receipt.read_bytes()
source_receipt=json.loads(source_receipt_bytes)
provenance={"label":_network_provenance_label,"formalAcceptance":False,"sourceReceipt":{"path":str(_network_source_receipt),"sha256":hashlib.sha256(source_receipt_bytes).hexdigest()},"harnessSha256":EXPECTED_HARNESS_SHA,"fixtureSha256":{p.name:hashlib.sha256(p.read_bytes()).hexdigest()for p in _network_here.iterdir()if p.is_file()},"binaries":{}}
(args.out/"binaries").mkdir()
for name in ("control","gateway","sidecar"):
    original=getattr(args,name);before=hashlib.sha256(original.read_bytes()).hexdigest();target=args.out/"binaries"/name
    receipt_name={"control":"pilotdeck-control","gateway":"pilotdeck-gateway","sidecar":"pilotdeck-agent-loop-sidecar"}[name]
    assert source_receipt["binaries"][receipt_name]==before,"binary does not match supplied source receipt"
    with original.open("rb") as source_file,target.open("xb") as destination:shutil.copyfileobj(source_file,destination)
    assert hashlib.sha256(target.read_bytes()).hexdigest()==before==hashlib.sha256(original.read_bytes()).hexdigest()
    target.chmod(0o700);provenance["binaries"][name]={"original":str(original),"snapshot":str(target),"sha256":before};setattr(args,name,target)
(args.out/"provenance.json").write_text(json.dumps(provenance,indent=2)+"\n")
'''
replace_once('args.out.mkdir(parents=True, exist_ok=False)','args.out.mkdir(parents=True, exist_ok=False)\n'+snapshot)
replace_once('diagnostics = []','env["PDRS_BASELINE"]=str(_network_baseline)\ndiagnostics = []')
anchor='    url = "ws://" + address + "/ws"\n    code, out, err = control("status", {})'
stimulus=textwrap.indent((HERE/'case.py').read_text(),'    ')
replace_once(anchor,'    url = "ws://" + address + "/ws"\n'+stimulus+'\n    code, out, err = control("status", {})')
release=r'''
    released_address=("127.0.0.1",int(address.rsplit(":",1)[1]))
    with socket.socket() as released_probe:
        released_probe.settimeout(1)
        assert released_probe.connect_ex(released_address)!=0,"listener still accepts after graceful shutdown"
    with socket.socket() as rebound:
        rebound.setsockopt(socket.SOL_SOCKET,socket.SO_REUSEADDR,1);rebound.bind(released_address);rebound.listen(1)
    rows.append({"network_listener_released":{"address":address,"refusedConnection":True,"rebound":True}})
'''
replace_once('    passed = True',release+'    passed = True')
replace_once('    (args.out / "result.json").write_text','    network_snapshot_unchanged=all(hashlib.sha256(Path(item["original"]).read_bytes()).hexdigest()==item["sha256"]==hashlib.sha256(Path(item["snapshot"]).read_bytes()).hexdigest() for item in provenance["binaries"].values())\n    (args.out / "result.json").write_text')
replace_once('{"pass": passed and cleanup_verified, "binaries":','{"pass": passed and cleanup_verified and network_snapshot_unchanged, "network_binary_snapshots_unchanged":network_snapshot_unchanged,"network_increment":True,"binaries":')
spec=importlib.util.spec_from_file_location('native_network_lock',ROOT/'scripts/gateway-parity/common.py');common=importlib.util.module_from_spec(spec);spec.loader.exec_module(common)
def interrupt(signum,frame):
 for item in (signal.SIGINT,signal.SIGTERM,signal.SIGHUP,signal.SIGALRM):signal.signal(item,signal.SIG_IGN)
 raise InterruptedError('network diagnostic signal '+str(signum))
for item in (signal.SIGINT,signal.SIGTERM,signal.SIGHUP,signal.SIGALRM):signal.signal(item,interrupt)
signal.alarm(140)
try:
 with common.parity_lock(timeout=1):
    common.LIVE_GROUPS.add(os.getpid())
    try:exec(compile(source,str(harness),'exec'),globals())
    finally:
        result=globals().get('args',None)
        path=result.out/'result.json' if result else None
        if path and path.exists() and json.loads(path.read_text()).get('cleanup_verified'):common.LIVE_GROUPS.discard(os.getpid())
finally:signal.alarm(0)
