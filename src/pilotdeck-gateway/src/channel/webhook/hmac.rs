//! The HMAC check of `WebhookChannel.verifyHmac` (baseline cc7c4e6): Node's
//! `createHmac("sha256", secret).update(body).digest("hex")` against
//! `Buffer.from(signature, "hex")` compared with `timingSafeEqual`, where any
//! throw — including a length mismatch — is `false`.
use sha2::{Digest, Sha256};

const BLOCK: usize = 64; // SHA-256 block size, the HMAC key limit before hashing

/// HMAC-SHA256 over the message with the UTF-8 bytes of the secret, exactly
/// what Node's `createHmac("sha256", secretString)` computes.
pub(super) fn hmac_sha256_hex(secret: &str, body: &str) -> [u8; 32] {
    let key = secret.as_bytes();
    let mut block_key = [0u8; BLOCK];
    if key.len() > BLOCK {
        block_key[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block_key[..key.len()].copy_from_slice(key);
    }
    let mut inner_key = [0x36u8; BLOCK];
    let mut outer_key = [0x5cu8; BLOCK];
    for index in 0..BLOCK {
        inner_key[index] ^= block_key[index];
        outer_key[index] ^= block_key[index];
    }
    let mut inner = Sha256::new();
    inner.update(inner_key);
    inner.update(body.as_bytes());
    let mut outer = Sha256::new();
    outer.update(outer_key);
    outer.update(inner.finalize());
    outer.finalize().into()
}

/// `Buffer.from(value, "hex")` (Node v22.22.0 probed): byte pairs decoded
/// case-insensitively until the first non-hex pair, an odd trailing nibble
/// ignored, so `"abc"` is `0xab` and `"1g"` is empty.
pub(super) fn node_hex_decode(value: &str) -> Vec<u8> {
    let mut decoded = Vec::new();
    for pair in value.as_bytes().chunks(2) {
        let Some(high) = hex_digit(pair[0]) else {
            break;
        };
        let Some(low) = pair.get(1).copied().and_then(hex_digit) else {
            break;
        };
        decoded.push(high << 4 | low);
    }
    decoded
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// The whole `verifyHmac`: an empty signature is false; the `sha256=` prefix
/// is optional; hex decoding or a length mismatch — the `timingSafeEqual`
/// throw — is false, never a diagnostic.
pub(super) fn verify_hmac(body: &str, secret: &str, signature: &str) -> bool {
    if signature.is_empty() {
        return false;
    }
    let digest = signature.strip_prefix("sha256=").unwrap_or(signature);
    let expected = hmac_sha256_hex(secret, body);
    let provided = node_hex_decode(digest);
    provided.len() == expected.len() && constant_time_equal(&provided, &expected)
}

/// The comparison limb of `crypto.timingSafeEqual`: no early exit on the
/// first differing byte.
fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let mut difference = 0u8;
    for index in 0..left.len() {
        difference |= left[index] ^ right[index];
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret() -> &'static str {
        "topsecret"
    }

    fn sign(body: &str) -> String {
        hex(&hmac_sha256_hex(secret(), body))
    }

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn matches_node_create_hmac_vectors() {
        // Node v22.22.0: createHmac("sha256","topsecret").update(body).digest("hex")
        assert_eq!(
            hex(&hmac_sha256_hex("topsecret", "hello")),
            "ed76fd36523b8becda5a3b36d0e3737e8ae5111f55e26c7c3a455a3ce29636d2"
        );
        assert_eq!(
            hex(&hmac_sha256_hex("topsecret", "")),
            "818f9cb88315ac08b5ef83f96650ca6f4e3dddcb4548e4879b746f56b57fa2b0"
        );
    }

    #[test]
    fn prefix_stripping_and_case_insensitive_hex() {
        let signature = sign("hello");
        assert!(verify_hmac("hello", secret(), &signature), "#1334 bare hex");
        assert!(
            verify_hmac("hello", secret(), &format!("sha256={signature}")),
            "#1334 prefixed hex"
        );
        assert!(
            verify_hmac(
                "hello",
                secret(),
                &format!("sha256={}", signature.to_uppercase())
            ),
            "#1334 uppercase hex"
        );
        assert!(
            verify_hmac(
                "hello",
                secret(),
                &format!("sha256={}", signature.to_uppercase())
            ),
            "#1334 uppercase digest"
        );
    }

    #[test]
    fn wrong_body_secret_and_garbage_signatures_are_false() {
        let signature = sign("hello");
        assert!(
            !verify_hmac("hellp", secret(), &signature),
            "#1334 wrong body"
        );
        assert!(
            !verify_hmac("hello", "other", &signature),
            "#1334 wrong secret"
        );
        assert!(!verify_hmac("hello", secret(), ""), "#1334 empty signature");
        assert!(!verify_hmac("hello", secret(), "zz"), "#1334 invalid hex");
        // Truncated digest decodes shorter, so timingSafeEqual throws: false.
        assert!(
            !verify_hmac("hello", secret(), &signature[..62]),
            "#1334 odd trailing nibble is ignored, length mismatches"
        );
        assert!(
            !verify_hmac("hello", secret(), &format!("{signature}00")),
            "#1334 longer digest mismatches"
        );
    }

    #[test]
    fn node_hex_decode_matches_the_probed_table() {
        assert_eq!(node_hex_decode("ab"), vec![0xab]);
        assert_eq!(node_hex_decode("abc"), vec![0xab]);
        assert!(node_hex_decode("1g").is_empty());
        assert!(node_hex_decode("x1").is_empty());
        assert_eq!(node_hex_decode("abcdZZef"), vec![0xab, 0xcd]);
        assert_eq!(node_hex_decode("AB"), vec![0xab]);
        assert_eq!(
            node_hex_decode(&"0".repeat(65)),
            vec![0u8; 32],
            "#1334 the odd trailing nibble is dropped"
        );
    }
}
