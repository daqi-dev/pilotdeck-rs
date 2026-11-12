// Fixed-TS transport seam. The actual CLI, adapters, SDKs, request bodies,
// authentication and WebSocket protocol are never replaced. Fixture services
// accept upstream paths verbatim; this preload changes only network origins.
import http from 'node:http';
import https from 'node:https';
import net from 'node:net';
import tls from 'node:tls';
import {appendFileSync} from 'node:fs';
import {syncBuiltinESMExports} from 'node:module';
const origin=new URL(process.env.PDRS_TEST_CHANNEL_LOOPBACK_ORIGIN);
if(origin.protocol!=='http:'||origin.hostname!=='127.0.0.1'||!origin.port||Number(origin.port)===0||origin.username||origin.password||origin.pathname!=='/'||origin.search||origin.hash)throw Error('strict loopback origin required');
const hosts={feishu:['open.feishu.cn','open.larksuite.com'],qq:['api.sgroup.qq.com','bots.qq.com'],telegram:['api.telegram.org'],discord:['discord.com','discordapp.com'],slack:['slack.com'],dingtalk:['api.dingtalk.com'],weixin:[],wecom:[]};
const channel=process.env.PDRS_TEST_CHANNEL_KIND??'feishu';
if(!Object.hasOwn(hosts,channel))throw Error('unknown loopback channel');
if(!process.env.PDRS_TEST_CHANNEL_REDIRECT_RECEIPT)throw Error('redirect receipt path required');
const allowed=new Set(hosts[channel]);
function redirect(url,transport){
 if(url.origin===origin.origin)return null;
 if(url.protocol!=='https:'||!allowed.has(url.hostname)||url.port||url.username||url.password)throw Error(`unregistered ${channel} fixture destination: ${url.origin}`);
 const target=new URL(origin);target.pathname=url.pathname;target.search=url.search;
 appendFileSync(process.env.PDRS_TEST_CHANNEL_REDIRECT_RECEIPT,JSON.stringify({transport,channel,upstream:url.origin,path:url.pathname,query:url.search,target:target.href})+'\n');return target;
}
const originalFetch=globalThis.fetch;
globalThis.fetch=function(input,init){const url=new URL(input instanceof Request?input.url:String(input)),target=redirect(url,'fetch');return target?(input instanceof Request?originalFetch(new Request(target,input),init):originalFetch(target,init)):originalFetch(input,init);};
const originalHttpsRequest=https.request.bind(https),originalHttpRequest=http.request.bind(http);
https.request=function(...args){
 let raw=args[0],options=args[1],callback=args[2],url;
 if(typeof raw==='string'||raw instanceof URL){url=new URL(raw);if(typeof options==='function'){callback=options;options={};}else options??={};}
 else{options=raw??{};callback=typeof args[1]==='function'?args[1]:undefined;url=new URL(`https://${options.hostname??options.host??'localhost'}`);}
 if(options.hostname||options.host){const authority=new URL('https://'+(options.hostname??options.host));url.hostname=authority.hostname;if(authority.port)url.port=authority.port;}
 if(options.port)url.port=String(options.port);
 if(options.protocol)url.protocol=options.protocol;
 if(options.path!==undefined){const at=options.path.indexOf('?');url.pathname=at<0?options.path:options.path.slice(0,at);url.search=at<0?'':options.path.slice(at);}
 const target=redirect(url,'https.request');if(!target)return originalHttpsRequest(...args);
 return originalHttpRequest(target,{...options,protocol:'http:',hostname:'127.0.0.1',host:undefined,port:Number(origin.port),path:target.pathname+target.search,agent:undefined,createConnection:undefined},callback);
};
https.get=function(...args){const request=https.request(...args);request.end();return request;};
// A dependency using another transport (for example its own undici dispatcher)
// must fail closed instead of escaping to a real account/service. No SDK method
// or dispatcher is mocked to make that dependency appear supported.
const originalConnect=net.Socket.prototype.connect;
net.Socket.prototype.connect=function(...args){
 const first=Array.isArray(args[0])?args[0][0]:args[0],options=first&&typeof first==='object'?first:null;
 const host=options?.host??options?.hostname??(typeof args[1]==='string'?args[1]:'localhost');
 if(options?.path||typeof first==='string'&&!/^\d+$/.test(first)||!['127.0.0.1','localhost','::1'].includes(host))throw Error(`unregistered ${channel} fixture socket destination`);
 return originalConnect.apply(this,args);
};
tls.connect=function(){throw Error(`unsupported ${channel} fixture TLS transport; real SDK loopback support required`);};
syncBuiltinESMExports();
