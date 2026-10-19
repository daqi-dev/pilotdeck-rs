import assert from 'node:assert/strict';
import {matrix,frame,decode} from './protocol.mjs';
assert.equal(matrix.length,6);assert.equal(new Set(matrix.map(x=>x.name)).size,6);
assert.equal(matrix.filter(x=>x.causal).length,1);
for(const size of [0,1,125,126,1000]){
  const bytes=frame('x'.repeat(size));assert.equal(bytes[1],254);assert.equal(bytes.readUInt16BE(2),size);assert.equal(bytes.length,size+8);
}
assert.throws(()=>frame('x'.repeat(65536)));
for(const opcode of [8,10])assert.equal(decode(Buffer.from([128|opcode,0])).opcode,opcode);
const raw=Buffer.from(JSON.stringify({type:'response',id:'owned',ok:true,result:{}}));
assert.equal(decode(Buffer.concat([Buffer.from([129,raw.length]),raw])).json.id,'owned');
assert.throws(()=>decode(Buffer.from([129,5,65])));
assert.throws(()=>decode(Buffer.from([129,128])));
console.log(JSON.stringify({status:'PREPARED',runtime:'NOT_RUN',groups:['matrix-cardinality','causal-cardinality','masked-frames','oversize-rejection','server-decode','malformed-rejection'],matrix:matrix.map(x=>x.name)}));
