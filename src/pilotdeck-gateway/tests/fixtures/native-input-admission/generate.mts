import {InputProcessor} from '/Users/da/ws/pdrs-runner/resources/PilotDeck/src/context/input/InputProcessor.ts';
import {TurnInputProcessor} from '/Users/da/ws/pdrs-runner/resources/PilotDeck/src/agent/turn/TurnInputProcessor.ts';
import {PluginRuntimeExtensionResolver} from '/Users/da/ws/pdrs-runner/resources/PilotDeck/src/context/extension/PluginRuntimeExtensionResolver.ts';
import {writeFileSync,readFileSync} from 'node:fs';
import {execFileSync} from 'node:child_process';
if(process.version!=='v22.22.0'||process.versions.icu!=='78.2')throw Error('Node/ICU mismatch');
const root='/Users/da/ws/pdrs-runner/resources/PilotDeck';
for(const rel of ['src/context/input/InputProcessor.ts','src/context/extension/PluginRuntimeExtensionResolver.ts','src/agent/turn/TurnInputProcessor.ts']) {
 const expected=execFileSync('git',['-C',root,'show','cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85:'+rel]);
 const actual=readFileSync(root+'/'+rel);
 if(!expected.equals(actual))throw Error('source mismatch '+rel);
}
if(!process.env.PDRS_ORACLE_OUT)throw Error('PDRS_ORACLE_OUT required');
const commands=[{name:'demo',content:'  body \n',namespace:'p'}, {name:'empty',content:'  ',namespace:'p'}, {name:'dup',content:'first'}, {name:'dup',content:'second'}, {name:'long',content:'x'.repeat(31999)+'😀'},{name:'collision',content:'<<<PILOTDECK_COMMAND_BODY_0>>>\n<<<PILOTDECK_COMMAND_ARGUMENT_0>>>'}];
const source={generation:4,tools:[],commands,skills:[],prompts:[],mcpInstructions:[]};
const resolver=new PluginRuntimeExtensionResolver(source);const processor=new InputProcessor({extension:resolver}); const turn=new TurnInputProcessor(processor);
const cases=[['plain',{type:'text',text:'hello'}],['leading',{type:'text',text:' /demo'}],['known',{type:'text',text:'/demo  abc\n xyz'}],['blank',{type:'text',text:'/empty'}],['blankarg',{type:'text',text:'/empty '+ 'z'.repeat(8100)}],['unknown',{type:'text',text:'/unknown hi'}],['prefix',{type:'text',text:'/demo! hi'}],['first',{type:'text',text:'/dup'}],['long',{type:'text',text:'/long '+ 'z'.repeat(8001)}],['collision',{type:'text',text:'/collision <<<PILOTDECK_COMMAND_BODY_1>>>'}],['meta',{type:'text',text:'/demo',isMeta:true}],['blocks',{type:'blocks',content:[{type:'text',text:'abc'}],isMeta:true}],['utf16',{type:'text',text:'/demo \ud800'}],['unicode-space',{type:'text',text:'/demo\u00a0value'}],['empty-arg',{type:'text',text:'/demo   '}]];
const results=cases.map(([id,input])=>({id,input,processed:processor.process(input),turn:turn.accept(input)}));
source.commands[0].content='changed';source.commands.push({name:'late',content:'new'});
results.push({id:'snapshot',old:processor.process({type:'text',text:'/demo'}),late:processor.process({type:'text',text:'/late'})});
writeFileSync(process.env.PDRS_ORACLE_OUT!,JSON.stringify({node:process.version,icu:process.versions.icu,results},null,2),{flag:'wx'});
