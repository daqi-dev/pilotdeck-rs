// Run with the pinned TS checkout's tsx on mbp2. All files belong to outputRoot.
import { mkdir, writeFile, symlink } from 'node:fs/promises';
import path from 'node:path';
import { pathToFileURL } from 'node:url';
const [baseline, outputRoot] = process.argv.slice(2);
if (!baseline || !outputRoot) throw new Error('capture.mts <baseline> <new-output-root>');
await mkdir(outputRoot);
const root = await import('node:fs/promises').then(fs => fs.realpath(outputRoot));
const project = path.join(root, 'project');
await mkdir(path.join(project, '.pilotdeck/work'), {recursive:true});
await mkdir(path.join(project, 'custom'), {recursive:true});
await mkdir(path.join(root, 'home'));
await writeFile(path.join(project, 'final.bin'), Buffer.from([0, 255, 9]));
await writeFile(path.join(project, '.pilotdeck/work/draft'), 'draft');
await writeFile(path.join(project, 'custom/draft'), 'draft');
await writeFile(path.join(root, 'external.bin'), 'external');
await symlink(path.join(project, '.pilotdeck/work/draft'), path.join(project, 'alias'));
await symlink(path.join(root, 'external.bin'), path.join(project, 'outside-alias'));
const {createSendAttachmentTool} = await import(pathToFileURL(path.join(baseline, 'src/tool/builtin/sendAttachment.ts')).href);
const tool = createSendAttachmentTool();
const rows = [];
const cases = [
  {id:'binary',input:{file_path:'final.bin'}},
  {id:'name',input:{file_path:'final.bin',name:'\ufeff a/b\\c\0 ',mime_type:'application/octet-stream'}},
  {id:'blank-name',input:{file_path:'final.bin',name:'\ufeff \n'}},
  {id:'missing',input:{file_path:'missing'}},
  {id:'directory',input:{file_path:'.'}},
  {id:'internal',input:{file_path:'.pilotdeck/work/draft'}},
  {id:'internal-alias',input:{file_path:'alias'}},
  {id:'custom-internal',input:{file_path:'custom/draft'},work:'custom'},
  {id:'external',input:{file_path:'../external.bin'}},
  {id:'external-allowed',input:{file_path:'../external.bin'},allow:true},
  {id:'registered',input:{file_path:'../external.bin'},registered:true},
  {id:'outside-alias',input:{file_path:'outside-alias'},allow:true},
  {id:'nul',input:{file_path:'a\0b'}},
];
for (const item of cases) {
  const context = {cwd:project, permissionMode:'default',permissionContext:{additionalWorkingDirectories:[]},
    env:{PILOT_HOME:path.join(root,'home'),...(item.work?{PILOTDECK_WORK_DIR:item.work}:{})},
    allowedReadFiles:item.registered?[path.join(root,'external.bin')]:[],
    ...(item.allow?{currentPermissionDecision:{type:'allow'}}:{})};
  const validation = await tool.validateInput(item.input, context);
  const permission = await tool.checkPermissions(item.input, context);
  let result;
  try { result = {ok:true,output:await tool.execute(item.input, context)}; }
  catch (error) { result = {ok:false,error:{code:error.code,message:error.message,...(error.details?{details:error.details}:{})}}; }
  rows.push({...item,validation,permission,result});
}
const normalized = JSON.parse(JSON.stringify({baseline:'cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85',node:process.version,rows}).replaceAll(root,'<root>'));
await writeFile(path.join(root,'oracle.json'),JSON.stringify(normalized,null,2)+'\n',{flag:'wx'});
