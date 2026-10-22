use super::*;
use crate::config::permission::{
    AllowRulesHandle, GatewaySessionPermissionRuleSetRegistry, PermissionRule,
};

#[derive(Default)]
pub struct InteractionOptions {
    pub reconnect_port: Option<InteractionReconnectPort>,
    pub elicitation_bus: Option<GatewayElicitationBus>,
    pub permission_bus: Option<GatewayPermissionBus>,
    pub permission_grants: Option<Arc<GatewaySessionPermissionRuleSetRegistry>>,
    pub on_elicitation_delivered: Option<DeliveredObserver>,
}
pub struct PermissionDecision {
    pub request_id: JsString,
    pub decision: JsString,
    pub remember: Option<JsValue>,
    pub reason: Option<JsValue>,
}
impl PermissionDecision {
    pub fn to_value(self) -> JsValue {
        object([
            ("requestId", JsValue::String(self.request_id)),
            ("decision", JsValue::String(self.decision)),
            ("remember", self.remember.unwrap_or(JsValue::Undefined)),
            ("reason", self.reason.unwrap_or(JsValue::Undefined)),
        ])
    }
}
/// Owns volatile round-trips; durable permission rules remain in the injected
/// registry. Router/Agent ownership and transport attachment belong to the host.
pub struct GatewayInteractionCoordinator {
    reconnect_port: InteractionReconnectPort,
    elicitation_bus: GatewayElicitationBus,
    permission_bus: GatewayPermissionBus,
    permission_grants: Arc<GatewaySessionPermissionRuleSetRegistry>,
    on_delivered: Option<DeliveredObserver>,
}
impl Default for GatewayInteractionCoordinator {
    fn default() -> Self {
        Self::new(InteractionOptions::default())
    }
}
impl GatewayInteractionCoordinator {
    pub fn new(options: InteractionOptions) -> Self {
        let reconnect_port = options.reconnect_port.unwrap_or_default();
        Self {
            elicitation_bus: options
                .elicitation_bus
                .unwrap_or_else(|| GatewayElicitationBus::new(reconnect_port.clone())),
            permission_bus: options
                .permission_bus
                .unwrap_or_else(|| GatewayPermissionBus::new(reconnect_port.clone())),
            reconnect_port,
            permission_grants: options.permission_grants.unwrap_or_default(),
            on_delivered: options.on_elicitation_delivered,
        }
    }
    pub fn elicitation_bus(&self) -> &GatewayElicitationBus {
        &self.elicitation_bus
    }
    pub fn permission_bus(&self) -> &GatewayPermissionBus {
        &self.permission_bus
    }
    pub fn reconnect_port(&self) -> &InteractionReconnectPort {
        &self.reconnect_port
    }
    pub fn get_binding(&self, owner: &JsString) -> Option<InteractionBinding> {
        self.reconnect_port.current_binding(owner)
    }
    pub fn reconnect(
        &self,
        owner: &JsString,
        next: Option<InteractionBinding>,
        previous: Option<&InteractionBinding>,
    ) -> Result<JsValue, JsString> {
        let Some(next) = next else {
            return Ok(reconnect::result("stale_binding", None, vec![]));
        };
        self.reconnect_port.reconnect(owner, next, previous)
    }
    pub fn reconnect_for_turn(
        &self,
        owner: &JsString,
        binding: InteractionBinding,
    ) -> Result<JsValue, JsString> {
        let current = self.reconnect_port.current_binding(owner);
        if current
            .as_ref()
            .is_some_and(|c| c.connection_id != binding.connection_id)
        {
            return Ok(reconnect::result("stale_binding", None, vec![]));
        }
        self.reconnect_port
            .reconnect(owner, binding, current.as_ref())
    }
    pub fn disconnect(
        &self,
        owner: &JsString,
        binding: &InteractionBinding,
    ) -> Result<JsValue, JsString> {
        let disconnected = self.reconnect_port.disconnect(owner, binding)?;
        let preserve = disconnected
            && (self.elicitation_bus.has_pending_session(owner)
                || self.permission_bus.has_pending_session(owner));
        Ok(object([
            ("disconnected", JsValue::Bool(disconnected)),
            ("preserveTurn", JsValue::Bool(preserve)),
        ]))
    }
    pub fn reject_pending_turn(&self, owner: &JsString, reason: &JsString) {
        self.elicitation_bus.reject_session(owner, reason);
        self.permission_bus.deny_session(owner, reason);
    }
    pub fn close_session(&self, owner: &JsString, reason: &JsString) {
        self.reject_pending_turn(owner, reason);
        self.reconnect_port.dispose_owner(owner);
        self.permission_grants.close_session(owner);
    }
    pub fn respond_elicitation(
        &self,
        owner: &JsString,
        request: &JsString,
        answer: JsValue,
        binding: Option<&InteractionBinding>,
    ) -> bool {
        let Some(entry) = self.elicitation_bus.consume(owner, request, binding) else {
            return false;
        };
        (entry.resolve)(answer);
        if let Some(observer) = &self.on_delivered {
            observer(owner, request);
        }
        true
    }
    pub fn decide_permission(
        &self,
        owner: &JsString,
        decision: PermissionDecision,
        binding: Option<&InteractionBinding>,
    ) -> bool {
        let Some(entry) = self
            .permission_bus
            .consume(owner, &decision.request_id, binding)
        else {
            return false;
        };
        (entry.resolve)(decision.to_value());
        true
    }
    pub fn grant_session_permission(&self, owner: &JsString, entry: &JsString) -> JsValue {
        let rule = entry_to_rule(entry);
        if rule.tool_name.is_empty() {
            return object([("granted", JsValue::Bool(false))]);
        }
        self.permission_grants.grant(owner, rule);
        object([
            ("granted", JsValue::Bool(true)),
            ("entry", JsValue::String(entry.clone())),
        ])
    }
    pub fn session_allow_rules(&self, owner: &JsString) -> AllowRulesHandle {
        self.permission_grants.allow_rules(owner)
    }
    pub fn dispose(&self, reason: &JsString) {
        self.permission_grants.dispose();
        self.elicitation_bus.dispose(Some(reason));
        self.permission_bus.dispose(Some(reason));
        self.reconnect_port.dispose();
    }
}

// settings.ts:75-84,100-113: only the pure entry conversion is used. No settings
// file is read and no new persistence owner is introduced.
fn entry_to_rule(entry: &JsString) -> PermissionRule {
    let trimmed = trim(entry.units());
    let mut normalized = trimmed.to_vec();
    if trimmed.starts_with(&[66, 97, 115, 104, 40])
        && trimmed.ends_with(&[41])
        && !trimmed
            .iter()
            .any(|u| matches!(u, 10 | 13 | 0x2028 | 0x2029))
    {
        let pattern = trim(&trimmed[5..trimmed.len() - 1]);
        normalized = "bash".encode_utf16().collect();
        if !pattern.is_empty() {
            normalized.push(58);
            normalized.extend_from_slice(pattern);
        }
    } else {
        for (alias, name) in [
            ("Read", "read_file"),
            ("Write", "write_file"),
            ("Edit", "edit_file"),
            ("NotebookEdit", "edit_notebook"),
            ("MultiEdit", "edit_file"),
            ("Glob", "glob"),
            ("Grep", "grep"),
            ("Bash", "bash"),
            ("Task", "agent"),
            ("TodoWrite", "todo_write"),
            ("WebFetch", "web_fetch"),
            ("WebSearch", "web_search"),
        ] {
            if trimmed.iter().copied().eq(alias.encode_utf16()) {
                normalized = name.encode_utf16().collect();
                break;
            }
        }
    }
    let colon = normalized.iter().position(|u| *u == 58);
    let tool = colon.map_or(normalized.as_slice(), |i| &normalized[..i]);
    let tool = if tool.is_empty() { &normalized } else { tool };
    let pattern = colon
        .map(|i| trim(&normalized[i + 1..]))
        .filter(|v| !v.is_empty());
    let mut rule = PermissionRule::new("session", "allow", JsString::from_utf16(tool.to_vec()));
    rule.pattern = pattern.map(|v| JsString::from_utf16(v.to_vec()));
    rule
}
fn trim(units: &[u16]) -> &[u16] {
    fn space(u: u16) -> bool {
        matches!(u, 9..=13 | 32 | 0xa0 | 0x1680 | 0x2000..=0x200a | 0x2028 | 0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff)
    }
    let start = units.iter().position(|u| !space(*u)).unwrap_or(units.len());
    let end = units
        .iter()
        .rposition(|u| !space(*u))
        .map_or(start, |i| i + 1);
    &units[start..end]
}
