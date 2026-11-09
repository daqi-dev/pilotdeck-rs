//! Gateway composition submit host: [`GatewaySubmitHost`] services and admission wiring.
use super::admission::{
    GatewaySubmitAdmission, SubmitAdmissionOptions, SubmitModelAssertion, SubmitSdkSessionConfig,
    SubmitTaskBudgetSnapshot,
};
use super::execute_payload::{AgentConfig, serialize_agent_config};
use super::submit::{NativeSubmitSnapshots, SubmitError, SubmitFuture, SubmitSnapshots};
use super::submit_host::{
    GatewaySubmitHost, NativeSubmitReplacement, SubmitAdmission, SubmitAttachmentLease,
    SubmitCwdService, SubmitHostServices, validate_sdk_session_config,
};
use super::submit_host::NativeSubmitProviders;
use crate::metadata::object;
use crate::runtime::interaction::GatewayInteractionCoordinator;
use crate::runtime::sdk_session::ModelRef;
use crate::runtime::session::{SessionOwner, TurnCompletionHandle};
use crate::storage::{RunRef, TaskBudgetScope};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

/// Host-injected composition context retained for request routing.
pub struct GatewayCompositionBundle {
    pub owner: SessionOwner,
    pub interactions: Arc<GatewayInteractionCoordinator>,
    pub providers: NativeSubmitProviders,
    pub cwd: JsString,
    pub policies: crate::cron::native::SessionPolicies,
    pub activity: Arc<crate::cron::activity::Activity>,
    pub failure_guard_enabled: bool,
    pub abort_turn_timeout_ms: u64,
    pub max_output_tokens: Option<u64>,
}

struct CompositionCwd;
impl SubmitCwdService for CompositionCwd {
    fn set(&self, _: &JsString, _: &JsString) -> Result<(), SubmitError> {
        Ok(())
    }
}

pub struct CompositionSubmitServices {
    pub bundle: Arc<GatewayCompositionBundle>,
    pending_model: Mutex<Option<JsValue>>,
}

fn permission_rule(entry: &str, behavior: &str) -> JsValue {
    object([
        ("source", "policy".into()),
        ("behavior", behavior.into()),
        ("toolName", entry.into()),
    ])
}

fn policy_strings(value: Option<&JsValue>, key: &str) -> Vec<String> {
    value
        .and_then(|value| value.get(key))
        .and_then(|value| match value {
            JsValue::Array(items) => Some(items),
            _ => None,
        })
        .into_iter()
        .flatten()
        .filter_map(JsValue::as_string)
        .filter_map(|value| value.to_utf8().ok())
        .collect()
}

fn apply_permission_policies(
    agent: &mut JsObject,
    organization: Option<&crate::runtime::sdk_session::OrganizationPolicy>,
    sdk: Option<&JsValue>,
) {
    let organization_permissions = organization.and_then(|policy| policy.permissions.as_ref());
    let sdk_permissions = sdk.and_then(|sdk| sdk.get("managedPermissions"));
    let force_plan = organization_permissions.is_some_and(|policy| policy.default_plan)
        || sdk_permissions
            .and_then(|policy| policy.get("defaultMode"))
            .and_then(JsValue::as_string)
            .is_some_and(|mode| mode.equals_str("plan"));
    let disable_prompt = organization_permissions.and_then(|policy| policy.can_prompt) == Some(false)
        || sdk_permissions
            .and_then(|policy| policy.get("canPrompt"))
            .and_then(JsValue::as_bool)
            == Some(false);
    let organization_deny = organization_permissions
        .map(|policy| policy.deny.clone())
        .unwrap_or_default();
    let organization_ask = organization_permissions
        .map(|policy| policy.ask.clone())
        .unwrap_or_default();
    let sdk_deny = policy_strings(sdk_permissions, "deny");
    let sdk_ask = policy_strings(sdk_permissions, "ask");
    if !force_plan
        && !disable_prompt
        && organization_deny.is_empty()
        && organization_ask.is_empty()
        && sdk_deny.is_empty()
        && sdk_ask.is_empty()
    {
        return;
    }
    let mut context = agent
        .get("permissionContext")
        .and_then(JsValue::as_object)
        .cloned()
        .unwrap_or_default();
    if force_plan {
        agent.insert("permissionMode".into(), "plan".into());
        context.insert("mode".into(), "plan".into());
    }
    if disable_prompt {
        context.insert("canPrompt".into(), JsValue::Bool(false));
        context.insert("policyCanPrompt".into(), JsValue::Bool(false));
    }
    let mut rules = context
        .get("rules")
        .and_then(JsValue::as_object)
        .cloned()
        .unwrap_or_default();
    for (key, organization_entries, sdk_entries) in [
        ("deny", organization_deny, sdk_deny),
        ("ask", organization_ask, sdk_ask),
    ] {
        let mut entries: Vec<JsValue> = organization_entries
            .iter()
            .chain(&sdk_entries)
            .map(|entry| permission_rule(entry, key))
            .collect();
        if let Some(JsValue::Array(existing)) = rules.get(key) {
            entries.extend(existing.clone());
        }
        rules.insert(key.into(), JsValue::Array(entries));
    }
    context.insert("rules".into(), JsValue::Object(rules));
    agent.insert("permissionContext".into(), JsValue::Object(context));
}

pub(crate) fn parse_positive_int_prefix(raw: &str) -> Option<f64> {
    let trimmed = raw.trim();
    let rest = trimmed.strip_prefix('+').unwrap_or(trimmed);
    let digits_len = rest.bytes().take_while(|b| b.is_ascii_digit()).count();
    if digits_len == 0 {
        return None;
    }
    let num = rest[..digits_len].parse::<i64>().ok()?;
    if num <= 0 {
        return None;
    }
    Some(num as f64)
}

pub(crate) fn resolve_sdk_model_or_snapshot(
    sdk_agent_settings: Option<&JsObject>,
    snapshot_agent: Option<&JsValue>,
    fallback_provider: &JsString,
    fallback_model: &JsString,
) -> (String, String) {
    let requested_model_str = sdk_agent_settings
        .and_then(|s| s.get("model"))
        .and_then(JsValue::as_string)
        .and_then(|s| s.to_utf8().ok());

    if let Some(model_str) = requested_model_str.as_ref().filter(|s| !s.is_empty())
        && let Some(slash) = model_str.find('/')
        && slash > 0
        && slash < model_str.len() - 1
    {
        return (
            model_str[..slash].to_string(),
            model_str[slash + 1..].to_string(),
        );
    }

    if let Some(agent_model) = snapshot_agent.and_then(|a| a.get("model")) {
        if let JsValue::Object(map) = agent_model {
            let p = map
                .get("provider")
                .and_then(JsValue::as_string)
                .and_then(|s| s.to_utf8().ok());
            let m = map
                .get("model")
                .and_then(JsValue::as_string)
                .and_then(|s| s.to_utf8().ok());
            if let (Some(p), Some(m)) = (p, m)
                && !p.is_empty()
                && !m.is_empty()
            {
                return (p, m);
            }
        } else if let Some(s) = agent_model.as_string().and_then(|s| s.to_utf8().ok())
            && let Some(slash) = s.find('/')
            && slash > 0
            && slash < s.len() - 1
        {
            return (s[..slash].to_string(), s[slash + 1..].to_string());
        }
    }

    (
        fallback_provider.to_utf8().unwrap_or_default(),
        fallback_model.to_utf8().unwrap_or_default(),
    )
}

pub(crate) fn resolve_model_token_caps(
    snapshot: Option<&JsValue>,
    provider: &str,
    model: &str,
) -> (Option<f64>, Option<f64>) {
    let model_def = snapshot
        .and_then(|s| s.get("config"))
        .and_then(|c| c.get("model"))
        .and_then(|m| m.get("providers"))
        .and_then(|p| p.get(provider))
        .and_then(|p| p.get("models"))
        .and_then(|m| m.get(model));

    let cap_context = model_def
        .and_then(|def| {
            def.get("capabilities")
                .and_then(|c| c.get("maxContextTokens").or_else(|| c.get("contextWindow")))
                .or_else(|| def.get("maxContextTokens"))
                .or_else(|| def.get("max_context_tokens"))
        })
        .and_then(JsValue::as_number);

    let cap_output = model_def
        .and_then(|def| {
            def.get("capabilities")
                .and_then(|c| c.get("maxOutputTokens"))
                .or_else(|| def.get("maxOutputTokens"))
                .or_else(|| def.get("max_output_tokens"))
        })
        .and_then(JsValue::as_number);

    (cap_context, cap_output)
}

fn explicit_model(input: &JsValue) -> Result<Option<ModelRef>, SubmitError> {
    let selection = input
        .get("modelSelection")
        .filter(|selection| {
            selection
                .get("mode")
                .and_then(JsValue::as_string)
                .is_some_and(|mode| mode.equals_str("model"))
        })
        .or_else(|| input.get("modelOverride"))
        .filter(|value| !value.is_undefined() && !matches!(value, JsValue::Null));
    selection
        .map(|selection| {
            let required = |field| {
                selection
                    .get(field)
                    .and_then(JsValue::as_string)
                    .cloned()
                    .ok_or_else(|| {
                        SubmitError::new(
                            "INVALID_MODEL_OVERRIDE",
                            "A model selection requires mode, provider, and model.",
                        )
                    })
            };
            Ok(ModelRef {
                provider: required("provider")?,
                model: required("model")?,
            })
        })
        .transpose()
}

/// The execute decoder reads speed/thinking from modelOverride, never from the
/// model_selection_changed event or a numeric reasoning field.
fn execute_model_override(selection: &JsValue) -> Result<JsValue, SubmitError> {
    let mut fields = JsObject::new();
    for key in ["provider", "model", "temperature", "speed"] {
        if let Some(value) = selection.get(key).filter(|value| !value.is_undefined()) {
            fields.insert(key.into(), value.clone());
        }
    }
    if let Some(reasoning) = selection
        .get("reasoning")
        .filter(|value| !value.is_undefined())
    {
        let mode = [
            (0.0, "off"),
            (0.2, "minimal"),
            (0.4, "low"),
            (0.6, "medium"),
            (0.8, "high"),
            (0.9, "xhigh"),
            (1.0, "max"),
        ]
        .into_iter()
        .find_map(|(value, mode)| (reasoning.as_number() == Some(value)).then_some(mode))
        .ok_or_else(|| {
            SubmitError::new(
                "UNSUPPORTED_MODEL_PARAMETER",
                format!(
                    "Unsupported reasoning value: {}",
                    crate::js_value::display(reasoning).unwrap_or_default()
                ),
            )
        })?;
        fields.insert(
            "thinking".into(),
            object([
                (
                    "enabled",
                    JsValue::Bool(reasoning.as_number().is_some_and(|value| value > 0.0)),
                ),
                ("mode", mode.into()),
            ]),
        );
    }
    Ok(JsValue::Object(fields))
}

fn string_of(value: &JsValue, key: &str) -> JsString {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .cloned()
        .unwrap_or_default()
}

fn project_key_or_cwd(cwd: &JsString, project: Option<&JsString>) -> JsString {
    project
        .filter(|value| !value.is_empty())
        .cloned()
        .unwrap_or_else(|| cwd.clone())
}

fn wants_workspace(input: &JsValue) -> bool {
    input.get("attachments").is_some() || input.get("uploadedAttachments").is_some()
}

fn agent_permission_context(
    policy: &crate::cron::native::SessionPolicy,
) -> Result<JsValue, SubmitError> {
    crate::cron::native::to_js(&serde_json::json!({"mode":policy.permission_mode,"canPrompt":policy.can_prompt,"bypassAvailable":true,"rules":policy.permission_rules}))
        .map_err(|e|SubmitError::new(&e.code,e.message))
}

impl CompositionSubmitServices {
    fn task_budget_for(
        &self,
        input: &JsValue,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Option<(TaskBudgetScope, f64, f64)>, SubmitError>> + Send + '_>> {
        let bundle = self.bundle.clone();
        let input = input.clone();
        Box::pin(async move {
            task_budget_inner(&bundle, &input).await
        })
    }

    fn transcript_path(&self, session: &JsString, project_root: &JsString) -> PathBuf {
        let cwd = PathBuf::from(self.bundle.cwd.to_utf8().unwrap_or_default());
        let project = project_root.to_utf8().unwrap_or_default();
        crate::session_chat_dir(&self.bundle.providers.pilot_home, &cwd, &project).join(format!(
            "{}.jsonl",
            crate::storage::sanitize_session_id(session)
        ))
    }

    async fn prepare_prepared_input(
        &self,
        input: &JsValue,
        run: &RunRef,
    ) -> Result<JsValue, SubmitError> {
                    if run.run_id.is_empty() {
                return Err(SubmitError::new(
                    "gateway_submit_failed",
                    "Turn identity is missing.",
                ));
            }
            if let Ok(params) = crate::cron::native::from_js(input) {
                self.bundle.activity.begin(
                    &format!(
                        "native:{}",
                        String::from_utf16_lossy(run.session_key.units())
                    ),
                    &run.run_id.to_utf8().unwrap_or_default(),
                    &params,
                );
            }
            let policy = self.bundle.policies.lock().unwrap().get(&run.session_key).cloned();
            let effective_cwd = policy
                .as_ref()
                .map(|p| JsString::from(p.cwd.to_string_lossy().as_ref()))
                .unwrap_or_else(|| self.bundle.cwd.clone());
            let project = string_of(input, "projectKey");
            let root = project_key_or_cwd(&effective_cwd, Some(&project));
            let config_input = match crate::config::routes::native::installed(
                &self.bundle.providers.pilot_home,
                &PathBuf::from(self.bundle.cwd.to_utf8().unwrap_or_default()),
            ) {
                Some(state) => Some(
                    crate::config::routes::native::session_input(
                        state,
                        run.session_key.clone(),
                        Some(root.clone()),
                    )
                    .await
                    .map_err(|e| SubmitError::new(e.code.as_str(), e.message.as_str()))?,
                ),
                None => None,
            };
            let sdk = self.bundle.providers.sdk.as_ref().ok_or_else(|| {
                SubmitError::new("gateway_submit_failed", "Native SDK registry is required.")
            })?;
            let (sdk_session_config, organization_policy) = {
                let sdk = sdk.lock().await;
                (
                    sdk.session_config(&run.session_key).cloned(),
                    sdk.options().organization_policy.clone(),
                )
            };
            let user_dialog_kinds = sdk_session_config
                .as_ref()
                .and_then(|config| config.get("userDialogKinds"))
                .cloned()
                .unwrap_or(JsValue::Array(vec![]));
            let selection = super::routes::models::resolve_for_turn(
                &super::routes::models::Models {
                    sdk,
                    owner: &self.bundle.owner,
                    replacement: &self.bundle.providers.replacement,
                    transcripts: &self.bundle.providers.transcripts,
                    pilot_home: &self.bundle.providers.pilot_home,
                },
                &run.session_key,
                &root,
                input,
            )
            .await
            .map_err(|error| {
                SubmitError::new(&error.code, String::from_utf16_lossy(error.message.units()))
            })?;
            let provider = selection
                .get("selection")
                .and_then(|value| value.get("provider"))
                .and_then(JsValue::as_string)
                .cloned()
                .ok_or_else(|| {
                    SubmitError::new("INVALID_MODEL_OVERRIDE", "Resolved provider is missing.")
                })?;
            let model = selection
                .get("selection")
                .and_then(|value| value.get("model"))
                .and_then(JsValue::as_string)
                .cloned()
                .ok_or_else(|| {
                    SubmitError::new("INVALID_MODEL_OVERRIDE", "Resolved model is missing.")
                })?;
            let session_mode = config_input
                .as_ref()
                .and_then(|config| config.overrides.as_ref())
                .and_then(|overrides| overrides.get("permissionMode"))
                .map(|mode| mode.to_value())
                .and_then(|mode| mode.as_string().cloned());
            let turn_input = super::submit_input::prepare_input_with_policy(
                input,
                &effective_cwd,
                &super::submit_input::InputPolicy {
                    session_mode: session_mode.as_ref(),
                    session_base_mode: session_mode.as_ref(),
                    max_turns: organization_policy
                        .as_ref()
                        .and_then(|policy| policy.max_turns),
                    max_budget_usd: organization_policy
                        .as_ref()
                        .and_then(|policy| policy.max_budget_usd),
                    ..Default::default()
                },
            )
            .await?;
            let project = input
                .get("projectKey")
                .and_then(JsValue::as_string)
                .cloned();
            let project_root = project_key_or_cwd(&self.bundle.cwd, project.as_ref());
            if let Some(recover) = &self.bundle.providers.recover_history {
                recover(&object([
                    ("sessionKey", JsValue::String(run.session_key.clone())),
                    ("projectKey", JsValue::String(project_root.clone())),
                ]))?;
            }
            let transcript = self.transcript_path(&run.session_key, &project_root);
            let mut messages = super::history::completed_messages(&transcript)
                .map_err(|error| SubmitError::new("gateway_submit_failed", error.to_string()))?;
            let mut accepted = Vec::new();
            if let Some(JsValue::Array(current)) = turn_input.get("messages") {
                accepted.extend(current.clone());
            }
            if let Some(JsValue::Array(synthetic)) = turn_input.get("syntheticMessages") {
                accepted.extend(synthetic.clone());
            }
            messages.extend(accepted.clone());
            let mut agent = serialize_agent_config(&AgentConfig {
                provider: &provider,
                model: &model,
                cwd: &effective_cwd,
                system_prompt: sdk_session_config
                    .as_ref()
                    .and_then(|config| config.get("systemPrompt"))
                    .and_then(JsValue::as_string),
                append_system_prompt: sdk_session_config
                    .as_ref()
                    .and_then(|config| config.get("appendSystemPrompt"))
                    .and_then(JsValue::as_string),
                plan_mode_instructions: None,
                runtime_context_surface: config_input
                    .as_ref()
                    .and_then(|config| config.snapshot.get("config"))
                    .and_then(|config| config.get("agent"))
                    .and_then(|agent| agent.get("runtimeContextSurface"))
                    .and_then(JsValue::as_string)
                    .map(|surface| {
                        if surface.equals_str("user_message") {
                            "user_message"
                        } else {
                            "system_prompt"
                        }
                    }),
                max_output_tokens: None,
                max_context_tokens: None,
                max_context_messages: None,
                thinking: None,
                tool_choice: None,
                metadata: None,
                subagent_model: None,
                stop_on_structured_output: None,
                json_self_correct: None,
                is_subagent: None,
                run_mode: None,
                permission_mode_before_plan: None,
            });
            if let JsValue::Object(fields) = &mut agent {
                for key in ["runMode", "permissionMode", "permissionContext"] {
                    if let Some(value) = turn_input.get(key) {
                        fields.insert(key.into(), value.clone());
                    }
                }
            }
            if let Some(policy) = &policy
                && let JsValue::Object(agent) = &mut agent
            {
                agent.insert("permissionMode".into(), policy.permission_mode.into());
                agent.insert("permissionContext".into(),crate::cron::native::to_js(&serde_json::json!({
                        "mode":policy.permission_mode,"canPrompt":policy.can_prompt,"bypassAvailable":true,"rules":policy.permission_rules
                    })).map_err(|e|SubmitError::new(&e.code,e.message))?);
            }
            let snapshot_agent = config_input.as_ref().and_then(|config| {
                config
                    .snapshot
                    .get("config")
                    .and_then(|config| config.get("agent"))
            });
            if let Some(config) = &config_input
                && let JsValue::Object(fields) = &mut agent
            {
                if let Some(settings) = config
                    .sdk_config
                    .as_ref()
                    .and_then(|c| c.get("settings"))
                    .and_then(|v| v.get("agent"))
                    .and_then(JsValue::as_object)
                {
                    for (key, value) in settings.entries() {
                        if !key.equals_str("model") && !key.equals_str("thinking") {
                            fields.insert(key.clone(), value.clone());
                        }
                    }
                }
                if let Some(style) = &config.output_style_content {
                    let existing = fields
                        .get("appendSystemPrompt")
                        .and_then(JsValue::as_string)
                        .and_then(|s| s.to_utf8().ok())
                        .unwrap_or_default();
                    fields.insert(
                        "appendSystemPrompt".into(),
                        (if existing.is_empty() {
                            style.clone()
                        } else {
                            format!("{existing}\n\n{style}")
                        })
                        .as_str()
                        .into(),
                    );
                }
            }
            if let JsValue::Object(fields) = &mut agent {
                let sdk_agent_settings = config_input
                    .as_ref()
                    .and_then(|c| c.sdk_config.as_ref())
                    .and_then(|c| c.get("settings"))
                    .and_then(|v| v.get("agent"))
                    .and_then(JsValue::as_object);

                // TS: const requestedModel = resolveSdkModel(sdk?.settings?.agent?.model, agent.model);
                // Turn temporary modelSelection does not participate in default capability lookup.
                let (target_provider, target_model) = resolve_sdk_model_or_snapshot(
                    sdk_agent_settings,
                    snapshot_agent,
                    &provider,
                    &model,
                );

                // TS: read capabilities.maxContextTokens/maxOutputTokens from snapshot catalog
                let (cap_context, cap_output) = resolve_model_token_caps(
                    config_input.as_ref().map(|c| &c.snapshot),
                    &target_provider,
                    &target_model,
                );

                let env_output_tokens = std::env::var("PILOTDECK_MAX_OUTPUT_TOKENS")
                    .ok()
                    .and_then(|v| parse_positive_int_prefix(&v));

                let sdk_context = sdk_agent_settings
                    .and_then(|s| s.get("maxContextTokens"))
                    .and_then(JsValue::as_number);
                let snapshot_context = snapshot_agent
                    .and_then(|a| a.get("maxContextTokens"))
                    .and_then(JsValue::as_number);
                let resolved_context = sdk_context.or(snapshot_context).or(cap_context);

                let sdk_output = sdk_agent_settings
                    .and_then(|s| s.get("maxOutputTokens"))
                    .and_then(JsValue::as_number);
                let snapshot_output = snapshot_agent
                    .and_then(|a| a.get("maxOutputTokens"))
                    .and_then(JsValue::as_number);
                let resolved_output = env_output_tokens
                    .or(sdk_output)
                    .or(snapshot_output)
                    .or(cap_output);

                if let Some(val) = resolved_context {
                    fields.insert("maxContextTokens".into(), JsValue::Number(val));
                }
                if let Some(val) = resolved_output {
                    fields.insert("maxOutputTokens".into(), JsValue::Number(val));
                }

                // thinking 顺序 (TS: SessionAgentConfigBundle.ts:165-171):
                // enforcedSessionSettings.agent.thinking ?? sdkThinking ?? sdk.settings.agent.thinking ?? agent.thinking
                let enforced_thinking = organization_policy
                    .as_ref()
                    .and_then(|op| op.settings.as_ref())
                    .and_then(|s| s.enforced_session_settings.as_ref())
                    .and_then(|es| es.get("agent"))
                    .and_then(|a| a.get("thinking"))
                    .cloned();

                let sdk_thinking = config_input.as_ref().and_then(|c| c.thinking.clone());

                let settings_thinking = sdk_agent_settings.and_then(|s| s.get("thinking")).cloned();

                let snapshot_thinking = snapshot_agent.and_then(|a| a.get("thinking")).cloned();

                let resolved_thinking = enforced_thinking
                    .or(sdk_thinking)
                    .or(settings_thinking)
                    .or(snapshot_thinking);

                if let Some(thinking) = resolved_thinking {
                    fields.insert("thinking".into(), thinking);
                }

                if let Some(policy) = organization_policy.as_ref() {
                    policy.apply_agent_runtime_caps(fields);
                }
                apply_permission_policies(
                    fields,
                    organization_policy.as_ref(),
                    sdk_session_config.as_ref(),
                );
                crate::runtime::session::subagent::apply_sdk_config_with_policy(
                    fields,
                    sdk_session_config.as_ref().or_else(|| {
                        config_input
                            .as_ref()
                            .and_then(|config| config.sdk_config.as_ref())
                    }),
                    snapshot_agent,
                    organization_policy.as_ref(),
                );
                if let Some(max_output_tokens) = self.bundle.max_output_tokens {
                    fields.insert(
                        "maxOutputTokens".into(),
                        JsValue::Number(max_output_tokens as f64),
                    );
                }
            }
            let mut prepared_input = object([
                ("hostUserDialogKinds", user_dialog_kinds),
                ("acceptedMessages", JsValue::Array(accepted)),
                ("agent", agent),
                ("messages", JsValue::Array(messages)),
                ("tools", JsValue::Array(Vec::new())),
                (
                    "modelSelectionSource",
                    selection
                        .get("source")
                        .cloned()
                        .unwrap_or(JsValue::Undefined),
                ),
            ]);
            if let JsValue::Object(fields) = &mut prepared_input {
                for (key, value) in turn_input.entries() {
                    if !key.equals_str("messages") {
                        fields.insert(key.clone(), value.clone());
                    }
                }
                if let Some(policy) = organization_policy.as_ref()
                    && let Some(tool_policy) = policy.tool_policy_value()
                {
                    fields.insert("organizationToolPolicy".into(), tool_policy);
                }
                if let Some(managed_tools) = sdk_session_config
                    .as_ref()
                    .and_then(|config| config.get("managedTools"))
                {
                    fields.insert("managedToolPolicy".into(), managed_tools.clone());
                }
            }
            if selection
                .get("source")
                .and_then(JsValue::as_string)
                .is_some_and(|source| source.equals_str("turn") || source.equals_str("session"))
                && let JsValue::Object(fields) = &mut prepared_input
            {
                fields.insert(
                    "modelOverride".into(),
                    execute_model_override(selection.get("selection").ok_or_else(|| {
                        SubmitError::new("INVALID_MODEL_OVERRIDE", "Resolved selection is missing.")
                    })?)?,
                );
            }
            if let Some((_, total, spent)) = self.task_budget_for(input).await?
                && let JsValue::Object(fields) = &mut prepared_input
            {
                fields.insert("taskBudgetUsd".into(), JsValue::Number(total));
                fields.insert("initialTaskBudgetSpentUsd".into(), JsValue::Number(spent));
            }
            if let (Some(policy), JsValue::Object(payload)) = (&policy, &mut prepared_input) {
                payload.insert("canPrompt".into(), JsValue::Bool(policy.can_prompt));
                payload.insert("canElicit".into(), JsValue::Bool(policy.can_prompt));
                payload.insert("basePermissionMode".into(), policy.permission_mode.into());
                payload.insert(
                    "permissionContext".into(),
                    agent_permission_context(policy)?,
                );
                payload.insert(
                    "excludedTools".into(),
                    JsValue::Array(
                        policy
                            .excluded_tools
                            .iter()
                            .map(|t| JsValue::from(*t))
                            .collect(),
                    ),
                );
            }
            *self.pending_model.lock().unwrap() = Some(selection.clone());
            Ok(prepared_input)
    }
}

async fn task_budget_inner(
    bundle: &GatewayCompositionBundle,
    input: &JsValue,
) -> Result<Option<(TaskBudgetScope, f64, f64)>, SubmitError> {
    let session = string_of(input, "sessionKey");
    let config = match &bundle.providers.sdk {
        Some(sdk) => sdk.lock().await.session_config(&session).cloned(),
        None => None,
    };
    let Some(budget) = config
        .as_ref()
        .and_then(|config| config.get("taskBudget"))
        .filter(|budget| !matches!(budget, JsValue::Null | JsValue::Undefined))
    else {
        return Ok(None);
    };
    let total = budget
        .get("total")
        .and_then(JsValue::as_number)
        .filter(|total| total.is_finite() && *total >= 0.0)
        .ok_or_else(|| SubmitError::new("TASK_BUDGET_UNAVAILABLE", "Invalid configured task budget."))?;
    let scope = if budget
        .get("scope")
        .and_then(JsValue::as_string)
        .is_some_and(|scope| scope.equals_str("project"))
    {
        TaskBudgetScope::Project
    } else {
        TaskBudgetScope::Session
    };
    let store = bundle.providers.task_budget.as_ref().ok_or_else(|| {
        SubmitError::new(
            "CAPABILITY_UNAVAILABLE",
            "Gateway task-budget accounting is unavailable.",
        )
    })?;
    let project = string_of(input, "projectKey");
    let root = project_key_or_cwd(&bundle.cwd, Some(&project));
    let mut store = store.lock().map_err(|_| {
        SubmitError::new("TASK_BUDGET_UNAVAILABLE", "Task budget lock is poisoned.")
    })?;
    let state = store
        .get(&root, scope, &session)
        .map_err(|error| SubmitError::new("TASK_BUDGET_UNAVAILABLE", error.to_string()))?;
    let total = if scope == TaskBudgetScope::Project {
        state
            .and_then(|state| state.total_usd.as_number())
            .unwrap_or(total)
    } else {
        total
    };
    Ok(Some((scope, total, state.map_or(0.0, |state| state.spent_usd))))
}

impl SubmitHostServices for CompositionSubmitServices {
    fn workspace_id(&self, _: &JsValue) -> Result<Option<JsString>, SubmitError> {
        match &self.bundle.providers.snapshots {
            Some(_) => Ok(self
                .bundle
                .providers
                .workspace_id
                .clone()
                .filter(|id| !id.is_empty())),
            None => Ok(None),
        }
    }

    fn snapshots(&self) -> Option<Arc<dyn SubmitSnapshots>> {
        match (&self.bundle.providers.snapshots, &self.bundle.providers.workspace_id) {
            (Some(recorder), Some(_)) => {
                Some(Arc::new(NativeSubmitSnapshots(recorder.clone())) as Arc<dyn SubmitSnapshots>)
            }
            _ => None,
        }
    }

    fn snapshots_required(&self) -> bool {
        self.snapshots().is_some()
    }

    fn cwd(&self) -> Option<Arc<dyn SubmitCwdService>> {
        Some(Arc::new(CompositionCwd))
    }

    fn refresh(&self) -> SubmitFuture<'_, Result<(), SubmitError>> {
        Box::pin(async { Ok(()) })
    }

    fn run_policy(&self) -> Option<JsValue> {
        if self.bundle.failure_guard_enabled {
            Some(object([(
                "failureGuard",
                object([("enabled", JsValue::Bool(true))]),
            )]))
        } else {
            None
        }
    }

    fn resolve_attachments<'a>(
        &'a self,
        input: &'a JsValue,
    ) -> SubmitFuture<'a, Result<SubmitAttachmentLease, SubmitError>> {
        let bundle = self.bundle.clone();
        let input = input.clone();
        Box::pin(async move {
            let cwd = bundle
                .policies
                .lock()
                .unwrap()
                .get(&string_of(&input, "sessionKey"))
                .map(|p| JsString::from(p.cwd.to_string_lossy().as_ref()))
                .unwrap_or_else(|| bundle.cwd.clone());
            let uploaded =
                super::submit_uploads::resolve(&input, &bundle.providers.pilot_home, &cwd)
                    .await?;
            let attachments = uploaded
                .input
                .get("attachments")
                .and_then(|value| match value {
                    JsValue::Array(values) => Some(values.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            Ok(SubmitAttachmentLease {
                attachments,
                release: Box::new(move || {
                    Box::pin(async move { uploaded.release() })
                }),
            })
        })
    }

    fn prepare_attachment_turn<'a>(
        &'a self,
        input: &'a JsValue,
        run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        let this = self;
        let input = input.clone();
        let run = run.clone();
        Box::pin(async move { this.prepare_prepared_input(&input, &run).await })
    }

    fn resolve_model_selection<'a>(
        &'a self,
        _: &'a JsValue,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        let pending = self.pending_model.lock().unwrap().take();
        Box::pin(async move {
            pending.ok_or_else(|| {
                SubmitError::new("gateway_submit_failed", "Turn model selection is missing.")
            })
        })
    }

    fn record_failure<'a>(
        &'a self,
        _: &'a RunRef,
        status: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        let status = status.clone();
        Box::pin(async move {
            if status
                .get("event")
                .and_then(JsValue::as_string)
                .is_some_and(|event| event.equals_str("turn_timeout"))
            {
                return Ok(());
            }
            Ok(())
        })
    }

    fn after_turn(&self, run: &RunRef) -> Result<(), SubmitError> {
        self.bundle.activity.end(
            &format!(
                "native:{}",
                String::from_utf16_lossy(run.session_key.units())
            ),
            &run.run_id.to_utf8().unwrap_or_default(),
        );
        self.bundle
            .providers
            .replacement
            .release_submit_claim(&run.session_key, &run.run_id);
        if let Some(maintenance) = &self.bundle.providers.memory_maintenance {
            let key = run.project_key.as_ref().unwrap_or(&self.bundle.cwd);
            maintenance.schedule(key);
        }
        Ok(())
    }

    fn now_ms(&self) -> f64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |elapsed| elapsed.as_millis() as f64)
    }

    fn abort_turn_timeout_ms(&self) -> u64 {
        self.bundle.abort_turn_timeout_ms
    }
}

struct ChannelAwareSubmitAdmission {
    inner: Arc<GatewaySubmitAdmission>,
    bundle: Arc<GatewayCompositionBundle>,
}

impl SubmitAdmission for ChannelAwareSubmitAdmission {
    fn normalize<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        self.inner.normalize(input)
    }
    fn route_input<'a>(
        &'a self,
        input: &'a mut JsValue,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        let home = self.bundle.providers.pilot_home.clone();
        let cwd = PathBuf::from(String::from_utf16_lossy(self.bundle.cwd.units()));
        let channel_key = String::from_utf16_lossy(string_of(input, "channelKey").units());
        let message_text = String::from_utf16_lossy(string_of(input, "message").units());
        let inner = self.inner.clone();
        let input_owned = input.clone();
        Box::pin(async move {
            let mut replies = Vec::<String>::new();
            let result = crate::config::routes::channel_inbound::production_route_input(
                &home,
                &cwd,
                channel_key.as_str(),
                message_text.as_str(),
                |reply| replies.push(reply.to_owned()),
            );
            if result.slash_handled {
                let mut events = Vec::new();
                for reply in replies {
                    events.push(object([
                        ("type", "channel_command_reply".into()),
                        ("channelKey", channel_key.as_str().into()),
                        ("message", reply.as_str().into()),
                    ]));
                }
                events.push(object([
                    ("type", "turn_completed".into()),
                    ("finishReason", "channel_command".into()),
                ]));
                return Ok(events);
            }
            let mut input = input_owned;
            inner.route_input(&mut input).await
        })
    }
    fn preflight<'a>(
        &'a self,
        input: &'a JsValue,
        run: &'a RunRef,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        self.inner.preflight(input, run)
    }
    fn admitted_preflight<'a>(
        &'a self,
        input: &'a JsValue,
        run: &'a RunRef,
        completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<Vec<JsValue>, SubmitError>> {
        self.inner.admitted_preflight(input, run, completion)
    }
    fn observe_event<'a>(
        &'a self,
        input: &'a JsValue,
        event: &'a JsValue,
    ) -> SubmitFuture<'a, Result<(), SubmitError>> {
        let bundle = self.bundle.clone();
        let input = input.clone();
        let event = event.clone();
        let inner = self.inner.clone();
        Box::pin(async move {
            let is_completion = event
                .get("type")
                .and_then(JsValue::as_string)
                .is_some_and(|kind| kind.equals_str("turn_completed"));
            if is_completion {
                if let Some(store) = &bundle.providers.task_budget {
                    if let Some(budget) = event
                        .get("result")
                        .and_then(|result| result.get("budget"))
                        .filter(|budget| {
                            budget
                                .get("taskBudgetUsd")
                                .is_some_and(|value| !value.is_undefined())
                        })
                    {
                        if let Some(cost) = budget
                            .get("turnSpentUsd")
                            .and_then(JsValue::as_number)
                            .filter(|value| value.is_finite() && *value >= 0.0)
                        {
                            if let Some((scope, _, _)) = task_budget_inner(&bundle, &input).await? {
                                let session = string_of(&input, "sessionKey");
                                let project = string_of(&input, "projectKey");
                                let run = string_of(&input, "runId");
                                let root = project_key_or_cwd(&bundle.cwd, Some(&project));
                                let now = SystemTime::now()
                                    .duration_since(UNIX_EPOCH)
                                    .map_or(0.0, |e| e.as_millis() as f64);
                                let mut store = store.lock().map_err(|_| {
                                    SubmitError::new("TASK_BUDGET_UNAVAILABLE", "Task budget lock is poisoned.")
                                })?;
                                store
                                    .settle(&root, scope, &session, &run, cost, now)
                                    .map_err(|error| {
                                        SubmitError::new("TASK_BUDGET_UNAVAILABLE", error.to_string())
                                    })?;
                            }
                        }
                    }
                }
            }
            inner.observe_event(&input, &event).await
        })
    }
    fn has_sdk_config(&self) -> bool {
        self.inner.has_sdk_config()
    }
    fn validate_sdk_config(&self, config: &JsValue) -> Result<(), SubmitError> {
        self.inner.validate_sdk_config(config)
    }
    fn set_sdk_config<'a>(
        &'a self,
        input: &'a JsValue,
        completion: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>> {
        self.inner.set_sdk_config(input, completion)
    }
    fn reject_pending(&self, session: &JsString, reason: &str) -> Result<(), SubmitError> {
        self.inner.reject_pending(session, reason)
    }
}

struct CompositionSdkConfigPort {
    bundle: Arc<GatewayCompositionBundle>,
}
impl SubmitSdkSessionConfig for CompositionSdkConfigPort {
    fn validate(&self, config: &JsValue) -> Result<(), SubmitError> {
        validate_sdk_session_config(config)
    }
    fn set<'a>(
        &'a self,
        session: &'a JsString,
        config: &'a JsValue,
        project: Option<&'a JsString>,
        _: &'a TurnCompletionHandle,
    ) -> SubmitFuture<'a, Result<bool, SubmitError>> {
        let bundle = self.bundle.clone();
        let session = session.clone();
        let config = config.clone();
        let project = project.cloned();
        Box::pin(async move {
            let Some(sdk) = &bundle.providers.sdk else {
                return Err(SubmitError::new(
                    "CAPABILITY_UNAVAILABLE",
                    "sdk_session_config is unavailable.",
                ));
            };
            let changed = {
                let mut registry = sdk.lock().await;
                registry
                    .set_session_config(&session, &config, project.as_ref(), None)
                    .await
                    .map_err(|error| SubmitError::new(&error.code, error.message))?
            };
            if changed {
                let _ = (&bundle.owner, &session);
            }
            Ok(changed)
        })
    }
}

struct CompositionModelAssertionPort {
    bundle: Arc<GatewayCompositionBundle>,
}
impl SubmitModelAssertion for CompositionModelAssertionPort {
    fn assert(
        &self,
        session: &JsString,
        selection: Option<&JsValue>,
        project: Option<&JsString>,
    ) -> Result<(), SubmitError> {
        let Some(sdk) = &self.bundle.providers.sdk else {
            return Ok(());
        };
        let explicit = selection.and_then(|selection| {
            let provider = selection.get("provider")?.as_string()?;
            let model = selection.get("model")?.as_string()?;
            Some(ModelRef {
                provider: provider.clone(),
                model: model.clone(),
            })
        });
        let root = project_key_or_cwd(&self.bundle.cwd, project);
        tokio::task::block_in_place(|| {
            tokio::runtime::Handle::current().block_on(async {
                sdk.lock()
                    .await
                    .assert_model_allowed(session, explicit.as_ref(), Some(&root))
                    .map_err(|error| SubmitError::new(&error.code, error.message))
            })
        })
    }
}

struct CompositionTaskBudgetPort {
    bundle: Arc<GatewayCompositionBundle>,
}
impl SubmitTaskBudgetSnapshot for CompositionTaskBudgetPort {
    fn snapshot<'a>(
        &'a self,
        session: &'a JsString,
        project: Option<&'a JsString>,
    ) -> SubmitFuture<'a, Result<JsValue, SubmitError>> {
        let bundle = self.bundle.clone();
        let session = session.clone();
        let project = project.cloned();
        Box::pin(async move {
            let input = object([
                ("sessionKey", JsValue::String(session)),
                (
                    "projectKey",
                    project.map(JsValue::String).unwrap_or(JsValue::Undefined),
                ),
            ]);
            let Some((_, total, spent)) = task_budget_inner(&bundle, &input).await? else {
                return Ok(object([("totalUsd", JsValue::Number(0.0)), ("spentUsd", JsValue::Number(0.0))]));
            };
            Ok(object([
                ("totalUsd", JsValue::Number(total)),
                ("spentUsd", JsValue::Number(spent)),
            ]))
        })
    }
}

pub fn build_composition_submit_host(
    owner: SessionOwner,
    interactions: Arc<GatewayInteractionCoordinator>,
    providers: NativeSubmitProviders,
    cwd: JsString,
    policies: crate::cron::native::SessionPolicies,
    activity: Arc<crate::cron::activity::Activity>,
    failure_guard_enabled: bool,
    abort_turn_timeout_ms: u64,
    max_output_tokens: Option<u64>,
) -> (Arc<GatewaySubmitHost>, Arc<GatewayCompositionBundle>) {
    let bundle = Arc::new(GatewayCompositionBundle {
        owner: owner.clone(),
        interactions: interactions.clone(),
        providers,
        cwd,
        policies,
        activity,
        failure_guard_enabled,
        abort_turn_timeout_ms,
        max_output_tokens,
    });
    let services = Arc::new(CompositionSubmitServices {
        bundle: bundle.clone(),
        pending_model: Mutex::new(None),
    });
    let mut admission_options =
        SubmitAdmissionOptions::new(bundle.providers.replacement.clone());
    admission_options.interactions = interactions;
    admission_options.sdk_config = Some(Arc::new(CompositionSdkConfigPort {
        bundle: bundle.clone(),
    }));
    admission_options.model_assertion = Some(Arc::new(CompositionModelAssertionPort {
        bundle: bundle.clone(),
    }));
    if bundle.providers.task_budget.is_some() {
        admission_options.task_budget = Some(Arc::new(CompositionTaskBudgetPort {
            bundle: bundle.clone(),
        }));
    }
    let admission = Arc::new(ChannelAwareSubmitAdmission {
        inner: Arc::new(GatewaySubmitAdmission::new(admission_options)),
        bundle: bundle.clone(),
    });
    let host = Arc::new(GatewaySubmitHost::new(
        services,
        Arc::new(NativeSubmitReplacement((*bundle.providers.replacement).clone())),
        admission,
    ));
    (host, bundle)
}

const USD: char = '\u{24}';

fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
    value.get(key).unwrap_or({
        static UNDEFINED: JsValue = JsValue::Undefined;
        &UNDEFINED
    })
}

#[cfg(test)]
mod policy_tests {
    use super::apply_permission_policies;
    use crate::metadata::object;
    use pilotdeck_protocol::{JsObject, JsValue, parse_json};

    #[test]
    fn organization_and_sdk_permission_policies_merge_into_agent() {
        let mut organization = crate::runtime::sdk_session::OrganizationPolicy::default();
        organization
            .apply_execution_limits_json(&serde_json::json!({
                "permissions": {
                    "deny": ["Bash(rm *)"],
                    "ask": ["Write"],
                    "defaultMode": "plan",
                    "canPrompt": false
                }
            }))
            .unwrap();
        let sdk = parse_json(
            r#"{"managedPermissions":{"deny":["WebFetch"],"ask":["Bash(git *)"],"defaultMode":"plan","canPrompt":false}}"#,
        )
        .unwrap();
        let mut agent = parse_json(
            r#"{"permissionMode":"default","permissionContext":{"mode":"default","canPrompt":true,"rules":{"deny":[{"source":"session","behavior":"deny","toolName":"legacy"}],"ask":[]}}}"#,
        )
        .unwrap()
        .as_object()
        .unwrap()
        .clone();
        apply_permission_policies(&mut agent, Some(&organization), Some(&sdk));
        assert_eq!(agent.get("permissionMode"), Some(&"plan".into()));
        let context = agent.get("permissionContext").unwrap();
        assert_eq!(context.get("mode"), Some(&"plan".into()));
        assert_eq!(context.get("canPrompt"), Some(&JsValue::Bool(false)));
        let deny = match context.get("rules").unwrap().get("deny").unwrap() {
            JsValue::Array(value) => value,
            other => panic!("unexpected deny rules: {other:?}"),
        };
        assert_eq!(deny.len(), 3);
        assert_eq!(deny[2].get("source"), Some(&"session".into()));
    }

    #[test]
    fn tool_policy_and_runtime_caps_apply_on_prepared_agent_fields() {
        let mut organization = crate::runtime::sdk_session::OrganizationPolicy::default();
        organization
            .apply_execution_limits_json(&serde_json::json!({
                "tools": {"deny": ["bash"]},
                "limits": {"maxBudgetUsd": 2.5, "maxTurns": 3}
            }))
            .unwrap();
        let mut fields = JsObject::new();
        fields.insert("maxOutputTokens".into(), JsValue::Number(8000.0));
        organization.apply_agent_runtime_caps(&mut fields);
        assert_eq!(fields.get("maxOutputTokens"), Some(&JsValue::Number(8000.0)));
        assert!(organization.tool_policy_value().is_some());
        let policy = super::super::submit_input::InputPolicy {
            max_turns: organization.max_turns,
            max_budget_usd: organization.max_budget_usd,
            ..Default::default()
        };
        assert_eq!(policy.max_budget_usd, Some(2.5));
        assert_eq!(policy.max_turns, Some(3.0));
    }
}
