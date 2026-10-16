// Test intervention only. OutcomeOwned is not natural settlement or Rust R2.
import assert from 'node:assert/strict';
import {sha} from './guard.mjs';

export function listAcceptanceGate(gateway, {mode, identity, mark}) {
  let calls=0, value, token, resolveGate, rejectGate, settled=false, accepted=false;
  let resolveOwned, rejectOwned;
  const owned=mode==='T0'?null:new Promise((resolve,reject)=>{resolveOwned=resolve;rejectOwned=reject;});
  const violations=[];
  const proxy=new Proxy(gateway, {get(target,key) {
    const original=Reflect.get(target,key,target);
    if(key!=='listProjects')return typeof original==='function'?original.bind(target):original;
    return (...args)=>{
      assert.equal(++calls,1,'single real listProjects call');
      mark('RealMethodCall',{...identity,calls});
      const originalPromise=Reflect.apply(original,target,args);
      assert.ok(originalPromise&&typeof originalPromise.then==='function','real listProjects Promise required');
      if(mode==='T0') {
        mark('OriginalPromiseBypass',{...identity,returnedOriginalPromise:true,settlementObserverAdded:false});
        return originalPromise; // Deliberately no then/await/settlement observer.
      }
      const gated=new Promise((resolve,reject)=>{resolveGate=resolve;rejectGate=reject;});
      originalPromise.then(result=>{
        value=result;
        token=Object.freeze({...identity,resultSha256:sha(JSON.stringify(result))});
        mark('OutcomeOwned',{...token,outcome:result,intervention:true,naturalSettlement:false});
        if(mode==='N1') {
          mark('mutant_hit',{...token,mutation:'actual-early-resolve'});
          settled=true;resolveGate(value);
          mark('PrematureResolved',{...token,sameOwnedObject:true});
        }
        resolveOwned(token);
      },error=>{rejectOwned(error);settled=true;rejectGate(error);});
      mark('WrappedPromiseReturned',{...identity,returnedOriginalPromise:false});
      return gated;
    };
  }});
  function release(supplied) {
    mark('ReleaseSent',{supplied});
    mark('ReleaseReceived',{supplied});
    if(!token||Object.keys(token).some(key=>supplied[key]!==token[key])||Object.keys(supplied).length!==Object.keys(token).length) {
      const error=Object.assign(new Error('TOKEN_MISMATCH'),{code:'TOKEN_MISMATCH'});
      mark('ReleaseRejected',{code:error.code,expected:token??null,supplied,gateSettled:settled});
      throw error;
    }
    assert.ok(!accepted,'single admission');
    assert.ok(!settled||mode==='N1','only real early-resolve mutant may already be settled');
    accepted=true;
    mark('AdmissionAccepted',{...token,alreadySettled:settled});
    if(settled) {
      resolveGate(value); // Real idempotent Promise resolve; no new settlement.
      mark('GateResolveAttemptAfterPremature',{...token,sameOwnedObject:true});
    }else {
      settled=true;resolveGate(value); // Actual owned object, no fixture substitution.
      mark('GateResolved',{...token,sameOwnedObject:true});
    }
  }
  function observeH1(result) {
    if(mode==='T0')return;
    const actualSha256=sha(JSON.stringify(result));
    if(!accepted)violations.push('H1_BEFORE_ACCEPTANCE');
    if(!token||actualSha256!==token.resultSha256)violations.push('H1_RESULT_IDENTITY_MISMATCH');
    mark('AcceptanceAtH1',{...identity,actualSha256,accepted,violations:[...violations]});
  }
  return {gateway:proxy,owned,release,observeH1,violations,
    snapshot:()=>({calls,settled,accepted,token:token??null,outcome:value??null}),
    cleanup:()=>{if(mode!=='T0'&&!settled&&rejectGate){mark('PendingCancelledAfterClose',{...identity});settled=true;rejectGate(new Error('owned connection cleanup'));}},
  };
}

// Same causal assertion for healthy interventions and the actual early-resolve mutant.
export function assertAdmissionBeforeH1(events, requestId) {
  const sends=events.filter(e=>e.event==='SendTextEnter'&&e.requestId===requestId);
  const writes=events.filter(e=>e.event==='SocketWrite'&&e.json?.id===requestId);
  assert.equal(sends.length,1,'one actual target sendText');
  assert.equal(writes.length,1,'one actual target socket.write');
  assert.equal(writes[0].json.ok,true,'successful real target result');
  assert.ok(sends[0].eventIndex<writes[0].eventIndex,'original sendText delegates socket.write');
  const outcome=events.find(e=>e.event==='OutcomeOwned'&&e.requestId===requestId);
  assert.ok(outcome,'real owned outcome required even for early-resolve rejection');
  assert.equal(sha(JSON.stringify(writes[0].json.result)),outcome.resultSha256,'same real result before causal verdict');
  const admission=events.find(e=>e.event==='AdmissionAccepted'&&e.requestId===requestId);
  if(!admission||admission.eventIndex>sends[0].eventIndex) {
    throw Object.assign(new Error('H1_BEFORE_ACCEPTANCE'),{code:'H1_BEFORE_ACCEPTANCE'});
  }
  assert.equal(sha(JSON.stringify(writes[0].json.result)),admission.resultSha256,'owned result hash at actual H1');
}
