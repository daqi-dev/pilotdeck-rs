// Test-only internal records. These records never enter client WebSocket frames.
import assert from 'node:assert/strict';
export class CommitWriter {
  constructor(connection,generation,native) {this.connection=connection;this.generation=generation;this.native=native;this.next=1;this.pending=new Map();this.seen=new Set();this.used=new Set();this.wire=[];this.closed=false;}
  accept(record) {
    assert.equal(record.connection,this.connection,'connection mismatch');assert.equal(record.generation,this.generation,'generation mismatch');
    assert.ok(Number.isSafeInteger(record.seq)&&record.seq>=this.next&&!this.seen.has(record.seq),'duplicate/invalid sequence');
    assert.ok(['HostFrame','NativeSlot'].includes(record.kind),'record kind');
    this.seen.add(record.seq);this.pending.set(record.seq,record);
    while(this.pending.has(this.next)) {
      const item=this.pending.get(this.next);this.pending.delete(this.next++);
      if(item.kind==='NativeSlot') {
        assert.ok(!this.closed,'local after close');assert.ok(this.native.has(item.token),'unknown token');assert.ok(!this.used.has(item.token),'duplicate token');
        this.used.add(item.token);this.wire.push(this.native.get(item.token));
      } else {
        assert.match(item.wireHex,/^(?:[a-f0-9]{2})+$/,'invalid wire');this.wire.push(item.wireHex);
        if((parseInt(item.wireHex.slice(0,2),16)&15)===8)this.closed=true;
      }
    }
  }
  finalize(count) {assert.equal(this.next,count+1,'missing commit');assert.equal(this.pending.size,0,'sequence gap');return this.wire;}
}
export class CommitOwner {
  constructor(connection,generation,deliver,mark) {this.connection=connection;this.generation=generation;this.deliver=deliver;this.mark=mark;this.seq=0;this.closed=false;this.records=[];}
  emit(kind,fields) {
    assert.ok(kind!=='NativeSlot'||!this.closed,'local after close');
    const record={connection:this.connection,generation:this.generation,seq:++this.seq,kind,...fields};this.records.push(record);this.mark('commit',record);this.deliver(record);
    if(kind==='HostFrame'&&(parseInt(fields.wireHex.slice(0,2),16)&15)===8)this.closed=true;
  }
  // One fulfilled-Promise await continuation, same as the measured no-await async
  // supportedAgents result consumed by GatewayWsConnection.handleRequest.
  async admit({token,readinessClass}) {
    assert.equal(readinessClass,'ReadyAtAdmission');assert.equal(typeof token,'string');
    this.mark('opaque_admission',{token,readinessClass});
    await Promise.resolve();
    if(this.closed){this.mark('slot_suppressed_after_close',{token});return;}
    this.emit('NativeSlot',{token});
  }
}
export function serverText(json) {
  const payload=Buffer.from(JSON.stringify(json));assert.ok(payload.length<=65535);
  const header=payload.length<126?Buffer.from([129,payload.length]):Buffer.from([129,126,payload.length>>8,payload.length&255]);
  return Buffer.concat([header,payload]).toString('hex');
}
