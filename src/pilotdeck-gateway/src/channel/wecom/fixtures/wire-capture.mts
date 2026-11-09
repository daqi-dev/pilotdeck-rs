import {EventEmitter} from "node:events";
import {pathToFileURL} from "node:url";
import {join} from "node:path";
const baseline=process.env.PDRS_BASELINE;if(!baseline)throw Error("PDRS_BASELINE required");
const {WeComChannel}=await import(pathToFileURL(join(baseline,"src/adapters/channel/wecom/WeComChannel.ts")).href);
let socket;const sent=[],turns=[],logs=[];
class Fake extends EventEmitter {
 readyState=0;
 constructor(url){super();socket=this;queueMicrotask(()=>{this.readyState=1;this.emit("open")})}
 send(raw){const frame=JSON.parse(raw);sent.push(frame);queueMicrotask(()=>this.emit("message",Buffer.from(JSON.stringify({headers:frame.headers,body:{errcode:0}}))))}
 close(){this.readyState=3;this.emit("close")}
}
const gateway={async *submitTurn(input){turns.push(input);yield {type:"assistant_text_delta",text:"answer"};},};
const channel=new WeComChannel({botKey:"bot-fixture",extra:{botSecret:"secret-fixture",dmPolicy:"open",textBatchDelayMs:0},webSocketCtor:Fake,uuid:()=>"00000000-0000-4000-8000-000000000001"});
const handle=await channel.start({gateway,logger:{info:(...x)=>logs.push(x),warn:(...x)=>logs.push(x),error:(...x)=>logs.push(x)}});
const callback={cmd:"aibot_msg_callback",headers:{req_id:"r-1"},body:{msgid:"m-1",from:{userid:"alice"},chatid:"alice",chattype:"single",msgtype:"text",text:{content:"hello"}}};
socket.emit("message",Buffer.from(JSON.stringify(callback)));
await new Promise(r=>setTimeout(r,30));socket.emit("message",Buffer.from(JSON.stringify(callback)));await new Promise(r=>setTimeout(r,30));
await handle.stop("fixture");
console.log(JSON.stringify({baseline:"cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85",callback,sent,turns,logs},null,2));
