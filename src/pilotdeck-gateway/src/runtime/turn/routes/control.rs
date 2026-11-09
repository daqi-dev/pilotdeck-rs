//! InProcessGateway steerTurn/cancelSteer over the current native writer.
use crate::runtime::{
    connection::{Call, RequestError},
    session::SessionOwner,
    steer::{SteerMessage, SteerReason},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use image::{ImageDecoder, ImageFormat, ImageReader};
use pilotdeck_protocol::{JsObject, JsString, JsValue};
use std::{
    fs::File,
    io::{Cursor, Read},
    path::{Component, Path, PathBuf},
};

pub struct PreparedSteer {
    pub content: Vec<JsValue>,
    pub allowed_read_files: Vec<JsString>,
}

/// Borrow the session's attachment composer, including its registered paths.
pub trait SteerTurnComposer: Send + Sync {
    fn prepare<'a>(&'a self, params: &'a JsValue) -> Call<'a, Result<PreparedSteer, RequestError>>;
}

pub struct ProductionSteerTurnComposer {
    cwd: PathBuf,
}

impl ProductionSteerTurnComposer {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        Self { cwd: cwd.into() }
    }

    fn compose(&self, params: &JsValue) -> Result<PreparedSteer, RequestError> {
        let message = required_string(params, "message")?;
        let attachments = match params.get("attachments") {
            None | Some(JsValue::Undefined | JsValue::Null) => &[][..],
            Some(JsValue::Array(items)) => items.as_slice(),
            _ => return Err(RequestError::new("attachments is not iterable")),
        };
        let mut content = Vec::new();
        let mut allowed = Vec::new();
        let mut diagnostics = Vec::new();
        let mut path_notes = Vec::new();
        let mut audio_paths = Vec::new();
        let mut diagnostic_guidance = false;
        let mut has_inspectable = false;
        for attachment in attachments {
            let kind = value_string(attachment, "type").unwrap_or_default();
            let mime = value_string(attachment, "mimeType").unwrap_or_default();
            let path = value_string(attachment, "path")
                .filter(|path| !path.is_empty())
                .map(|path| resolve_path(&self.cwd, &path));
            let name = value_string(attachment, "name").unwrap_or_default();
            let ext = extension(if name.is_empty() {
                path.as_deref().unwrap_or(Path::new(""))
            } else {
                Path::new(&name)
            });
            let audio = mime.to_lowercase().starts_with("audio/")
                || is_audio(path.as_deref().unwrap_or(Path::new(&name)));
            let binary = is_binary(&ext);
            let registered = attachment
                .get("metadata")
                .and_then(|metadata| metadata.get("channelKey"))
                .is_some_and(truthy);
            if let Some(path) = &path
                && registered
                && std::fs::metadata(path).is_ok_and(|metadata| metadata.is_file())
            {
                for candidate in [Some(path.clone()), std::fs::canonicalize(path).ok()]
                    .into_iter()
                    .flatten()
                {
                    let candidate = JsString::from(candidate.to_string_lossy().as_ref());
                    if !allowed.contains(&candidate) {
                        allowed.push(candidate);
                    }
                }
                let label = if name.is_empty() {
                    path.file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned()
                } else {
                    name.clone()
                };
                let label = label.replace(['\r', '\n'], " ").trim().to_string();
                let note = format!(
                    "- {}: {}",
                    if label.is_empty() {
                        "attachment"
                    } else {
                        &label
                    },
                    path.display()
                );
                if !path_notes.contains(&note) {
                    path_notes.push(note);
                }
                has_inspectable |= !audio && !binary;
                diagnostic_guidance |= audio || binary;
                if audio && let Some(project) = value_string(params, "projectKey") {
                    let root = resolve_path(&self.cwd, &project);
                    if path
                        .strip_prefix(&root)
                        .is_ok_and(|relative| !relative.as_os_str().is_empty())
                    {
                        audio_paths.push(path.display().to_string());
                    }
                }
            }
            let inline = attachment.get("content").filter(|value| truthy(value));
            if kind == "image"
                && !mime.is_empty()
                && let Some(inline) = inline
            {
                let mut image = JsObject::new();
                image.insert("type".into(), "image".into());
                image.insert("source".into(), "base64".into());
                image.insert("data".into(), inline.clone());
                image.insert("mimeType".into(), JsValue::String(mime.clone().into()));
                if let Some(bytes @ JsValue::Number(_)) = attachment.get("bytes") {
                    image.insert("bytes".into(), bytes.clone());
                }
                content.push(JsValue::Object(image));
                continue;
            }
            if kind == "text" && inline.is_some() {
                content.push(text_block(required_string(attachment, "content")?));
                continue;
            }
            let Some(path) = path else { continue };
            if audio {
                continue;
            }
            let info = match std::fs::metadata(&path) {
                Ok(info) if info.is_file() => info,
                Ok(_) => {
                    diagnostics.push(format!(
                        "Attachment not found: {} (not a regular file).",
                        path.display()
                    ));
                    continue;
                }
                Err(error) => {
                    diagnostics.push(format!(
                        "Attachment not found: {} ({error}).",
                        path.display()
                    ));
                    continue;
                }
            };
            if kind == "image" || mime.starts_with("image/") {
                if info.len() > crate::runtime::mcp::bridge::MCP_FILE_IMAGE_MAX_BYTES {
                    diagnostics.push(format!("Image {} is {} bytes (limit 5242880); skipped (PilotDeck does not resize, intentional_difference §4.5).", path.display(), info.len()));
                    continue;
                }
                let bytes =
                    read_limited(&path, crate::runtime::mcp::bridge::MCP_FILE_IMAGE_MAX_BYTES)?;
                let declared = if mime.is_empty() {
                    image_mime(&ext).unwrap_or_default()
                } else {
                    &mime
                };
                let Some(actual) = detected_image_mime(&bytes) else {
                    diagnostics.push(format!("Image {} could not be identified from its bytes; skipped and left as a local path diagnostic.", path.display()));
                    continue;
                };
                let (bytes, output_mime) = match validate_file_image(bytes, actual) {
                    Ok(image) => image,
                    Err(error) => {
                        diagnostics.push(format!("Image {} bytes look like {actual} but could not be decoded locally; skipped. Diagnostic: {error}", path.display()));
                        continue;
                    }
                };
                if !declared.eq_ignore_ascii_case(actual) {
                    diagnostics.push(format!("Image {} was declared as {declared} but bytes look like {actual}; skipped and left as a local path diagnostic.", path.display()));
                    continue;
                }
                let mut image = JsObject::new();
                image.insert("type".into(), "image".into());
                image.insert("source".into(), "base64".into());
                image.insert("data".into(), JsValue::String(BASE64.encode(&bytes).into()));
                image.insert("mimeType".into(), output_mime.into());
                image.insert("bytes".into(), JsValue::Number(bytes.len() as f64));
                content.push(JsValue::Object(image));
                if !registered {
                    diagnostics.push("PilotDeck does not resize images; original bytes forwarded (intentional_difference §4.5).".into());
                }
            } else if mime == "application/pdf" || extension(&path) == "pdf" {
                let pages = (info.len() as f64 / 102_400.0).round().max(1.0) as u64;
                content.push(text_block(format!("[PDF attachment: {}, {} bytes, estimated {pages} pages. Use read_file on this registered attachment path to inspect it.]", path.display(), info.len()).into()));
                if !registered {
                    diagnostics.push(format!("Estimated {pages} pages from {} bytes (PilotDeck does not invoke pdfinfo, intentional_difference §4.5).", info.len()));
                }
            } else if is_text(&ext) {
                if info.len() > 1_000_000 {
                    diagnostics.push(format!(
                        "Attachment {} is {} bytes (limit 1000000); skipped.",
                        path.display(),
                        info.len()
                    ));
                    continue;
                }
                let bytes = read_limited(&path, 1_000_000)?;
                content.push(text_block(
                    format!(
                        "<attachment path=\"{}\">\n{}\n</attachment>",
                        path.display(),
                        String::from_utf8_lossy(&bytes)
                    )
                    .into(),
                ));
            } else if !binary || !registered {
                diagnostics.push(if binary { format!("Attachment {} has Office/archive/binary extension .{ext}; it was registered as a file path but not shown inline, and read_file cannot inspect this format directly.", path.display()) } else { format!("File extension {} is not in the inline text whitelist; skipped. If this is plain text, use read_file with the exact path; otherwise convert it before inspection.", if ext.is_empty() { "(none)".into() } else { format!(".{ext}") }) });
            }
        }
        if !diagnostics.is_empty() {
            diagnostic_guidance = true;
            content.push(text_block(
                format!(
                    "[Attachment diagnostics]\n{}",
                    diagnostics
                        .iter()
                        .map(|message| format!("- {message}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                )
                .into(),
            ));
        }
        if !path_notes.is_empty() {
            let guidance = if attachments.iter().any(|attachment| {
                value_string(attachment, "mimeType")
                    .is_some_and(|mime| mime.to_lowercase().starts_with("audio/"))
                    || value_string(attachment, "path")
                        .or_else(|| value_string(attachment, "name"))
                        .is_some_and(|path| is_audio(Path::new(&path)))
            }) {
                let mapped = if audio_paths.is_empty() {
                    " Pass a project-local host path to transcribe_audio; paths outside this project are rejected.".into()
                } else {
                    format!(
                        " Pass the registered project-local path{} {} to transcribe_audio.",
                        if audio_paths.len() == 1 { "" } else { "s" },
                        audio_paths.join(", ")
                    )
                };
                format!(
                    "Audio attachments are not readable with read_file. When the user asks for transcription, subtitles, or audio analysis, use the funasr MCP server's mcp__funasr__transcribe_audio tool.{mapped} If that tool reports that its runtime is missing, run npm run install:asr and retry the tool in this session."
                )
            } else if !diagnostic_guidance {
                "These are path references for reuse. If an image/PDF is already visible in this turn, do not call read_file just to view it.".into()
            } else if has_inspectable {
                "Some attachments were not shown inline. Use read_file with the exact path only for readable text, image, PDF, or notebook attachments; Office/archive/binary files need conversion before inspection.".into()
            } else {
                "Some attachments were not shown inline. These registered files are not directly inspectable with read_file; ask for a supported export or convert them before inspection.".into()
            };
            content.push(text_block(
                format!(
                    "\n\n[Registered attachment files in this session:]\n{}\n{guidance}",
                    path_notes.join("\n")
                )
                .into(),
            ));
        }
        if content.is_empty() || !message.is_empty() {
            content.insert(0, text_block(message));
        }
        Ok(PreparedSteer {
            content,
            allowed_read_files: allowed,
        })
    }
}

impl SteerTurnComposer for ProductionSteerTurnComposer {
    fn prepare<'a>(&'a self, params: &'a JsValue) -> Call<'a, Result<PreparedSteer, RequestError>> {
        let params = params.clone();
        let composer = Self::new(self.cwd.clone());
        Box::pin(async move {
            tokio::task::spawn_blocking(move || composer.compose(&params))
                .await
                .map_err(|error| RequestError::new(error.to_string()))?
        })
    }
}

fn text_block(text: JsString) -> JsValue {
    let mut block = JsObject::new();
    block.insert("type".into(), "text".into());
    block.insert("text".into(), JsValue::String(text));
    JsValue::Object(block)
}

fn value_string(value: &JsValue, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok())
}

fn truthy(value: &JsValue) -> bool {
    match value {
        JsValue::Null | JsValue::Undefined => false,
        JsValue::Bool(value) => *value,
        JsValue::String(value) => !value.is_empty(),
        JsValue::Number(value) => *value != 0.0 && !value.is_nan(),
        _ => true,
    }
}

fn resolve_path(cwd: &Path, path: &str) -> PathBuf {
    let path = if Path::new(path).is_absolute() {
        PathBuf::from(path)
    } else {
        cwd.join(path)
    };
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            component => resolved.push(component.as_os_str()),
        }
    }
    resolved
}

fn extension(path: &Path) -> String {
    path.extension()
        .unwrap_or_default()
        .to_string_lossy()
        .to_lowercase()
}
fn is_audio(path: &Path) -> bool {
    matches!(
        extension(path).as_str(),
        "aac" | "flac" | "m4a" | "mp3" | "oga" | "ogg" | "opus" | "wav" | "webm"
    )
}
fn is_binary(ext: &str) -> bool {
    matches!(
        ext,
        "zip"
            | "gz"
            | "tar"
            | "7z"
            | "rar"
            | "doc"
            | "docx"
            | "ppt"
            | "pptx"
            | "xls"
            | "xlsx"
            | "odt"
            | "ods"
            | "odp"
            | "pages"
            | "key"
            | "numbers"
    )
}
fn is_text(ext: &str) -> bool {
    matches!(
        ext,
        "txt"
            | "md"
            | "json"
            | "yaml"
            | "yml"
            | "csv"
            | "tsv"
            | "xml"
            | "html"
            | "css"
            | "ts"
            | "tsx"
            | "js"
            | "jsx"
            | "py"
            | "go"
            | "rs"
            | "java"
            | "sh"
            | "log"
    )
}
fn image_mime(ext: &str) -> Option<&'static str> {
    match ext {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

fn read_limited(path: &Path, limit: u64) -> Result<Vec<u8>, RequestError> {
    let file = File::open(path).map_err(|error| RequestError::new(error.to_string()))?;
    if !file
        .metadata()
        .map_err(|error| RequestError::new(error.to_string()))?
        .is_file()
    {
        return Err(RequestError::new("Attachment is not a regular file."));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| RequestError::new(error.to_string()))?;
    if bytes.len() as u64 > limit {
        return Err(RequestError::new(
            "Attachment grew beyond its byte limit while reading.",
        ));
    }
    Ok(bytes)
}

fn detected_image_mime(bytes: &[u8]) -> Option<&'static str> {
    match image::guess_format(bytes).ok()? {
        ImageFormat::Png => Some("image/png"),
        ImageFormat::Jpeg => Some("image/jpeg"),
        ImageFormat::Gif => Some("image/gif"),
        ImageFormat::WebP => Some("image/webp"),
        _ => None,
    }
}

// sharp's default limitInputPixels; keep the same limit before any pixel allocation.
const MAX_IMAGE_PIXELS: u64 = 0x3fff * 0x3fff;

fn image_reader(bytes: &[u8]) -> Result<ImageReader<Cursor<&[u8]>>, String> {
    ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| error.to_string())
}

fn check_image_dimensions(width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 {
        return Err("Image dimensions must be nonzero.".into());
    }
    if u64::from(width) * u64::from(height) > MAX_IMAGE_PIXELS {
        return Err(format!(
            "Image dimensions exceed the {MAX_IMAGE_PIXELS} pixel limit."
        ));
    }
    Ok(())
}

fn image_metadata(bytes: &[u8]) -> Result<(), String> {
    let mut decoder = image_reader(bytes)?
        .into_decoder()
        .map_err(|error| error.to_string())?;
    let (width, height) = decoder.dimensions();
    check_image_dimensions(width, height)?;
    decoder.icc_profile().map_err(|error| error.to_string())?;
    decoder.exif_metadata().map_err(|error| error.to_string())?;
    Ok(())
}

#[cfg(test)]
fn image_metadata_mime(bytes: &[u8]) -> Option<&'static str> {
    image_metadata(bytes).ok()?;
    detected_image_mime(bytes)
}

fn repair_image(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut decoder = image_reader(bytes)?
        .into_decoder()
        .map_err(|error| error.to_string())?;
    let (width, height) = decoder.dimensions();
    check_image_dimensions(width, height)?;
    // from_decoder does not enforce the reader's output allocation budget itself.
    if decoder.total_bytes() > image::Limits::default().max_alloc.unwrap_or(u64::MAX) {
        return Err("Image exceeds the decoder allocation limit.".into());
    }
    let orientation = decoder.orientation().map_err(|error| error.to_string())?;
    let mut decoded =
        image::DynamicImage::from_decoder(decoder).map_err(|error| error.to_string())?;
    decoded.apply_orientation(orientation);
    let mut output = Vec::new();
    decoded
        .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut output,
            90,
        ))
        .map_err(|error| error.to_string())?;
    if output.len() as u64 > crate::runtime::mcp::bridge::MCP_FILE_IMAGE_MAX_BYTES {
        return Err("Repaired image exceeds the 5242880 byte limit.".into());
    }
    Ok(output)
}

fn validate_file_image(
    bytes: Vec<u8>,
    actual_mime: &'static str,
) -> Result<(Vec<u8>, &'static str), String> {
    // Metadata success preserves original bytes, including animation and EXIF, as TS does.
    match image_metadata(&bytes) {
        Ok(()) => Ok((bytes, actual_mime)),
        Err(_) => repair_image(&bytes)
            .map(|repaired| (repaired, "image/jpeg"))
            .map_err(|error| {
                error
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
                    .chars()
                    .take(300)
                    .collect()
            }),
    }
}

pub struct ControlRoutes<'a> {
    owner: &'a SessionOwner,
    composer: &'a dyn SteerTurnComposer,
}

impl<'a> ControlRoutes<'a> {
    pub fn new(owner: &'a SessionOwner, composer: &'a dyn SteerTurnComposer) -> Self {
        Self { owner, composer }
    }

    pub fn handle<'b>(
        &'b self,
        method: &str,
        params: &'b JsValue,
    ) -> Option<Call<'b, Result<JsValue, RequestError>>> {
        match method {
            "steer_turn" => Some(Box::pin(self.steer_turn(params))),
            "cancel_steer" => Some(Box::pin(self.cancel_steer(params))),
            _ => None,
        }
    }

    fn active(&self, params: &JsValue) -> Result<(JsString, JsString), SteerReason> {
        let session = params
            .get("sessionKey")
            .and_then(JsValue::as_string)
            .ok_or(SteerReason::NoActiveTurn)?;
        let run = self
            .owner
            .active_turn_run_id(session)
            .filter(|run| !run.is_empty())
            .ok_or(SteerReason::NoActiveTurn)?;
        if params.get("runId").and_then(JsValue::as_string) != Some(&run) {
            return Err(SteerReason::TurnMismatch);
        }
        Ok((session.clone(), run))
    }

    pub async fn steer_turn(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let (session, run) = match self.active(params) {
            Ok(active) => active,
            Err(reason) => return Ok(response("accepted", false, Some(reason))),
        };
        let item_id = required_string(params, "itemId")?;
        let prepared = self.composer.prepare(params).await?;
        // Re-select after attachment I/O, as TS SessionRouter.steer does.
        let Some(handle) = self.owner.current_handle(&session) else {
            return Ok(response("accepted", false, Some(SteerReason::NoActiveTurn)));
        };
        let mut metadata = JsObject::new();
        metadata.insert("purpose".into(), "mid_turn_steer".into());
        metadata.insert("queueItemId".into(), JsValue::String(item_id.clone()));
        let mut message = JsObject::new();
        message.insert("role".into(), "user".into());
        message.insert("content".into(), JsValue::Array(prepared.content));
        message.insert("metadata".into(), JsValue::Object(metadata));
        let result = handle
            .steer(
                run,
                SteerMessage {
                    item_id,
                    message: JsValue::Object(message),
                    allowed_read_files: Some(prepared.allowed_read_files),
                },
            )
            .await
            .map_err(|error| RequestError::new(error.to_string()))?;
        Ok(response("accepted", result.accepted, result.reason))
    }

    pub async fn cancel_steer(&self, params: &JsValue) -> Result<JsValue, RequestError> {
        let (session, run) = match self.active(params) {
            Ok(active) => active,
            Err(reason) => return Ok(response("cancelled", false, Some(reason))),
        };
        let item_id = required_string(params, "itemId")?;
        let Some(handle) = self.owner.current_handle(&session) else {
            return Ok(response(
                "cancelled",
                false,
                Some(SteerReason::NoActiveTurn),
            ));
        };
        let result = handle
            .cancel_steer(run, item_id)
            .await
            .map_err(|error| RequestError::new(error.to_string()))?;
        Ok(response("cancelled", result.cancelled, result.reason))
    }
}

fn required_string(params: &JsValue, key: &str) -> Result<JsString, RequestError> {
    params
        .get(key)
        .and_then(JsValue::as_string)
        .cloned()
        .ok_or_else(|| RequestError::new(format!("{key} must be a string.")))
}

fn response(field: &str, value: bool, reason: Option<SteerReason>) -> JsValue {
    let mut result = JsObject::new();
    result.insert(field.into(), JsValue::Bool(value));
    if let Some(reason) = reason {
        result.insert("reason".into(), reason.as_str().into());
    }
    JsValue::Object(result)
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "image_tests.rs"]
mod image_tests;
