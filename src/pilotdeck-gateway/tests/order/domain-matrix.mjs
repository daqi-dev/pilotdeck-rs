// Fixed input scripts. No expected order is derived from the native implementation.
export const cases = [
  {id:'D1-forward',gate:'D1',items:['describe_server','supported_agents'],split:'same'},
  {id:'D1-reverse',gate:'D1',items:['supported_agents','describe_server'],split:'same'},
];
for (const method of ['supported_agents','describe_server']) for (const control of ['ping','close']) for (const split of ['same','sync-data','next-turn'])
  cases.push({id:`D2-${method}-${control}-${split}`,gate:'D2',items:[method,control],split});
for (const method of ['unknown_probe_method','session_model_get']) for (const other of ['supported_agents','ping']) for (const reverse of [false,true])
  cases.push({id:`D3-${method}-${other}-${reverse?'reverse':'forward'}`,gate:'D3',items:reverse?[other,method]:[method,other],split:'same'});
for (const [id,items,split] of [
  ['list-supported',['list_projects','supported_agents'],'same'],['supported-list',['supported_agents','list_projects'],'same'],
  ['list-ping',['list_projects','ping'],'same'],['list-close',['list_projects','close'],'same'],
  ['list-response-ping',['list_projects','ping'],'response'],
]) cases.push({id:'D4-'+id,gate:'D4',items,split});
for (const intervention of ['host-pending','host-committed-channel-gate']) cases.push({id:'P1-'+intervention,gate:'P1',items:['describe_server','supported_agents'],split:intervention==='host-pending'?'sync-data':'response',intervention});
export const missing = [
  {id:'P2',status:'NOT_RUN',reason:'No causal marker for old Rust taskConsumable before subsequent admission; route return/COMPLETE samples do not establish that natural window. No new Cargo allowed.'},
  {id:'P3',status:'INDETERMINATE',reason:'No common causal clock mapping independent runtime LaterReady delivery before/during/after a host callback. Node-only cached results do not prove this mapping.'},
];

// Input frames are generated only by protocol.frame (16-bit length, zero mask).
export function fixtureMethod(hex) {
  const bytes=Buffer.from(hex,'hex');
  if((bytes[0]&15)!==1)return null;
  if(bytes[1]!==254||!bytes.subarray(4,8).equals(Buffer.alloc(4)))throw Error('unexpected fixture frame');
  return JSON.parse(bytes.subarray(8).toString()).method;
}
