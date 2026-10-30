//! Default LSP and model-backed title owners for the native composition root.
use crate::{
    metadata::object,
    runtime::{
        model::router::{AbortFlag, RouterModelInvocation},
        project::{
            DisposeFuture, Family, FamilyResource, Resource, ResourceFamilySource, RuntimeError,
            RuntimeResult, StageContext, StageSink, model::ModelResources,
        },
        turn::runner::tail::TitleProvider,
    },
};
use pilotdeck_protocol::{JsString, JsValue, parse_json};
use std::{
    any::Any,
    path::Path,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

/// The TS default has no registered language providers. Keeping a real owner
/// preserves disposed/query validation, including the explicit unavailable error.
#[derive(Default)]
pub struct DefaultLsp {
    disposed: AtomicBool,
}
impl DefaultLsp {
    pub fn bind(&self) -> Result<Self, RuntimeError> {
        if self.disposed.load(Ordering::Acquire) {
            return self.query("", "", 0.0, 0.0).map(|_| Self::default());
        }
        Ok(Self::default())
    }
    pub fn query(
        &self,
        workspace: &str,
        path: &str,
        line: f64,
        character: f64,
    ) -> Result<JsValue, RuntimeError> {
        let (code, message) = if self.disposed.load(Ordering::Acquire) {
            ("LSP_DISPOSED", "LSP service is disposed".to_owned())
        } else if crate::metadata::js_trim(workspace).is_empty() {
            (
                "LSP_WORKSPACE_REQUIRED",
                "LSP query requires a workspace root".to_owned(),
            )
        } else if crate::metadata::js_trim(path).is_empty() {
            (
                "LSP_MALFORMED_RESPONSE",
                "LSP query requires a file path".to_owned(),
            )
        } else if [line, character]
            .iter()
            .any(|n| !n.is_finite() || n.fract() != 0.0 || *n < 0.0)
        {
            (
                "LSP_MALFORMED_RESPONSE",
                "LSP position must contain non-negative integer coordinates".to_owned(),
            )
        } else {
            let root = crate::metadata::resolve_path(
                &std::env::current_dir().map_err(|e| RuntimeError::new(e.to_string()))?,
                Path::new(workspace),
            );
            let file = crate::metadata::resolve(&root, path);
            if !file.starts_with(&root)
                || file
                    .strip_prefix(&root)
                    .ok()
                    .and_then(|p| p.to_str())
                    .is_some_and(|p| p.starts_with(".."))
            {
                (
                    "LSP_MALFORMED_RESPONSE",
                    format!("LSP source {path} resolves outside the workspace"),
                )
            } else {
                let name = path.trim_end_matches('/').rsplit('/').next().unwrap_or("");
                let extension = name
                    .rfind('.')
                    .filter(|i| *i > 0 && name != "..")
                    .map(|i| &name[i..]);
                (
                    "LSP_UNAVAILABLE",
                    format!(
                        "No LSP provider is registered for {}",
                        extension.unwrap_or("the requested file")
                    ),
                )
            }
        };
        Err(RuntimeError::new(message.clone()).with_js_error(
            crate::runtime::project::RuntimeJsError {
                name: "LspError".into(),
                code: Some(code.into()),
                message: message.into(),
                requires_host: false,
            },
        ))
    }
}
impl FamilyResource for DefaultLsp {
    fn dispose(&self) -> DisposeFuture<'_> {
        self.disposed.store(true, Ordering::Release);
        Box::pin(async { Ok(()) })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
pub struct LspFamily;
impl ResourceFamilySource for LspFamily {
    fn family(&self) -> Family {
        Family::LspService
    }
    fn stage(&self, _: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        sink.register_owner(self.family(), Arc::new(DefaultLsp::default()));
        Ok(())
    }
}

pub struct TitleResource(pub TitleProvider);
impl FamilyResource for TitleResource {
    fn dispose(&self) -> DisposeFuture<'_> {
        // Native TS title provider has no dispose method. SessionTail owns
        // cancellation of pending work; the model registry owns its providers.
        Box::pin(async { Ok(()) })
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
}
pub struct TitleFamily;
impl ResourceFamilySource for TitleFamily {
    fn family(&self) -> Family {
        Family::SessionTitleProvider
    }
    fn stage(&self, context: &StageContext, sink: &mut StageSink<'_>) -> RuntimeResult {
        let resource = context
            .resource(Resource::Model)
            .ok_or_else(|| RuntimeError::new("Title model resource is missing."))?;
        let model = resource
            .as_any()
            .downcast_ref::<ModelResources>()
            .ok_or_else(|| RuntimeError::new("Title model resource is not native."))?;
        let selection = context
            .snapshot()
            .get("config")
            .and_then(|v| v.get("agent"))
            .and_then(|v| v.get("model"))
            .cloned()
            .unwrap_or(JsValue::Undefined);
        let language = language(&context.env());
        sink.register_owner(
            self.family(),
            Arc::new(TitleResource(title_provider(
                model.providers.clone(),
                selection,
                language,
                Duration::from_secs(30),
            ))),
        );
        Ok(())
    }
}

fn language(env: &crate::config::Env) -> String {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Some(raw) = env.get(key) {
            let raw = raw.split(['.', '@']).next().unwrap_or("").replace('_', "-");
            if !["c", "posix", "und", ""].contains(&raw.to_lowercase().as_str())
                && let Ok(locale) = raw.parse::<icu_locale_core::Locale>()
            {
                return locale.to_string();
            }
        }
    }
    "en-US".into()
}

const TITLE_PROMPT: &str = "Generate a concise title (3-7 words) that captures the main topic or goal of this coding session. The title should be clear enough that the user recognizes the session in a list. For Latin-script languages, use sentence case: capitalize only the first word and proper nouns.\n\nLanguage requirement (highest priority): write the title in the same natural language as the user's input. Do not translate the user's input into English. If the input contains multiple languages, use the language of the user's main request (or the most recent natural-language request), while leaving product names and code identifiers unchanged. If the user's language cannot be determined, write the title in the system language specified below.\n\nReturn JSON with a single \"title\" field.\n\nExamples (the title language must still follow the user's input):\n{\"title\": \"Fix login button on mobile\"}\n{\"title\": \"Add OAuth authentication\"}\n{\"title\": \"修复移动端登录按钮\"}\n{\"title\": \"添加 OAuth 认证\"}\n\nBad (too vague): {\"title\": \"Code changes\"}\nBad (too long): {\"title\": \"Investigate and fix the issue where the login button does not respond on mobile devices\"}\nBad (wrong case): {\"title\": \"Fix Login Button On Mobile\"}\n\nDo not output Markdown, code fences, explanations, analysis, thinking text, <think> tags, or extra fields.";

pub fn title_provider(
    model: Arc<dyn RouterModelInvocation>,
    selection: JsValue,
    language: String,
    timeout: Duration,
) -> TitleProvider {
    let provenance = object([
        ("provider", field(&selection, "provider")),
        ("model", field(&selection, "model")),
    ]);
    TitleProvider {
        provider_id: Some("native-llm".into()),
        model_provenance: Some(provenance),
        generate: Arc::new(move |input| {
            let model = model.clone();
            let selection = input.model.clone().unwrap_or_else(|| selection.clone());
            let system_prompt = format!("{TITLE_PROMPT}\n\nSystem language: {language}");
            Box::pin(async move {
                let mut text = normalized(&input.text);
                if text.is_empty() {
                    return Ok(None);
                }
                if text.len() > 1200 {
                    let mut short = text[..597].to_vec();
                    short.extend(" ... ".encode_utf16());
                    short.extend_from_slice(&text[text.len() - 598..]);
                    text = short;
                }
                let request = object([
                    ("provider", field(&selection, "provider")),
                    ("model", field(&selection, "model")),
                    ("systemPrompt", JsValue::String(system_prompt.into())),
                    (
                        "messages",
                        JsValue::Array(vec![object([
                            ("role", "user".into()),
                            (
                                "content",
                                JsValue::Array(vec![object([
                                    ("type", "text".into()),
                                    ("text", JsValue::String(JsString::from_utf16(text))),
                                ])]),
                            ),
                        ])]),
                    ),
                    ("maxOutputTokens", JsValue::Number(4096.0)),
                    (
                        "metadata",
                        object([
                            ("purpose", "session_title_generation".into()),
                            ("sessionId", JsValue::String(input.session_id)),
                            ("turnId", JsValue::String(input.turn_id)),
                        ]),
                    ),
                ]);
                let abort = AbortFlag::new();
                if input.signal.is_cancelled() {
                    abort.abort();
                }
                let response = model.complete(request, Some(abort.clone()));
                tokio::pin!(response);
                let result = tokio::select! {
                    result = &mut response => result.ok(),
                    _ = input.signal.cancelled() => { abort.abort(); response.await.ok() },
                    _ = tokio::time::sleep(timeout) => { abort.abort(); response.await.ok() },
                };
                Ok(result.as_ref().and_then(parse_title))
            })
        }),
    }
}
fn field(value: &JsValue, name: &str) -> JsValue {
    value.get(name).cloned().unwrap_or(JsValue::Undefined)
}
fn normalized(text: &JsString) -> Vec<u16> {
    let mut output = Vec::new();
    let mut space = false;
    for &unit in text.units() {
        let white = matches!(unit, 0x0009..=0x000d | 0x0020 | 0x00a0 | 0x1680 | 0x2000..=0x200a | 0x2028..=0x2029 | 0x202f | 0x205f | 0x3000 | 0xfeff);
        if white {
            space = !output.is_empty();
        } else {
            if space {
                output.push(32);
            }
            space = false;
            output.push(unit);
        }
    }
    output
}
fn parse_title(response: &JsValue) -> Option<JsString> {
    let JsValue::Array(blocks) = response.get("content")? else {
        return None;
    };
    let mut units = Vec::new();
    for block in blocks {
        if block.get("type") == Some(&"text".into()) {
            units.extend_from_slice(block.get("text")?.as_string()?.units());
        }
    }
    // Preserve literal unpaired UTF-16 units in JSON strings by escaping them
    // before the UTF-8 parser boundary, as JSON.parse does on JS strings.
    let mut text = String::new();
    for decoded in char::decode_utf16(units) {
        match decoded {
            Ok(ch) => text.push(ch),
            Err(error) => text.push_str(&format!("\\u{:04x}", error.unpaired_surrogate())),
        }
    }
    let text = crate::metadata::js_trim(&text);
    let fence = regex::Regex::new(r"(?is)^```(?:json)?\s*([\s\S]*?)\s*```$").ok()?;
    let body = fence
        .captures(text)
        .and_then(|c| c.get(1))
        .map_or(text, |m| m.as_str());
    let parsed = parse_json(crate::metadata::js_trim(body)).ok()?;
    let mut title = normalized(parsed.get("title")?.as_string()?);
    title.truncate(80);
    (!title.is_empty()).then(|| JsString::from_utf16(title))
}
