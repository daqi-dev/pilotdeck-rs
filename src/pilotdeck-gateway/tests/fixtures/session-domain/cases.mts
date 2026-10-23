// Inputs only. All expected outcomes are obtained from the guarded TS baseline.
export const e = (type: string, extra: any = {}) => ({ type, turnId: 't', ...extra });
const start = e('turn_started');
const step = e('step_started', { step: 1 });
const model = e('model_request', { step: 1 });
const context = e('context_snapshot', { step: 1 });
const call = e('tool_call', { step: 1, call: { id: 'call' } });
const result = e('turn_result', { result: { usage: {}, permissionDenials: [] } });
const end = e('step_completed', { step: 1, outcome: 'completed' });
const mutation = (mutation: string, extra: any = {}) => e('inbox_mutation', { itemId: 'i', mutation, ...extra });
const insert = mutation('insert', { message: { role: 'user', content: [] } });
const claim = mutation('claim');
const queued = (itemId = 'q', turnId = 't') => e('agent_turn_enqueued', { itemId, turnId, input: { type: 'text', text: '' }, submitOptions: {} });
const permission = { operationId: 'p', step: 1, toolCallId: 'call', toolName: 'tool', mode: 'ask' };
const question = { operationId: 'q', questionCount: 1, toolCallId: 'call', toolName: 'tool' };
const todo = { id: 'a', content: 'work', status: 'pending' };
const written = e('plan_todo_written', { mode: 'structured', merge: true, todos: [todo] });
const goal = { id: 'g', objective: 'work', revision: 1, phase: 'active' };
export const cases: { name: string, entries: any[], code?: string }[] = [];
function test(name: string, entries: any[], code?: string) { cases.push({ name, entries, code }); }
function bad(name: string, prefix: any[], entry: any, code: string) { test(name, [...prefix, entry], code); }
test('complete-turn', [start, step, context, e('agent_instructions', { step: 1 }), model, e('model_stream_event', { step: 1 }), call, e('tool_result', { step: 1, result: { toolCallId: 'call' } }), end, result]);
test('legacy-result', [result]);
test('unknown-event', [e('unknown')]);
bad('duplicate-start', [start], start, 'duplicate_turn_start');
for (const type of ['step_started','step_completed','context_snapshot','agent_instructions','model_request','model_stream_event','tool_call','tool_result','compaction_started','compaction_completed','compaction_failed','question_started','question_completed','question_failed','permission_started','permission_completed','permission_failed','inbox_mutation','plan_todo_plan_changed','plan_todo_written','plan_todo_progressed','goal_changed']) {
 bad(`outside-${type}`, [], e(type), 'event_outside_turn');
 bad(`closed-${type}`, [start,result], e(type), 'event_outside_turn');
}
for (const n of [0,-1,1.5,9007199254740992,'1',null,undefined]) bad(`step-invalid-${String(n)}`, [start], e('step_started', { step:n }), 'invalid_step');
bad('step-not-increasing', [start,step,end], step, 'invalid_step');
bad('step-already-open', [start,step], e('step_started',{step:2}), 'step_already_open');
for (const type of ['context_snapshot','agent_instructions','model_request','model_stream_event','tool_call','tool_result','step_completed']) {
 bad(`step-missing-${type}`, [start], e(type,{step:1}), 'step_not_open');
 bad(`step-mismatch-${type}`, [start,step], e(type,{step:2}), 'step_not_open');
}
bad('context-duplicate', [start,step,context], context, 'duplicate_context_snapshot');
bad('context-after-request', [start,step,model], context, 'context_after_model_request');
bad('instructions-after-request', [start,step,model], e('agent_instructions',{step:1}), 'instructions_after_model_request');
bad('request-duplicate', [start,step,model], model, 'duplicate_model_request');
bad('stream-before-request', [start,step], e('model_stream_event',{step:1}), 'model_request_missing');
bad('call-before-request', [start,step], call, 'model_request_missing');
bad('call-duplicate', [start,step,model,call], call, 'duplicate_tool_call');
bad('result-without-call', [start,step], e('tool_result',{step:1,result:{toolCallId:'call'}}), 'tool_result_without_call');
bad('step-unresolved-call', [start,step,model,call], end, 'step_has_open_tool_calls');
test('failed-step-clears-calls', [start,step,model,call,e('step_completed',{step:1,outcome:'failed'}),result]);
bad('result-open-step', [start,step], result, 'turn_has_open_step');
bad('result-closed', [start,result], result, 'event_outside_turn');
for (const [family,meta] of [['compaction',{operationId:'c',messageCount:0}],['question',question],['permission',permission]] as const) {
 const prefix=family==='permission'?[start,step,model]:[start];
 const begin=e(`${family}_started`,meta);
 const completed=e(`${family}_completed`,{...meta,decision:'allow',reason:'user'});
 const failed=e(`${family}_failed`,{...meta,error:'failed'});
 const code=`invalid_${family}`;
 test(`${family}-completed`,[...prefix,begin,completed]);
 test(`${family}-failed`,[...prefix,begin,failed]);
 bad(`${family}-duplicate`,[...prefix,begin],begin,code);
 for (const terminal of [completed,failed]) bad(`${family}-${terminal.type}-unopened`,prefix,terminal,code);
 for (const phase of ['started','completed','failed']) {
  const valid=phase==='started'?begin:phase==='completed'?completed:failed;
  bad(`${family}-${phase}-empty-id`,prefix,{...valid,operationId:''},code);
  if (phase==='failed') for (const error of ['',[],{length:0},null,undefined]) bad(`${family}-${phase}-error-${JSON.stringify(error)}`,prefix,{...valid,error}, error==null?'TypeError':code);
 }
 const closing=family==='permission'?[end,result]:[result];
 test(`${family}-pending-before-result`,[...prefix,begin,...closing],`turn_has_pending_${family}`);
 if(family==='compaction') {
  for (const messageCount of [-1,1.5,'1',null,undefined]) for(const phase of ['started','completed']) bad(`compaction-${phase}-count-${String(messageCount)}`,prefix,e(`compaction_${phase}`,{...meta,messageCount}),code);
  for (const maxContextTokens of [0,-1,1.5,'1',null]) bad(`compaction-max-${String(maxContextTokens)}`,prefix,{...begin,maxContextTokens},code);
 } else if(family==='question') {
  for(const questionCount of [0,-1,1.5,'1',null,undefined]) for(const phase of ['started','completed']) bad(`question-${phase}-count-${String(questionCount)}`,prefix,e(`question_${phase}`,{...meta,questionCount}),code);
  for(const key of ['toolCallId','toolName']) bad(`question-missing-${key}`,prefix,{...begin,[key]:''},code);
 } else {
  for(const phase of ['started','completed','failed']) {
   const valid=phase==='started'?begin:phase==='completed'?completed:failed;
   for(const key of ['toolCallId','toolName','mode','step']) bad(`permission-${phase}-missing-${key}`,prefix,{...valid,[key]:undefined},code);
   for(const step of [0,-1,1.5,'1',null]) bad(`permission-${phase}-step-${String(step)}`,prefix,{...valid,step},code);
  }
  for(const key of ['decision','reason']) bad(`permission-completed-${key}`,prefix,{...completed,[key]:''},code);
  bad('permission-before-model',[start,step],begin,'model_request_missing');
  bad('permission-no-step',[start],begin,'step_not_open');
 }
}
for(const type of ['durable_message','assistant_message','tool_result_message']) {
 const message=e(type,{message:{role:'user',content:[],metadata:{queueItemId:'i'}}});
 test(`inbox-consume-${type}`,[start,insert,claim,message,result]);
 bad(`inbox-unclaimed-${type}`,[start,insert],message,'invalid_inbox_mutation');
 bad(`inbox-closed-${type}`,[start,result],message,'invalid_inbox_mutation');
 test(`inbox-legacy-${type}`,[message]);
 for(const value of [null,undefined]) bad(`message-${type}-${value}`,[],e(type,{message:value}),'TypeError');
}
test('inbox-cancel',[start,insert,mutation('cancel'),result]);
test('inbox-cancel-unknown',[start,mutation('cancel'),result]);
test('inbox-discard-pending',[start,insert,mutation('discard'),result]);
test('inbox-discard-claimed',[start,insert,claim,mutation('discard'),result]);
test('inbox-unknown-mutation',[start,mutation('unknown')]);
bad('inbox-empty-item',[start],mutation('insert',{itemId:''}),'invalid_inbox_mutation');
bad('inbox-no-message',[start],mutation('insert'),'invalid_inbox_mutation');
bad('inbox-double-insert',[start,insert],insert,'invalid_inbox_mutation');
bad('inbox-cancel-claimed',[start,insert,claim],mutation('cancel'),'invalid_inbox_mutation');
bad('inbox-claim-unknown',[start],claim,'invalid_inbox_mutation');
bad('inbox-claim-cancelled',[start,mutation('cancel')],claim,'invalid_inbox_mutation');
bad('inbox-double-claim',[start,insert,claim],claim,'invalid_inbox_mutation');
bad('inbox-discard-unknown',[start],mutation('discard'),'invalid_inbox_mutation');
bad('inbox-result-pending',[start,insert],result,'turn_has_pending_inbox');
bad('inbox-result-claimed',[start,insert,claim],result,'turn_has_pending_inbox');
test('queue-fifo',[queued(),queued('q2','t2'),e('turn_started',{inboxItemId:'q'}),e('turn_started',{turnId:'t2',inboxItemId:'q2'})]);
test('queue-discard',[queued(),e('agent_turn_discarded',{itemId:'q',reason:'cancelled'}),start]);
test('queue-block-input',[{...queued(),input:{type:'blocks',content:[]}}]);
for(const key of ['itemId','turnId','input','submitOptions']) bad(`queue-missing-${key}`,[],{...queued(),[key]:undefined},'invalid_inbox_mutation');
for(const input of [null,[],{type:'text'},{type:'text',text:4},{type:'blocks',content:{}}]) bad(`queue-input-${JSON.stringify(input)}`,[],{...queued(),input},'invalid_inbox_mutation');
bad('queue-options-array',[],{...queued(),submitOptions:[]},'invalid_inbox_mutation');
bad('queue-reused-item',[queued()],queued('q','t2'),'invalid_inbox_mutation');
bad('queue-reused-turn',[queued()],queued('q2'),'invalid_inbox_mutation');
bad('queue-active-turn',[start],queued(),'invalid_inbox_mutation');
bad('queue-forgot-claim',[queued()],start,'invalid_inbox_mutation');
bad('queue-claim-empty',[queued()],e('turn_started',{inboxItemId:''}),'invalid_inbox_mutation');
bad('queue-claim-wrong',[queued()],e('turn_started',{inboxItemId:'wrong'}),'invalid_inbox_mutation');
bad('queue-bypass',[queued(),queued('q2','t2')],e('turn_started',{turnId:'t2',inboxItemId:'q2'}),'invalid_inbox_mutation');
for(const extra of [{itemId:''},{itemId:'wrong'},{reason:'wrong'},{turnId:'wrong'}]) bad(`queue-discard-${JSON.stringify(extra)}`,[queued()],e('agent_turn_discarded',{itemId:'q',reason:'cancelled',...extra}),'invalid_inbox_mutation');
test('queue-dispose',[queued(),e('agent_turn_discarded',{itemId:'q',reason:'agent_disposed'})]);
test('queue-item-cannot-reuse-after-discard',[queued(),e('agent_turn_discarded',{itemId:'q',reason:'cancelled'}),queued()],'invalid_inbox_mutation');
test('plan-and-goal',[start,e('plan_todo_plan_changed',{plan:' next '}),written,e('plan_todo_progressed',{toolName:' tool '}),e('goal_changed',{revision:1,goal}),e('goal_changed',{revision:2,goal:null}),result]);
test('plan-null',[start,e('plan_todo_plan_changed',{plan:null})]);
for(const plan of ['', ' \uFEFF ',42,undefined]) bad(`plan-invalid-${String(plan)}`,[start],e('plan_todo_plan_changed',{plan}),'invalid_plan_todo');
for(const extra of [{mode:'other'},{merge:0},{markdown:null},{reason:null},{todos:null}]) bad(`todos-metadata-${JSON.stringify(extra)}`,[start],{...written,...extra},'invalid_plan_todo');
for(const extra of [{id:''},{content:' '},{status:'other'},{priority:4}]) bad(`todo-item-${JSON.stringify(extra)}`,[start],{...written,todos:[{...todo,...extra}]},'invalid_plan_todo');
bad('todo-duplicate',[start],{...written,todos:[todo,todo]},'invalid_plan_todo');
bad('todo-null',[start],{...written,todos:[null]},'TypeError');
for(const toolName of [' ',null,undefined,4]) bad(`plan-tool-${String(toolName)}`,[start],e('plan_todo_progressed',{toolName}),toolName===' '?'invalid_plan_todo':'TypeError');
for(const revision of [0,-1,1.5,'1',null,undefined]) bad(`goal-revision-${String(revision)}`,[start],e('goal_changed',{revision,goal:null}),'invalid_goal');
bad('goal-skip',[start],e('goal_changed',{revision:2,goal:null}),'invalid_goal');
for(const extra of [{id:''},{objective:' '},{revision:2},{phase:'other'},{blockedReason:3},{maxGoalRounds:0},{maxGoalRounds:1.5},{maxGoalRounds:'1'}]) bad(`goal-snapshot-${JSON.stringify(extra)}`,[start],e('goal_changed',{revision:1,goal:{...goal,...extra}}),'invalid_goal');
bad('goal-blocked-no-reason',[start],e('goal_changed',{revision:1,goal:{...goal,phase:'blocked',blockedReason:' '}}),'invalid_goal');
test('goal-blocked',[start,e('goal_changed',{revision:1,goal:{...goal,phase:'blocked',blockedReason:'wait'}})]);
bad('goal-missing',[start],e('goal_changed',{revision:1}),'TypeError');
for(const turnId of [42,0,false,null,undefined]) {
 test(`raw-turn-id-${String(turnId)}`,[e('turn_started',{turnId}),e('step_started',{turnId,step:1}),e('step_completed',{turnId,step:1}),{...result,turnId}]);
 bad(`raw-duplicate-turn-${String(turnId)}`,[e('turn_started',{turnId})],e('turn_started',{turnId}),'duplicate_turn_start');
}
for(const callValue of [null,undefined]) {
 bad(`null-call-${callValue}`,[start,step,model],e('tool_call',{step:1,call:callValue}),'TypeError');
 bad(`null-result-${callValue}`,[start,step],e('tool_result',{step:1,result:callValue}),'TypeError');
}
test('undefined-tool-call-id',[start,step,model,e('tool_call',{step:1,call:{}}),e('tool_result',{step:1,result:{}}),end,result]);
test('multiple-pending-questions',[start,e('question_started',question),e('question_started',{...question,operationId:'second'}),result],'turn_has_pending_question');
test('multi-turn-goal-revision',[start,e('goal_changed',{revision:1,goal:null}),result,e('turn_started',{turnId:'t2'}),e('goal_changed',{turnId:'t2',revision:2,goal:null})]);

test('null-entry',[null],'TypeError');
// Map.get returns undefined for absent keys; preserve TS's malformed-input behavior.
test('queue-discard-absent-undefined-turn',[e('agent_turn_discarded',{turnId:undefined,itemId:'absent',reason:'cancelled'})]);
bad('queue-claim-absent-undefined-turn',[],e('turn_started',{turnId:undefined,inboxItemId:'absent'}),'invalid_inbox_mutation');
bad('queue-claim-other-owner',[queued()],e('turn_started',{turnId:'other',inboxItemId:'q'}),'invalid_inbox_mutation');
