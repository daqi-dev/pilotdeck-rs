import test from 'node:test';
import assert from 'node:assert/strict';
import {isolatedEnv,ports,backendPaths} from './coexist-support.mjs';
test('all service state is isolated and inherited deployment/recording settings are removed',()=>{
  const env=isolatedEnv({HOME:'/operator',PATH:'/bin',XDG_CONFIG_HOME:'/operator/config',
    TMPDIR:'/operator/tmp',PILOT_HOME:'/operator/pilot',DATABASE_PATH:'/operator/db',
    PILOTDECK_GATEWAY_URL:'ws://operator',EXP008_UPSTREAM:'http://operator',EXP010_RECORD_LOG:'/operator/log',
    REAL_COMPOSITION_URL:'http://operator',VITE_API_URL:'http://operator',PROBE_PORT:'3001'},'/run');
  assert.equal(env.PATH,'/bin');
  for(const name of ['HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_CACHE_HOME','XDG_STATE_HOME','XDG_RUNTIME_DIR','TMPDIR','TMP','TEMP','PILOT_HOME','DATABASE_PATH'])
    assert(env[name].startsWith('/run/'),name);
  for(const name of ['PILOTDECK_GATEWAY_URL','EXP008_UPSTREAM','EXP010_RECORD_LOG','REAL_COMPOSITION_URL','VITE_API_URL','PROBE_PORT'])
    assert.equal(env[name],undefined,name);
  assert.equal(env.PILOTDECK_SKIP_BROWSER_OPEN,'1');
  assert.equal(env.PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD,'1');
});
test('all reserved ports are high and unique',()=>{
  assert.equal(new Set(Object.values(ports)).size,4);
  assert(Object.values(ports).every(port=>port>=20000&&port<=65535));
});
test('Vite backend routing includes token queries and keeps segment boundaries',()=>{
  const routes=backendPaths.map(pattern=>new RegExp(pattern));
  for(const path of ['/ws?token=f1','/shell?token=f1','/plugin-ws/echo?token=f1','/api?token=f1','/api/auth/login','/__probe/sse'])
    assert(routes.some(route=>route.test(path)),path);
  for(const path of ['/wsx?token=f1','/shellx','/apix','/probex','/probe/leptos/'])
    assert(!routes.some(route=>route.test(path)),path);
});
