import {mkdir,writeFile} from 'node:fs/promises';
import {pathToFileURL} from 'node:url';
const [baseline,home,project]=process.argv.slice(2);
const {resolveAlwaysOnPaths}=await import(pathToFileURL(baseline+'/src/always-on/storage/AlwaysOnPaths.ts').href);
const paths=resolveAlwaysOnPaths({pilotHome:home,projectKey:project});
await mkdir(paths.plansDir,{recursive:true});
await writeFile(paths.planIndexFile,JSON.stringify({schemaVersion:1,plans:[{id:'suite-plan',title:'Suite plan',createdAt:'2026-01-01T00:00:00.000Z',status:'ready',summary:'execute fixture',rationale:'test',dedupeKey:'suite',sourceRunId:'seed',planFilePath:'plans/suite-plan.md'}]}));
await writeFile(paths.plansDir+'/suite-plan.md','# Suite plan\n\nBUSINESS_READ fixture.txt then report the result.\n');
console.log(JSON.stringify(paths));
