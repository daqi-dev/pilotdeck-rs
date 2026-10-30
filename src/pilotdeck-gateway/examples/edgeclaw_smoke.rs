//! #1120 opt-in real-provider driver. The Python harness owns keys and budgets.
use pilotdeck_gateway::runtime::memory::edgeclaw::{
    llm::config::LlmOptions,
    messages::CaptureStrategy,
    retriever::RetrievalOptions,
    service::{CaptureTurnInput, EdgeClawMemoryService, FlushOptions, ServiceOptions},
};
use pilotdeck_protocol::{JsObject, JsValue, parse_json, stringify_json};
use std::{env, error::Error, fs, io::Write, path::PathBuf};

fn save(dir: &std::path::Path, name: &str, value: &JsValue) -> Result<(), Box<dyn Error>> {
    let text = stringify_json(value)?.ok_or("#1120 observation is undefined")?;
    let mut file = fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(dir.join(name))?;
    writeln!(file, "{text}")?;
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let dir = PathBuf::from(env::var("EDGECLAW_SMOKE_CELL")?);
    let case = parse_json(&fs::read_to_string(dir.join("input.json"))?)?;
    let messages = match case.get("messages") {
        Some(JsValue::Array(messages)) => messages,
        _ => return Err("#1120 messages must be an array".into()),
    };
    let query = case
        .get("query")
        .and_then(JsValue::as_string)
        .ok_or("#1120 query must be a string")?;
    let mut options = ServiceOptions::new(dir.join("project"));
    options.root_dir = Some(dir.join("memory"));
    options.capture_strategy = CaptureStrategy::FullSession;
    options.include_assistant = Some(true);
    options.source = Some("issue-1120-synthetic".into());
    options.llm = Some(LlmOptions {
        provider: Some("smoke".into()),
        model: Some(env::var("EDGECLAW_SMOKE_MODEL")?.into()),
        api_type: Some("anthropic".into()),
        base_url: Some(env::var("EDGECLAW_SMOKE_URL")?.into()),
        api_key: Some(env::var("EDGECLAW_SMOKE_TOKEN")?.into()),
        ..Default::default()
    });
    let service = EdgeClawMemoryService::new(options)?;
    let result: Result<(), Box<dyn Error>> = async {
        let capture = service.capture_turn(
            messages,
            CaptureTurnInput {
                session_key: "capture-session".into(),
                timestamp: None,
                source: None,
            },
        )?;
        save(&dir, "capture.json", &capture.to_value())?;
        save(&dir, "before-flush.json", &service.snapshot(None)?)?;
        let stats = service
            .flush(FlushOptions {
                session_keys: Some(vec!["capture-session".into()]),
                reason: Some("manual".into()),
                ..Default::default()
            })
            .await?;
        save(&dir, "flush.json", &stats.to_value())?;
        let entries = service.list(&JsObject::new())?;
        let ids = match &entries {
            JsValue::Array(items) => JsValue::Array(
                items
                    .iter()
                    .map(|item| {
                        item.get("relativePath")
                            .filter(|value| value.as_string().is_some())
                            .cloned()
                            .ok_or("#1120 memory entry requires relativePath")
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            ),
            _ => return Err("#1120 memory entries must be an array".into()),
        };
        save(&dir, "entries.json", &entries)?;
        save(
            &dir,
            "records.json",
            &service.get(&ids, Some(&JsValue::Number(10_000.0)))?,
        )?;
        save(&dir, "user-summary.json", &service.get_user_summary()?)?;
        save(&dir, "after-flush.json", &service.snapshot(None)?)?;
        save(
            &dir,
            "index-traces.json",
            &JsValue::Array(service.list_index_traces(Some(100))?),
        )?;
        // A fresh context turn has no capture transcript or previous messages.
        let recall = service
            .retrieve_context(query, &RetrievalOptions::auto())
            .await?;
        save(&dir, "recall.json", &JsValue::Object(recall))?;
        Ok(())
    }
    .await;
    let closed = service.close();
    result?;
    closed?;
    println!("#1120 native capture/flush/recall complete");
    Ok(())
}
