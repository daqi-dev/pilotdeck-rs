# anchor-selfcheck: unsupported read-only TS evidence integrity checker, no source mutation
#!/usr/bin/env python3
"""Check actual TS observations, replay equality and executed probe-level negative controls."""
import argparse
import base64
from collections import Counter
import hashlib
import json
from pathlib import Path

ROOT = Path(__file__).resolve().parent
BASELINE = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85'
NAMES = '''resume-missing resume-valid resume-malformed resume-eisdir resume-load-reject resume-checkpoint-reject resume-rollback-reject resume-release-reject admission-read-reject recreate-read-reject recreate-rollback-reject recreate-idle recreate-draining recreate-running recreate-disposed same-key-concurrent same-key-cached create-factory-dispose create-owner-shutdown create-dispose-before-mcp'''.split()
LEASES = ['project', 'permission', 'execution', 'plugin', 'router', 'sharedMcp', 'mcp', 'binding']

def require(ok, label):
    if not ok:
        raise AssertionError(label)

def snap(d, label='result'):
    return next((s for s in d['snapshots'] if s['label'] == label), None)

def op(d, label):
    return next((o for o in d['results'] if o['label'] == label), {})

def entries(d, kind, **attrs):
    return [e for e in d['timeline'] if e['type'] == kind and all(e['data'].get(k) == v for k, v in attrs.items())]

def pos(d, kind, **attrs):
    found = entries(d, kind, **attrs)
    require(len(found) == 1, 'unique-event:' + kind + ':' + str(attrs))
    return found[0]['ordinal']

def file_bytes(files, suffix):
    return [x['utf8'] for x in files if x['kind'] == 'file' and x['path'].endswith(suffix)]

def primary(d):
    """One explicit conclusion per cell; these are also the negative control kill points."""
    n = d['name']
    s = snap(d)
    require(s is not None, n + ':result-present')
    o = op(d, 'recreate' if n.startswith('recreate-') else 'create')
    ok = False
    if n == 'resume-missing':
        ok = o['status'] == 'fulfilled' and [v['code'] for v in s['storages'][0]['restores'][0]['diagnostics']] == ['transcript_missing']
    elif n == 'resume-valid':
        ok = o['status'] == 'fulfilled' and s['handles'][0]['reload']['metadata'] == {'firstPrompt': ' 已存 中文 ', 'title': '原题', 'titleSource': 'user'}
    elif n == 'resume-malformed':
        ok = o['status'] == 'fulfilled' and s['storages'][0]['restores'][0]['diagnostics'] == [{'code': 'transcript_line_invalid', 'severity': 'error', 'message': "Expected property name or '}' in JSON at position 1 (line 1 column 2)", 'line': 1}]
    elif n == 'resume-eisdir':
        ok = o['status'] == 'rejected' and o['error'] == {'name': 'Error', 'message': 'EISDIR: illegal operation on a directory, read', 'code': 'EISDIR'}
    elif n in ['resume-load-reject', 'resume-release-reject', 'admission-read-reject']:
        ok = o['status'] == 'rejected' and o['error'] == {'name': 'Error', 'message': 'LOAD_BOOM'}
    elif n == 'resume-checkpoint-reject':
        ok = o['status'] == 'rejected' and o['error'] == {'name': 'Error', 'message': 'CHECKPOINT_BOOM'} and s['storages'][0]['entriesAtCheckpoint'] == {'sequence': 1, 'lastEntryId': 'seed-entry'}
    elif n in ['resume-rollback-reject', 'recreate-rollback-reject']:
        verb = 'resume' if n.startswith('resume') else 'recreate'
        ok = o['status'] == 'rejected' and o['error'] == {'name': 'AggregateError', 'message': f'Failed to {verb} and roll back agent session session-745.', 'errors': [{'name': 'Error', 'message': 'LOAD_BOOM' if verb == 'resume' else 'RECREATE_LOAD_BOOM'}, {'name': 'Error', 'message': 'STORAGE_DISPOSE_BOOM'}]}
    elif n == 'recreate-read-reject':
        ok = o['status'] == 'rejected' and o['error'] == {'name': 'Error', 'message': 'RECREATE_LOAD_BOOM'}
    elif n == 'recreate-idle':
        ok = o == {'label': 'recreate', 'status': 'fulfilled', 'handle': 2} and [h['state'] for h in s['handles']] == ['disposed', 'active']
    elif n == 'recreate-draining':
        mid = snap(d, 'second-returned-old-draining')
        ok = mid is not None and [h['state'] for h in mid['handles']] == ['draining', 'active'] and mid['registry']['handle'] == 2 and mid['active']['project'] == [1, 2] and next(r for r in mid['results'] if r['label'] == 'recreate')['status'] == 'pending' and next(r for r in mid['results'] if r['label'] == 'cached-during-drain')['status'] == 'fulfilled'
    elif n == 'recreate-running':
        mid = snap(d, 'replacement-ready')
        ok = o['status'] == 'fulfilled' and [h['state'] for h in mid['handles']] == ['draining', 'active'] and mid['handles'][0]['inFlight'] == 1 and mid['handles'][1]['session']['status'] == 'idle' and mid['handles'][1]['reload']['transcriptWriterState']['sequence'] == 5
    elif n == 'recreate-disposed':
        ok = o['status'] == 'rejected' and o['error'] == {'name': 'Error', 'message': 'Session projection driver is disposed.'}
    elif n == 'same-key-concurrent':
        ok = op(d, 'first') == {'label': 'first', 'status': 'fulfilled', 'handle': 1} and op(d, 'second') == {'label': 'second', 'status': 'rejected', 'error': {'name': 'Error', 'message': 'Agent is already registered: session-745'}}
    elif n == 'same-key-cached':
        ok = op(d, 'first')['handle'] == op(d, 'second')['handle'] == 1 and len(s['handles']) == 1
    elif n == 'create-factory-dispose':
        ok = o['status'] == 'fulfilled' and s['handles'][0]['state'] == 'active' and s['active']['project'] == [1] and s['active']['mcp'] == []
    elif n == 'create-owner-shutdown':
        ok = o['status'] == 'rejected' and o['error'] == {'name': 'Error', 'message': 'Session router shut down during session creation.'} and s['handles'][0]['state'] == 'disposed' and s['active']['project'] == []
    elif n == 'create-dispose-before-mcp':
        ok = o['status'] == 'rejected' and o['error'] == {'name': 'Error', 'message': 'Cannot register per-session MCP runtime; registry is disposed.'} and not s['handles'] and not s['storages']
    require(ok, n + ':conclusion')

def semantics(d):
    primary(d)
    n = d['name']; s = snap(d)
    require(list(s['active']) == LEASES, 'lease-inventory')
    if n.startswith('resume-') or n == 'admission-read-reject':
        failed = op(d, 'create')['status'] == 'rejected'
        require(s['active']['project'] == ([] if failed else [1]), 'resume-project-lease')
        require(len(s['handles']) == (0 if failed else 1), 'resume-handle-count')
        if failed and n != 'admission-read-reject':
            require(s['storages'][0]['disposeCalls'] == 1 and not s['storages'][0]['persistenceActive'], 'resume-storage-disposed-once')
            require(pos(d, 'storage.dispose.end', id=1) < pos(d, 'lease.release.done', type='project', id=1) < pos(d, 'operation.rejected', label='create'), 'resume-storage-before-project-release-before-reject')
        if n == 'admission-read-reject':
            for label in ['result', 'after-owned-cleanup']:
                r = snap(d, label)['storages'][0]
                require(r['disposeCalls'] == 0 and r['persistenceActive'], 'admission-orphan-before-probe-cleanup')
            require(len(entries(d, 'probe.orphan-storage.cleanup', id=1)) == 1, 'admission-orphan-rescued-by-probe')
        elif not failed:
            require(s['storages'][0]['disposeCalls'] == 0 and s['storages'][0]['persistenceActive'], 'resume-ownership-transfer')
    if n == 'resume-release-reject':
        require(len(entries(d,'lease.release.error',type='project',id=1))==1, 'release-failure-injected-and-swallowed')
    if n in ['recreate-read-reject', 'recreate-rollback-reject']:
        require(s['registry']['handle'] == 1 and [h['state'] for h in s['handles']] == ['active'] and all(v == [1] for v in s['active'].values()), 'recreate-failure-keeps-old-owner')
        require(s['storages'][1]['disposeCalls'] == 1 and not s['storages'][1]['persistenceActive'], 'recreate-new-storage-disposed')
        require(pos(d, 'lease.release.done', type='project', id=2) < pos(d, 'storage.dispose.begin', id=2) < pos(d, 'operation.rejected', label='recreate'), 'recreate-release-before-storage-before-reject')
        require(s['handles'][0]['reload'] == snap(d, 'before-recreate')['handles'][0]['reload'], 'old-session-unchanged-on-failure')
    if n in ['recreate-idle', 'recreate-draining', 'recreate-running']:
        old = snap(d, 'before-recreate')['handles'][0]['reload']
        new = s['handles'][1]['reload']
        require(new == old, 'recreate-full-reload-snapshot-equal')
        require(new['state']['status'] == 'idle' and 'currentTurnId' not in new['state'], 'recreate-idle-no-current-turn')
        require(new['fileState'] == {}, 'initial-stdio-file-state')
        constructors = entries(d, 'loop.construct')
        require(constructors[1]['data']['seedState'] == old['fileState'], 'factory-passes-seed-state-to-loop')
        require(s['active']['project'] == [2] and s['active']['mcp'] == [2], 'old-release-keeps-replacement-resources')
    if n == 'recreate-running':
        require(d['modelRequests'] == [], 'existing-user-title-no-model-request')
        require([e['event']['type'] for e in d['events']] == ['session_started','setup_completed','turn_started','input_accepted','user_prompt_submitted','turn_completed','session_ended'], 'exact-old-turn-event-types')
        require(d['events'][-2]['event']['result']['type'] == 'aborted', 'old-turn-aborted')
        require([c['entry']['sequence'] for c in d['commits']] == list(range(1,8)), 'old-late-append-sequences')
        require(s['storages'][1]['writer']['sequence'] == 5 and s['storages'][0]['writer']['sequence'] == 7, 'replacement-does-not-track-old-tail')
        checkpoint = json.loads(file_bytes(d['finalFiles'], '.projections.json')[0])
        require(checkpoint['asOfSequence'] == 5, 'replacement-cleanup-checkpoint-is-stale-five')
        mid=snap(d,'replacement-ready')
        require(mid['handles'][0]['session']['abortController']=={'aborted':True,'reason':'explicit-old-drain'} and mid['handles'][1]['session']['abortController']=={'aborted':False,'reason':None} and mid['handles'][1]['controllerSameAsFirst'] is False, 'fresh-cancellation-controller')
        require(mid['registry']['handle']==1, 'direct-factory-has-not-published-new-handle')
        require(pos(d, 'loop.abort') < snap(d, 'replacement-ready')['ordinal'] < pos(d, 'loop.resume'), 'gated-abort-snapshot-before-completion')
    else:
        require(d['modelRequests'] == [] and d['events'] == [], 'no-turn-no-model-request')
    if n == 'recreate-draining':
        require(pos(d, 'operation.fulfilled', label='cached-during-drain') < pos(d, 'descendants.drain.end', id=1) < pos(d, 'operation.fulfilled', label='recreate'), 'cached-return-before-old-drain-before-original-return')
    if n == 'recreate-disposed':
        require(len(s['storages']) == 1 and not s['active']['project'], 'snapshot-fails-before-new-storage')
        require(len(entries(d, 'lease.release.done', type='project', id=2)) == 1, 'failed-snapshot-releases-prepared-generation')
    if n == 'same-key-concurrent':
        require(snap(d,'both-creating')['active']['project'] == [1,2] and [h['state'] for h in s['handles']] == ['active','disposed'], 'both-factories-created-second-rolled-back')
        require(s['registry']['handle'] == 1 and s['active']['mcp'] == [1] and s['storages'][1]['disposeCalls'] == 1, 'rollback-only-second-identity')
    if n == 'create-owner-shutdown':
        require(next(r for r in snap(d,'dispose-during-create')['results'] if r['label']=='dispose')['status']=='pending', 'owner-shutdown-waits-inflight')
        require(s['registry'] == {'size':0,'handle':None,'hasSession':False} and all(not x for x in s['active'].values()), 'owner-shutdown-drains-all')
    if n == 'create-factory-dispose':
        require(s['factoryMcp']=={'state':'disposed','size':0}, 'factory-mcp-disposed-with-live-session')
        mid = snap(d, 'dispose-during-create')
        require(next(r for r in mid['results'] if r['label']=='dispose')['status']=='fulfilled' and not mid['handles'] and mid['active']['project']==[1] and mid['active']['mcp']==[], 'factory-dispose-returns-before-handle')
    if n == 'create-dispose-before-mcp':
        require(len(entries(d, 'mcp.construct'))==1 and not entries(d,'mcp.start') and not entries(d,'mcp.stop'), 'dormant-mcp-never-started')
    return True

def integrity(d):
    require(d['name'] in NAMES and d['baseline']==BASELINE and d['node']=='v22.22.0', 'identity')
    require(d['status']=='OBSERVED' and 'error' not in d and 'cleanupError' not in d, 'probe-valid')
    require(d['forbidden']==[], 'no-network-or-child-attempt')
    require([x['ordinal'] for x in d['timeline']] == list(range(1,len(d['timeline'])+1)), 'complete-event-sequence')
    require(all(not ids for ids in d['cleanup']['active'].values()) and all(h=='disposed' for h in d['cleanup']['handles']) and not any(d['cleanup']['storageBindings']), 'probe-final-clean')
    require(all(n==1 for n in d['cleanup']['releases'].values()), 'leases-released-exactly-once')
    acquired=Counter((e['data']['type'],e['data']['id']) for e in d['timeline'] if e['type']=='lease.acquire')
    called=Counter((e['data']['type'],e['data']['id']) for e in d['timeline'] if e['type']=='lease.release.call')
    released=Counter((e['data']['type'],e['data']['id']) for e in d['timeline'] if e['type']=='lease.release.done')
    require(acquired==called==released and all(count==1 for count in acquired.values()), 'every-lease-release-callback-exactly-once')
    require(all(m['stops'] in [0,1] for m in d['cleanup']['mcpStops']), 'mcp-stopped-at-most-once')
    for group, event_type in [('modelRequests','model.request'),('events','agent.event'),('commits','commit'),('snapshots','snapshot')]:
        for item in d[group]:
            require(d['timeline'][item['ordinal']-1]['type']==event_type, 'cross-reference:'+group)
    case_root=(ROOT/d['caseDir']).resolve()
    require(case_root.is_relative_to(ROOT) and case_root.is_dir(), 'isolated-case-root')
    for files in [s['files'] for s in d['snapshots']]+[d['finalFiles']]:
        require(len({f['path'] for f in files})==len(files), 'snapshot-unique-paths')
        for f in files:
            require((case_root/f['path']).resolve().is_relative_to(case_root), 'snapshot-contained')
            if f['kind']=='file':
                data=base64.b64decode(f['base64'],validate=True)
                require(data.decode()==f['utf8'] and hashlib.sha256(data).hexdigest()==f['sha256'], 'snapshot-byte-integrity')
    expected={f['path'] for f in d['finalFiles']}
    require({str(p.relative_to(case_root)) for p in case_root.rglob('*')}==expected, 'physical-file-inventory')
    for f in d['finalFiles']:
        p=case_root/f['path']
        require(p.is_dir() if f['kind']=='directory' else p.read_bytes()==base64.b64decode(f['base64']), 'physical-file-bytes')
    if d['name'].startswith('recreate-'):
        data=file_bytes(d['finalFiles'], 'session-745.jsonl')
        require(len(data)==1, 'recreate-transcript-present')
        parsed=[json.loads(line) for line in data[0].splitlines()]
        require(parsed==[c['entry'] for c in d['commits']], 'commits-match-physical-transcript')
        parent=None
        for i, e in enumerate(parsed,1):
            require(e['sequence']==i and e['parentEntryId']==parent, 'transcript-parent-chain')
            parent=e['entryId']

def normalize(d):
    # Only the exact generated run-directory component and monotonic timings vary.
    token=Path(d['caseDir']).parts[0]
    require(token.startswith('run-') and len(token)==10, 'run-token')
    def visit(x):
        if isinstance(x,dict):return {k:visit(v) for k,v in x.items() if k!='elapsedNs'}
        if isinstance(x,list):return [visit(v) for v in x]
        if isinstance(x,str):return x.replace(token,'run-<RUN>')
        return x
    return visit(d)

def load(directory,names,negative):
    where=ROOT/directory
    idx=json.loads((where/'index.json').read_text())
    require(idx['baseline']==BASELINE and idx['node']=='v22.22.0' and idx['forbidden']==[] and idx['negative']==negative, 'index-identity')
    ids=[r['name'] for r in idx['results']]
    require(len(set(ids))==len(ids) and set(ids)==set(names), 'exact-case-denominator')
    require({p.stem for p in where.glob('*.json')}==set(names)|{'index'}, 'exact-raw-inventory')
    out={n:json.loads((where/(n+'.json')).read_text()) for n in names}
    for n,d in out.items():
        require(d['name']==n and d['negative']==negative, 'raw-identity')
        integrity(d)
    return out

if __name__ == '__main__':
    import sys
    ROOT = Path(sys.argv[1]).resolve()
    for data in load('raw', NAMES, False).values():
        semantics(data)
    print('PASS: 20 TS lifecycle cells and physical evidence')
