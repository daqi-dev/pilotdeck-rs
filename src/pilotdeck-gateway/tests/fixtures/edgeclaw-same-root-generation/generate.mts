// #1082 generator: real cc7c4e6 probe, writes an exclusive oracle.json.
import {mkdirSync,writeFileSync} from 'node:fs';
import {join} from 'node:path';
import {matrix} from './observe.mts';

if(process.version!=='v22.22.0')throw new Error('#1082 Node v22.22.0 is required');
const baseline=process.env.PDRS_BASELINE;
if(!baseline)throw new Error('#1082 PDRS_BASELINE is required');
const out=process.argv[2];
if(!out)throw new Error('#1082 output directory argument is required');
mkdirSync(out,{recursive:true});
const answer=await matrix(baseline,join(out,'work'));
writeFileSync(join(out,'oracle.json'),JSON.stringify(answer,null,2)+'\n',{flag:'wx'});
console.log(JSON.stringify(answer,null,2));
