use super::{Diagnostics, Env, Error, value::*};
use crate::metadata::object;
use pilotdeck_protocol::{JsString, JsValue};
use std::sync::OnceLock;
pub(crate) struct Provider {
    pub id: JsString,
    pub available: bool,
    pub speed_mapping: bool,
    pub models: Vec<Model>,
}
pub(crate) struct Model {
    pub id: JsString,
    pub display_name: JsString,
    pub reasoning: Option<Vec<f64>>,
    pub speed: bool,
    pub enumerable: bool,
}
fn catalog() -> &'static JsValue {
    static DATA: OnceLock<JsValue> = OnceLock::new();
    DATA.get_or_init(|| {
        pilotdeck_protocol::parse_json(include_str!("catalog-data.json"))
            .expect("recorded catalog data")
    })
}
fn find_model<'a>(models: &'a JsValue, id: &JsString) -> Option<&'a JsValue> {
    models.as_object()?.get_key(id).or_else(|| {
        entries(models)
            .into_iter()
            .find_map(|(_, m)| match field(m, "aliases") {
                JsValue::Array(a) if a.iter().any(|v| v.as_string() == Some(id)) => Some(m),
                _ => None,
            })
    })
}
fn lookup(provider: &JsString, id: &JsString) -> &'static JsValue {
    if let Some(p) = catalog().as_object().and_then(|o| o.get_key(provider))
        && let Some(m) = find_model(field(p, "models"), id)
    {
        return m;
    }
    fn all(id: &JsString) -> Option<&'static JsValue> {
        for (_, p) in entries(catalog()) {
            if let Some(m) = field(p, "models").as_object().and_then(|o| o.get_key(id)) {
                return Some(m);
            }
        }
        for (_, p) in entries(catalog()) {
            if let Some(m) = find_model(field(p, "models"), id) {
                return Some(m);
            }
        }
        None
    }
    all(id)
        .or_else(|| {
            id.units()
                .iter()
                .position(|u| *u == 47)
                .and_then(|i| all(&JsString::from_utf16(id.units()[i + 1..].to_vec())))
        })
        .unwrap_or(&UNDEFINED)
}
fn err(code: &str, msg: impl Into<DiagnosticText>) -> Error {
    Error::new(&format!("MODEL_{}", code.to_uppercase()), msg)
}
fn invalid(msg: impl Into<DiagnosticText>) -> Error {
    err("invalid_config_value", msg)
}
pub(super) fn credential(value: &JsValue, env: &Env) -> Result<(), Error> {
    let Some(s) = value.as_string() else {
        return Err(err(
            "missing_api_key",
            "Provider apiKey must be a non-empty string.",
        ));
    };
    let s = trim(s);
    if s.is_empty() {
        return Err(err(
            "missing_api_key",
            "Provider apiKey must be a non-empty string.",
        ));
    }
    if let Ok(s) = s.to_utf8() {
        let re = regex::Regex::new(r"^\$\{([A-Za-z_][A-Za-z0-9_]*)\}$").unwrap();
        if let Some(c) = re.captures(&s) {
            let name = &c[1];
            if env
                .get(name)
                .is_none_or(|v| crate::metadata::js_trim(v).is_empty())
            {
                return Err(err(
                    "missing_api_key",
                    format!("Environment variable {name} is not set."),
                ));
            }
        }
    }
    Ok(())
}
pub(super) fn parse(raw: &JsValue, env: &Env, d: &mut Diagnostics) -> Result<Vec<Provider>, Error> {
    if !record(raw) {
        d.fatal(
            "MODEL_INVALID_MODEL_CONFIG",
            "Model config must be an object.",
            "model",
        );
        d.check()?;
    }
    let raw = field(raw, "providers");
    if !record(raw) || entries(raw).is_empty() {
        d.fatal(
            "MODEL_MISSING_PROVIDER",
            "Model config must contain at least one provider.",
            "model",
        );
        d.check()?;
    }
    let mut providers = vec![];
    for (id, raw) in entries(raw) {
        match provider(id, raw, env) {
            Ok(p) => providers.push(p),
            Err(e) if !e.code.starts_with("MODEL_") => return Err(e),
            Err(e) => {
                let path = format!(
                    "model.providers.{}",
                    js_string(&JsValue::String(id.clone()))
                );
                d.warn(&e.code, e.message, &path);
                if let Some(JsValue::Object(last)) = d.0.last_mut() {
                    last.insert("hint".into(),"This provider was excluded from the runtime. Correct its settings to enable it.".into());
                }
            }
        }
    }
    Ok(providers)
}
fn positive(v: &JsValue, key: &str) -> Result<(), Error> {
    if !v.is_undefined() && !number(v, true, false) {
        return Err(invalid(format!("{key} must be a positive number.")));
    }
    Ok(())
}
fn string_list(v: &JsValue, key: &str) -> Result<(), Error> {
    if !v.is_undefined() && !strings(v) {
        return Err(invalid(format!("{key} must be a string list.")));
    }
    Ok(())
}
fn provider(id: &JsString, raw: &JsValue, env: &Env) -> Result<Provider, Error> {
    let name = js_string(&JsValue::String(id.clone()));
    if !record(raw) {
        return Err(err(
            "invalid_provider",
            format!("Provider {name} must be an object."),
        ));
    }
    let cat = catalog()
        .as_object()
        .and_then(|o| o.get_key(id))
        .unwrap_or(&UNDEFINED);
    let protocol = field(raw, "protocol");
    let protocol = if ["openai", "openai-responses", "anthropic", "google"]
        .iter()
        .any(|s| eq(protocol, s))
    {
        text(protocol)
    } else {
        text(field(cat, "protocol"))
    }
    .ok_or_else(|| {
        err(
            "unsupported_protocol",
            format!("Provider {name} has unsupported protocol."),
        )
    })?;
    let default_url = if name == "google" && protocol == "openai" {
        Some("https://generativelanguage.googleapis.com/v1beta/openai".into())
    } else {
        text(field(cat, "defaultUrl"))
    };
    let endpoint = nonempty(field(raw, "url"))
        .or(default_url.clone())
        .ok_or_else(|| invalid(format!("Provider {name} requires a url.")))?;
    let canonical = http_url(&endpoint)
        .ok_or_else(|| err("invalid_url", format!("Provider {name} url is invalid.")))?;
    let raw_models = field(raw, "models");
    if !record(raw_models) || entries(raw_models).is_empty() {
        return Err(err(
            "empty_models",
            format!("Provider {name} must contain at least one model."),
        ));
    }
    if cat.is_undefined() && builtin_property(id) {
        let first = entries(raw_models)[0].0;
        let mut error = Error::new(
            "gateway_request_failed",
            format!(
                "Cannot read properties of undefined (reading '{}')",
                js_string(&JsValue::String(first.clone()))
            ),
        );
        error.name = "TypeError";
        return Err(error);
    }
    let mut models = vec![];
    for (mid, m) in entries(raw_models) {
        models.push(model(id, mid, &protocol, m)?);
    }
    let raw_key = field(raw, "apiKey");
    let blank = raw_key.as_string().is_some_and(|s| trim(s).is_empty());
    let effective = if name == "ollama" && raw_key.is_undefined() {
        "ollama".into()
    } else if !nullish(raw_key) && !blank {
        raw_key.clone()
    } else {
        let same_protocol =
            eq(field(cat, "protocol"), &protocol) || (name == "google" && protocol == "openai");
        let same_url = default_url
            .as_deref()
            .and_then(http_url)
            .is_some_and(|u| u.trim_end_matches('/') == canonical.trim_end_matches('/'));
        match text(field(cat, "apiKeyEnvVar")).filter(|_| same_protocol && same_url) {
            Some(var) => JsValue::String(format!("${{{var}}}").into()),
            None => raw_key.clone(),
        }
    };
    credential(&effective, env)?;
    positive(field(raw, "timeoutMs"), "timeoutMs")?;
    let headers = field(raw, "headers");
    if !headers.is_undefined() {
        if !record(headers) {
            return Err(invalid("headers must be an object."));
        }
        for (k, v) in entries(headers) {
            if v.as_string().is_none() {
                return Err(invalid(format!(
                    "headers.{} must be a string.",
                    js_string(&JsValue::String(k.clone()))
                )));
            }
        }
    }
    let sm = field(raw, "speedMapping");
    let speed_mapping = if !sm.is_undefined() {
        if (eq(sm, "openai_service_tier")
            && ["openai", "openai-responses"].contains(&protocol.as_str()))
            || (eq(sm, "anthropic_speed") && protocol == "anthropic")
        {
            true
        } else {
            return Err(invalid(
                "speedMapping must match the provider protocol: openai_service_tier for OpenAI or anthropic_speed for Anthropic.",
            ));
        }
    } else {
        (name == "openai" && ["openai", "openai-responses"].contains(&protocol.as_str()))
            || (name == "anthropic" && protocol == "anthropic")
    };
    for key in [
        "requestMaxRetries",
        "streamMaxRetries",
        "streamIdleTimeoutMs",
        "maxStreamingDurationMs",
        "repeatedChunkLimit",
        "baseDelayMs",
        "maxDelayMs",
        "jitter",
    ] {
        let v = field(field(raw, "retry"), key);
        if record(field(raw, "retry")) && !v.is_undefined() && !number(v, false, false) {
            return Err(invalid(format!(
                "retry.{key} must be a non-negative number."
            )));
        }
    }
    Ok(Provider {
        id: id.clone(),
        available: true,
        speed_mapping,
        models,
    })
}
fn model(
    provider: &JsString,
    id: &JsString,
    protocol: &str,
    raw: &JsValue,
) -> Result<Model, Error> {
    let empty = object([]);
    let raw = if nullish(raw) { &empty } else { raw };
    if !record(raw) {
        return Err(err(
            "invalid_model",
            format!(
                "Model {} must be an object.",
                js_string(&JsValue::String(id.clone()))
            ),
        ));
    }
    let cat = lookup(provider, id);
    let caps = field(raw, "capabilities");
    if !caps.is_undefined() {
        if !record(caps) {
            return Err(err(
                "invalid_capabilities",
                "Model capabilities must be an object.",
            ));
        }
        for key in [
            "supportsToolUse",
            "supportsStreaming",
            "supportsParallelToolCalls",
            "supportsThinking",
            "supportsSpeed",
            "supportsJsonSchema",
            "supportsSystemPrompt",
            "supportsPromptCache",
        ] {
            let v = field(caps, key);
            if !v.is_undefined() && v.as_bool().is_none() {
                return Err(err(
                    "invalid_capabilities",
                    format!("Capability {key} must be boolean."),
                ));
            }
        }
        if field(caps, "maxContextTokens").is_undefined() {
            positive(field(caps, "contextWindow"), "contextWindow")?;
        }
        for key in ["maxContextTokens", "maxOutputTokens"] {
            positive(field(caps, key), key)?;
        }
    }
    let mm = field(raw, "multimodal");
    if !mm.is_undefined() {
        if !record(mm) {
            return Err(err(
                "invalid_multimodal",
                "Model multimodal config must be an object.",
            ));
        }
        let JsValue::Array(input) = field(mm, "input") else {
            return Err(err(
                "invalid_multimodal_input",
                "multimodal.input must be a string list.",
            ));
        };
        for item in input {
            if !["text", "image", "pdf", "audio"]
                .iter()
                .any(|s| eq(item, s))
            {
                return Err(err(
                    "invalid_multimodal_input",
                    "multimodal.input contains unsupported modality.",
                ));
            }
        }
        for key in ["maxImagesPerRequest", "maxImageBytes"] {
            positive(field(mm, key), key)?;
        }
        string_list(
            field(mm, "supportedImageMimeTypes"),
            "supportedImageMimeTypes",
        )?;
        for key in ["maxPdfPages", "maxPdfBytes", "maxAudioSeconds"] {
            positive(field(mm, key), key)?;
        }
        let detail = field(mm, "imageDetail");
        if !detail.is_undefined() && !["auto", "low", "high"].iter().any(|s| eq(detail, s)) {
            return Err(err(
                "invalid_multimodal",
                "multimodal.imageDetail must be auto, low or high.",
            ));
        }
    }
    let thinking = field(raw, "thinking");
    let mut reasoning = None;
    if !thinking.is_undefined() {
        if !record(thinking) {
            return Err(invalid("Model thinking must be an object."));
        }
        let state = field(thinking, "state");
        if !state.is_undefined()
            && !["default", "enabled", "disabled"]
                .iter()
                .any(|s| eq(state, s))
        {
            return Err(invalid(
                "thinking.state must be default, enabled or disabled.",
            ));
        }
        let efforts = field(thinking, "efforts");
        let empty = vec![];
        let efforts = if efforts.is_undefined() {
            &empty
        } else if let JsValue::Array(a) = efforts {
            a
        } else {
            return Err(invalid(
                "thinking.efforts must contain only low, medium, high, xhigh or max.",
            ));
        };
        let modes = ["low", "medium", "high", "xhigh", "max"];
        if efforts.iter().any(|v| !modes.iter().any(|s| eq(v, s))) {
            return Err(invalid(
                "thinking.efforts must contain only low, medium, high, xhigh or max.",
            ));
        }
        let format = field(thinking, "format");
        let allowed: &[&str] = match protocol {
            "anthropic" => &["provider", "anthropic", "server-default"],
            "google" => &["provider", "google", "server-default"],
            "openai-responses" => &["provider", "openai", "server-default"],
            _ => &[
                "provider",
                "openai",
                "thinking-type",
                "qwen-cloud",
                "qwen-local",
                "openrouter",
                "server-default",
            ],
        };
        if !format.is_undefined() && !allowed.iter().any(|s| eq(format, s)) {
            return Err(invalid("thinking.format must match the provider protocol."));
        }
        if eq(state, "enabled") {
            reasoning = Some(
                modes
                    .iter()
                    .zip([0.4, 0.6, 0.8, 0.9, 1.0])
                    .filter_map(|(mode, n)| efforts.iter().any(|e| eq(e, mode)).then_some(n))
                    .collect(),
            );
        }
    }
    string_list(field(raw, "aliases"), "aliases")?;
    let display_name = field(raw, "displayName")
        .as_string()
        .or_else(|| field(cat, "displayName").as_string())
        .unwrap_or(id)
        .clone();
    let speed = field(caps, "supportsSpeed").as_bool().unwrap_or_else(|| {
        provider.equals_str("openai")
            || provider.equals_str("anthropic")
            || field(field(cat, "capabilities"), "supportsSpeed").as_bool() == Some(true)
    });
    Ok(Model {
        id: id.clone(),
        enumerable: !id.equals_str("__proto__"),
        display_name,
        reasoning,
        speed,
    })
}
