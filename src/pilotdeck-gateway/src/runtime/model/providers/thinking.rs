//! Original thinking plan implementation shared with independent providers.
pub(crate) use pilotdeck_model_anthropic::thinking::ThinkingPlan;
use pilotdeck_protocol::JsValue;
use crate::runtime::model::http::ModelStreamError;
pub(crate) fn resolve(request:&JsValue,provider:&JsValue,model:&JsValue)->Result<ThinkingPlan,ModelStreamError>{pilotdeck_model_anthropic::thinking::resolve(request,provider,model).map_err(super::anthropic::convert)}
pub(crate) fn throw_if_unsupported(plan:&ThinkingPlan,request:&JsValue)->Result<(),ModelStreamError>{pilotdeck_model_anthropic::thinking::throw_if_unsupported(plan,request).map_err(super::anthropic::convert)}
