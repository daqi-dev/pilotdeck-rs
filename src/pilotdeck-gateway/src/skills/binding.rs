//! createLocalGateway.ts:851–870 skills binding selection. Each gateway call
//! resolves `snapshot.config.modules?.skills` for its project and routes to
//! the native manager, the disabled port (2024–2040) or the external module
//! port (composition/domainPorts.ts:75 createSkillManagementPort).
//!
//! The external port talks to an HTTP module host (HttpModuleClient), which
//! Rust does not have yet: an external binding is *selected* here, and every
//! call on it answers with an explicit host-required error rather than a
//! partial forward.
use super::{Result, SkillError, SkillManager, field};
use pilotdeck_protocol::JsValue;

/// composition/types.ts:3 MODULE_HTTP_TRANSPORT.
const MODULE_HTTP_TRANSPORT: &str = "module-http-v2";

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

/// The gateway's frozen `skillManager` facade. `binding_for` receives the
/// call's `projectKey` (`None` for validate/scan) and returns that project's
/// `snapshot.config.modules?.skills` (`undefined` when absent); applying
/// `?? projectRoot` and the registry lookup is the caller's job.
pub struct SkillsFacade<F> {
    native: SkillManager,
    binding_for: F,
}
impl<F: Fn(Option<&JsValue>) -> Result<JsValue>> SkillsFacade<F> {
    pub fn new(native: SkillManager, binding_for: F) -> Self {
        Self {
            native,
            binding_for,
        }
    }

    pub fn call(&self, method: SkillMethod, input: &JsValue) -> Result<JsValue> {
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
            SkillsBinding::External => Err(external_host_required(method)),
        }
    }
}

/// createLocalGateway.ts:2025–2028: a plain Error with a code, for every method.
pub fn disabled() -> SkillError {
    SkillError::host_failure(
        "SKILL_MODULE_DISABLED",
        "Skill module is disabled for this profile.".into(),
    )
}

/// No TS counterpart: the external module host is not ported yet.
fn external_host_required(method: SkillMethod) -> SkillError {
    let mut e = SkillError::host_failure(
        "SKILL_MODULE_HOST_REQUIRED",
        format!(
            "Skill module {} needs the external module host.",
            method.operation()
        ),
    );
    e.requires_host = true;
    e
}
