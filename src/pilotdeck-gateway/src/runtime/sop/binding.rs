use super::{common::*, controller::StepBinding, tool};
use crate::{
    host::{
        HostFuture,
        modules::{capability, context, model},
    },
    runtime::turn::loop_control::SopRunBinding,
};
use pilotdeck_protocol::{JsObject, JsValue};
use std::sync::Arc;
impl SopRunBinding for StepBinding {
    fn tools(
        &self,
        base: Arc<dyn capability::ToolExecution>,
    ) -> Arc<dyn capability::ToolExecution> {
        Arc::new(tool::Tools {
            base,
            binding: self.clone(),
        })
    }
    fn model(&self, base: Arc<dyn model::ModelExecution>) -> Arc<dyn model::ModelExecution> {
        Arc::new(Model {
            base,
            binding: self.clone(),
        })
    }
    fn materializer(
        &self,
        base: Option<Arc<dyn model::RequestMaterializer>>,
    ) -> Option<Arc<dyn model::RequestMaterializer>> {
        base.map(|base| {
            Arc::new(Materializer {
                base,
                binding: self.clone(),
            }) as Arc<dyn model::RequestMaterializer>
        })
    }
    fn context(&self, base: Arc<dyn context::ContextRuntime>) -> Arc<dyn context::ContextRuntime> {
        Arc::new(Context {
            base,
            binding: self.clone(),
        })
    }
}
/// Projection is deliberately shared by prepare and candidate materialization.
/// Stream consumes the already prepared request and its original route handle.
fn project(request: &mut JsObject, step: &JsValue) {
    let required = !strings(field(step, "requiredToolNames")).is_empty()
        || field(field(step, "node"), "type") == &JsValue::from("knowledge_query")
        || field(step, "optionalKnowledge").as_object().is_some();
    request.insert(
        "toolChoice".into(),
        if required {
            "required".into()
        } else {
            crate::metadata::object([("type", "tool".into()), ("name", tool::CONTROL.into())])
        },
    );
    if strings(field(step, "requiredToolNames")).is_empty()
        && let Some(JsValue::Array(tools)) = request.get("tools")
    {
        let filtered = tools
            .iter()
            .filter(|t| field(t, "name") == &JsValue::from(tool::CONTROL)
                || (field(step, "optionalKnowledge").as_object().is_some() && field(t, "name") == &JsValue::from("knowledge_query")))
            .map(|t| {
                let mut value = t.clone();
                if field(&value, "name") == &JsValue::from("knowledge_query") {
                    super::common::set(&mut value, "inputSchema", super::authority::optional_schema());
                }
                value
            })
            .collect();
        request.insert("tools".into(), JsValue::Array(filtered));
    }
}
struct Model {
    base: Arc<dyn model::ModelExecution>,
    binding: StepBinding,
}
impl model::ModelExecution for Model {
    fn prepare(
        &self,
        mut input: model::PrepareInput,
    ) -> model::ModelFuture<model::PreparedInvocation> {
        let mut request = object(&input.request);
        project(&mut request, &self.binding.step());
        input.request = JsValue::Object(request);
        self.base.prepare(input)
    }
    fn stream(&self, input: model::StreamInput) -> model::ModelFuture<Box<dyn model::ModelStream>> {
        self.base.stream(input)
    }
}
struct Materializer {
    base: Arc<dyn model::RequestMaterializer>,
    binding: StepBinding,
}
impl model::RequestMaterializer for Materializer {
    fn materialize(
        &self,
        prepared: model::PreparedInvocation,
        mut candidate: JsObject,
    ) -> model::ModelFuture<JsValue> {
        project(&mut candidate, &self.binding.step());
        self.base.materialize(prepared, candidate)
    }
}
struct Context {
    base: Arc<dyn context::ContextRuntime>,
    binding: StepBinding,
}
impl context::ContextRuntime for Context {
    fn supports(&self, operation: context::Operation) -> bool {
        self.base.supports(operation)
    }
    fn prepare_for_model(&self, mut input: context::ContextInput) -> HostFuture<JsValue> {
        let base = self.base.clone();
        let binding = self.binding.clone();
        Box::pin(async move {
            let instruction = binding
                .instruction(input.fields.get("stepId").unwrap_or(&JsValue::Undefined))
                .await?;
            let prompt = super::controller::prompt::with_existing(
                input
                    .fields
                    .get("appendSystemPrompt")
                    .and_then(JsValue::as_string),
                instruction,
            );
            input
                .fields
                .insert("appendSystemPrompt".into(), JsValue::String(prompt));
            base.prepare_for_model(input).await
        })
    }
    fn apply_tool_results(&self, input: context::ContextInput) -> HostFuture<JsValue> {
        self.base.apply_tool_results(input)
    }
    fn recover_from_model_error(&self, input: context::ContextInput) -> HostFuture<JsValue> {
        self.base.recover_from_model_error(input)
    }
    fn capture_turn(&self, input: context::ContextInput) -> HostFuture<JsValue> {
        self.base.capture_turn(input)
    }
    fn try_auto_compact(&self, input: context::ContextInput) -> HostFuture<JsValue> {
        self.base.try_auto_compact(input)
    }
}
