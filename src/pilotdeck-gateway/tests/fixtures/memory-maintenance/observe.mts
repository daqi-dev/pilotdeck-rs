import assert from 'node:assert/strict';
export const names = ['idle', 'coalesce', 'settling', 'reject', 'reject_requested', 'generations', 'disposed', 'absent', 'resolver_error', 'never', 'cancelled', 'before_await'];
export async function observe(Controller:any, Service:any, Refresh:any, scratch:string, name:string) {
  const events:any[] = [], pending:any = {A:[], B:[]}; let current='A', fail=false, closed=false, hook=false;
  const error=(e:any)=>({name:e.name,message:e.message,...(e.code?{code:e.code}:{})});
  const real = name==='disposed' ? new Service({workspaceDir:scratch,rootDir:scratch+'/memory',logger:{}}) : undefined;
  const runtimes:any = Object.fromEntries(['A','B'].map(id=>[id,{projectRoot:'/project', memoryService:name==='absent'?undefined:{
    runDueScheduledMaintenance(reason:string) {events.push(['call',id,reason]);
      if(closed && id==='A') return real.runDueScheduledMaintenance(reason);
      if(name==='before_await') return Promise.resolve();
      return new Promise((resolve,reject)=>pending[id].push({resolve,reject}));
    },
  }}]));
  const c = new Controller({resolveRuntime(key:any){events.push(['resolve',key]);if(fail)throw new TypeError('resolve failed');return runtimes[current];},
    telemetry:{trackFeatureLoopStage(v:any){events.push(['success',v]);if(hook){hook=false;queueMicrotask(()=>{
      const state=c.states.get(runtimes.A);assert.equal(state.requested,false);assert.ok(state.inFlight);events.push(['settling']);schedule();
    });}},trackError(e:any,v:any){events.push(['error',error(e),v]);}},
    onDiagnostic(message:any,e:any){events.push(['diagnostic',message,error(e)]);}});
  const schedule=()=>{try{c.schedule('/project');}catch(e){events.push(['throw',error(e)]);}};
  // Flush only runnable reactions: never wait for the intentionally pending A.
  const pump=()=>new Promise<void>(resolve=>setImmediate(resolve));
  const settle=(id='A',kind='resolve',e?:any)=>{const p=pending[id].shift();assert.ok(p,'controlled predecessor exists');p[kind](e);};
  schedule();events.push(['returned']);
  if(name==='absent'){await pump();return events;}
  if(name==='resolver_error') {
    fail=true;schedule();const bundle=new Refresh({configStore:{subscribe(){return()=>{};}},projectRoot:'/project',memoryMaintenance:c});
    try{await bundle.afterTurnCompleted({sessionKey:'s'});}catch(e){events.push(['refresh_reject',error(e)]);}
    fail=false;settle();await pump();return events;
  }
  if(['coalesce','before_await','never','cancelled','reject_requested','disposed'].includes(name)) {schedule();schedule();}
  if(['generations','never'].includes(name)) {current='B';schedule();settle('B');await pump();if(name==='never')return events;}
  if(name==='before_await'){await pump();return events;}
  if(name==='disposed'){real.close();closed=true;events.push(['closed']);}
  if(name==='settling')hook=true;
  if(['reject','reject_requested','cancelled'].includes(name)) {const e=new TypeError('maintenance failed');if(name==='cancelled'){e.name='AbortError';e.message='cancelled';}settle('A','reject',e);} else settle();
  await pump();
  if(pending.A.length){settle();await pump();}
  assert.equal(pending.A.length,0);assert.equal(pending.B.length,0);
  return events;
}
