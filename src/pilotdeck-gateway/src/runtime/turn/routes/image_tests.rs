use super::*;
use crate::scratch::Scratch;
use image::{DynamicImage, ImageEncoder};
use pilotdeck_protocol::parse_json;

fn encoded(format: ImageFormat) -> Vec<u8> {
    let image = DynamicImage::ImageRgb8(image::RgbImage::from_fn(2, 3, |x, y| {
        image::Rgb([(x * 90) as u8, (y * 70) as u8, 120])
    }));
    let mut output = Cursor::new(Vec::new());
    image.write_to(&mut output, format).unwrap();
    output.into_inner()
}

async fn compose_file(bytes: &[u8], name: &str, mime: &str) -> PreparedSteer {
    let scratch = Scratch::new("turn-images");
    std::fs::write(scratch.path().join(name), bytes).unwrap();
    let params = serde_json::json!({
        "message": "inspect",
        "attachments": [{
            "type": "image", "path": name, "mimeType": mime,
            "metadata": {"channelKey": "cli"}
        }]
    });
    ProductionSteerTurnComposer::new(scratch.path())
        .prepare(&parse_json(&params.to_string()).unwrap())
        .await
        .unwrap()
}

fn images(prepared: &PreparedSteer) -> Vec<&JsValue> {
    prepared
        .content
        .iter()
        .filter(|block| block.get("type") == Some(&JsValue::from("image")))
        .collect()
}

fn diagnostic(prepared: &PreparedSteer) -> String {
    prepared
        .content
        .iter()
        .filter_map(|block| block.get("text").and_then(JsValue::as_string))
        .map(|text| text.to_utf8().unwrap())
        .collect::<Vec<_>>()
        .join("\n")
}

fn image_bytes(block: &JsValue) -> Vec<u8> {
    BASE64
        .decode(
            block
                .get("data")
                .unwrap()
                .as_string()
                .unwrap()
                .to_utf8()
                .unwrap(),
        )
        .unwrap()
}

#[tokio::test]
async fn real_png_jpeg_gif_webp_files_preserve_original_bytes() {
    for (format, name, mime) in [
        (ImageFormat::Png, "normal.png", "image/png"),
        (ImageFormat::Jpeg, "normal.jpg", "image/jpeg"),
        (ImageFormat::Gif, "normal.gif", "image/gif"),
        (ImageFormat::WebP, "normal.webp", "image/webp"),
    ] {
        let bytes = encoded(format);
        assert_eq!(
            image::load_from_memory(&bytes).unwrap().dimensions(),
            (2, 3)
        );
        let prepared = compose_file(&bytes, name, mime).await;
        let blocks = images(&prepared);
        assert_eq!(blocks.len(), 1, "{name}: {}", diagnostic(&prepared));
        assert_eq!(blocks[0].get("mimeType"), Some(&mime.into()));
        assert_eq!(
            blocks[0].get("bytes"),
            Some(&JsValue::Number(bytes.len() as f64))
        );
        assert_eq!(image_bytes(blocks[0]), bytes);
        assert!(!prepared.allowed_read_files.is_empty());
    }
}

#[tokio::test]
async fn valid_png_header_without_image_data_is_diagnosed_and_not_forwarded() {
    let bytes = encoded(ImageFormat::Png);
    let header = &bytes[..33]; // Complete signature + IHDR including its valid CRC.
    assert_eq!(&header[12..16], b"IHDR");
    assert_eq!(u32::from_be_bytes(header[16..20].try_into().unwrap()), 2);
    let prepared = compose_file(header, "truncated.png", "image/png").await;
    assert!(images(&prepared).is_empty());
    assert!(diagnostic(&prepared).contains("could not be decoded locally"));
    assert!(!prepared.allowed_read_files.is_empty());
}

#[tokio::test]
async fn matching_magic_is_insufficient_for_fake_webp_chunks() {
    let mut bytes = b"RIFF".to_vec();
    bytes.extend_from_slice(&22_u32.to_le_bytes());
    bytes.extend_from_slice(b"WEBPJUNK");
    bytes.extend_from_slice(&10_u32.to_le_bytes());
    bytes.extend_from_slice(&[0; 10]);
    let prepared = compose_file(&bytes, "invalid.webp", "image/webp").await;
    assert!(images(&prepared).is_empty());
    assert!(diagnostic(&prepared).contains("could not be decoded locally"));
}

#[tokio::test]
async fn valid_image_with_mismatched_mime_is_not_forwarded() {
    let prepared = compose_file(&encoded(ImageFormat::Png), "declared.jpg", "image/jpeg").await;
    assert!(images(&prepared).is_empty());
    assert!(diagnostic(&prepared).contains("declared as image/jpeg but bytes look like image/png"));
}

#[tokio::test]
async fn oversized_pixel_dimensions_are_rejected_before_pixel_allocation() {
    let mut bytes = encoded(ImageFormat::Jpeg);
    let start = bytes.windows(2).position(|b| b == [0xff, 0xc0]).unwrap();
    bytes[start + 5..start + 7].copy_from_slice(&20_000_u16.to_be_bytes());
    bytes[start + 7..start + 9].copy_from_slice(&20_000_u16.to_be_bytes());
    let prepared = compose_file(&bytes, "huge.jpg", "image/jpeg").await;
    assert!(images(&prepared).is_empty());
    assert!(diagnostic(&prepared).contains("pixel limit"));
    assert!(check_image_dimensions(16_383, 16_383).is_ok());
    assert!(check_image_dimensions(16_384, 16_384).is_err());
}

#[tokio::test]
async fn files_over_five_mib_are_not_decoded_or_forwarded() {
    let mut bytes = encoded(ImageFormat::Png);
    bytes.resize(
        crate::runtime::mcp::bridge::MCP_FILE_IMAGE_MAX_BYTES as usize + 1,
        0,
    );
    let prepared = compose_file(&bytes, "too-large.png", "image/png").await;
    assert!(images(&prepared).is_empty());
    assert!(diagnostic(&prepared).contains("limit 5242880"));
}

#[tokio::test]
async fn metadata_failure_with_decodable_pixels_repairs_to_jpeg_in_composer() {
    let original = encoded(ImageFormat::WebP);
    // Extended WebP with a truncated ICC profile. Pixels remain complete.
    let mut bytes = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0".to_vec();
    bytes.extend_from_slice(&[0x20, 0, 0, 0, 1, 0, 0, 2, 0, 0]);
    bytes.extend_from_slice(&original[12..]);
    bytes.extend_from_slice(b"ICCP\x04\0\0\0");
    let size = bytes.len() as u32 - 8 + 4;
    bytes[4..8].copy_from_slice(&size.to_le_bytes());
    assert!(image_metadata(&bytes).is_err());
    assert_eq!(
        image::load_from_memory(&bytes).unwrap().dimensions(),
        (2, 3)
    );
    let prepared = compose_file(&bytes, "repair.webp", "image/webp").await;
    let blocks = images(&prepared);
    assert_eq!(blocks.len(), 1, "{}", diagnostic(&prepared));
    assert_eq!(blocks[0].get("mimeType"), Some(&"image/jpeg".into()));
    let repaired = image_bytes(blocks[0]);
    assert_eq!(image::guess_format(&repaired).unwrap(), ImageFormat::Jpeg);
    assert_eq!(
        image::load_from_memory(&repaired).unwrap().dimensions(),
        (2, 3)
    );
    assert_eq!(repaired, repair_image(&bytes).unwrap());
}

#[test]
fn jpeg_repair_applies_exif_orientation_and_quality_ninety() {
    let source = DynamicImage::new_rgb8(2, 3);
    let mut original = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut original, 90);
    encoder
        .set_exif_metadata(vec![
            b'I', b'I', 42, 0, 8, 0, 0, 0, 1, 0, 0x12, 0x01, 3, 0, 1, 0, 0, 0, 6, 0, 0, 0, 0, 0, 0,
            0,
        ])
        .unwrap();
    source.write_with_encoder(encoder).unwrap();
    let repaired = repair_image(&original).unwrap();
    let actual = image::load_from_memory(&repaired).unwrap();
    assert_eq!(actual.dimensions(), (3, 2));
    let mut expected = image::load_from_memory(&original).unwrap();
    expected.apply_orientation(image::metadata::Orientation::Rotate90);
    let mut quality_ninety = Vec::new();
    expected
        .write_with_encoder(image::codecs::jpeg::JpegEncoder::new_with_quality(
            &mut quality_ninety,
            90,
        ))
        .unwrap();
    assert_eq!(repaired, quality_ninety);
}
use image::GenericImageView;
