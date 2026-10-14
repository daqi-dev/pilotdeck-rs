import net from 'node:net';
import {mkdir, mkdtemp} from 'node:fs/promises';
import {resolve} from 'node:path';

export const ports = Object.freeze({mock:28370, proxy:28373, vite:28374, router:28375});
// Vite matches req.url, including query: /ws?token= must match the WS route.
export const backendPaths = Object.freeze(['^/api(?:[/?]|$)','^/ws(?:[/?]|$)','^/shell(?:[/?]|$)','^/plugin-ws/','^/__probe/']);
export function isolatedEnv(parent, directory) {
  const env=Object.fromEntries(Object.entries(parent).filter(([name])=>
    !/^(FORMAL_|REAL_|PILOTDECK_|REPLACEMENT_|FE_|PLAYWRIGHT_|EXP007_|EXP008_|EXP010_|PROBE_PORT$|PILOT_HOME$|DATABASE_PATH$|VITE_)/.test(name)));
  return {...env, HOME:resolve(directory,'home'), XDG_CONFIG_HOME:resolve(directory,'config'),
    XDG_DATA_HOME:resolve(directory,'data'), XDG_CACHE_HOME:resolve(directory,'cache'),
    XDG_STATE_HOME:resolve(directory,'state'), XDG_RUNTIME_DIR:resolve(directory,'runtime'),
    TMPDIR:resolve(directory,'tmp'), TMP:resolve(directory,'tmp'), TEMP:resolve(directory,'tmp'),
    PILOT_HOME:resolve(directory,'pilot-home'), DATABASE_PATH:resolve(directory,'database.sqlite'),
    PILOTDECK_SKIP_BROWSER_OPEN:'1', PLAYWRIGHT_SKIP_BROWSER_DOWNLOAD:'1'};
}
export async function makeIsolation(root, parent) {
  const base=resolve(root,'_tmp/exp007');await mkdir(base,{recursive:true});
  const directory=await mkdtemp(resolve(base,'coexist-'));
  const env=isolatedEnv(parent,directory);
  for(const name of ['HOME','XDG_CONFIG_HOME','XDG_DATA_HOME','XDG_CACHE_HOME','XDG_STATE_HOME','XDG_RUNTIME_DIR','TMPDIR','PILOT_HOME'])
    await mkdir(env[name],{recursive:true,mode:0o700});
  return {directory,env};
}
export async function assertPortsFree() {
  // Bind then release each exact loopback port before starting any service.
  // Services retain strictPort/bind failure as protection against races afterwards.
  for(const port of Object.values(ports))await new Promise((resolve,reject)=>{
    const server=net.createServer();server.once('error',reject);
    server.listen({host:'127.0.0.1',port,exclusive:true},()=>server.close(error=>error?reject(error):resolve()));
  });
}
