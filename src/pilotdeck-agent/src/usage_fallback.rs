//! Budget fallback separates source text construction from the real tokenizer.
//! The owning turn must supply an actually computed o200k_base token count.
use crate::ports::PortResult;
use pilotdeck_protocol::JsString;
use pilotdeck_protocol::model::BoundarySnapshot;

pub struct BudgetUsageInput<'a> {
    pub usage: Option<&'a BoundarySnapshot>,
    pub estimated_input_tokens: Option<f64>,
    pub assistant_message: &'a BoundarySnapshot,
}
#[derive(Debug, Clone)]
pub enum BudgetUsageSelection {
    /// Finite provider usage wins before response text conversion.
    Original(Option<BoundarySnapshot>),
    NeedsOutputEstimate(BudgetUsageContinuation),
}
#[derive(Debug, Clone)]
pub struct BudgetUsageContinuation {
    pub(crate) response_text: JsString,
    pub(crate) original_usage: Option<BoundarySnapshot>,
    pub(crate) estimated_input_tokens: Option<f64>,
}
impl BudgetUsageContinuation {
    pub fn response_text(&self) -> &JsString {
        &self.response_text
    }
    /// Must receive the count from the real pinned ordinary-text tokenizer;
    /// no default zero or provider cost is implied by this pure composition.
    pub fn resolve(self, output_tokens: u64) -> Option<BoundarySnapshot> {
        crate::usage_fallback_policy::resolve(self, output_tokens)
    }
}
pub struct OriginalBudgetUsageFallback;
impl OriginalBudgetUsageFallback {
    pub fn select(&self, input: BudgetUsageInput<'_>) -> PortResult<BudgetUsageSelection> {
        crate::usage_fallback_policy::select(input)
    }
}
