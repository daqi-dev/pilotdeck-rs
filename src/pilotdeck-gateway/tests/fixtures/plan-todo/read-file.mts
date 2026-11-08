import {mkdir,writeFile} from "node:fs/promises";
import {resolve} from "node:path";
import {pathToFileURL} from "node:url";
const {createPlanFileManager}=await import(pathToFileURL(resolve(process.env.PILOTDECK_TS_ROOT!,"src/tool/builtin/planFile.ts")).href);
const root=resolve(process.argv[2]);await mkdir(root);const project=resolve(root,"project");await mkdir(project);
const manager=createPlanFileManager({projectRoot:project,pilotHome:root+"/home",env:{PILOT_HOME:root+"/home"}});
const path=resolve(manager.getPlanDirectoryPath(),"plan.md");const rows=[];
for(const [name,content] of [["empty",""],["space"," \r\n\t"],["js_whitespace","\uFEFF\u00A0"],["content"," \n# Plan\nDo it\n "]]){await writeFile(path,content);const result=manager.readPlanFile(path,project);rows.push({name,content,result:result===undefined?{undefined:true}:{value:result}})}
await writeFile(root+"/oracle.json",JSON.stringify({node:process.version,rows},null,2)+"\n",{flag:"wx"});
