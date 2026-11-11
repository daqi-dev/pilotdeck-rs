// Ordinary same-input fixed-TS transport seam. It changes only the configured
// mock Feishu HTTPS origin; the real adapter, SDK, auth and protocol are intact.
import http from 'node:http';
import https from 'node:https';
import {appendFileSync} from 'node:fs';
import {syncBuiltinESMExports} from 'node:module';
const origin=new URL(process.env.PDRS_TEST_CHANNEL_LOOPBACK_ORIGIN);
if(origin.protocol!=='http:'||origin.hostname!=='127.0.0.1'||!origin.port||Number(origin.port)===0||origin.username||origin.password||origin.pathname!=='/'||origin.search||origin.hash)throw Error('strict loopback origin required');
const allowed=new Set(['open.feishu.cn','open.larksuite.com']);
function redirect(url,transport){if(url.protocol!=='https:'||!allowed.has(url.hostname)||url.port)return null;const target=new URL(url.pathname+url.search,origin);appendFileSync(process.env.PDRS_TEST_CHANNEL_REDIRECT_RECEIPT,JSON.stringify({transport,upstream:url.origin,path:url.pathname,query:url.search,target:target.href})+'\n');return target;}
const originalFetch=globalThis.fetch;
globalThis.fetch=function(input,init){const url=new URL(input instanceof Request?input.url:String(input)),target=redirect(url,'fetch');return target?(input instanceof Request?originalFetch(new Request(target,input),init):originalFetch(target,init)):originalFetch(input,init);};
const originalHttpsRequest=https.request.bind(https),originalHttpRequest=http.request.bind(http);
https.request=function(...args){let raw=args[0],options=args[1],callback=args[2],url;if(typeof raw==='string'||raw instanceof URL){url=new URL(raw);if(typeof options==='function'){callback=options;options={};}else options??={};}else{options=raw??{};callback=typeof args[1]==='function'?args[1]:undefined;url=new URL(`https://${options.hostname??options.host}${options.path??'/'}`);}const target=redirect(url,'https.request');if(!target)return originalHttpsRequest(...args);return originalHttpRequest(target,{...options,protocol:'http:',hostname:'127.0.0.1',host:undefined,port:Number(origin.port),path:target.pathname+target.search,agent:undefined},callback);};
https.get=function(...args){const request=https.request(...args);request.end();return request;};
syncBuiltinESMExports();
