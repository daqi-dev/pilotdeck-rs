//! Pure AgentLoop-owned request transforms. Routing effects remain host-owned.
use crate::ports::{PortResult, Route};
use pilotdeck_protocol::model::BoundarySnapshot;

pub struct BudgetCandidateInput<'a> {
    pub base_request: &'a BoundarySnapshot,
    pub preview_request: &'a BoundarySnapshot,
}
pub struct PreparedWindowInput<'a> {
    pub prepared_request: &'a BoundarySnapshot,
    pub candidate_request: &'a BoundarySnapshot,
    pub route: &'a Route,
}
pub struct RequestCapsInput<'a> {
    pub request: &'a BoundarySnapshot,
    pub route: &'a Route,
    pub current_max_output_tokens: Option<f64>,
    /// Source attempt/requested cap uses nullish presence, not truthiness.
    pub attempt_override_present: bool,
}
pub struct OriginalRequestMaterialization;
impl OriginalRequestMaterialization {
    pub fn budget_candidate(
        &self,
        input: BudgetCandidateInput<'_>,
    ) -> PortResult<BoundarySnapshot> {
        crate::request_materialization_policy::budget_candidate(input)
    }
    pub fn default_prepared_window(
        &self,
        input: PreparedWindowInput<'_>,
    ) -> PortResult<BoundarySnapshot> {
        crate::request_materialization_policy::default_prepared_window(input)
    }
    pub fn apply_caps(&self, input: RequestCapsInput<'_>) -> PortResult<BoundarySnapshot> {
        crate::request_materialization_policy::apply_caps(input)
    }
}
