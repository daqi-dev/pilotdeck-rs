//! Actual native file permission predicates from readFile/writePermissions.ts.
//! Metadata work is off the async runtime; execution still uses the same gate.
use std::sync::{Arc,LazyLock};
use pilotdeck_protocol::{JsObject,JsString,JsValue};
use crate::{host::{HostError,ModuleError,modules::tool_context::{HostToolCheckpoint,ToolRuntimeContext}},runtime::{permission::{PermissionContext,PermissionFuture},tools::{ToolRunContext,ToolError}}};
use super::{object,string,resolve_workspace_path,resolve_write_path,dirname,join};
static SLOTS:LazyLock<Arc<tokio::sync::Semaphore>>=LazyLock::new(||Arc::new(tokio::sync::Semaphore::new(4)));
fn context(permission:PermissionContext,checkpoint:HostToolCheckpoint,env:JsObject)->ToolRunContext {
    ToolRunContext::new(Arc::new(ToolRuntimeContext{fields:object([("cwd",string(permission.cwd.clone())),("permissionMode",string(permission.mode.as_js_string().clone())),("permissionContext",permission.to_value()),("env",JsValue::Object(env))]),files:checkpoint.tool_context_state(),abort_signal:None,model:None,elicitation:None,user_dialog:None,audit_recorder:None,file_history:None,file_update_notifier:None,now:None,progress:None,subagent:None,plan_todo:None,goal:None,plan_directory:None}))
}
fn denied(error:ToolError)->JsValue{let reason=JsValue::Object(object([("type","safety".into()),("message",JsValue::String(error.message.clone()))]));JsValue::Object(object([("type","deny".into()),("reason",reason),("message",JsValue::String(error.message))]))}
fn ask(tool:&str,path:JsString)->Result<JsValue,HostError>{
    let directory=dirname(&path);let pattern=join(&directory,&"*".into());
    let rule=JsValue::Object(object([("source","session".into()),("behavior","allow".into()),("toolName",tool.into()),("pattern",string(pattern.clone()))]));
    let reason=JsValue::Object(object([("type","tool".into()),("toolName",tool.into()),("message",format!("{tool} targets a path outside the workspace.").as_str().into())]));
    let summary=pilotdeck_protocol::stringify_json(&JsValue::Object(object([("file_path",string(path.clone()))]))).map_err(|e|ModuleError::new(e.to_string()))?.ok_or_else(||ModuleError::new("File permission input is not serializable."))?;
    let options=vec![
        JsValue::Object(object([("id","allow_once".into()),("label","Allow once".into())])),
        JsValue::Object(object([("id","allow_session".into()),("label","Allow this folder for this session".into()),("rules",JsValue::Array(vec![rule]))])),
        JsValue::Object(object([("id","deny".into()),("label","Deny".into())])),
        JsValue::Object(object([("id","cancel".into()),("label","Cancel".into())])),
    ];
    let metadata=JsValue::Object(object([("externalPath",string(path)),("allowedDirectory",string(directory)),("pattern",string(pattern))]));
    let request=JsValue::Object(object([("toolCallId","".into()),("toolName",tool.into()),("inputSummary",summary.as_str().into()),("reason",reason.clone()),("options",JsValue::Array(options)),("metadata",metadata)]));
    Ok(JsValue::Object(object([("type","ask".into()),("reason",reason),("request",request)])))
}
fn evaluate(tool:&str,path:&JsString,context:&ToolRunContext,read:bool)->Result<JsValue,HostError>{
    if read {
        match resolve_workspace_path(path,context,true,false){Ok(_)=>return Ok(JsValue::Object(object([("type","passthrough".into())]))),Err(error) if error.code!="path_not_allowed"=>return Ok(denied(error)),Err(_)=>{}}
        match resolve_workspace_path(path,context,false,true){Ok(path)=>ask(tool,path.absolute),Err(error)=>Ok(denied(error))}
    }else{
        if resolve_write_path(path,context,false,false).is_ok(){return Ok(JsValue::Object(object([("type","passthrough".into())])));}
        match resolve_write_path(path,context,false,true){Ok(path)=>ask(tool,path.absolute),Err(error)=>Ok(denied(error))}
    }
}
fn check(tool:&'static str,path:JsString,context:ToolRunContext,read:bool)->PermissionFuture<'static,Option<JsValue>>{
    Box::pin(async move {let permit=SLOTS.clone().acquire_owned().await.map_err(|_|ModuleError::new("Native file permission IO is unavailable."))?;let result=tokio::task::spawn_blocking(move||{let _permit=permit;evaluate(tool,&path,&context,read)}).await.map_err(|e|ModuleError::new(format!("Native file permission IO failed: {e}")))??;Ok(Some(result))})
}
pub(crate) fn read(input:&JsValue,permission:&PermissionContext,checkpoint:HostToolCheckpoint,env:JsObject)->PermissionFuture<'static,Option<JsValue>>{check("read_file",super::field(input,"file_path").as_string().cloned().unwrap_or_default(),context(permission.clone(),checkpoint,env),true)}
/// Shared exact write/edit predicate; callers use their canonical tool name.
pub(crate) fn write(tool:&'static str,input:&JsValue,permission:&PermissionContext)->PermissionFuture<'static,Option<JsValue>>{check(tool,super::field(input,"file_path").as_string().cloned().unwrap_or_default(),context(permission.clone(),HostToolCheckpoint::default(),JsObject::new()),false)}

#[cfg(test)]mod tests {
    use super::*;
    use crate::{config::permission::{PermissionRule,PermissionRuleSet},runtime::permission::{create_default_permission_context,DefaultPermissionContextOptions}};
    struct Scratch(std::path::PathBuf);impl Drop for Scratch {fn drop(&mut self){let _=std::fs::remove_dir_all(&self.0);}}
    fn mapped(value:&JsValue,from:&str,to:&str)->JsValue{match value {JsValue::String(s)=>JsValue::String(s.to_utf8().unwrap().replace(from,to).as_str().into()),JsValue::Array(values)=>JsValue::Array(values.iter().map(|v|mapped(v,from,to)).collect()),JsValue::Object(values)=>{let mut out=JsObject::new();for(k,v)in values.entries(){out.insert(k.clone(),mapped(v,from,to));}JsValue::Object(out)},_=>value.clone()}}
    fn strings(value:Option<&JsValue>)->Vec<JsString>{match value{Some(JsValue::Array(values))=>values.iter().filter_map(JsValue::as_string).cloned().collect(),_=>Vec::new()}}
    #[tokio::test]
    async fn actual_ts_file_permission_predicates_match_all_ten_portable_cases(){
        let scratch=Scratch(std::env::temp_dir().join(format!("pdrs-file-permission-{}",uuid::Uuid::new_v4())));for dir in["project","extra","foreign","home"]{std::fs::create_dir_all(scratch.0.join(dir)).unwrap();std::fs::write(scratch.0.join(dir).join("file.txt"),"FIXTURE").unwrap();}
        for (bytes,expected) in [(include_str!("../../../../tests/fixtures/native-file-permissions/oracle.json"),8),(include_str!("../../../../tests/fixtures/native-file-permissions/edit-oracle.json"),2)] { let fixture=pilotdeck_protocol::parse_json(bytes).unwrap();let JsValue::Array(cases)=fixture.get("cases").unwrap() else{panic!("cases")};let root=scratch.0.to_str().unwrap();
        for case in cases {let actual=mapped(case,"/fixture",root);let source=actual.get("context").unwrap();let permission=source.get("permissionContext").unwrap();let rules=permission.get("rules").unwrap();let array=|name|match rules.get(name){Some(JsValue::Array(values))=>values.iter().map(|r|PermissionRule::from_value(r).unwrap()).collect(),_=>Vec::new()};
            let permission=create_default_permission_context(DefaultPermissionContextOptions{cwd:source.get("cwd").unwrap().as_string().unwrap().clone(),mode:permission.get("mode").and_then(JsValue::as_string).cloned().map(Into::into),can_prompt:permission.get("canPrompt").and_then(JsValue::as_bool),bypass_available:Some(true),accept_edits:permission.get("acceptEdits").and_then(JsValue::as_bool),additional_working_directories:Some(strings(permission.get("additionalWorkingDirectories"))),rules:PermissionRuleSet{allow:array("allow"),deny:array("deny"),ask:array("ask")},..Default::default()});
            let checkpoint=HostToolCheckpoint::default();checkpoint.allow_read_files(&strings(source.get("allowedReadFiles")));let result=if actual.get("tool")==Some(&"read_file".into()){read(actual.get("input").unwrap(),&permission,checkpoint,source.get("env").unwrap().as_object().unwrap().clone()).await}else if actual.get("tool")==Some(&"edit_file".into()){use crate::host::modules::capability::ToolDefinition;super::super::edit_file::EditFileTool::new().check_permissions(actual.get("input").unwrap(),&permission).await}else{write("write_file",actual.get("input").unwrap(),&permission).await}.unwrap().unwrap();assert_eq!(mapped(&result,root,"/fixture"),case.get("predicate").unwrap().clone(),"{:?}",case.get("name"));
        }assert_eq!(cases.len(),expected); }
    }
}
