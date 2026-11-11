#[path = "common/oracle_replay.rs"]
mod oracle_replay;
use oracle_replay::*;

#[path = "common/baseline.rs"]
mod baseline;

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use pilotdeck_gateway::{
    host::modules::tool_context::{HostToolCheckpoint, ToolRuntimeContext},
    runtime::tools::{
        ExecutableTool, ToolRunContext,
        builtin::{
            glob::GlobTool, read_file::ReadFileTool, register_read_only_tools,
            time::GetCurrentTimeTool,
        },
        registry::ToolRegistry,
    },
};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fs,
    path::Path,
    sync::{Arc, Mutex},
};

const ORACLE: &str = include_str!("fixtures/builtin-tools/oracle.json");
const FIXED_NOW: f64 = 1_767_225_600_000.0;

struct ReplayContext {
    context: ToolRunContext,
    now: Arc<Mutex<f64>>,
}
impl ReplayContext {
    fn new(scratch: &Scratch, cwd: &Path) -> Self {
        let now = Arc::new(Mutex::new(FIXED_NOW));
        let clock = {
            let now = now.clone();
            Arc::new(move || JsValue::Number(*now.lock().unwrap()))
        };
        let checkpoint = HostToolCheckpoint::default();
        let permission = object([
            ("mode", JsValue::from("default")),
            ("canPrompt", JsValue::Bool(true)),
            ("additionalWorkingDirectories", JsValue::Array(Vec::new())),
        ]);
        let fields = object([
            ("cwd", JsValue::from(cwd.to_string_lossy().as_ref())),
            ("permissionMode", JsValue::from("default")),
            ("permissionContext", JsValue::Object(permission)),
            (
                "env",
                JsValue::Object(object([
                    (
                        "PILOT_HOME",
                        JsValue::from(scratch.root.join("pilot-home").to_string_lossy().as_ref()),
                    ),
                    ("HOME", JsValue::from(scratch.text(&scratch.root).as_str())),
                ])),
            ),
        ]);
        let host = ToolRuntimeContext {
            fields,
            files: checkpoint.tool_context_state(),
            abort_signal: None,
            model: None,
            elicitation: None,
            user_dialog: None,
            audit_recorder: None,
            file_history: None,
            file_update_notifier: None,
            now: Some(clock),
            progress: None,
            subagent: None,
            plan_todo: None,
            goal: None,
            plan_directory: None,
        };
        Self {
            context: ToolRunContext::new(Arc::new(host)),
            now,
        }
    }
    fn set_now(&self, value: f64) {
        *self.now.lock().unwrap() = value;
    }
}

fn validate_value(
    tool_name: &str,
    tool: &dyn ExecutableTool,
    input: &JsValue,
    context: ToolRunContext,
) -> JsValue {
    if tool_name == "read_file" {
        match futures_block(tool.validate_input(input.clone(), context)) {
            Ok(None) => JsValue::Object(object([("ok", JsValue::Bool(true))])),
            Ok(Some(issues)) => JsValue::Object(object([
                ("ok", JsValue::Bool(false)),
                (
                    "issues",
                    JsValue::Array(issues.into_iter().map(JsValue::Object).collect()),
                ),
            ])),
            Err(error) => error_value(&error),
        }
    } else {
        JsValue::Null
    }
}

fn tool_for(name: &str) -> Box<dyn ExecutableTool> {
    match name {
        "read_file" => Box::new(ReadFileTool::new()),
        "glob" => Box::new(GlobTool::new()),
        "get_current_time" => Box::new(GetCurrentTimeTool::new()),
        other => panic!("unknown fixture tool {other}"),
    }
}

#[test]
fn builtin_tool_outputs_match_real_ts() {
    let oracle = baseline::oracle(ORACLE);
    let scenarios = array(field(&oracle, "scenarios"));
    assert_eq!(scenarios.len(), 57, "scenario count");
    let mut calls = 0;
    let mut compared = 0;
    for scenario in scenarios {
        let name = text(field(scenario, "name"));
        let scratch = Scratch::new();
        for outside in optional_array(field(scenario, "outside")) {
            let path = text(field(outside, "path"));
            let absolute = scratch.outside.join(path);
            if let Some(parent) = absolute.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            fs::write(&absolute, text(field(outside, "content"))).unwrap();
        }
        let cwd = match field(scenario, "cwd").as_string() {
            Some(cwd) => scratch.root.join(cwd.to_utf8().unwrap()),
            None => scratch.root.clone(),
        };
        let mut replay = ReplayContext::new(&scratch, &cwd);
        let calls_value = array(field(scenario, "calls"));
        for step in array(field(scenario, "steps")) {
            if text(field(step, "op")) != "call" {
                apply_step(step, &scratch).unwrap_or_else(|e| panic!("{name}: setup: {e}"));
                continue;
            }
            let index = number(field(step, "index")).expect("call index") as usize;
            let call = &calls_value[index];
            if let Some(context) = field(call, "context").as_object() {
                for (key, value) in context.entries() {
                    replay.context.fields.insert(key.clone(), value.clone());
                }
            }
            calls += 1;
            let tool_name = text(field(call, "tool"));
            let input = substitute(field(call, "input"), &scratch);
            let tool = tool_for(&tool_name);
            replay.set_now(number(field(call, "now")).unwrap_or(FIXED_NOW));
            let actual = validate_value(&tool_name, tool.as_ref(), &input, replay.context.clone());
            assert_eq!(
                normalize(&actual, &scratch),
                wire(field(call, "validate")),
                "{name}: validate"
            );
            let Some(expected) = expected_execute(call) else {
                continue;
            };
            compared += 1;
            let actual = match futures_block(tool.execute(input, replay.context.clone())) {
                Ok(output) => output_value(output),
                Err(error) => error_value(&error),
            };
            assert_eq!(
                normalize(&actual, &scratch),
                wire(expected),
                "{name}: execute"
            );
        }
    }
    assert_eq!(calls, 65, "call count");
    assert_eq!(compared, 62, "compared call count");
}

fn multimodal(input: &[&str], image_detail: Option<&str>) -> JsValue {
    let mut value = object([(
        "input",
        JsValue::Array(
            input
                .iter()
                .map(|kind| JsValue::from(*kind))
                .collect::<Vec<_>>(),
        ),
    )]);
    if let Some(detail) = image_detail {
        value.insert("imageDetail".into(), JsValue::from(detail));
    }
    JsValue::Object(value)
}

#[test]
fn multimodal_success_branches_emit_model_visible_payloads() {
    let scratch = Scratch::new();
    let mut replay = ReplayContext::new(&scratch, &scratch.root);

    fs::write(scratch.root.join("doc.pdf"), b"not a pdf").unwrap();
    replay
        .context
        .fields
        .insert("modelMultimodal".into(), multimodal(&["pdf"], None));
    let output = futures_block(ReadFileTool::new().execute(
        JsValue::Object(object([("file_path", JsValue::from("doc.pdf"))])),
        replay.context.clone(),
    ))
    .unwrap();
    let supplemental = output.supplemental_messages.expect("PDF supplemental");
    assert_eq!(supplemental.len(), 1);
    let message = &supplemental[0];
    assert_eq!(text(field(message, "role")), "user");
    assert_eq!(field(message, "isMeta"), &JsValue::Bool(true));
    let content = &array(field(message, "content"))[0];
    assert_eq!(text(field(content, "type")), "pdf");
    assert_eq!(text(field(content, "mimeType")), "application/pdf");
    assert_eq!(text(field(content, "data")), BASE64.encode(b"not a pdf"));
    assert_eq!(field(content, "bytes"), &JsValue::Number(9.0));

    let jpeg = [vec![b'A'; 1024], vec![0xff, 0xd9]].concat();
    fs::write(scratch.root.join("pic.jpg"), &jpeg).unwrap();
    replay.context.fields.insert(
        "modelMultimodal".into(),
        multimodal(&["image"], Some("low")),
    );
    let output = futures_block(ReadFileTool::new().execute(
        JsValue::Object(object([("file_path", JsValue::from("pic.jpg"))])),
        replay.context.clone(),
    ))
    .unwrap();
    let content = &output.content[0];
    assert_eq!(text(field(content, "type")), "image");
    assert_eq!(text(field(content, "mimeType")), "image/jpeg");
    assert_eq!(text(field(content, "data")), BASE64.encode(&jpeg));
    assert_eq!(field(content, "bytes"), &JsValue::Number(jpeg.len() as f64));
    assert_eq!(text(field(content, "detail")), "low");
    assert!(output.supplemental_messages.is_none());
}

#[test]
fn builtin_family_registers_as_read_only_catalog_entries() {
    let registry = ToolRegistry::default();
    let registrations = register_read_only_tools(&registry).expect("register builtins");
    assert_eq!(registrations.len(), 4);
    let names: Vec<String> = registry
        .list()
        .map(|tools| {
            tools
                .iter()
                .map(|tool| text(field(&JsValue::Object(tool.fields().clone()), "name")))
                .collect()
        })
        .expect("list builtins");
    let mut names = names;
    names.sort();
    assert_eq!(
        names,
        vec!["get_current_time", "glob", "grep", "read_file"],
        "registered names"
    );
    assert!(registry.get(&JsString::from("Read")).is_some(), "alias");

    let tools = pilotdeck_gateway::runtime::tools::builtin::read_only_tools();
    for tool in tools {
        let empty = JsValue::Object(JsObject::new());
        let name = text(field(&JsValue::Object(tool.fields().clone()), "name"));
        assert_eq!(
            tool.is_read_only(&empty).unwrap(),
            JsValue::Bool(true),
            "{name} read-only"
        );
        let expected_safe = name != "read_file";
        assert_eq!(
            tool.is_concurrency_safe(&empty).unwrap(),
            JsValue::Bool(expected_safe),
            "{name} concurrency"
        );
    }
}

fn tiny_pdf(pages: &[&str]) -> Vec<u8> {
    let font = 3 + pages.len() * 2;
    let kids = (0..pages.len())
        .map(|i| format!("{} 0 R", 3 + i * 2))
        .collect::<Vec<_>>()
        .join(" ");
    let mut objects = vec![
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        format!("<< /Type /Pages /Count {} /Kids [{kids}] >>", pages.len()),
    ];
    for (i, text) in pages.iter().enumerate() {
        let content = 4 + i * 2;
        let stream = format!("BT /F1 12 Tf 40 700 Td ({text}) Tj ET");
        objects.push(format!("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 {font} 0 R >> >> /Contents {content} 0 R >>"));
        objects.push(format!(
            "<< /Length {} >>\nstream\n{stream}\nendstream",
            stream.len()
        ));
    }
    if !pages.is_empty() {
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned());
    }
    let mut out = b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n".to_vec();
    let mut offsets = vec![0usize];
    for (i, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes());
    for offset in &offsets[1..] {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            offsets.len()
        )
        .as_bytes(),
    );
    out
}

#[test]
fn read_file_real_pdf_page_count_and_range_contract() {
    let scratch = Scratch::new();
    for n in [0usize, 1, 2, 10, 11] {
        let pages = vec!["fixture page"; n];
        fs::write(scratch.root.join(format!("{n}.pdf")), tiny_pdf(&pages)).unwrap();
    }
    fs::write(scratch.root.join("bad.pdf"), b"not a pdf").unwrap();
    let run = |name: &str, modalities: &[&str], pages: Option<&str>| {
        let mut replay = ReplayContext::new(&scratch, &scratch.root);
        replay
            .context
            .fields
            .insert("modelMultimodal".into(), multimodal(modalities, None));
        let mut input = object([("file_path", JsValue::from(name))]);
        if let Some(pages) = pages {
            input.insert("pages".into(), JsValue::from(pages));
        }
        futures_block(ReadFileTool::new().execute(JsValue::Object(input), replay.context))
    };
    for n in [1usize, 10] {
        let name = format!("{n}.pdf");
        let output = run(&name, &["pdf"], None).unwrap();
        let data = output.data.unwrap();
        assert_eq!(field(&data, "pageCount"), &JsValue::Number(n as f64));
        let supplemental = output.supplemental_messages.unwrap();
        let document = &array(field(&supplemental[0], "content"))[0];
        assert_eq!(field(document, "pages"), &JsValue::Number(n as f64));
        assert_eq!(
            text(field(document, "data")),
            BASE64.encode(fs::read(scratch.root.join(name)).unwrap())
        );
    }
    let error = match run("11.pdf", &["pdf"], None) {
        Err(error) => error,
        Ok(_) => panic!("11 pages must be rejected"),
    };
    assert_eq!(error.code, "invalid_tool_input");
    assert!(
        error
            .message
            .to_utf8()
            .unwrap()
            .contains("This PDF has 11 pages")
    );
    let error = match run("2.pdf", &[], Some("3")) {
        Err(error) => error,
        Ok(_) => panic!("range must be rejected before modality"),
    };
    assert_eq!(error.code, "invalid_tool_input");
    assert!(
        error
            .message
            .to_utf8()
            .unwrap()
            .contains("exceeds the detected page count (2)")
    );
    let no_image = run("2.pdf", &[], Some("1-2")).unwrap();
    assert_eq!(
        field(&no_image.data.unwrap(), "pageCount"),
        &JsValue::Number(2.0)
    );
    for name in ["0.pdf", "bad.pdf"] {
        let output = run(name, &["pdf"], None).unwrap();
        assert!(field(&output.data.unwrap(), "pageCount").is_undefined());
        let message = output.supplemental_messages.unwrap();
        assert!(field(&array(field(&message[0], "content"))[0], "pages").is_undefined());
    }
}

#[test]
fn read_file_pdf_renders_fixed_ts_jpeg_pages_and_metadata() {
    use sha2::{Digest, Sha256};
    let scratch = Scratch::new();
    let pdf = tiny_pdf(&["PAGE 1", "PAGE 2"]);
    fs::write(scratch.root.join("two.pdf"), pdf).unwrap();
    let mut replay = ReplayContext::new(&scratch, &scratch.root);
    replay.context.fields.insert(
        "modelMultimodal".into(),
        multimodal(&["image"], Some("high")),
    );
    let output = futures_block(ReadFileTool::new().execute(
        JsValue::Object(object([
            ("file_path", JsValue::from("two.pdf")),
            ("pages", JsValue::from("1-2")),
        ])),
        replay.context,
    ))
    .unwrap();
    let data = output.data.unwrap();
    assert_eq!(field(&data, "pageCount"), &JsValue::Number(2.0));
    assert_eq!(text(field(&data, "requestedPages")), "1-2");
    assert_eq!(field(&data, "pdfPagesRendered"), &JsValue::Bool(true));
    assert_eq!(
        output.metadata.unwrap().get("truncated"),
        Some(&JsValue::Bool(false))
    );
    let messages = output.supplemental_messages.unwrap();
    let images = array(field(&messages[0], "content"));
    assert_eq!(images.len(), 2);
    for (image, expected) in images.iter().zip([
        "09d9e2bbd72719c52482dfc8a25d447632584db63ae80f1f8e4200681d9d3340",
        "a690bb8c5a3a2c76259d58bcfa4ef0c467e351efa1cfefabeaf6181953cb6492",
    ]) {
        assert_eq!(text(field(image, "type")), "image");
        assert_eq!(text(field(image, "mimeType")), "image/jpeg");
        assert_eq!(text(field(image, "detail")), "high");
        let bytes = BASE64.decode(text(field(image, "data"))).unwrap();
        assert_eq!(format!("{:x}", Sha256::digest(&bytes)), expected);
        assert_eq!(field(image, "bytes"), &JsValue::Number(bytes.len() as f64));
    }
}

#[cfg(unix)]
#[test]
fn read_file_media_rejects_special_and_oversized_sources_but_allows_regular_symlink() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let jpeg = [vec![b'A'; 1024], vec![0xff, 0xd9]].concat();
    fs::write(scratch.root.join("regular.jpg"), &jpeg).unwrap();
    symlink("regular.jpg", scratch.root.join("alias.jpg")).unwrap();
    let huge = fs::File::create(scratch.root.join("huge.pdf")).unwrap();
    huge.set_len(64 * 1024 * 1024 + 1).unwrap();
    assert!(
        std::process::Command::new("mkfifo")
            .arg(scratch.root.join("pipe.png"))
            .status()
            .unwrap()
            .success()
    );
    symlink("pipe.png", scratch.root.join("pipe-alias.png")).unwrap();
    symlink("/dev/zero", scratch.root.join("device.pdf")).unwrap();
    let run = |name: &str| {
        let mut replay = ReplayContext::new(&scratch, &scratch.root);
        replay
            .context
            .fields
            .insert("modelMultimodal".into(), multimodal(&["image"], None));
        futures_block(ReadFileTool::new().execute(
            JsValue::Object(object([("file_path", JsValue::from(name))])),
            replay.context,
        ))
    };
    let output = run("alias.jpg").expect("in-workspace regular symlink stays supported");
    assert_eq!(text(field(&output.content[0], "type")), "image");
    assert_eq!(run("huge.pdf").unwrap_err().code, "result_too_large");
    assert_eq!(run("pipe.png").unwrap_err().code, "invalid_tool_input");
    assert_eq!(
        run("pipe-alias.png").unwrap_err().code,
        "invalid_tool_input"
    );
    assert!(run("device.pdf").is_err());
}

#[test]
fn read_file_rejects_truncated_gif_after_header_and_preserves_valid_input() {
    // The fixed TS sharp metadata oracle accepts the full image and rejects
    // all three truncations; this is generated fixture data, not user content.
    let original = include_bytes!("fixtures/builtin-tools/noise.gif");
    let scratch = Scratch::new();
    for (name, len) in [
        ("full.gif", original.len()),
        ("early.gif", 1001),
        ("half.gif", original.len() / 2),
        ("tail.gif", original.len() - 20),
    ] {
        fs::write(scratch.root.join(name), &original[..len]).unwrap();
        let mut replay = ReplayContext::new(&scratch, &scratch.root);
        replay
            .context
            .fields
            .insert("modelMultimodal".into(), multimodal(&["image"], None));
        let output = futures_block(ReadFileTool::new().execute(
            JsValue::Object(object([("file_path", JsValue::from(name))])),
            replay.context,
        ))
        .unwrap();
        if name == "full.gif" {
            assert_eq!(text(field(&output.content[0], "type")), "image");
            assert_eq!(
                BASE64
                    .decode(text(field(&output.content[0], "data")))
                    .unwrap()
                    .as_slice(),
                original.as_slice()
            );
        } else {
            assert_eq!(text(field(&output.content[0], "type")), "text");
            let data = output.data.unwrap();
            assert_eq!(field(&data, "imageDecodeFailed"), &JsValue::Bool(true));
            assert_eq!(
                text(field(&data, "error")),
                format!("Image file appears truncated or corrupted ({len} bytes). Cannot decode.")
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn read_file_media_allows_symlinked_workspace_and_additional_roots() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let scratch = Scratch::new();
    let physical = scratch.root.join("physical");
    fs::create_dir(&physical).unwrap();
    let alias = scratch.root.join("alias");
    symlink(&physical, &alias).unwrap();
    let jpeg = [vec![b'A'; 1024], vec![0xff, 0xd9]].concat();
    fs::write(physical.join("image.jpg"), &jpeg).unwrap();
    let non_utf8 = physical.join(std::ffi::OsString::from_vec(b"non-utf8-\xff.jpg".to_vec()));
    fs::write(&non_utf8, &jpeg).unwrap();
    symlink(&non_utf8, physical.join("non-utf8-alias.jpg")).unwrap();
    let cwd = scratch.root.join("ordinary-cwd");
    fs::create_dir(&cwd).unwrap();
    let extra_alias = scratch.root.join("extra-alias");
    symlink(&scratch.outside, &extra_alias).unwrap();
    fs::write(scratch.outside.join("extra.jpg"), &jpeg).unwrap();
    symlink(
        scratch.outside.join("extra.jpg"),
        physical.join("escape.jpg"),
    )
    .unwrap();
    let run = |cwd: &Path, name: &str, extra: Option<&Path>| {
        let mut replay = ReplayContext::new(&scratch, cwd);
        replay
            .context
            .fields
            .insert("modelMultimodal".into(), multimodal(&["image"], None));
        if let Some(extra) = extra {
            replay.context.fields.insert(
                "permissionContext".into(),
                JsValue::Object(object([
                    ("mode", JsValue::from("default")),
                    ("canPrompt", JsValue::Bool(true)),
                    (
                        "additionalWorkingDirectories",
                        JsValue::Array(vec![JsValue::from(extra.to_str().unwrap())]),
                    ),
                ])),
            );
        }
        futures_block(ReadFileTool::new().execute(
            JsValue::Object(object([("file_path", JsValue::from(name))])),
            replay.context,
        ))
    };
    let output = run(&alias, "image.jpg", None).expect("symlinked workspace root");
    assert_eq!(text(field(&output.content[0], "type")), "image");
    let output = run(&alias, "non-utf8-alias.jpg", None).expect("non-UTF-8 symlink target");
    assert_eq!(text(field(&output.content[0], "type")), "image");
    let extra_file = extra_alias.join("extra.jpg");
    let output = run(&cwd, extra_file.to_str().unwrap(), Some(&extra_alias))
        .expect("symlinked additional root");
    assert_eq!(text(field(&output.content[0], "type")), "image");
    assert_eq!(
        run(&alias, "escape.jpg", None).unwrap_err().code,
        "path_not_allowed"
    );
}
