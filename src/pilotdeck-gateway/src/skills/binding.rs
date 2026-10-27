//! createLocalGateway.ts:851–870 skills binding selection. Each gateway call
//! resolves `snapshot.config.modules?.skills` for its project and routes to
//! the native manager, the disabled port (2024–2040) or the external module
//! port (composition/domainPorts.ts:75 createSkillManagementPort).
//!
//! The external port (external.rs) is the real `createSkillManagementPort`:
//! it loads the module manifest and forwards each management method over the
//! module protocol. It stays in this component package, not on gateway
//! dispatch (`createLocalGateway.ts:868-869` still resolves validate/scan
//! without a project key).
use super::{
    Result, SkillError, SkillManager,
    external::{
        ExternalSkillBinding, ExternalSkillsPort, HyperModuleTransport, MODULE_HTTP_TRANSPORT,
        ModuleHttpTransport,
    },
    field,
};
use pilotdeck_protocol::JsValue;
use std::sync::Arc;

/// The eight skill methods of GatewayWsConnection.ts:542–557.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillMethod {
    List,
    Read,
    Write,
    Create,
    Delete,
    Import,
    Validate,
    Scan,
}
impl SkillMethod {
    pub const ALL: [Self; 8] = [
        Self::List,
        Self::Read,
        Self::Write,
        Self::Create,
        Self::Delete,
        Self::Import,
        Self::Validate,
        Self::Scan,
    ];
    pub fn from_wire(method: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.wire() == method)
    }
    pub fn wire(self) -> &'static str {
        match self {
            Self::List => "skill_list",
            Self::Read => "skill_read",
            Self::Write => "skill_write",
            Self::Create => "skill_create",
            Self::Delete => "skill_delete",
            Self::Import => "skill_import",
            Self::Validate => "skill_validate",
            Self::Scan => "skill_scan",
        }
    }
    /// SkillManagementPort operation name (domainPorts.ts:96–103).
    pub fn operation(self) -> &'static str {
        &self.wire()["skill_".len()..]
    }
    /// validate/scan resolve the binding without a projectKey (868–869).
    fn routes_by_project(self) -> bool {
        !matches!(self, Self::Validate | Self::Scan)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillsBinding {
    Native,
    Disabled,
    External,
}

/// isDisabledModuleBinding, then isExternalModuleBinding (types.ts:81–87).
/// `"implementationId" in binding` throws on a primitive or null, as in TS.
pub fn select_binding(binding: &JsValue) -> Result<SkillsBinding> {
    let property = |key: &str| match binding {
        JsValue::Object(o) => o.get_property(key),
        _ => None,
    };
    if property("enabled") == Some(&JsValue::Bool(false)) {
        return Ok(SkillsBinding::Disabled);
    }
    let has_implementation = match binding {
        JsValue::Undefined => return Ok(SkillsBinding::Native),
        JsValue::Object(o) => o.get_property("implementationId").is_some(),
        JsValue::Array(_) => false,
        other => {
            return Err(SkillError::type_error(
                "",
                format!(
                    "Cannot use 'in' operator to search for 'implementationId' in {}",
                    crate::js_value::display(other)?
                ),
            ));
        }
    };
    let http = property("transport")
        .and_then(JsValue::as_string)
        .is_some_and(|t| t.equals_str(MODULE_HTTP_TRANSPORT));
    Ok(if has_implementation && http {
        SkillsBinding::External
    } else {
        SkillsBinding::Native
    })
}

/// `registry.resolve(projectKey ?? projectRoot).snapshot.config.modules?.skills`
/// (createLocalGateway.ts:853–854): the registry lookup stays with the caller,
/// this helper applies the optional-chain projection the facade consumes.
/// `config` missing/undefined still throws on `config.modules`, as TS does;
/// a nullish `modules` short-circuits to `undefined`.
pub fn skill_binding_from_snapshot(snapshot: &JsValue) -> Result<JsValue> {
    let modules = field(field(snapshot, "config")?, "modules")?;
    if matches!(modules, JsValue::Undefined | JsValue::Null) {
        return Ok(JsValue::Undefined);
    }
    Ok(modules.get("skills").cloned().unwrap_or(JsValue::Undefined))
}

/// The gateway's frozen `skillManager` facade. `binding_for` receives the
/// call's `projectKey` (`None` for validate/scan) and returns that project's
/// `snapshot.config.modules?.skills` (`undefined` when absent); applying
/// `?? projectRoot` and the registry lookup is the caller's job.
pub struct SkillsFacade<F, T = HyperModuleTransport> {
    native: SkillManager,
    binding_for: F,
    external: Arc<T>,
}
impl<F: Fn(Option<&JsValue>) -> Result<JsValue>> SkillsFacade<F, HyperModuleTransport> {
    pub fn new(native: SkillManager, binding_for: F) -> Self {
        Self::with_transport(native, binding_for, Arc::new(HyperModuleTransport::new()))
    }
}
impl<F, T> SkillsFacade<F, T>
where
    F: Fn(Option<&JsValue>) -> Result<JsValue>,
    T: ModuleHttpTransport + 'static,
{
    pub fn with_transport(native: SkillManager, binding_for: F, external: Arc<T>) -> Self {
        Self {
            native,
            binding_for,
            external,
        }
    }

    pub async fn call(&self, method: SkillMethod, input: &JsValue) -> Result<JsValue> {
        // `input.projectKey` is read before the binding resolves.
        let project_key = if method.routes_by_project() {
            Some(field(input, "projectKey")?)
        } else {
            None
        };
        let binding = (self.binding_for)(project_key)?;
        match select_binding(&binding)? {
            SkillsBinding::Native => self.native.call(method, input),
            SkillsBinding::Disabled => Err(disabled()),
            SkillsBinding::External => {
                let binding = ExternalSkillBinding::from_value(&binding)?;
                ExternalSkillsPort::new(binding, self.external.clone())
                    .call(method, input)
                    .await
            }
        }
    }
}

/// createLocalGateway.ts:2025–2028: a plain Error with a code, for every method.
pub fn disabled() -> SkillError {
    SkillError::host_failure(
        "SKILL_MODULE_DISABLED",
        "Skill module is disabled for this profile.",
    )
}
