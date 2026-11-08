//! One-shot observer admission uses the same detached fork owner and body.
use super::*;
use super::observer::{ObserverActivity, ObserverInput, ObserverOutcome};

pub(super) struct ObserverLaunch {
    pub factory: Arc<ProjectSessionFactory>,
    pub definition: String,
    pub subagent: JsString,
    pub observer: JsObject,
    pub directive: JsString,
    pub observer_message: Option<JsString>,
    pub activity: ObserverActivity,
    pub outcome: ObserverOutcome,
    pub depth: f64,
    pub max: f64,
    pub context: ToolRunContext,
}
impl ForkApi {
    pub(super) fn observer_definition(&self, spec: &JsObject, definition: &str) -> Result<Option<JsObject>, ToolError> {
        let Some(value)=spec.get("observer").filter(|value|crate::runtime::mcp::sdk::js_truthy(value)) else {return Ok(None);};
        let id=match value {
            JsValue::String(value)=>value.clone(),
            _=>JsString::from_utf16(crate::js_value::to_string_units_checked(value).map_err(|_|error("Cannot convert observer definition to a property key."))?),
        };
        let available=definitions(&self.turn.config);
        let selected=available.get_key(&id).and_then(JsValue::as_object).cloned().ok_or_else(||error(concat(&[&"Observer definition ".into(),&id,&" for ".into(),&definition.into(),&" is not configured.".into()])))?;
        if !self.turn.owner.as_ref().and_then(Weak::upgrade).is_some_and(|owner|owner.is_open()) {
            let name=string(&selected,"id").unwrap_or(id);
            return Err(error(concat(&[&"Observer definition ".into(),&name,&" requires a host observer-subagent launcher.".into()])));
        }
        Ok(Some(selected))
    }
    pub(super) fn launch_observer(self:&Arc<Self>, launch:ObserverLaunch) -> Result<(),ToolError> {
        let ObserverLaunch {factory,definition,subagent,observer,directive,observer_message,activity,outcome,depth,max,context}=launch;
        let observer_type=string(&observer,"id").filter(|id|!id.is_empty()).ok_or_else(||error("Observer definition must have a nonempty id."))?;
        let description=string(&observer,"description").unwrap_or_else(||observer_type.clone());
        let timeout=context.fields.get("subagentTimeoutMs").and_then(JsValue::as_number);
        let input=ObserverInput {
            session_id:self.turn.parent.session_id.clone(),parent_turn_id:self.turn.parent.turn_id.clone(),
            observed_subagent_id:subagent,observed_subagent_type:definition.as_str().into(),
            observer_subagent_id:uuid::Uuid::new_v4().to_string().into(),definition:observer.clone(),
            directive,activity,observer_message,timeout_ms:timeout,outcome,
        };
        self.launch_detached(super::background::DetachedFork {
            factory,permit:None,definition:String::from_utf16_lossy(observer_type.units()),spec:observer,
            description,directive:JsString::default(),parent_mode:"plan".into(),parent_ask:true,
            depth,max,context,observer:Some(input),
        }).map(|_|())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime::project::{ProjectRuntimeRegistry, ResourceBundle};
    fn make_api(config: JsObject) -> Arc<ForkApi> {
        let turn = ForkTurn::new(
            NativeForkRegistry::new(),
            ExecutionIdentity { request_id:"request".into(), run_id:"run".into(), operation_id:"operation".into(), session_id:"parent".into(), turn_id:"turn".into() },
            config, crate::runtime::turn::live::test_recorder(), RuleSetHandle::empty(), None, None,
            ToolRegistry::new(None,None), super::super::super::skills::NativeSkills::default(), None,
            #[cfg(unix)] None,
        );
        Arc::new(ForkApi { turn, read:Arc::default(), write:Arc::default() })
    }
    #[test]
    fn observer_preflight_rejects_missing_definition_and_launcher_before_fork() {
        let api=make_api(JsObject::new());
        let spec=object([("observer","missing".into())]).as_object().unwrap().clone();
        assert_eq!(api.observer_definition(&spec,"research").unwrap_err().message, "Observer definition missing for research is not configured.".into());
        assert!(lock(&api.turn.registry.scopes).is_empty());
        assert!(lock(&api.turn.jobs).is_empty());
        let definition=object([("id","watcher".into())]);
        let config=object([("subagentDefinitions",object([("watcher",definition)]))]).as_object().unwrap().clone();
        let api=make_api(config);
        let spec=object([("observer","watcher".into())]).as_object().unwrap().clone();
        assert_eq!(api.observer_definition(&spec,"research").unwrap_err().message,"Observer definition watcher requires a host observer-subagent launcher.".into());
        assert!(lock(&api.turn.registry.scopes).is_empty());
    }
    #[tokio::test]
    async fn observer_scope_consumes_host_identity_and_keeps_empty_tools_plan_memory_disabled() {
        let api=make_api(object([("cwd","/fixture-observer".into()),("permissionMode","bypassPermissions".into())]).as_object().unwrap().clone());
        let resources=ProjectRuntimeRegistry::new("/fixture-observer",Arc::new(|_:&std::path::Path,_:&mut ResourceBundle|Ok(())));
        api.turn.bind_generation(Arc::new(resources.acquire(None).unwrap())).unwrap();
        let spec=object([
            ("id","watcher".into()),("allowedTools",JsValue::Array(vec![])),("disallowedTools",JsValue::Array(vec![])),
            ("permissionMode","plan".into()),("isReadOnly",true.into()),("memory","disabled".into()),("nativeObserver",true.into()),
            ("mcpServers",JsValue::Undefined),
        ]).as_object().unwrap().clone();
        let (scope,context,_)=api.compose_child("watcher",&spec,&"plan".into(),true,0.0,4.0,&"child-session".into(),&"child-id".into(),None,false).unwrap();
        assert_eq!(scope.permission_mode,"plan".into());
        assert_eq!(context.allowed_tools,Some(vec![]));
        for key in ["nativeObserver","nativeMemoryDisabled"] {assert_eq!(scope.config.get(key),Some(&JsValue::Bool(true)));}
        assert_eq!(scope.config.get("runMode"),Some(&JsValue::String("ask".into())));
        assert!(scope.config.get("nativeForkMcpServers").is_some_and(JsValue::is_undefined));
        let normal=object([("allowedTools",JsValue::Array(vec![]))]).as_object().unwrap().clone();
        let (normal,_,_)=api.compose_child("research",&normal,&"default".into(),false,0.0,4.0,&"normal-session".into(),&"normal-id".into(),None,false).unwrap();
        assert!(normal.config.get("nativeObserver").is_none());
        drop(scope);
        drop(normal);
        drop(api);
        resources.dispose().await.unwrap();
    }
}
