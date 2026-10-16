import assert from 'node:assert/strict';
export const matrix = [
  {name:'describe-supported', frames:['describe_server','supported_agents']},
  {name:'list-supported', frames:['list_projects','supported_agents']},
  {name:'supported-list', frames:['supported_agents','list_projects']},
  {name:'list-ping', frames:['list_projects','ping']},
  {name:'list-close', frames:['list_projects','close']},
  {name:'list-response-then-ping', frames:['list_projects','ping'], causal:true},
];
export function frame(payload, opcode=1) {
  const bytes=Buffer.from(payload); assert.ok(bytes.length<=65535);
  const header=Buffer.alloc(8); header[0]=0x80|opcode; header[1]=0x80|126;
  header.writeUInt16BE(bytes.length,2); // four-byte all-zero mask, owned fixture
  return Buffer.concat([header,bytes]);
}
export function decode(bytes) {
  assert.ok(bytes.length>=2); const opcode=bytes[0]&15;
  assert.equal(bytes[1]&128,0,'server frames must be unmasked');
  let size=bytes[1]&127, offset=2;
  if(size===126){size=bytes.readUInt16BE(2);offset=4;}
  if(size===127){size=Number(bytes.readBigUInt64BE(2));offset=10;}
  assert.equal(bytes.length,offset+size,'one complete frame per baseline socket.write');
  const payload=bytes.subarray(offset); return {opcode,payloadHex:payload.toString('hex'),json:opcode===1?JSON.parse(payload.toString()):null};
}
