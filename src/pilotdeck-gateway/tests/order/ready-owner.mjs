// PREPARED_NOT_RUN: explicit envelopes, private pipe protocol, never client WS.
import assert from 'node:assert/strict';
import {CommitOwner} from './domain-commit.mjs';
export class ReadyOwner extends CommitOwner {
  constructor(...args){super(...args);this.slots=new Map();}
  immediate(item) {
    assert.equal(item.readinessClass,'ReadyAtAdmission');assert.ok(!this.slots.has(item.token),'duplicate immediate slot');
    this.slots.set(item.token,{accepted:true});this.mark('ReadyReceived',item);this.mark('AdmissionAccepted',{token:item.token,readinessClass:item.readinessClass});
    return super.admit(item);
  }
  register({token,readinessClass}) {
    assert.equal(readinessClass,'JoinConsumed');assert.ok(!this.slots.has(token),'duplicate slot');
    let release;const gate=new Promise(resolve=>release=resolve);
    this.slots.set(token,{release,accepted:false});
    this.mark('SlotRegistered',{token,readinessClass});
    // One actual gated await continuation. No extra microtask-count adjustment.
    void (async()=>{await gate;if(this.closed)return;this.emit('NativeSlot',{token});})();
  }
  receive({token,readinessClass}) {
    this.mark('ReadyReceived',{token,readinessClass});
    const slot=this.slots.get(token);
    if(!slot)throw Object.assign(Error('TOKEN_MISMATCH'),{code:'TOKEN_MISMATCH'});
    if(readinessClass!=='JoinConsumed')throw Object.assign(Error('R2_REQUIRED'),{code:'R2_REQUIRED'});
    assert.equal(slot.accepted,false,'duplicate acceptance');slot.accepted=true;
    this.mark('AdmissionAccepted',{token});slot.release();
    return {event:'Accepted',token};
  }
  cancel() {this.closed=true;for(const [token,slot]of this.slots)if(!slot.accepted){this.mark('PendingCancelledAfterClose',{token});slot.release();}}
}
