//! The two XML readers of `WeComCallbackChannel.ts:46-57`, kept on the same
//! regular-expression shape as TS (case-insensitive; CDATA first, then a
//! plain-text alternative).

use crate::metadata::js_trim;
use regex::Regex;

/// `xmlTag(xml, tag)`: `<tag><![CDATA[…]]></tag>` or `<tag>…</tag>`,
/// case-insensitive, with a JavaScript `trim()` on the captured value.
pub(crate) fn xml_tag(xml: &str, tag: &str) -> Option<String> {
    let tag = regex::escape(tag);
    let pattern = format!(r"(?is)<{tag}>(?:<!\[CDATA\[((?s:.*?))\]\]>|([^<]*))</{tag}>");
    let regex = Regex::new(&pattern).ok()?;
    let captures = regex.captures(xml)?;
    let value = captures
        .get(1)
        .or_else(|| captures.get(2))
        .map(|value| value.as_str())
        .unwrap_or("");
    Some(js_trim(value).to_owned())
}

/// `extractEncryptFromXml(xml)`: CDATA content when non-empty after `trim`,
/// otherwise the plain node (`<Encrypt>([^<]+)</Encrypt>`).
pub(crate) fn extract_encrypt_from_xml(xml: &str) -> Option<String> {
    let cdata = Regex::new(r"(?is)<Encrypt><!\[CDATA\[((?s:.*?))\]\]></Encrypt>").ok()?;
    if let Some(captures) = cdata.captures(xml) {
        let value = js_trim(captures.get(1).map_or("", |m| m.as_str()));
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    let plain = Regex::new(r"(?is)<Encrypt>([^<]+)</Encrypt>").ok()?;
    plain
        .captures(xml)
        .and_then(|captures| captures.get(1))
        .map(|value| js_trim(value.as_str()).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_cdata_and_plain_nodes_case_insensitively() {
        assert_eq!(
            xml_tag("<xml><MsgType><![CDATA[text]]></MsgType></xml>", "MsgType"),
            Some("text".into())
        );
        assert_eq!(
            xml_tag("<XML><msgtype> event </msgtype></XML>", "MsgType"),
            Some("event".into())
        );
        // `[^<]*` cannot step over a nested node to reach the closing tag,
        // so the whole pattern fails — Node-verified `undefined` for this input.
        assert_eq!(xml_tag("<MsgType>a<b>c</b></MsgType>", "MsgType"), None);
        assert_eq!(xml_tag("<xml/>", "MsgType"), None);
    }

    #[test]
    fn encrypt_prefers_non_empty_cdata_then_plain() {
        assert_eq!(
            extract_encrypt_from_xml("<xml><Encrypt><![CDATA[ abc ]]></Encrypt></xml>"),
            Some("abc".into())
        );
        assert_eq!(
            extract_encrypt_from_xml(
                "<xml><Encrypt><![CDATA[]]></Encrypt><Encrypt>plain</Encrypt></xml>"
            ),
            Some("plain".into())
        );
        assert_eq!(
            extract_encrypt_from_xml("<xml><Encrypt>plain</Encrypt></xml>"),
            Some("plain".into())
        );
        assert_eq!(extract_encrypt_from_xml("<xml></xml>"), None);
    }
}
