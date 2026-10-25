// PDRS_BASELINE=<cc7c4e6> node --import <baseline>/node_modules/tsx/dist/loader.mjs probe.mts
import assert from 'node:assert/strict';
import {createHash} from 'node:crypto';
import {readFileSync} from 'node:fs';
import {resolve, join} from 'node:path';
import {pathToFileURL} from 'node:url';
import {bindSourceTree} from '../../baseline_source_guard.mjs';
assert.ok(process.env.PDRS_BASELINE);
const root = resolve(process.env.PDRS_BASELINE);
const commit = 'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85';
bindSourceTree(root, commit, '/usr/bin/git');
const sources = ['src/agent/session/AgentSession.ts','src/agent/session/SteerMailbox.ts'];
const [{AgentSession}, {SteerMailbox}] = await Promise.all(sources.map(p=>import(pathToFileURL(join(root,p)).href)));
const message = {role:'user',content:[],metadata:{queueItemId:'a'}};
const item = {itemId:'a',message,allowedReadFiles:['/safe']};
const mutations = [], trace = [];
const runner = {
  sessionEventRecorder:{recordInboxMutation:async(s,t,m)=>{mutations.push({turn:t,...m});}},
  async *run(p) {
    p.openSteerMailbox();
    trace.push(await session.steer({turnId:'t',...item}));
    trace.push(await session.steer({turnId:'t',...item}));
    trace.push(await session.cancelSteer({turnId:'t',itemId:'b'}));
    trace.push(await session.steer({turnId:'t',itemId:'b',message}));
    trace.push(p.drainSteerMessages());
    await p.claimSteerMessage('a');
    p.ackSteerMessage('a');
    trace.push(p.drainOrCloseSteerMailbox());
    trace.push(await p.closeSteerMailbox(),await p.closeSteerMailbox());
    return {messages:[],result:{type:'success',usage:{},permissionDenials:[]}};
  },
};
const session = new AgentSession({sessionId:'s',turnRunner:runner});
for await (const _ of session.submit({type:'text',text:'start'},{turnId:'t'})) {}
assert.equal(trace[0].accepted, true);
const afterFinish = await session.steer({turnId:'t',...item});
// Actual baseline race, reported separately: never a Rust expected result.
let release, entered;
const gate = new Promise(r=>release=r), started = new Promise(r=>entered=r);
const mailbox = new SteerMailbox({recordMutation:async()=>{entered();await gate;}});
mailbox.start('same');
const old = mailbox.enqueue('same',item);
await started;
mailbox.finish('same'); mailbox.start('same'); release();
const difference = {accepted:await old,leaked:mailbox.drain('same')};
console.log(JSON.stringify({commit,sourceSha256:Object.fromEntries(sources.map(p=>[p,createHash('sha256').update(readFileSync(join(root,p))).digest('hex')])),trace,mutations,afterFinish,difference}));
