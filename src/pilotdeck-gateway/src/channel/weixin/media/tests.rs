//! #1823 weixin media: independent AES vectors, the Node decoder semantics the
//! port reproduces by hand, and a replay of the `weixin-channel` oracle's
//! media cases.
//!
//! The replay is the parity check: `generate.mts` runs the baseline
//! `WeixinChannel.ts` helpers and the real `ImAttachmentStore.saveFromUrl`
//! against a scripted loopback CDN and records their exact output, so every
//! difference between the TS baseline and this port shows up as a failing
//! cell rather than as a changed expectation.
//!
//! `download_replays_the_oracle_cases` binds an **ephemeral** loopback port
//! (`127.0.0.1:0`) and substitutes it for the fixture's `<port>` mask, so the
//! fixture holds no host-global port number and cannot collide with another
//! test host's fixed segment.
use super::*;
use pilotdeck_protocol::{JsValue, parse_json};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

const ORACLE: &str = include_str!("../../../../tests/fixtures/weixin-channel/oracle.json");

fn oracle() -> Value {
    crate::test_baseline::oracle(ORACLE);
    serde_json::from_str(ORACLE).expect("#1823 weixin media oracle JSON")
}

fn media_cases<'a>(oracle: &'a Value, kind: &str) -> Vec<&'a Value> {
    oracle["cases"]
        .as_array()
        .expect("#1823 weixin oracle cases")
        .iter()
        .filter(|case| case["kind"] == kind)
        .collect()
}

fn from_hex(text: &str) -> Vec<u8> {
    crate::negative_assert!(
        text.len().is_multiple_of(2),
        "#1823 fixture hex must have whole bytes"
    );
    text.as_bytes()
        .chunks(2)
        .map(|pair| {
            let digit = |byte: u8| {
                (byte as char)
                    .to_digit(16)
                    .expect("#1823 fixture hex digit") as u8
            };
            digit(pair[0]) << 4 | digit(pair[1])
        })
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The fixture records `mediaOwner` as JSON; the port takes a `JsValue`.
fn js(value: &Value) -> JsValue {
    parse_json(&value.to_string()).expect("#1823 media owner JSON")
}

// ── declarations ────────────────────────────────────────────────────────────

#[test]
fn oracle_declares_the_media_cases() {
    let oracle = oracle();
    let decrypt = media_cases(&oracle, "decrypt");
    let download = media_cases(&oracle, "download");
    let all = oracle["cases"].as_array().expect("#1823 oracle cases");
    crate::negative_assert_eq!(
        all.len() - decrypt.len() - download.len(),
        15,
        "#1823 the channel cases are untouched"
    );
    crate::negative_assert_eq!(
        oracle["expectedCaseCount"],
        json!(15),
        "#1823 generator channel declaration"
    );
    crate::negative_assert_eq!(
        oracle["expectedMediaCaseCount"],
        json!(decrypt.len() + download.len()),
        "#1823 generator media declaration"
    );
    crate::negative_assert_eq!(decrypt.len(), 24, "#1823 decrypt case count");
    crate::negative_assert_eq!(download.len(), 5, "#1823 download case count");
    crate::negative_assert_eq!(
        oracle["cdnBaseUrl"],
        json!(WEIXIN_CDN_BASE_URL),
        "#1823 CDN base URL matches the baseline literal"
    );
    crate::negative_assert_eq!(
        WEIXIN_ATTACHMENT_MAX_BYTES,
        50 * 1024 * 1024,
        "#1823 attachment byte limit"
    );
    let slice = oracle["mediaSliceSha256"]
        .as_str()
        .expect("#1823 media slice hash");
    crate::negative_assert!(
        slice.len() == 64 && slice.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "#1823 media slice hash is a sha256"
    );
}

// ── AES ─────────────────────────────────────────────────────────────────────

/// FIPS-197 / NIST SP 800-38A single-block ECB vectors, checked against Node's
/// `aes-*-ecb` before being written down here.
#[test]
fn aes_ecb_decrypts_the_fips_197_vectors() {
    let plaintext = from_hex("00112233445566778899aabbccddeeff");
    for (key, ciphertext) in [
        (
            "000102030405060708090a0b0c0d0e0f",
            "69c4e0d86a7b0430d8cdb78070b4c55a",
        ),
        (
            "000102030405060708090a0b0c0d0e0f1011121314151617",
            "dda97ca4864cdfe06eaf70a0ec0d7191",
        ),
        (
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            "8ea2b7ca516745bfeafc49904b496089",
        ),
    ] {
        let candidates = try_decrypt_weixin_media(&from_hex(ciphertext), &from_hex(key));
        crate::negative_assert_eq!(
            candidates,
            vec![plaintext.clone()],
            "#1823 FIPS-197 ECB vector"
        );
    }
}

#[test]
fn decryption_candidates_follow_the_node_cipher_contract() {
    let key = from_hex("000102030405060708090a0b0c0d0e0f");
    crate::negative_assert!(
        try_decrypt_weixin_media(&[0u8; 37], &key).is_empty(),
        "#1823 a non-block-multiple input yields no candidate"
    );
    crate::negative_assert!(
        try_decrypt_weixin_media(&[0u8; 16], &[0u8; 20]).is_empty(),
        "#1823 a key outside 16/24/32 bytes yields no candidate"
    );
    crate::negative_assert_eq!(
        try_decrypt_weixin_media(&[], &key),
        vec![Vec::<u8>::new()],
        "#1823 an empty input decrypts to an empty candidate"
    );
}

#[test]
fn strip_pkcs7_padding_matches_the_baseline() {
    crate::negative_assert_eq!(
        strip_pkcs7_padding(&[1, 2, 3, 1]),
        Some(&[1u8, 2, 3][..]),
        "#1823 single pad byte"
    );
    crate::negative_assert_eq!(
        strip_pkcs7_padding(&[1, 2, 2, 2]),
        Some(&[1u8, 2][..]),
        "#1823 two pad bytes"
    );
    crate::negative_assert_eq!(
        strip_pkcs7_padding(&[2, 2]),
        Some(&[][..]),
        "#1823 a whole buffer of padding strips to empty"
    );
    crate::negative_assert_eq!(
        strip_pkcs7_padding(&[1, 2, 3, 0]),
        None,
        "#1823 a zero pad byte is rejected"
    );
    crate::negative_assert_eq!(
        strip_pkcs7_padding(&[1, 2, 3, 4]),
        None,
        "#1823 inconsistent padding is rejected"
    );
    crate::negative_assert_eq!(strip_pkcs7_padding(&[]), None, "#1823 empty buffer");
    let mut block = [9u8; 16];
    block[15] = 17;
    crate::negative_assert_eq!(
        strip_pkcs7_padding(&block),
        None,
        "#1823 a pad byte above the block size is rejected"
    );
}

// ── Node decoders ───────────────────────────────────────────────────────────

/// `Buffer.from(value, "base64")` on Node v22.22.0: skips characters outside
/// the alphabet, accepts `-`/`_`, and stops at the first `=`.
#[test]
fn node_base64_semantics() {
    for (input, expected) in [
        (
            "AAECAwQFBgcICQoLDA0ODw==",
            "000102030405060708090a0b0c0d0e0f",
        ),
        ("AAECAwQFBgcICQoLDA0ODw", "000102030405060708090a0b0c0d0e0f"),
        ("AA==AAEC", "00"),
        ("AA EC", "000102"),
        ("AAEC!AwQF", "000102030405"),
        ("-_", "fb"),
        ("AB", "00"),
        ("A", ""),
        ("!", ""),
        ("", ""),
    ] {
        crate::negative_assert_eq!(
            to_hex(&node_base64_decode(input)),
            expected,
            "#1823 base64 decode"
        );
    }
}

/// `Buffer.from(value, "hex")` stops at the first non-hex pair and ignores a
/// trailing odd character.
#[test]
fn node_hex_semantics() {
    for (input, expected) in [
        ("00ff10", "00ff10"),
        ("AbCd", "abcd"),
        ("abc", "ab"),
        ("00ff1", "00ff"),
        ("0g", ""),
        ("", ""),
    ] {
        crate::negative_assert_eq!(
            to_hex(&node_hex_decode(input)),
            expected,
            "#1823 hex decode"
        );
    }
}

#[test]
fn read_string_and_length_follow_js() {
    let blank = JsValue::from("   ");
    crate::negative_assert!(read_string(Some(&blank)).is_none(), "#1823 blank is absent");
    let padded = JsValue::from("  key  ");
    crate::negative_assert_eq!(
        read_string(Some(&padded)).as_deref(),
        Some("  key  "),
        "#1823 readString returns the string untrimmed"
    );
    crate::negative_assert!(read_string(None).is_none(), "#1823 missing is absent");
    let number = JsValue::Number(7.0);
    crate::negative_assert!(
        read_string(Some(&number)).is_none(),
        "#1823 non-string is absent"
    );
    crate::negative_assert_eq!(js_len(Some("abc")), 3, "#1823 ascii length");
    crate::negative_assert_eq!(js_len(Some("中")), 1, "#1823 BMP length");
    crate::negative_assert_eq!(js_len(Some("😀")), 2, "#1823 astral length is two units");
    crate::negative_assert_eq!(js_len(None), 0, "#1823 undefined length");
}

#[test]
fn encode_uri_component_matches_js() {
    crate::negative_assert_eq!(
        encode_uri_component("a b/c?d=中&x=1-_.!~*'()"),
        "a%20b%2Fc%3Fd%3D%E4%B8%AD%26x%3D1-_.!~*'()",
        "#1823 percent-encoding set"
    );
}

// ── plaintext detection ─────────────────────────────────────────────────────

#[test]
fn known_buffer_predicates_match_the_baseline() {
    let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0];
    jpeg.extend_from_slice(&[0u8; 8]);
    crate::negative_assert!(is_known_image_buffer(&jpeg), "#1823 jpeg magic");
    crate::negative_assert!(
        !is_known_image_buffer(&jpeg[..11]),
        "#1823 images shorter than 12 bytes are rejected"
    );
    let png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0];
    crate::negative_assert!(is_known_image_buffer(&png), "#1823 png magic");
    let mut gif = b"GIF89a".to_vec();
    gif.extend_from_slice(&[0u8; 6]);
    crate::negative_assert!(is_known_image_buffer(&gif), "#1823 gif magic");
    let mut webp = b"RIFF\0\0\0\0WEBP".to_vec();
    webp.extend_from_slice(&[0u8; 2]);
    crate::negative_assert!(is_known_image_buffer(&webp), "#1823 webp magic");

    crate::negative_assert!(
        is_likely_known_file_buffer(b"%PDF-1.7\n"),
        "#1823 pdf is a known file"
    );
    crate::negative_assert!(
        is_likely_known_file_buffer(b"PK\x03\x04rest"),
        "#1823 zip is a known file"
    );
    crate::negative_assert!(
        is_likely_known_file_buffer(br"{\rtf1"),
        "#1823 rtf is a known file"
    );
    crate::negative_assert!(
        is_likely_known_file_buffer(b"plain ascii text\n"),
        "#1823 mostly-text falls through to a known file"
    );
    crate::negative_assert!(
        !is_likely_known_file_buffer(&[0x00, 0x01, 0x02, 0x03]),
        "#1823 a NUL byte rejects the mostly-text heuristic"
    );
    crate::negative_assert!(
        !is_likely_known_file_buffer(&[0x01, 0x02]),
        "#1823 buffers shorter than four bytes are rejected"
    );
}

#[test]
fn image_decrypt_failure_carries_the_baseline_message() {
    let owner = js(&json!({"aeskey": "zz"}));
    let error = decrypt_weixin_image_or_throw(&[0u8; 16], &owner)
        .expect_err("#1823 undecryptable image must not pass");
    crate::negative_assert_eq!(error.name, "Error", "#1823 error name");
    crate::negative_assert_eq!(
        error.message,
        "微信图片解密后仍不是可识别图片（keyShape=ownerAesKeyLen=2,ownerAesKeyHex=false,\
         mediaAesKeyLen=0,mediaAesKeyDecodedLen=0 candidates=[]）",
        "#1823 error message"
    );
    let mut jpeg = vec![0xff, 0xd8, 0xff, 0xe0];
    jpeg.extend_from_slice(&[0u8; 8]);
    crate::negative_assert_eq!(
        decrypt_weixin_image_or_throw(&jpeg, &owner).expect("#1823 plaintext image passes"),
        jpeg,
        "#1823 plaintext image is returned unchanged"
    );
}

// ── oracle replay: decryption ───────────────────────────────────────────────

#[test]
fn decrypt_replays_the_oracle_media_cases() {
    let oracle = oracle();
    let cases = media_cases(&oracle, "decrypt");
    crate::negative_assert!(!cases.is_empty(), "#1823 decrypt cases present");
    for case in cases {
        let name = case["name"].as_str().expect("#1823 case name");
        let owner = js(&case["input"]["mediaOwner"]);
        let buffer = from_hex(
            case["input"]["bufferHex"]
                .as_str()
                .expect("#1823 input buffer"),
        );
        let expected = &case["expected"];

        let keys: Vec<String> = extract_weixin_media_keys(&owner)
            .iter()
            .map(|key| to_hex(key))
            .collect();
        crate::negative_assert_eq!(json!(keys), expected["keys"], "#1823 {name} keys");
        crate::negative_assert_eq!(
            summarize_weixin_media_key_shape(&owner),
            expected["keyShape"].as_str().expect("#1823 keyShape"),
            "#1823 {name} keyShape"
        );
        crate::negative_assert_eq!(
            summarize_weixin_decrypt_candidates(&buffer, &owner),
            expected["candidates"].as_str().expect("#1823 candidates"),
            "#1823 {name} candidates"
        );
        crate::negative_assert_eq!(
            summarize_weixin_image_decrypt(&buffer, &owner),
            expected["imageLog"].as_str().expect("#1823 image log"),
            "#1823 {name} image log"
        );

        let decrypted = try_decrypt_weixin_media_if_needed(&buffer, &owner, is_known_image_buffer);
        let result = decrypted.clone().unwrap_or_else(|| buffer.clone());
        crate::negative_assert_eq!(
            decrypted.is_some(),
            expected["changed"].as_bool().expect("#1823 changed"),
            "#1823 {name} changed"
        );
        crate::negative_assert_eq!(
            to_hex(&result),
            expected["resultHex"].as_str().expect("#1823 result"),
            "#1823 {name} result"
        );
        crate::negative_assert_eq!(
            is_known_image_buffer(&result),
            expected["image"].as_bool().expect("#1823 image"),
            "#1823 {name} image"
        );
        crate::negative_assert_eq!(
            to_hex(&decrypt_weixin_media_if_needed(&buffer, &owner)),
            to_hex(&result),
            "#1823 {name} default-predicate entry point"
        );

        let file = try_decrypt_weixin_media_if_needed(&buffer, &owner, is_likely_known_file_buffer);
        let file_result = file.clone().unwrap_or_else(|| buffer.clone());
        crate::negative_assert_eq!(
            file.is_some(),
            expected["fileChanged"]
                .as_bool()
                .expect("#1823 file changed"),
            "#1823 {name} file changed"
        );
        crate::negative_assert_eq!(
            to_hex(&file_result),
            expected["fileResultHex"]
                .as_str()
                .expect("#1823 file result"),
            "#1823 {name} file result"
        );
        crate::negative_assert_eq!(
            is_likely_known_file_buffer(&file_result),
            expected["fileKnown"].as_bool().expect("#1823 file known"),
            "#1823 {name} file known"
        );
        crate::negative_assert_eq!(
            to_hex(&decrypt_weixin_file(&buffer, &owner)),
            to_hex(&file_result),
            "#1823 {name} file entry point"
        );

        let url = extract_weixin_media_url(&owner);
        match expected["downloadUrl"].as_str() {
            Some(text) => {
                crate::negative_assert_eq!(url.as_deref(), Some(text), "#1823 {name} media url")
            }
            None => crate::negative_assert!(url.is_none(), "#1823 {name} has no media url"),
        }
    }
}

// ── oracle replay: CDN download ─────────────────────────────────────────────

/// A scripted loopback CDN: one scripted response per request, in order, with
/// the request line recorded *before* the response is written (so a caller
/// that has its bytes sees its own request already recorded).
struct ScriptedCdn {
    port: u16,
    requests: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
}

impl Drop for ScriptedCdn {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl ScriptedCdn {
    async fn start(responses: &[Value]) -> Self {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("#1823 cdn bind");
        let port = listener.local_addr().expect("#1823 cdn local addr").port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let queue = responses.to_vec();
        let task = tokio::spawn(async move {
            let mut queue = queue.into_iter();
            while let Ok((mut stream, _)) = listener.accept().await {
                let head = read_head(&mut stream).await;
                let mut parts = head.split(' ');
                let method = parts.next().unwrap_or_default().to_owned();
                let path = parts.next().unwrap_or_default().to_owned();
                seen.lock()
                    .expect("#1823 cdn requests")
                    .push(json!({"method": method, "path": path}));
                let response = queue.next().unwrap_or_else(|| json!({"status": 500}));
                write_response(&mut stream, &response).await;
            }
        });
        Self {
            port,
            requests,
            task,
        }
    }
}

async fn read_head(stream: &mut TcpStream) -> String {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 512];
    while !buffer.windows(4).any(|window| window == b"\r\n\r\n") {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
        }
    }
    String::from_utf8_lossy(&buffer).into_owned()
}

async fn write_response(stream: &mut TcpStream, response: &Value) {
    let status = response["status"].as_u64().unwrap_or(500);
    let reason = match status {
        200 => "OK",
        302 => "Found",
        404 => "Not Found",
        _ => "Error",
    };
    let headers = response["headers"].as_object();
    let mut head = format!("HTTP/1.1 {status} {reason}\r\n");
    if let Some(headers) = headers {
        for (name, value) in headers {
            head.push_str(&format!(
                "{name}: {}\r\n",
                value.as_str().unwrap_or_default()
            ));
        }
    }
    let body = response["bodyHex"]
        .as_str()
        .map(from_hex)
        .unwrap_or_default();
    let declared_only = response["declaredOnly"].as_bool().unwrap_or(false);
    if !declared_only && !headers.is_some_and(|headers| headers.contains_key("content-length")) {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    let _ = stream.write_all(head.as_bytes()).await;
    let _ = stream.write_all(&body).await;
    if declared_only {
        // Fewer bytes than the declared content-length, then close: the client
        // must reject on the declared size alone, before reading the body.
        let _ = stream.write_all(b"x").await;
    }
    let _ = stream.shutdown().await;
}

#[tokio::test]
async fn download_replays_the_oracle_cases() {
    let oracle = oracle();
    let cases = media_cases(&oracle, "download");
    crate::negative_assert!(!cases.is_empty(), "#1823 download cases present");
    let client = WeixinCdnClient::new();
    for case in cases {
        let name = case["name"].as_str().expect("#1823 case name");
        let expected = &case["expected"];
        let recorded = case["input"]["url"].as_str().expect("#1823 download url");
        if case["input"]["server"] == "closed" {
            // Bind and drop: the port is then refused, like a dead CDN host.
            let listener = TcpListener::bind(("127.0.0.1", 0))
                .await
                .expect("#1823 probe bind");
            let port = listener.local_addr().expect("#1823 probe addr").port();
            drop(listener);
            let url = recorded.replace("<port>", &port.to_string());
            let error = client
                .download(&url)
                .await
                .expect_err("#1823 a closed port must fail");
            crate::negative_assert_eq!(
                error.formatted(),
                expected["error"].as_str().expect("#1823 download error"),
                "#1823 {name} connection failure"
            );
            continue;
        }
        let responses = case["input"]["responses"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let cdn = ScriptedCdn::start(&responses).await;
        let url = recorded.replace("<port>", &cdn.port.to_string());
        let outcome = match client.download(&url).await {
            Ok(bytes) => json!({"ok": true, "bytesHex": to_hex(&bytes)}),
            Err(error) => json!({"ok": false, "error": error.formatted()}),
        };
        crate::negative_assert_eq!(outcome["ok"], expected["ok"], "#1823 {name} download ok");
        if expected["ok"] == json!(true) {
            crate::negative_assert_eq!(
                outcome["bytesHex"],
                expected["bytesHex"],
                "#1823 {name} download bytes"
            );
        } else {
            crate::negative_assert_eq!(
                outcome["error"],
                expected["error"],
                "#1823 {name} download error"
            );
        }
        let requests = cdn.requests.lock().expect("#1823 cdn requests").clone();
        crate::negative_assert_eq!(
            Value::Array(requests),
            expected["requests"],
            "#1823 {name} download requests"
        );
    }
}
