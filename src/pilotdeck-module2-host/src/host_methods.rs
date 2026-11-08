//! Host method inventory follows the original PD protocol module table.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HostMethod {
    pub module: &'static str,
    pub method: &'static str,
}
const fn host(module: &'static str, method: &'static str) -> HostMethod {
    HostMethod { module, method }
}
pub const HOST_METHODS: [HostMethod; 24] = [
    host("model", "prepare"),
    host("model", "materialize_prepared_request"),
    host("model", "stream"),
    host("model", "stream_next"),
    host("model", "close_stream"),
    host("model", "get_metadata"),
    host("budget", "estimate_request_input"),
    host("budget", "evaluate_request_budget"),
    host("budget", "estimate_usage_cost"),
    host("turn", "drain_steer"),
    host("turn", "drain_or_close_steer"),
    host("turn", "persist_compaction"),
    host("capability", "execute"),
    host("capability", "execute_batch"),
    host("capability", "list_tools"),
    host("capability", "plan_todo"),
    host("permission", "decide"),
    host("context", "prepare_for_model"),
    host("context", "apply_tool_results"),
    host("context", "recover_from_model_error"),
    host("context", "capture_turn"),
    host("context", "try_auto_compact"),
    host("lifecycle", "dispatch"),
    host("event", "emit"),
];
pub fn host_method(module: &str, method: &str) -> Option<HostMethod> {
    HOST_METHODS
        .iter()
        .copied()
        .find(|m| m.module == module && m.method == method)
}

#[cfg(test)]
mod tests {
    #[test]
    fn host_inventory_matches_the_only_protocol_inventory() {
        use super::*;
        use pilotdeck_protocol::module::*;
        let modules = [
            ("model", HOST_MODEL_METHODS),
            ("budget", HOST_BUDGET_METHODS),
            ("turn", HOST_TURN_METHODS),
            ("capability", HOST_CAPABILITY_METHODS),
            ("permission", HOST_PERMISSION_METHODS),
            ("context", HOST_CONTEXT_METHODS),
            ("lifecycle", HOST_LIFECYCLE_METHODS),
            ("event", HOST_EVENT_METHODS),
        ];
        let expected: std::collections::BTreeSet<_> = modules
            .iter()
            .flat_map(|(module, methods)| methods.iter().map(move |method| (*module, *method)))
            .collect();
        let actual: std::collections::BTreeSet<_> = HOST_METHODS
            .iter()
            .map(|method| (method.module, method.method))
            .collect();
        assert_eq!(actual, expected);
        assert_eq!(actual.len(), 24);
    }
}
