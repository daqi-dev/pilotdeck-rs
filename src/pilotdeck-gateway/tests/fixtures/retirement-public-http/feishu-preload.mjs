import assert from 'node:assert/strict';
const origin=process.env.PDRS_FEISHU_MOCK_ORIGIN;const u=new URL(origin);assert.equal(u.protocol,'http:');assert.equal(u.hostname,'127.0.0.1');assert(Number(u.port)>0);assert.equal(u.pathname,'/');
const original=globalThis.fetch;
globalThis.fetch=async(input,init)=>{const url=new URL(typeof input==='string'||input instanceof URL?input:input.url);if(url.origin==='https://open.feishu.cn')return original(new URL(url.pathname+url.search,u),init);return original(input,init);};
