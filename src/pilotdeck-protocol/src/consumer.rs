//! The original sidecar consumer's narrow wire admission, not a handshake.
use crate::{
    JsString, JsValue, MODULE_PROTOCOL_VERSION,
    admission::{
        AdmissionError, AdmissionErrorKind, CapabilityMethod, SidecarBinding, SidecarCapabilities,
    },
    module::ModuleMethod,
    wire::ResponseRef,
};

fn failure(kind: AdmissionErrorKind, message: &str) -> AdmissionError {
    AdmissionError {
        kind,
        message: message.into(),
    }
}

pub(crate) fn parse_binding(response: ResponseRef<'_>) -> Result<SidecarBinding, AdmissionError> {
    let version = response.fields.get("protocolVersion");
    if !version
        .and_then(JsValue::as_string)
        .is_some_and(|v| v.equals_str(MODULE_PROTOCOL_VERSION))
    {
        let version = match version {
            None | Some(JsValue::Undefined | JsValue::Null) => "missing".into(),
            Some(value) => wire_string(value)?,
        };
        let mut message: Vec<u16> = "Sidecar protocol version is unsupported: "
            .encode_utf16()
            .collect();
        message.extend_from_slice(version.units());
        message.push(u16::from(b'.'));
        return Err(AdmissionError {
            kind: AdmissionErrorKind::UnsupportedVersion,
            message: JsString::from_utf16(message),
        });
    }
    let field = |name: &str| {
        response
            .fields
            .get(name)
            .and_then(JsValue::as_string)
            .filter(|value| !value.is_empty())
            .cloned()
            .ok_or_else(|| {
                failure(
                    AdmissionErrorKind::MissingBinding,
                    &format!("Sidecar handshake response is missing {name}."),
                )
            })
    };
    // Error order is significant even when several fields are invalid.
    Ok(SidecarBinding {
        module_id: field("moduleId")?,
        module_instance_id: field("moduleInstanceId")?,
        connection_generation: field("connectionGeneration")?,
        capabilities_version: field("capabilitiesVersion")?,
    })
}

pub(crate) fn parse_capabilities(
    response: ResponseRef<'_>,
    expected_version: &JsString,
) -> Result<SidecarCapabilities, AdmissionError> {
    let mismatch = || {
        failure(
            AdmissionErrorKind::CapabilitiesMismatch,
            "Sidecar capabilities response does not match its hello binding.",
        )
    };
    let payload = response.payload.as_object().ok_or_else(mismatch)?;
    let version = payload
        .get("capabilitiesVersion")
        .and_then(JsValue::as_string)
        .filter(|v| *v == expected_version)
        .ok_or_else(mismatch)?;
    let Some(JsValue::Array(methods)) = payload.get("methods") else {
        return Err(mismatch());
    };
    let invalid = || {
        failure(
            AdmissionErrorKind::InvalidMethod,
            "Sidecar capabilities response contains an invalid method.",
        )
    };
    let mut admitted = Vec::with_capacity(methods.len());
    for method in methods {
        let fields = method.as_object().ok_or_else(invalid)?;
        let name = fields
            .get("name")
            .and_then(JsValue::as_string)
            .and_then(|name| {
                [
                    ("execute", ModuleMethod::Execute),
                    ("cancel", ModuleMethod::Cancel),
                    ("status", ModuleMethod::Status),
                    ("resume", ModuleMethod::Resume),
                    ("ack", ModuleMethod::Ack),
                ]
                .into_iter()
                .find_map(|(text, method)| name.equals_str(text).then_some(method))
            })
            .ok_or_else(invalid)?;
        admitted.push(CapabilityMethod {
            name,
            fields: fields.clone(),
        });
    }
    Ok(SidecarCapabilities {
        version: version.clone(),
        methods: admitted,
    })
}

/// Template interpolation for JSON-compatible wire values. This does not
/// pretend to execute JS functions, getters, Symbol.toPrimitive or prototypes.
/// An own non-callable toString shadows the plain object's inherited method;
/// its inherited valueOf returns the object, so JS throws a distinct TypeError.
fn wire_string(value: &JsValue) -> Result<JsString, AdmissionError> {
    enum Task<'a> {
        Value(&'a JsValue),
        Comma,
    }
    let mut pending = vec![Task::Value(value)];
    let mut units = Vec::new();
    let mut buffer = ryu_js::Buffer::new();
    while let Some(task) = pending.pop() {
        let Task::Value(value) = task else {
            units.push(u16::from(b','));
            continue;
        };
        let text = match value {
            JsValue::Undefined => "undefined",
            JsValue::Null => "null",
            JsValue::Bool(true) => "true",
            JsValue::Bool(false) => "false",
            JsValue::Number(number) if number.is_nan() => "NaN",
            JsValue::Number(number) if *number == f64::INFINITY => "Infinity",
            JsValue::Number(number) if *number == f64::NEG_INFINITY => "-Infinity",
            JsValue::Number(number) if *number == 0.0 => "0",
            JsValue::Number(number) => buffer.format_finite(*number),
            JsValue::String(text) => {
                units.extend_from_slice(text.units());
                continue;
            }
            JsValue::Array(items) => {
                // Array#join leaves null/undefined elements empty, including
                // nested arrays, with no recursive Rust call stack.
                for (index, item) in items.iter().enumerate().rev() {
                    if !matches!(item, JsValue::Undefined | JsValue::Null) {
                        pending.push(Task::Value(item));
                    }
                    if index > 0 {
                        pending.push(Task::Comma);
                    }
                }
                continue;
            }
            JsValue::Object(object) if object.contains_key("toString") => {
                return Err(failure(
                    AdmissionErrorKind::TypeError,
                    "Cannot convert object to primitive value",
                ));
            }
            JsValue::Object(_) => "[object Object]",
        };
        units.extend(text.encode_utf16());
    }
    Ok(JsString::from_utf16(units))
}
