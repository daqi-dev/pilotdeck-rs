//! Shared run/resume control and query lifecycle. JSON output and query
//! cancellation remain #123 P07–P08.
use super::{Connection, output::TextOutput, positional, read_value, transport};
use crate::{Failure, paths};
use pilotdeck_protocol::JsValue;
use serde::Serialize;
use serde_json::json;
use std::io::{IsTerminal, Read};
use transport::{Transport, string_field};

pub(super) fn run(
    argv: &[String],
    connection: &Connection,
    resume: Option<&str>,
) -> Result<u8, Failure> {
    if argv
        .iter()
        .any(|arg| arg == "--json" || arg == "--stream-json")
    {
        return Err(Failure::Todo("pilotdeck run JSON output (#123 P07)"));
    }
    if read_value(argv, "--model").is_some() && read_value(argv, "--project").is_none() {
        // The TS query leaks its socket after this validation error. Keep the
        // exceptional lifecycle explicitly unimplemented until P06/P08.
        return Err(Failure::Todo(
            "pilotdeck run model without project (#123 P06/P08)",
        ));
    }
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Failure::Error(e.to_string()))?;
    runtime.block_on(run_connected(argv, connection, resume))
}

async fn run_connected(
    argv: &[String],
    connection: &Connection,
    resume: Option<&str>,
) -> Result<u8, Failure> {
    let project = read_value(argv, "--project");
    let mut control = Transport::connect(connection, "pilotdeck-cli", true).await?;
    let result = async {
        let session = if let Some(session) = resume {
            // runSdkCli keeps the argv ID, even if sessions.resume returns a
            // different key. The query resumes this same ID a second time.
            control
                .request("resume_session", json!({"sessionKey": session}))
                .await?;
            session.to_owned()
        } else {
            let created = control
                .request(
                    "new_session",
                    CreateSession {
                        project_key: project,
                        channel_key: read_value(argv, "--channel").unwrap_or("cli"),
                    },
                )
                .await?;
            string_field(&created, "sessionKey")
                .ok_or_else(|| Failure::Error("Gateway returned no sessionKey.".into()))?
        };
        let prompt = read_prompt(argv, usize::from(resume.is_some()))?;
        if prompt.is_empty() {
            return Err(Failure::Error(
                "A prompt is required, or provide it through stdin.".into(),
            ));
        }
        let mut query = Transport::connect(connection, "pilotdeck-cli", true).await?;
        let result = query_turn(&mut query, argv, &session, &prompt, project).await;
        query.close().await;
        result
    }
    .await;
    control.close().await;
    result
}

fn read_prompt(argv: &[String], offset: usize) -> Result<String, Failure> {
    let joined = positional(argv)[offset..].join(" ");
    let prompt = paths::js_trim(&joined);
    if !prompt.is_empty() {
        return Ok(prompt.to_owned());
    }
    if !std::io::stdin().is_terminal() {
        let mut bytes = Vec::new();
        std::io::stdin()
            .read_to_end(&mut bytes)
            .map_err(|e| Failure::Error(e.to_string()))?;
        return Ok(paths::js_trim(&String::from_utf8_lossy(&bytes)).to_owned());
    }
    Ok(String::new())
}

async fn query_turn(
    query: &mut Transport,
    argv: &[String],
    session: &str,
    prompt: &str,
    project: Option<&str>,
) -> Result<u8, Failure> {
    query
        .request("resume_session", json!({"sessionKey": session}))
        .await?;
    let model = match read_value(argv, "--model") {
        Some(model) => Some(resolve_model(query, project, model).await?),
        None => None,
    };
    let permission = read_value(argv, "--permission-mode");
    let sdk_permission = permission.filter(|p| matches!(*p, "acceptEdits" | "dontAsk"));
    let config = if let Some(permission) = sdk_permission {
        Some(json!({"permissionMode":permission}))
    } else if query
        .server_info
        .get("capabilities")
        .and_then(array)
        .is_some_and(|values| {
            values.iter().any(|v| {
                v.as_string()
                    .is_some_and(|v| v.equals_str("sdk_session_defaults"))
            })
        })
    {
        Some(json!({}))
    } else {
        None
    };
    let run_id = uuid::Uuid::new_v4().to_string();
    let request_id = query
        .start_stream(
            "submit_turn",
            SubmitTurn {
                session_key: session,
                channel_key: "api_server",
                project_key: project,
                message: prompt,
                can_prompt: false,
                can_elicit: false,
                mode: permission.map(|p| {
                    if matches!(p, "acceptEdits" | "dontAsk" | "auto") {
                        "default"
                    } else {
                        p
                    }
                }),
                base_permission_mode: permission.map(|p| {
                    if p == "bypassPermissions" {
                        p
                    } else {
                        "default"
                    }
                }),
                sdk_permission_mode: sdk_permission,
                sdk_session_config: config,
                model_override: model,
                run_id: run_id.clone(),
            },
        )
        .await?;
    let mut output = TextOutput::new(session, &run_id);
    loop {
        let Some((event, final_frame)) = query.stream_event(&request_id).await? else {
            return output.finish();
        };
        output.event(&event)?;
        if final_frame {
            return output.finish();
        }
    }
}

async fn resolve_model(
    query: &mut Transport,
    project: Option<&str>,
    model: &str,
) -> Result<ModelOverride, Failure> {
    let catalog = query
        .request("model_catalog_list", json!({"projectKey":project}))
        .await?;
    select_model(&catalog, model)
}

fn select_model(catalog: &JsValue, model: &str) -> Result<ModelOverride, Failure> {
    let candidates = catalog.get("items").and_then(array).unwrap_or(&[]);
    let matches: Vec<_> = candidates
        .iter()
        .filter(|item| {
            item.get("available").and_then(JsValue::as_bool) != Some(false)
                && (string_field(item, "id").as_deref() == Some(model)
                    || string_field(item, "model").as_deref() == Some(model)
                    || format!(
                        "{}/{}",
                        string_field(item, "provider").unwrap_or_else(|| "undefined".into()),
                        string_field(item, "model").unwrap_or_else(|| "undefined".into())
                    ) == model)
        })
        .collect();
    if let [item] = matches.as_slice()
        && let (Some(provider), Some(model)) =
            (string_field(item, "provider"), string_field(item, "model"))
        && !provider.is_empty()
        && !model.is_empty()
    {
        return Ok(ModelOverride {
            mode: "model",
            provider,
            model,
        });
    }
    Err(Failure::Error(format!(
        "Model is not uniquely resolvable: {model}"
    )))
}

// Field order follows the observed SDK JSON.stringify request, including omitted
// optional keys. A serde_json::Value would sort these object keys.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateSession<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    project_key: Option<&'a str>,
    channel_key: &'a str,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SubmitTurn<'a> {
    session_key: &'a str,
    channel_key: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    project_key: Option<&'a str>,
    message: &'a str,
    can_prompt: bool,
    can_elicit: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    mode: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_permission_mode: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sdk_permission_mode: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    sdk_session_config: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    model_override: Option<ModelOverride>,
    run_id: String,
}
#[derive(Serialize)]
struct ModelOverride {
    mode: &'static str,
    provider: String,
    model: String,
}

fn array(value: &JsValue) -> Option<&[JsValue]> {
    match value {
        JsValue::Array(values) => Some(values),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_negative_receipt::verify;

    #[test]
    fn model_resolution_matches_ts_boundaries() {
        let oracle: serde_json::Value = serde_json::from_str(include_str!(
            "../../tests/fixtures/sdk-run-validation/oracle.json"
        ))
        .expect("#1058 model validation oracle");
        let rows = oracle["models"].as_array().expect("#1058 model rows");
        verify(rows.len() == 12, "#1058 registered model validation rows");
        for case in rows {
            let input = &case["input"];
            let catalog =
                pilotdeck_protocol::parse_json(&json!({"items":input["items"]}).to_string())
                    .expect("#1058 model catalog JSON");
            let result = select_model(
                &catalog,
                input["model"].as_str().expect("#1058 requested model"),
            );
            let actual = match result {
                Ok(value) => json!({"value":value}),
                Err(Failure::Error(message)) => json!({"error":message}),
                Err(other) => json!({"unexpected":format!("{other:?}")}),
            };
            if actual != case["output"] {
                eprintln!(
                    "#1058 model {}: {actual}, expected {}",
                    input["name"], case["output"]
                );
            }
            verify(actual == case["output"], "#1058 model resolution bytes");
        }
    }
}
