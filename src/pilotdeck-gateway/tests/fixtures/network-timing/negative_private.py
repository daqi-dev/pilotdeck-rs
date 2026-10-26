#!/usr/bin/env python3
"""Private caller controls. Run after committing newly added/changed sources.

Every mutant binds exact test names and panic anchors. Source ownership and
restoration use negative_process; compilation failures never count as kills.
CARGO_TARGET_DIR must be supplied by the caller; no alternate repo or target.
"""
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from negative_process import mutating_sources, run_owned, wait_for_slot

root = Path(__file__).resolve().parents[5]
network = 'src/pilotdeck-gateway/src/runtime/network.rs'
mcp = 'src/pilotdeck-gateway/src/runtime/mcp/http.rs'
hooks = 'src/pilotdeck-gateway/src/runtime/hooks/exec.rs'
fixture = 'src/pilotdeck-gateway/tests/common/network_timing_private.rs'
M = 'runtime::mcp::http::network_timing_tests::'
H = 'runtime::hooks::exec::network_timing_tests::'
def cell(id, anchor=None):
    prefix = H if id.startswith('hooks/') else M
    return ('lib', prefix + id.replace('/', '_').replace('-', '_'), anchor or 'assertion `left == right` failed: network timing cell: ' + id)
def guard(prefix, id):
    return ('lib', prefix + 'known_differences_remain_exact_and_ignored', 'assertion `left == right` failed: known difference changed: "' + id + '"')
# Hook headers/body cancellation coalesce to the same returned error; the
# I/O-kind mutant is therefore bound only to MCP's observable distinction.
cases = [
 ('private-io-kind', network, '            timer: None,\n            microtask: false,', '            timer: None,\n            microtask: true,',
  [cell(p+'/io-headers/fulfilled/tie-parent-last') for p in ['mcp_get','mcp_post']]),
 ('private-response', network, 'return Poll::Ready(Exchange::Response(result, aborted));', 'let _ = result; return Poll::Ready(Exchange::Aborted);',
  [cell(p+'/'+phase+'/fulfilled/'+order) for p in ['mcp_get','mcp_post','hooks'] for phase in ['headers','redirect-hop'] for order in ['continuation-first','tie-parent-last']]),
 ('private-frame', network, 'return Poll::Ready(Exchange::Response(result, false));', 'let _ = result; return Poll::Ready(Exchange::Aborted);',
  [cell(p+'/body/fulfilled/'+order) for p in ['mcp_get','mcp_post','hooks'] for order in ['continuation-first','tie-parent-last']]),
 ('private-timeout', network, 'return Poll::Ready(Exchange::Timeout {\n                before_abort: aborted && timeout_before_parent,\n            });', 'return Poll::Ready(Exchange::Aborted);',
  [cell(p+'/'+phase+'/idle/'+order) for p in ['mcp_get','mcp_post','hooks'] for phase in ['headers','redirect-hop'] for order in ['continuation-first','tie-parent-last']]),
 ('private-parent', network, 'if aborted {\n            return Poll::Ready(Exchange::Aborted);\n        }\n        if !response_first', 'if aborted {\n            return Poll::Ready(Exchange::Timeout { before_abort: false });\n        }\n        if !response_first',
  [cell(p+'/'+phase+'/'+event+'/parent-first') for p in ['mcp_get','mcp_post','hooks'] for phase in ['headers','redirect-hop'] for event in ['fulfilled','rejected','idle']]),
 ('private-io-parent', network, '        if aborted && parent_at.is_none() {\n            return Poll::Ready(Exchange::Aborted);', '        if aborted && parent_at.is_none() {\n            return Poll::Ready(Exchange::Timeout { before_abort: false });',
  [cell(p+'/io-headers/'+event+'/'+order) for p in ['mcp_get','mcp_post','hooks'] for event in ['fulfilled','rejected','idle'] for order in ['parent-first','tie-parent-first','tie-parent-last']]),
 ('private-retry-tie', network, 'timer_precedes_parent(next, at, false)', 'timer_precedes_parent(next, at, true)',
  [guard(M, 'mcp_get/retry-error/fulfilled/tie-parent-first')]),
 ('mcp-get-retry', mcp, 'let retryable_method = retries && method == Method::GET;', 'let retryable_method = false && retries && method == Method::GET;',
  [cell('mcp_get/retry-error/fulfilled/continuation-first', 'retry starts pending')]),
 ('mcp-post-retry', mcp, 'let retryable_method = retries && method == Method::GET;', 'let retryable_method = retries && method == Method::POST;',
  [cell('mcp_post/headers/rejected/continuation-first')]),
 ('mcp-retry-message', mcp, '"Network retry aborted.".into(),', '"wrong retry message".into(),',
  [cell('mcp_get/retry-'+event+'/fulfilled/parent-first') for event in ['status','error']]),
 ('mcp-error-name', mcp, 'McpClientError::with_code("NetworkFetchError", McpErrorCode::Text(code), message)', 'McpClientError::with_code("Error", McpErrorCode::Text(code), message)',
  [cell('mcp_post/headers/rejected/continuation-first')]),
 ('mcp-jitter-501', mcp, 'Duration::from_millis(500 + jitter)', 'Duration::from_millis(501 + jitter * 0)',
  [cell('mcp_get/retry-error/fulfilled/continuation-first', 'bounded retry sampling exhausted without a real 500ms candidate')]),
 ('hook-failure-shape', hooks, 'if let Some(code) = self.exit_code {', 'if let Some(code) = self.exit_code.or(Some(0)) {',
  [cell('hooks/headers/rejected/continuation-first')]),
 ('hook-failure-outcome', hooks, 'outcome: if cancelled {', 'outcome: if true || cancelled {',
  [cell('hooks/body/fulfilled/parent-first')]),
 ('hook-body-abort', hooks, 'JsString::from("This operation was aborted")\n            }\n        })?;', 'JsString::from("terminated")\n            }\n        })?;',
  [cell('hooks/body/fulfilled/parent-first')]),
 ('fixture-redirect-target', fixture, 'response = response.header("location", "/next");', 'response = response.header("location", "/wrong");',
  [cell(p+'/redirect-hop/fulfilled/continuation-first', 'assertion `left == right` failed: private request trace: "'+p+'/redirect-hop/fulfilled/continuation-first"') for p in ['mcp_get','mcp_post','hooks']]),
 ('fixture-route-boundary', fixture, 'let target = request.uri().path().to_owned();', 'let target = "/wrong".to_owned();',
  [cell('hooks/headers/fulfilled/continuation-first', 'assertion `left == right` failed: private request trace: "hooks/headers/fulfilled/continuation-first"')]),
 ('fixture-body-content', fixture, 'Bytes::from_static(b"FIRST")', 'Bytes::from_static(b"WRONG")',
  [cell(p+'/body/fulfilled/continuation-first') for p in ['mcp_get','mcp_post','hooks']]),
]

def catalog():
    data = []
    for name, path, old, new, tests in cases:
        assert (root/path).read_text().count(old) == 1, (name, 'mutation anchor must be unique')
        data.append(dict(name=name, path=path, tests=[dict(target=t, name=n, assertionAnchor=a) for t,n,a in tests]))
    return data

if sys.argv[1:] == ['--catalog']:
    print(json.dumps(catalog(), indent=2)); sys.exit(0)
assert os.environ.get('CARGO_TARGET_DIR'), 'caller must set CARGO_TARGET_DIR'
env = os.environ.copy()
out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=False)
selected = set(sys.argv[2:])
if selected:
    cases = [c for c in cases if c[0] in selected]
    assert len(cases) == len(selected), 'unknown control'
catalog()
def save(name, text):
    with (out/name).open('x') as stream: stream.write(text)
def run(name, tests):
    # One bounded whole private suite gives per-cell output while all failures
    # must still include the declared test+anchor pairs below.
    slots = wait_for_slot(root/'scripts/cargo-slots.sh', timeout=900)
    save(name+'.slots.log', slots.stdout+slots.stderr)
    command = ['cargo','test','--offline','--manifest-path','src/Cargo.toml','-p','pilotdeck-gateway','--lib','network_timing_tests','--','--nocapture','--test-threads=1']
    result = run_owned(command, cwd=root, env=env, timeout=300)
    log = result.stdout+result.stderr
    save(name+'.log', 'command: '+repr(command)+'\n'+log+'\nexit: '+str(result.returncode)+'\n')
    return result.returncode, log
summary=[]
for name, relative, old, new, tests in cases:
    path=root/relative
    code,log=run(name+'-positive',tests)
    assert code==0 and '0 failed;' in log, (name,'positive',code)
    with mutating_sources(root,[path]) as originals:
        source=originals[path].decode()
        assert source.count(old)==1,(name,'unique anchor')
        changed=source.replace(old,new)
        save(name+'.patch',''.join(difflib.unified_diff(source.splitlines(True),changed.splitlines(True),fromfile=relative,tofile=relative)))
        path.write_text(changed)
        code,log=run(name+'-negative',tests)
        assert code==101 and 'test result: FAILED.' in log,(name,'must fail during test execution',code)
        for target,test,anchor in tests:
            pattern=r"thread '"+re.escape(test)+r"'[^\n]*\n[^\n]*"+re.escape(anchor)
            assert re.search(pattern,log),(name,'bound test/anchor did not fail',test,anchor)
        killed=[]
        for line in log.splitlines():
            if '{"id":' in line:
                row=json.loads(line[line.index('{"id":'):])
                if row['ts']!=row['rust']:killed.append(row['id'])
    code,log=run(name+'-restored',tests)
    assert code==0 and '0 failed;' in log,(name,'restored',code)
    summary.append(dict(name=name,source=relative,sha256=hashlib.sha256(source.encode()).hexdigest(),tests=tests,killedCells=sorted(set(killed)),restored=True))
    print(name+': bound failures and restored PASS',flush=True)
save('summary.json',json.dumps(summary,indent=2)+'\n')
