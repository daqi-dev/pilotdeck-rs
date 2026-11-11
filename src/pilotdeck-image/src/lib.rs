//! Scoped image pipeline; all input bytes outlive the lazy libvips graph.
use libvips::{VipsApp, VipsImage, ops};
use std::sync::OnceLock;

fn initialize() -> Result<(), String> {
    static INIT: OnceLock<Result<(), String>> = OnceLock::new();
    INIT.get_or_init(|| {
        let app = VipsApp::new("pilotdeck-image", false).map_err(|e| e.to_string())?;
        // Cached lazy loaders may otherwise retain borrowed input after this call.
        app.cache_set_max(0);
        // libvips shutdown is process-global; keep initialization alive across calls.
        std::mem::forget(app);
        Ok(())
    })
    .clone()
}

fn encode(
    input: &[u8],
    output: &str,
    side: Option<f64>,
    q: i32,
    rotate: bool,
) -> Result<Vec<u8>, String> {
    initialize()?;
    // libvips is lazy: `input` remains live until save has completed and all
    // image nodes below are destroyed. Never return a VipsImage from here.
    let loaded =
        VipsImage::new_from_buffer(input, "[fail_on=warning]").map_err(|e| e.to_string())?;
    let rotated = if rotate {
        Some(ops::autorot(&loaded).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let image = rotated.as_ref().unwrap_or(&loaded);
    let scale = side
        .map(|s| {
            (s / f64::from(image.get_width()))
                .min(s / f64::from(image.get_height()))
                .min(1.0)
        })
        .unwrap_or(1.0);
    let resized = if scale < 1.0 {
        Some(ops::resize(image, scale).map_err(|e| e.to_string())?)
    } else {
        None
    };
    let image = resized.as_ref().unwrap_or(image);
    let bytes = match output {
        "image/jpeg" => ops::jpegsave_buffer_with_opts(
            image,
            &ops::JpegsaveBufferOptions {
                q,
                optimize_coding: true,
                subsample_mode: ops::ForeignSubsample::On,
                keep: ops::ForeignKeep::None,
                profile: None,
                ..Default::default()
            },
        ),
        "image/png" => ops::pngsave_buffer_with_opts(
            image,
            &ops::PngsaveBufferOptions {
                compression: 9,
                keep: ops::ForeignKeep::None,
                profile: None,
                ..Default::default()
            },
        ),
        "image/webp" => ops::webpsave_buffer_with_opts(
            image,
            &ops::WebpsaveBufferOptions {
                q,
                keep: ops::ForeignKeep::None,
                profile: None,
                ..Default::default()
            },
        ),
        _ => return Err("unsupported image output".into()),
    }
    .map_err(|e| e.to_string())?;
    Ok(bytes)
}

pub fn prepare(input: &[u8], mime: &str, max: f64) -> Result<(Vec<u8>, String), String> {
    let invalid = || {
        format!(
            "Image file appears truncated or corrupted ({} bytes). Cannot decode.",
            input.len()
        )
    };
    let fast_jpeg = mime == "image/jpeg" && input.len() > 1000 && input.ends_with(&[0xff, 0xd9]);
    let metadata_ok = if !fast_jpeg && mime != "image/jpeg" && input.len() > 1000 {
        initialize()
            .and_then(|()| {
                VipsImage::new_from_buffer(input, "[fail_on=warning]")
                    .map(|_| ())
                    .map_err(|e| e.to_string())
            })
            .is_ok()
    } else {
        false
    };
    let (data, kind) = if fast_jpeg || metadata_ok {
        (input.to_vec(), mime.to_owned())
    } else {
        (
            encode(input, "image/jpeg", None, 90, false).map_err(|_| invalid())?,
            "image/jpeg".to_owned(),
        )
    };
    compress(data, kind, max)
}

pub fn compress(
    mut data: Vec<u8>,
    mut kind: String,
    max: f64,
) -> Result<(Vec<u8>, String), String> {
    if data.len() as f64 > max {
        let initial_mime = kind.clone();
        let first_kind = match kind.as_str() {
            "image/png" | "image/gif" => "image/png",
            "image/webp" => "image/webp",
            _ => "image/jpeg",
        };
        let first_side = if kind == "image/gif" { 1200.0 } else { 1600.0 };
        if let Ok(bytes) = encode(&data, first_kind, Some(first_side), 80, true) {
            data = bytes;
            kind = first_kind.into();
            if data.len() as f64 > max
                && let Ok(bytes) = encode(&data, "image/jpeg", Some(1200.0), 55, false)
            {
                data = bytes;
                kind = "image/jpeg".into();
                if data.len() as f64 > max
                    && let Ok(bytes) = encode(&data, "image/jpeg", Some(800.0), 40, false)
                {
                    data = bytes;
                    kind = "image/jpeg".into();
                }
            }
        }
        if data.len() as f64 > max {
            return Err(format!(
                "Image content exceeds the read_file byte budget after compression attempts ({initial_mime})."
            ));
        }
    }
    Ok((data, kind))
}
