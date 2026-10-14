// No OS probes: record Number(string) and process.kill's validation boundary.
import {writeFile} from 'node:fs/promises';
const samples=['',' \n\t ','\uFEFF\u00a0 0\n','0','-0','+0','-1','+1','001','1.0','1.5','.5','1.','1e0','1e+3','1E-3','0x1','0X7fffffff','0o17','0O17','0b101','0B101','0x','0o','0b','0xgg','0o8','0b2','+0x1','-0b1','Infinity','+Infinity','-Infinity','infinity','inf','NaN','1e309','1e-324','9007199254740991','9007199254740992','2147483647','2147483648','4294967296','\u00851','1\u0085','１','1 2','1_000','0x1p0','null','undefined','0x10000000000000000','  +0001e0\n'];
const cases=[];const syscall=process._kill;
try {
 for(const text of samples){
  const number=Number(text);let calls=[];process._kill=(pid,signal)=>{calls.push({pid,signal});return 0;};
  let valid=Number.isSafeInteger(number)&&number>0;let killError;
  if(valid){try{process.kill(number,0);}catch(e){valid=false;killError=e.code;}}
  cases.push({text,number:String(number),safeInteger:Number.isSafeInteger(number),positive:number>0,pid:valid?number:null,syscallCalls:calls,killError});
 }
}finally{process._kill=syscall;}
await writeFile(process.argv[2],JSON.stringify({node:process.version,icu:process.versions.icu,syscallInjected:true,cases},null,2)+'\n');
