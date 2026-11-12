// Evidence selectors only: these never construct or modify production messages.
import assert from 'node:assert/strict';

function inputText(message){
 if(typeof message.content==='string')return message.content;
 if(Array.isArray(message.content))return message.content.filter(block=>block.type==='text').map(block=>block.text??'').join('\n');
 return '';
}

export function selectModelRoundInput(messages,marker){
 assert(Array.isArray(messages),'model request contains actual messages');
 const matches=messages.flatMap((message,index)=>message.role==='user'&&inputText(message).includes(marker)?[{index,message}]:[]);
 assert(matches.length<=1,'one current-round model user input');
 return matches[0]??null;
}

export function selectLatestModelUser(messages){
 assert(Array.isArray(messages),'model request contains actual messages');
 const index=messages.findLastIndex(message=>message.role==='user');
 return index<0?null:{index,message:messages[index]};
}

export function selectPersistedOutputTurns(messages,outputMarker,excludedTurnIds=[]){
 assert(Array.isArray(messages),'history response contains actual messages');
 const outputs=messages.flatMap((message,index)=>message.source==='history'&&message.role==='assistant'&&message.kind==='text'&&String(message.text??'').includes(outputMarker)?[{index,message}]:[]);
 const turnIds=[...new Set(outputs.map(({message})=>message.turnId))];
 for(const turnId of turnIds){assert.equal(typeof turnId,'string','durable output has a real turn id');assert(turnId.length);assert(!excludedTurnIds.includes(turnId),'current output cannot reuse a previous-round turn');}
 return turnIds.map(turnId=>{
  const indexed=messages.flatMap((message,index)=>message.source==='history'&&message.turnId===turnId?[{index,message}]:[]);
  const users=indexed.filter(({message})=>message.role==='user'&&message.kind==='text');
  return {turnId,userIndices:users.map(row=>row.index),outputIndices:outputs.filter(row=>row.message.turnId===turnId).map(row=>row.index),messageIndices:indexed.map(row=>row.index),users:users.map(row=>row.message)};
 });
}

export function selectPersistedRound(messages,inputMarker,outputMarker){
 assert(Array.isArray(messages),'history response contains actual messages');
 const anchors=messages.flatMap((message,index)=>message.source==='history'&&message.role==='user'&&message.kind==='text'&&String(message.text??'').includes(inputMarker)?[{index,message}]:[]);
 if(!anchors.length)return null;
 const turnIds=new Set(anchors.map(({message})=>message.turnId));
 assert.equal(turnIds.size,1,'current input belongs to one persisted turn');
 const turnId=anchors[0].message.turnId;assert.equal(typeof turnId,'string','durable input has a real turn id');assert(turnId.length,'durable turn id is nonempty');
 const indexed=messages.flatMap((message,index)=>message.source==='history'&&message.turnId===turnId?[{index,message}]:[]);
 const outputs=indexed.filter(({message})=>message.role==='assistant'&&message.kind==='text'&&String(message.text??'').includes(outputMarker));
 if(!outputs.length)return null;
 const users=indexed.filter(({message})=>message.role==='user'&&message.kind==='text');
 return {turnId,anchorIndices:anchors.map(row=>row.index),userIndices:users.map(row=>row.index),outputIndices:outputs.map(row=>row.index),messageIndices:indexed.map(row=>row.index),users:users.map(row=>row.message)};
}
