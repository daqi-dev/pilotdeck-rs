// Node 22's real fs validation errors. Every path contains NUL, so no syscall.
import {statSync} from 'node:fs';
import {writeFile} from 'node:fs/promises';
if(process.versions.node.split('.')[0]!=='22')throw Error('Node 22 required');
const paths=[
 '/fixture/home/projects/registered/chats/web:\0.jsonl',
 "single'\0", 'single\'double"\0', 'single\'double"back`\0', "single'double\"${\0",
 ...Array.from({length:32},(_,i)=>'control'+String.fromCharCode(i)+'\0'),
 ...Array.from({length:33},(_,i)=>'control'+String.fromCharCode(127+i)+'\0'),
 '中é😀\0', 'lone-high\ud800\0', 'lone-low\udfff\0', '\\\0',
 'x'.repeat(75)+'\n\0', 'x'.repeat(76)+'\n\0', '\n'.repeat(80)+'\0',
 'x'.repeat(126)+'\0', 'x'.repeat(127)+'\0', '😀'.repeat(70)+'\0',
 'x'.repeat(10_001)+'\0', '\n'.repeat(10_001)+'\0',
];
const cases=paths.map(path=>{try{statSync(path);throw Error('NUL unexpectedly accepted');}catch(e){if(e.code!=='ERR_INVALID_ARG_VALUE')throw e;return {path,code:e.code,message:e.message};}});
await writeFile(process.argv[2],JSON.stringify({node:process.version,icu:process.versions.icu,cases},null,2)+'\n');
