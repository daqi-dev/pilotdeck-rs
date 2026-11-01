use super::Handler;

// Keep this inventory in sync with Requests::management_request's entry gate
// in ../management.rs and SkillMethod::ALL. registry_management_gate_agrees
// exercises both the direct gate and the registry's pre-admission handler.
pub(super) const METHODS: &[&str] = &[
    "snapshot_list",
    "snapshot_get",
    "snapshot_restore",
    "manager_sessions",
    "manager_browsers",
    "commands_list",
    "skill_list",
    "skill_read",
    "skill_write",
    "skill_create",
    "skill_delete",
    "skill_import",
    "skill_validate",
    "skill_scan",
];

pub(super) const HANDLER: Handler = Handler::BeforeAdmission(|requests, method, params| {
    Box::pin(requests.management_request(method, params))
});
