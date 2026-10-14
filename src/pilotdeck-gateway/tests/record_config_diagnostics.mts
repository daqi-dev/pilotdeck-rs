// Isolated offline Node 22 recording. Locale-sensitive cases use fresh runtimes.
import {execFileSync} from 'node:child_process';
import {readFileSync,writeFileSync,mkdirSync} from 'node:fs';
import {resolve,join,dirname} from 'node:path';
import {fileURLToPath} from 'node:url';
const [baseline,output,scratch]=process.argv.slice(2).map(x=>resolve(x));
const here=dirname(fileURLToPath(import.meta.url));
mkdirSync(scratch,{recursive:true});
const inputs=join(scratch,'inputs.json');
execFileSync(process.execPath,[join(here,'config_cases.mjs'),inputs],{stdio:'inherit'});
const cases=JSON.parse(readFileSync(inputs,'utf8'));
const groups=new Map<string,any[]>();for(const test of cases){const locale=test.env.LANG??'en_US.UTF-8';const group=test.id.includes('prototype')?locale+'@'+test.id:locale;if(!groups.has(group))groups.set(group,[]);groups.get(group)!.push(test);}
let manifest:any;const results:any[]=[];
for(const [group,tests]of groups){const locale=group.split("@")[0];const name=group.replaceAll(/[^a-zA-Z0-9]/g,'_');const input=join(scratch,name+'-inputs.json');const result=join(scratch,name+'-output.json');writeFileSync(input,JSON.stringify(tests));
 const home=join(scratch,name+'-home');const xdg=join(scratch,name+'-xdg');const tmp=join(scratch,name+'-tmp');for(const p of [home,xdg,tmp])mkdirSync(p,{recursive:true});
 execFileSync(process.execPath,['--import',join(baseline,'node_modules/tsx/dist/loader.mjs'),join(here,'record_config_catalog.mts'),baseline,result,tmp,input],{env:{...process.env,HOME:home,XDG_CONFIG_HOME:xdg,XDG_CACHE_HOME:xdg,XDG_DATA_HOME:xdg,TMPDIR:tmp,PILOT_HOME:home,PILOTDECK_SKIP_BROWSER_OPEN:'1',LANG:locale,LC_ALL:locale,LC_MESSAGES:locale},stdio:'inherit'});
 const recorded=JSON.parse(readFileSync(result,'utf8'));manifest??=recorded;results.push(...recorded.cases);
}
const host = (test: any) => /prototype-(provider|model|always)/.test(test.id);
writeFileSync(output,JSON.stringify({...manifest,cases:results.filter(test=>!host(test))},null,2)+'\n');
writeFileSync(join(dirname(output),'host-boundaries.json'),JSON.stringify({...manifest,cases:results.filter(host)},null,2)+'\n');console.log(JSON.stringify({cases:results.length,output}));
