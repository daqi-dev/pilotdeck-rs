import assert from 'node:assert/strict';
import {mkdir,writeFile,readFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
import {createHash} from 'node:crypto';
const [baseline,out]=process.argv.slice(2);await mkdir(out,{recursive:false});
for(const key of ['HOME','PILOT_HOME','XDG_CONFIG_HOME','XDG_CACHE_HOME','XDG_DATA_HOME']){const dir=out+'/'+key.toLowerCase();await mkdir(dir);process.env[key]=dir;}
const load=(p:string)=>import(pathToFileURL(baseline+'/'+p).href);
const {PluginRuntime}=await load('src/extension/plugins/runtime/PluginRuntime.ts');
const {loadSkillFromPath}=await load('src/extension/plugins/loading/PluginLoader.ts');
const results:any[]=[];
for(const mode of ['builtin-only','same-key-disk','disabled-builtin-keeps-disk','project-wins']){
 const root=out+'/'+mode;await mkdir(root);const home=root+'/home',project=root+'/project',assets=root+'/assets',disk=root+'/disk';
 for(const p of [home,project,assets,disk])await mkdir(p);
 const skill=async(dir:string,name:string,body:string)=>{await mkdir(dir,{recursive:true});await writeFile(dir+'/SKILL.md',`---\nname: ${name}\ndescription: ${body} description\n---\n${body}\n`);};
 await skill(assets+'/browser-use','browser-use','BUILTIN CONTENT');
 const builtin=await loadSkillFromPath(assets+'/browser-use','builtin');
 if(mode!=='builtin-only'){await skill(disk+'/a-first','a-first','FIRST CONTENT');await skill(disk+'/browser-use','browser-use','DISK CONTENT');await skill(disk+'/z-last','z-last','LAST CONTENT');}
 if(mode==='project-wins'){await skill(home+'/skills/browser-use','browser-use','GLOBAL CONTENT');await skill(project+'/.pilotdeck/skills/browser-use','browser-use','PROJECT CONTENT');}
 const runtime=new PluginRuntime({projectRoot:project,pilotHome:home,builtinPlugins:[builtin],...(mode==='builtin-only'?{}:{builtinSkillsRoot:disk}),builtinPluginsEnabled:mode==='disabled-builtin-keeps-disk'?{'browser-use':false}:{}});
 try{await runtime.refresh();const skills=runtime.getAllSkills();const read:any={};for(const skill of skills)read[skill.name]=await runtime.loadSkillPrompt(skill.name);
 results.push({mode,plugins:runtime.snapshot().map((p:any)=>({name:p.name,source:p.source,path:p.path})),skills:skills.map((s:any)=>({name:s.name,description:s.description,path:s.path,content:s.content})),read});}finally{await runtime.dispose();}
}
const sources:any={};for(const path of ['src/extension/plugins/runtime/PluginRuntime.ts','src/extension/plugins/runtime/PluginRegistry.ts','src/extension/plugins/loading/PluginLoader.ts'])sources[path]=createHash('sha256').update(await readFile(baseline+'/'+path)).digest('hex');
const raw={node:process.versions,sources,results};await writeFile(out+'/oracle.raw.json',JSON.stringify(raw,null,2)+'\n');
const normalized=JSON.parse(JSON.stringify(raw).split(out).join('<root>'));await writeFile(out+'/oracle.json',JSON.stringify({...normalized,normalization:'Only the generated fixture root becomes <root>; content/descriptions/catalog order/read output preserved.'},null,2)+'\n');
