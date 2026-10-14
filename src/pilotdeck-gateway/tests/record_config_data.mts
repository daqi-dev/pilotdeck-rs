// Runtime data projection for the pinned configuration parser; no listeners.
import {writeFile, readdir} from 'node:fs/promises';
import {join, resolve} from 'node:path';
import {pathToFileURL} from 'node:url';
const baseline = resolve(process.argv[2]);
const output = resolve(process.argv[3]);
if (process.versions.node.split('.')[0] !== '22') throw Error('Node 22 required');
const source = (path: string) => import(pathToFileURL(join(baseline, 'src', path)).href);
const {PROVIDER_CATALOG} = await source('model/catalog/index.ts');
const {supportedMethods, supportedContract} = await source('composition/registry.ts');
const contracts = Object.fromEntries(['agentLoop','skills','modelProvider','tools','context','knowledge'].map(slot => [slot,{contract:supportedContract(slot),methods:supportedMethods(slot)}]));
const zones = new Set<string>(Intl.supportedValuesOf('timeZone'));
async function walk(path: string, prefix = '') {
  for (const entry of await readdir(path,{withFileTypes:true})) {
    const name = prefix + entry.name;
    if (entry.isDirectory()) await walk(join(path,entry.name),name+'/');
    else {try {new Intl.DateTimeFormat('en-US',{timeZone:name}).format(); zones.add(name)} catch {}}
  }
}
await walk('/usr/share/zoneinfo');
await writeFile(join(output,'catalog-data.json'),JSON.stringify(PROVIDER_CATALOG,null,2)+'\n');
await writeFile(join(output,'contracts.json'),JSON.stringify(contracts,null,2)+'\n');
await writeFile(join(output,'timezones.json'),JSON.stringify([...zones].map(x=>x.toLowerCase()).sort(),null,2)+'\n');
console.log(JSON.stringify({providers:Object.keys(PROVIDER_CATALOG).length,zones:zones.size,node:process.version,icu:process.versions.icu}));
