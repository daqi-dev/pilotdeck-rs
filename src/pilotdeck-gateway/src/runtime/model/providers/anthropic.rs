//! Original Anthropic protocol, shared with the independent model provider.
pub(crate) use pilotdeck_model_anthropic::anthropic::StreamState;
pub(super) use pilotdeck_model_anthropic::anthropic::event;
use pilotdeck_protocol::JsValue;
use crate::runtime::model::http::ModelStreamError;
pub(super) fn convert(error:pilotdeck_model_anthropic::AdapterError)->ModelStreamError {match error {pilotdeck_model_anthropic::AdapterError::Request{code,message,details}=>ModelStreamError::Request{code,message,details},pilotdeck_model_anthropic::AdapterError::Provider(value)=>ModelStreamError::Provider(value),pilotdeck_model_anthropic::AdapterError::Thrown{name,message}=>ModelStreamError::Thrown{name,message}}}
pub(crate) fn build_request(request:&JsValue,model:&JsValue,provider:&JsValue)->Result<JsValue,ModelStreamError>{pilotdeck_model_anthropic::anthropic::build_request(request,model,provider).map_err(convert)}
pub(crate) fn normalize_stream_event(raw:&JsValue,state:&mut StreamState)->Result<Vec<JsValue>,ModelStreamError>{pilotdeck_model_anthropic::anthropic::normalize_stream_event_with_limit(raw,state,crate::runtime::model::http::string_limit()).map_err(convert)}
