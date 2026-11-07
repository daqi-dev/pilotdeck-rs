// Run with the fixed baseline's tsx and Node 22.22; see README.md.
import { createHash } from 'node:crypto';
import { readFile, writeFile } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
import { execFileSync } from 'node:child_process';
const [baseline, output] = process.argv.slice(2);
if (!baseline || !output) throw new Error('usage: generate.mjs BASELINE OUTPUT');
if (process.version !== 'v22.22.0') throw new Error(`unexpected Node version: ${process.version}`);
const head = execFileSync('git', ['-C', baseline, 'rev-parse', 'HEAD'], {encoding:'utf8'}).trim();
if (head !== 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85') throw new Error(`unexpected TS checkout: ${head}`);
const file = resolve(baseline, 'src/adapters/channel/tui/app/types.ts');
const sha = createHash('sha256').update(await readFile(file)).digest('hex');
if (sha !== '85e8528dff0c6a3596415aebbfbb01039b3f90c3a2d64f781e1667fe332696cc') throw new Error(`unexpected TS source: ${sha}`);
const { applyGatewayEventToTuiState } = await import(pathToFileURL(file).href);
const initial = { messages: [{role:'user',text:'question'}], activity: [], mode:'default', isRunning:false, pendingPermissions:[] };
const events = [
  {type:'turn_started'},
  {type:'assistant_thinking_delta',text:'😀'}, {type:'assistant_thinking_delta',text:''},
  {type:'assistant_text_delta',text:'hello'}, {type:'assistant_text_delta',text:' world'},
  ...Array.from({length:10},(_,i)=>({type:'tool_call_started',toolCallId:`t${i}`,name:`tool${i}`})),
  {type:'permission_request',requestId:'p1',toolName:'bash',payload:{command:'git status'}},
  {type:'permission_request',requestId:'p2',toolName:'read_file',payload:{path:'a'}},
  {type:'tool_call_finished',toolCallId:'t9',toolName:'tool9',ok:true,resultPreview:'  done \n',resultLineCount:8,resultPath:'/fake/output'},
  {type:'tool_result_detail_available',toolCallId:'absent',fullText:'ignored'},
  {type:'tool_result_detail_available',toolCallId:'t9',resultPath:null,fullText:'details'},
  {type:'assistant_text_delta',text:'after tool'},
  {type:'structured_output',payload:{z:1,a:[true,{x:'a\nb'}]}},
  {type:'structured_output',payload:{}}, {type:'structured_output',payload:[]},
  {type:'structured_output',payload:undefined},
  {type:'assistant_text_delta',text:'\ud800'},
  {type:'structured_output',payload:{lone:'\ud800',empty:[],object:{}}},
  {type:'permission_request',requestId:'undefined-payload',toolName:'test',payload:undefined},
  {type:'plan_mode_changed',mode:'plan'},
  {type:'turn_completed'}, {type:'turn_started'},
  {type:'permission_request',requestId:'p3',toolName:'bash',payload:{}},
  {type:'error',message:'failure'}, {type:'future_event'},
];
let state = initial;
const states = events.map(event => { state = applyGatewayEventToTuiState(state,event); return state; });
await writeFile(output, JSON.stringify({sourceSha256:sha,initial,events,states},null,2)+'\n', {flag:'wx'});
