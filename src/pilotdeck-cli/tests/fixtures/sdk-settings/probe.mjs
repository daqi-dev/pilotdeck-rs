// Supplemental real CLI observations. This is not the D0 sandbox apparatus.
import assert from 'node:assert/strict';
import { createRequire } from 'node:module';
import { mkdirSync, writeFileSync, readFileSync, readdirSync, lstatSync, chmodSync, rmSync } from 'node:fs';
import { resolve, join } from 'node:path';
import { createHash, randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import { once } from 'node:events';

assert.equal(process.version, 'v22.22.0', '#1029 probe Node version');
assert.ok(process.env.PDRS_BASELINE, '#1029 set PDRS_BASELINE');
assert.ok(process.argv.length >= 3, '#1029 probe OUTPUT [cell-id ...]');
const baseline = resolve(process.env.PDRS_BASELINE);
const output = resolve(process.argv[2]);
mkdirSync(output, {recursive:true});
const require = createRequire(join(baseline, 'package.json'));
const { WebSocketServer } = require('ws');
const token = 'd1-fixture-not-a-secret';
const sha = bytes => createHash('sha256').update(bytes).digest('hex');
const save = (name, value) => writeFileSync(join(output, name), JSON.stringify(value,null,2)+'\n', {flag:'wx'});
const definitions = [
  {
    "axis": "get-complete",
    "argv": [
      "settings",
      "get"
    ],
    "result": {
      "settings": {
        "language": "\u4e2d\u6587",
        "nested": [
          {},
          [],
          null,
          false
        ]
      },
      "source": "fixture",
      "extra": {
        "10": 10,
        "2": 2,
        "z": 0
      }
    }
  },
  {
    "axis": "get-null",
    "argv": [
      "settings",
      "get"
    ],
    "result": null
  },
  {
    "axis": "get-array",
    "argv": [
      "settings",
      "get"
    ],
    "result": [
      1,
      "x"
    ]
  },
  {
    "axis": "get-false",
    "argv": [
      "settings",
      "get"
    ],
    "result": false
  },
  {
    "axis": "get-zero",
    "argv": [
      "settings",
      "get"
    ],
    "result": 0
  },
  {
    "axis": "get-string",
    "argv": [
      "settings",
      "get"
    ],
    "result": ""
  },
  {
    "axis": "get-missing",
    "argv": [
      "settings",
      "get"
    ],
    "missingResult": true
  },
  {
    "axis": "get-surrogate",
    "argv": [
      "settings",
      "get"
    ],
    "result": {
      "z": "\ud800",
      "a": 1e+21,
      "b": 1e-07
    }
  },
  {
    "axis": "set-complete",
    "argv": [
      "settings",
      "set",
      "{\"language\":\"zh-CN\",\"nested\":{\"a\":null,\"b\":[{},[],false,3.5]},\"unknown\":\"keep\",\"__proto__\":{\"x\":1}}"
    ],
    "result": {
      "updated": false,
      "extra": "ignored"
    }
  },
  {
    "axis": "set-js-numbers",
    "argv": [
      "settings",
      "set",
      "{\"large\":9007199254740993,\"overflow\":1e400,\"negativeZero\":-0,\"tiny\":1e-400}"
    ],
    "result": null
  },
  {
    "axis": "set-surrogate",
    "argv": [
      "settings",
      "set",
      "{\"s\":\"\\ud800\",\"\\udfff\":\"key\"}"
    ],
    "result": null
  },
  {
    "axis": "set-duplicate-key",
    "argv": [
      "settings",
      "set",
      "{\"z\":1,\"a\":2,\"z\":3,\"10\":10,\"2\":2}"
    ],
    "result": {}
  },
  {
    "axis": "set-empty-argument",
    "argv": [
      "settings",
      "set",
      ""
    ],
    "result": null
  },
  {
    "axis": "set-whitespace",
    "argv": [
      "settings",
      "set",
      "  "
    ],
    "result": null
  },
  {
    "axis": "set-empty-json-flag",
    "argv": [
      "settings",
      "set",
      "--json",
      ""
    ],
    "result": null
  },
  {
    "axis": "set-json-flag-followed-option",
    "argv": [
      "settings",
      "set",
      "--json",
      "--stream-json"
    ],
    "result": null
  },
  {
    "axis": "set-json-flag-first-empty",
    "argv": [
      "settings",
      "set",
      "--json",
      "",
      "--json",
      "{}"
    ],
    "result": null
  },
  {
    "axis": "set-json-flag-first-missing",
    "argv": [
      "settings",
      "set",
      "--json",
      "--json",
      "{}"
    ],
    "result": null
  },
  {
    "axis": "set-json-flag-invalid-wins",
    "argv": [
      "settings",
      "set",
      "{}",
      "--json",
      "{"
    ],
    "result": null
  },
  {
    "axis": "set-split-object",
    "argv": [
      "settings",
      "set",
      "{",
      "\"language\":",
      "\"en\"",
      "}"
    ],
    "result": null
  },
  {
    "axis": "set-stdin-no-argv",
    "argv": [
      "settings",
      "set"
    ],
    "result": null,
    "stdinHex": "7b226f6e65223a317d0a7b2274776f223a327d0a"
  },
  {
    "axis": "set-null-empty-url",
    "argv": [
      "settings",
      "set",
      "null"
    ],
    "result": null,
    "emptyUrl": true
  },
  {
    "axis": "set-array-empty-url",
    "argv": [
      "settings",
      "set",
      "[]"
    ],
    "result": null,
    "emptyUrl": true
  },
  {
    "axis": "set-scalar-empty-url",
    "argv": [
      "settings",
      "set",
      "42"
    ],
    "result": null,
    "emptyUrl": true
  },
  {
    "axis": "set-syntax-empty-url",
    "argv": [
      "settings",
      "set",
      "{"
    ],
    "result": null,
    "emptyUrl": true
  },
  {
    "axis": "set-object-empty-url",
    "argv": [
      "settings",
      "set",
      "{}"
    ],
    "result": null,
    "emptyUrl": true
  },
  {
    "axis": "set-null-no-token",
    "argv": [
      "settings",
      "set",
      "null"
    ],
    "result": null,
    "auth": false
  },
  {
    "axis": "set-syntax-no-token",
    "argv": [
      "settings",
      "set",
      "{"
    ],
    "result": null,
    "auth": false
  },
  {
    "axis": "set-empty-no-token",
    "argv": [
      "settings",
      "set",
      ""
    ],
    "result": null,
    "auth": false
  },
  {
    "axis": "empty-leaf",
    "argv": [
      "settings",
      ""
    ],
    "result": null
  },
  {
    "id": "settings-unknown--argv",
    "axis": "argv",
    "argv": [
      "settings",
      "bogus"
    ],
    "result": null
  },
  {
    "id": "settings-unknown--signal-sigint",
    "axis": "signal-sigint",
    "argv": [
      "settings",
      "bogus"
    ],
    "result": null
  },
  {
    "id": "settings-unknown--signal-sigterm",
    "axis": "signal-sigterm",
    "argv": [
      "settings",
      "bogus"
    ],
    "result": null
  }
];
definitions.push({axis:'set-deep-object',argv:['settings','set','{"nested":'+ '['.repeat(160)+'0'+']'.repeat(160)+'}'],result:{updated:true}});
definitions.push(
  {axis:'set-request-literal',argv:['settings','set','{"value":"<request>","id":"<request>","nested":{"id":"<request>"}}'],result:{id:'<request>',value:'<request>'}},
  {axis:'get-request-literal',argv:['settings','get'],result:{id:'<request>',value:'<request>',nested:{id:'<request>'}}},
);
const captures = [];
function snapshot(root) {
  const entries = {};
  function visit(path, name) {
    const stat = lstatSync(path);
    entries[name] = stat.isDirectory() ? {type:'directory',mode:stat.mode&0o777} :
      {type:'file',mode:stat.mode&0o777,size:stat.size,sha256:sha(readFileSync(path))};
    if (stat.isDirectory()) for (const file of readdirSync(path).sort()) visit(join(path,file),name==='.'?file:`${name}/${file}`);
  }
  visit(root,'.');
  return entries;
}
for (const definition of definitions) {
  if (process.argv.length > 3 && !process.argv.slice(3).includes(definition.id ?? `settings-extra--${definition.axis}`)) continue;
  const {axis, argv, auth=true, emptyUrl=false, missingResult=false} = definition;
  const result = missingResult ? undefined : definition.result;
  const input = {id:definition.id ?? `settings-extra--${axis}`,leaf:'settings-extra',axis,
    argv,stdinHex:definition.stdinHex ?? '',env:auth?{PILOTDECK_SDK_AUTH_TOKEN:token}:{},
    mockGateway:true,acceptToken:token,mode:'supplemental',timeoutSeconds:12};
  if (definition.id?.startsWith('settings-unknown--signal-')) {
    input.signal = axis === 'signal-sigint' ? 'SIGINT' : 'SIGTERM';
    input.holdMethod = 'unreachable-for-local-branch';
    input.signalAt = 'request:unreachable-for-local-branch';
  }
  const runtime = join(output,`runtime-${randomUUID()}`);
  mkdirSync(runtime,{mode:0o700});
  for (const dir of ['home','pilot','project','tmp','bin','xdg-config','xdg-cache','xdg-data','xdg-state']) mkdirSync(join(runtime,dir),{mode:0o700});
  for (const [file,bytes] of [['pilot/pilotdeck.yaml',Buffer.from('schemaVersion: 1\n')],['project/keep.txt',Buffer.from([68,48,32,102,105,120,116,117,114,101,0,255,10])],['openssl.cnf',Buffer.alloc(0)]]) {
    writeFileSync(join(runtime,file),bytes,{flag:'wx',mode:0o600});
    chmodSync(join(runtime,file),0o600);
  }
  const env = {LANG:'C',LC_ALL:'C',TZ:'UTC',TERM:'dumb',NO_COLOR:'1',ANALYTICS_ENABLED:'0',DOCKER_CONTAINER:'1',
    COMMIT_HASH:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',...input.env};
  for (const [key,path] of Object.entries({HOME:'home',USERPROFILE:'home',CFFIXED_USER_HOME:'home',PILOT_HOME:'pilot',PILOTDECK_CONFIG_PATH:'pilot/pilotdeck.yaml',XDG_CONFIG_HOME:'xdg-config',XDG_CACHE_HOME:'xdg-cache',XDG_DATA_HOME:'xdg-data',XDG_STATE_HOME:'xdg-state',TMPDIR:'tmp',TMP:'tmp',TEMP:'tmp',PWD:'project',PATH:'bin',OPENSSL_CONF:'openssl.cnf'})) env[key]=join(runtime,path);
  const server = new WebSocketServer({host:'127.0.0.1',port:0});
  const wire = []; const closes = []; let connections=0;
  let child; let timer; let timedOut=false; let signalDelivered=false;
  try {
    await once(server,'listening');
    const port=server.address().port;
    Object.assign(env,{PILOTDECK_GATEWAY_HOST:'127.0.0.1',PILOTDECK_GATEWAY_PORT:String(port),PILOTDECK_SDK_GATEWAY_URL:`ws://127.0.0.1:${port}/ws`,PILOTDECK_GATEWAY_URL:`ws://127.0.0.1:${port}/ws`});
    server.on('connection',socket=>{
      connections++;
      socket.on('error',()=>{});
      socket.on('close',(code,reason)=>closes.push({code,reason:reason.toString()}));
      socket.on('message',bytes=>{
        const frame=JSON.parse(bytes.toString());
        if (input.signal && input.signalAt === `request:${frame.method}`) { signalDelivered=child.kill(input.signal); }
        wire.push({kind:'receive',frame,raw:bytes.toString()});
        const response=frame.type==='hello' ? {type:'hello_ok',protocolVersion:'1.2',serverVersion:'test',serverInfo:{}} : {type:'response',id:frame.id,ok:true,...(result===undefined?{}:{result})};
        const raw=JSON.stringify(response);
        wire.push({kind:'send',frame:response,raw});
        socket.send(raw);
      });
    });
    if (emptyUrl) { env.PILOTDECK_SDK_GATEWAY_URL=''; input.env.PILOTDECK_SDK_GATEWAY_URL=''; }
    const disk=snapshot(runtime);
    child=spawn(process.execPath,[join(baseline,'dist/src/cli/pilotdeck.js'),...input.argv],{cwd:join(runtime,'project'),env,stdio:['pipe','pipe','pipe']});
    const stdout=[]; const stderr=[];
    child.stdout.on('data',data=>stdout.push(data)); child.stderr.on('data',data=>stderr.push(data));
    timer=setTimeout(()=>{timedOut=true;child.kill('SIGKILL');},12000);
    child.stdin.end(Buffer.from(input.stdinHex,'hex'));
    const [exit,signal]=await once(child,'close');
    assert.equal(timedOut,false,`#1029 bounded live cell ${axis}`);
    assert.deepEqual(snapshot(runtime),disk,`#1029 live disk unchanged ${axis}`);
    const capture={status: input.signal && !signalDelivered ? 'TRIGGER_NOT_REACHED' : 'OBSERVED',signalDelivered,input,stdout:Buffer.concat(stdout).toString(),stderrRaw:Buffer.concat(stderr).toString(),exit,signal,connections,wire,close:closes,disk};
    captures.push(capture);
    console.log(JSON.stringify(capture));
  } finally {
    clearTimeout(timer);
    if (child && child.exitCode===null && child.signalCode===null) { child.kill('SIGKILL'); await once(child,'close'); }
    for (const socket of server.clients) socket.terminate();
    await new Promise(done=>server.close(done));
    rmSync(runtime,{recursive:true});
  }
}
save('supplemental.json',{nodeVersion:process.version,sourceSha256:Object.fromEntries(['src/cli/sdkCli.ts','packages/sdk/src/client.ts','dist/src/cli/pilotdeck.js'].map(file=>[file,sha(readFileSync(join(baseline,file)))])),cases:captures});
