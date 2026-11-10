// Product fixture only: deny non-loopback before Node opens a transport.
import net from 'node:net';
import tls from 'node:tls';
import dns from 'node:dns';
import dgram from 'node:dgram';
import fs from 'node:fs';
import {syncBuiltinESMExports} from 'node:module';
import {resolve,dirname,sep} from 'node:path';
const logPath=process.env.PDRS_LOOPBACK_GUARD_LOG,credentialsPath=process.env.PDRS_FIXTURE_CREDENTIALS_PATH,origin=process.env.PDRS_FIXTURE_ORIGIN;
if(!logPath||!credentialsPath||!origin)throw Error('loopback guard requires explicit fixture receipt paths');
const append=fs.appendFileSync.bind(fs),read=fs.readFileSync.bind(fs);
function record(row){append(logPath,JSON.stringify({pid:process.pid,...row})+'\n');}
function loopback(host){return typeof host==='string'&&(host==='::1'||/^127(?:\.\d{1,3}){3}$/.test(host)&&host.split('.').every(n=>Number(n)<=255));}
function deny(operation,host){record({type:'denied',operation,host:String(host??'missing'),fixtureFailure:true});const error=Error('PDRS_NON_LOOPBACK_NETWORK: '+operation+' '+String(host));error.code='PDRS_NON_LOOPBACK_NETWORK';throw error;}
function check(host,operation){if(!loopback(host))deny(operation,host);}
function options(args){
 const a=args[0];
 if(Array.isArray(a))return options(a);
 if(a&&typeof a==='object')return a;
 if(typeof a==='number'||typeof a==='string'&&/^\d+$/.test(a))return {port:a,host:typeof args[1]==='string'?args[1]:undefined};
 return {path:a};
}
const socketConnect=net.Socket.prototype.connect;
net.Socket.prototype.connect=function(...args){
 const o=options(args);
 if(o.path){
  let allowed=false;
  try{const root=fs.realpathSync(process.env.TMPDIR),parent=fs.realpathSync(dirname(resolve(o.path)));allowed=parent===root||parent.startsWith(root+sep);}catch{}
  if(!allowed)deny('net.unix',o.path);
  record({type:'allowed-owned-ipc',path:resolve(o.path)});
  return socketConnect.apply(this,args);
 }
 check(o.host??o.hostname,'net.connect');return socketConnect.apply(this,args);
};
const tlsConnect=tls.connect;
tls.connect=function(...args){const o=options(args);check(o.host??o.hostname,'tls.connect');return tlsConnect.apply(this,args);};
if(globalThis.fetch){
 const fetch=globalThis.fetch.bind(globalThis);
 globalThis.fetch=async function(input,init){const url=new URL(typeof input==='string'||input instanceof URL?input:input.url);
  if(['data:','file:','blob:'].includes(url.protocol)){record({type:'local-resource',scheme:url.protocol});return fetch(input,init);}
  check(url.hostname.replace(/^\[|\]$/g,''),'fetch');return fetch(input,init);};
}
for(const name of ['lookup','resolve','resolve4','resolve6','resolveAny','resolveCaa','resolveCname','resolveMx','resolveNaptr','resolveNs','resolvePtr','resolveSoa','resolveSrv','resolveTxt','reverse']){
 if(typeof dns[name]==='function'){const original=dns[name];dns[name]=function(host,...args){check(host,'dns.'+name);return original.call(this,host,...args);};}
 if(typeof dns.promises[name]==='function'){const original=dns.promises[name];dns.promises[name]=async function(host,...args){check(host,'dns.promises.'+name);return original.call(this,host,...args);};}
}
for(const name of ['send','connect']){
 const original=dgram.Socket.prototype[name];
 dgram.Socket.prototype[name]=function(...args){deny('dgram.'+name,'UDP not used by this fixture');};
}
const configured=JSON.parse(read(credentialsPath,'utf8'));
check(new URL(configured.baseUrl).hostname,'fixture credentials origin');
if(configured.baseUrl!==origin||!configured.botToken||!configured.accountId)throw Error('fixture credentials do not match owned loopback');
fs.readFileSync=function(path,...args){
 const bytes=read(path,...args);
 if((typeof path==='string'||path instanceof URL)&&resolve(path instanceof URL?path.pathname:path)===resolve(credentialsPath)){
  const value=JSON.parse(String(bytes));
  const valid=value.baseUrl===origin&&!!value.botToken&&!!value.accountId;
  record({type:'credentials-read',path:resolve(credentialsPath),valid,baseUrl:value.baseUrl});
  if(!valid)throw Error('actual credentials read was not the owned mock');
 }
 return bytes;
};
syncBuiltinESMExports();
record({type:'ready',node:process.versions.node,icu:process.versions.icu,credentialsPath:resolve(credentialsPath),origin});
