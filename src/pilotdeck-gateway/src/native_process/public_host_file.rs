//! `file_parse` for the selected native public host runtime.
//!
//! Validation order and response shape follow cc7c4e6's public host adapter
//! and active text parsing port. The HTTP owner runs this blocking function
//! away from the async reactor; its existing transport owns cancellation.
use base64::{
    Engine as _, alphabet,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use pilotdeck_protocol::{JsString, JsValue};

use crate::{
    metadata::object,
    module_host::{HostBody, HostResponse},
    runtime::model::router::AbortFlag,
};

const MAX_BYTES: usize = 20 * 1024 * 1024;
const BASE64: GeneralPurpose = GeneralPurpose::new(
    &alphabet::STANDARD,
    GeneralPurposeConfig::new()
        .with_decode_allow_trailing_bits(true)
        .with_decode_padding_mode(DecodePaddingMode::RequireCanonical),
);

fn response(status: u16, body: JsValue) -> HostResponse {
    HostResponse {
        status,
        headers: Vec::new(),
        body: HostBody::Value(body),
        raw_body: None,
    }
}
fn error(status: u16, code: &'static str, message: &'static str) -> HostResponse {
    response(
        status,
        object([
            ("code", JsValue::String(code.into())),
            ("message", JsValue::String(message.into())),
        ]),
    )
}
fn required<'a>(
    input: &'a JsValue,
    key: &'static str,
    message: &'static str,
) -> Result<&'a JsString, HostResponse> {
    match input.get(key).and_then(JsValue::as_string) {
        Some(value) if !crate::model_catalog::trim(value).is_empty() => Ok(value),
        _ => Err(error(502, "Error", message)),
    }
}

// The adapter's regex validates alphabet/padding shape, but deliberately does
// not reject nonzero unused bits: Node Buffer.from("YR==", "base64") is "a".
fn base64_shape(value: &JsString) -> Option<(String, usize)> {
    let raw = value.to_utf8().ok()?;
    let bytes = raw.as_bytes();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return None;
    }
    let padding = bytes.iter().rev().take_while(|c| **c == b'=').count();
    if padding > 2 {
        return None;
    }
    let data_len = bytes.len().checked_sub(padding)?;
    if !bytes[..data_len]
        .iter()
        .all(|c| c.is_ascii_alphanumeric() || *c == b'+' || *c == b'/')
    {
        return None;
    }
    if !bytes[data_len..].iter().all(|c| *c == b'=') {
        return None;
    }
    if (padding == 1 && data_len % 4 != 3) || (padding == 2 && data_len % 4 != 2) {
        return None;
    }
    let decoded = (bytes.len() / 4).checked_mul(3)?.checked_sub(padding)?;
    Some((raw, decoded))
}

// POSIX extname ignores trailing slashes and a single leading dot. Work in
// UTF-16 so a filename containing an unpaired surrogate survives the response.
fn extension(filename: &JsString) -> String {
    let units = filename.units();
    let end = units
        .iter()
        .rposition(|u| *u != u16::from(b'/'))
        .map_or(0, |i| i + 1);
    let start = units[..end]
        .iter()
        .rposition(|u| *u == u16::from(b'/'))
        .map_or(0, |i| i + 1);
    let base = &units[start..end];
    if base == [u16::from(b'.')] || base == [u16::from(b'.'), u16::from(b'.')] {
        return String::new();
    }
    let Some(dot) = base.iter().rposition(|u| *u == u16::from(b'.')) else {
        return String::new();
    };
    if dot == 0 {
        return String::new();
    }
    base[dot..]
        .iter()
        .map(|u| {
            char::from_u32(u32::from(*u))
                .unwrap_or('\u{fffd}')
                .to_ascii_lowercase()
        })
        .collect()
}

fn cancelled(abort: Option<&AbortFlag>) -> bool {
    abort.is_some_and(AbortFlag::is_aborted)
}

fn cancelled_response() -> HostResponse {
    response(
        499,
        object([("code", JsValue::String("PUBLIC_HOST_CANCELLED".into()))]),
    )
}

// The callback is a no-op in production; tests use it to establish an exact
// page boundary without relying on scheduler timing.
fn pdf_text(
    bytes: &[u8],
    abort: Option<&AbortFlag>,
    after_page: &mut impl FnMut(),
) -> Result<String, mupdf::Error> {
    let document = mupdf::Document::from_bytes(bytes, "application/pdf")?;
    let mut pages = Vec::new();
    for page in 0..document.page_count()? {
        if cancelled(abort) {
            break;
        }
        pages.push(
            document
                .load_page(page)?
                .to_text_page(mupdf::TextPageFlags::empty())?
                .to_text()?,
        );
        after_page();
    }
    Ok(pages.join("\n"))
}

/// Parse one already-authorized capability input on a blocking worker.
#[cfg(test)]
pub(super) fn parse(input: &JsValue) -> HostResponse {
    parse_with_abort(input, None)
}

pub(super) fn parse_with_abort(input: &JsValue, abort: Option<&AbortFlag>) -> HostResponse {
    parse_inner(input, abort, &mut || {})
}

#[cfg(test)]
pub(super) fn parse_observed_for_test(
    input: &JsValue,
    abort: Option<&AbortFlag>,
    after_page: &mut impl FnMut(),
) -> HostResponse {
    parse_inner(input, abort, after_page)
}

fn parse_inner(
    input: &JsValue,
    abort: Option<&AbortFlag>,
    after_page: &mut impl FnMut(),
) -> HostResponse {
    if cancelled(abort) {
        return cancelled_response();
    }
    if input.as_object().is_none() {
        return error(400, "PUBLIC_HOST_INPUT_INVALID", "input must be an object.");
    }
    let encoded = match required(
        input,
        "content_base64",
        "content_base64 must be a non-empty string.",
    ) {
        Ok(v) => v,
        Err(e) => {
            return if cancelled(abort) {
                cancelled_response()
            } else {
                e
            };
        }
    };
    let Some((encoded, byte_count)) = base64_shape(encoded) else {
        return error(
            400,
            "PUBLIC_HOST_INPUT_INVALID",
            "content_base64 must be canonical base64.",
        );
    };
    let filename = match required(input, "filename", "filename must be a non-empty string.") {
        Ok(v) => v.clone(),
        Err(e) => {
            return if cancelled(abort) {
                cancelled_response()
            } else {
                e
            };
        }
    };
    let ext = extension(&filename);
    if !matches!(ext.as_str(), ".txt" | ".md" | ".markdown" | ".pdf") {
        return error(
            415,
            "PUBLIC_FILE_TYPE_UNSUPPORTED",
            "This PD text extractor accepts PDF, .txt and Markdown",
        );
    }
    let media_type = match input.get("media_type") {
        None | Some(JsValue::Undefined | JsValue::Null) => JsValue::String(
            if ext == ".pdf" {
                "application/pdf"
            } else {
                "text/plain"
            }
            .into(),
        ),
        Some(JsValue::String(value)) => JsValue::String(value.clone()),
        _ => {
            return error(
                400,
                "PUBLIC_FILE_INPUT_INVALID",
                "mediaType must be a string",
            );
        }
    };
    let limit = match input.get("max_bytes") {
        None | Some(JsValue::Undefined) => MAX_BYTES,
        Some(JsValue::Number(value))
            if value.is_finite()
                && value.fract() == 0.0
                && *value > 0.0
                && *value <= MAX_BYTES as f64 =>
        {
            *value as usize
        }
        _ => {
            return error(
                400,
                "PUBLIC_FILE_INPUT_INVALID",
                "maxBytes must be within 20 MiB",
            );
        }
    };
    if byte_count > limit {
        return error(
            413,
            "PUBLIC_FILE_TOO_LARGE",
            "File exceeds selected extraction limit",
        );
    }
    let bytes = match BASE64.decode(encoded.as_bytes()) {
        Ok(bytes) => bytes,
        Err(_) => {
            return error(
                400,
                "PUBLIC_HOST_INPUT_INVALID",
                "content_base64 must be canonical base64.",
            );
        }
    };
    if cancelled(abort) {
        return cancelled_response();
    }
    let text = if ext == ".pdf" {
        match pdf_text(&bytes, abort, after_page) {
            Ok(text) => text,
            Err(_) => {
                if cancelled(abort) {
                    return cancelled_response();
                }
                return error(
                    422,
                    "PUBLIC_FILE_PARSE_FAILED",
                    "PDF text extraction failed",
                );
            }
        }
    } else {
        match std::str::from_utf8(&bytes) {
            Ok(text) => text.strip_prefix('\u{feff}').unwrap_or(text).to_owned(),
            Err(_) => {
                if cancelled(abort) {
                    return cancelled_response();
                }
                return error(
                    422,
                    "PUBLIC_FILE_ENCODING_UNSUPPORTED",
                    "File is not valid UTF-8",
                );
            }
        }
    };
    if cancelled(abort) {
        return cancelled_response();
    }
    response(
        200,
        object([
            ("filename", JsValue::String(filename)),
            ("text", JsValue::String(text.into())),
            ("mediaType", media_type),
            (
                "metadata",
                object([
                    ("fileType", JsValue::String(ext[1..].into())),
                    ("bytes", JsValue::Number(byte_count as f64)),
                ]),
            ),
        ]),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use pilotdeck_protocol::parse_json;
    fn run(raw: &str) -> (u16, JsValue) {
        let value = parse_json(raw).unwrap();
        let response = parse(&value);
        match response.body {
            HostBody::Value(body) => (response.status, body),
            _ => panic!("expected JSON body"),
        }
    }
    fn field<'a>(value: &'a JsValue, key: &str) -> &'a JsValue {
        value.get(key).unwrap()
    }
    #[test]
    fn text_and_base64_contract() {
        let (status, body) = run(r#"{"filename":"folder/test.MD/","content_base64":"77u/aGk="}"#);
        assert_eq!(status, 200);
        assert_eq!(field(&body, "text"), &JsValue::String("hi".into()));
        assert_eq!(
            field(field(&body, "metadata"), "fileType"),
            &JsValue::String("md".into())
        );
        assert_eq!(
            run(r#"{"filename":"a.txt","content_base64":"YR=="}"#).0,
            200
        );
        assert_eq!(run(r#"{"filename":"a.txt","content_base64":"YQ"}"#).0, 400);
        assert_eq!(run(r#"{"filename":"a.txt","content_base64":""}"#).0, 502);
        assert_eq!(run(r#"{"filename":".txt","content_base64":"YQ=="}"#).0, 415);
    }
    #[test]
    fn validation_order_and_fatal_utf8() {
        assert_eq!(
            run(r#"{"filename":"x.jpg","content_base64":"YQ==","media_type":1}"#).0,
            415
        );
        assert_eq!(
            run(r#"{"filename":"x.txt","content_base64":"YQ==","media_type":1}"#).0,
            400
        );
        assert_eq!(
            run(r#"{"filename":"x.txt","content_base64":"YQ==","max_bytes":null}"#).0,
            400
        );
        assert_eq!(
            run(r#"{"filename":"x.txt","content_base64":"YWI=","max_bytes":1}"#).0,
            413
        );
        assert_eq!(
            run(r#"{"filename":"x.txt","content_base64":"/w=="}"#).0,
            422
        );
        assert_eq!(
            run(r#"{"filename":"x.pdf","content_base64":"YQ=="}"#).0,
            422
        );
    }
    #[test]
    fn preserves_unpaired_filename_surrogate() {
        let (status, body) = run(r#"{"filename":"x\ud800.txt","content_base64":"YQ=="}"#);
        assert_eq!(status, 200);
        assert_eq!(
            field(&body, "filename").as_string().unwrap().units(),
            &[120, 0xd800, 46, 116, 120, 116]
        );
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
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", offsets.len()).as_bytes(),
        );
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
    fn pdf_real_pages_and_empty_document() {
        for (pages, expected) in [
            (
                vec!["FIRST PAGE 42", "SECOND PAGE 84"],
                "FIRST PAGE 42\n\n\nSECOND PAGE 84\n\n",
            ),
            (vec![], ""),
        ] {
            let encoded = BASE64.encode(tiny_pdf(&pages));
            let input = object([
                ("filename", JsValue::String("sample.PDF".into())),
                ("content_base64", JsValue::String(encoded.into())),
            ]);
            let response = parse(&input);
            assert_eq!(response.status, 200);
            match response.body {
                HostBody::Value(body) => {
                    assert_eq!(field(&body, "text"), &JsValue::String(expected.into()))
                }
                _ => panic!("expected JSON body"),
            }
        }
    }
    #[test]
    fn cancellation_precedes_input_validation_and_has_no_message() {
        let flag = AbortFlag::new();
        flag.abort();
        let response = parse_with_abort(&JsValue::Null, Some(&flag));
        assert_eq!(response.status, 499);
        match response.body {
            HostBody::Value(body) => {
                assert_eq!(
                    body.get("code"),
                    Some(&JsValue::String("PUBLIC_HOST_CANCELLED".into()))
                );
                assert!(body.get("message").is_none());
            }
            _ => panic!("expected JSON body"),
        }
    }

    #[test]
    fn cancellation_between_real_pdf_pages_stops_before_next_page() {
        let flag = AbortFlag::new();
        let encoded = BASE64.encode(tiny_pdf(&["FIRST", "SECOND"]));
        let input = object([
            ("filename", JsValue::String("sample.pdf".into())),
            ("content_base64", JsValue::String(encoded.into())),
        ]);
        let mut pages = 0;
        let response = parse_inner(&input, Some(&flag), &mut || {
            pages += 1;
            flag.abort();
        });
        assert_eq!(pages, 1);
        assert_eq!(response.status, 499);
        match response.body {
            HostBody::Value(body) => {
                assert_eq!(
                    body.get("code"),
                    Some(&JsValue::String("PUBLIC_HOST_CANCELLED".into()))
                );
                assert!(body.get("message").is_none());
            }
            _ => panic!("expected JSON body"),
        }
    }
}
