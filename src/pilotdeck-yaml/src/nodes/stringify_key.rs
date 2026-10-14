// Parsing dependency of dist/nodes/addPairToJSMap.js (yaml@2.9.0, ISC).
// Flow-key branch of dist/stringify/{stringify,stringifyCollection,stringifyPair,
// stringifyString}.js. This is not a public YAML stringify API.
use crate::{
    doc::Document,
    nodes::{NodeId, NodeKind, Pair, ScalarValue},
    string::{append, concat, json, slice},
    value::ConversionError,
};
use pilotdeck_protocol::JsString;
pub(super) fn stringify_key(
    doc: &Document,
    id: NodeId,
    anchors: &[NodeId],
) -> Result<JsString, ConversionError> {
    let mut ctx = KeyContext {
        doc,
        anchors: anchors.to_vec(),
    };
    match &doc.nodes[id].kind {
        NodeKind::Scalar(ScalarValue::Binary(data)) => {
            Ok(String::from_utf8_lossy(data).into_owned().into())
        }
        _ => ctx.node(id, 0, false, false, None),
    }
}
struct KeyContext<'a> {
    doc: &'a Document,
    anchors: Vec<NodeId>,
}
impl KeyContext<'_> {
    fn node(
        &mut self,
        id: NodeId,
        indent: usize,
        implicit: bool,
        props: bool,
        mut indent_at_start: Option<usize>,
    ) -> Result<JsString, ConversionError> {
        let n = &self.doc.nodes[id];
        if let NodeKind::Alias(source) = &n.kind {
            if !self
                .anchors
                .iter()
                .any(|id| self.doc.nodes[*id].anchor.as_ref() == Some(source))
            {
                return Err(ConversionError::new(
                    "Error",
                    crate::string::message(
                        "Unresolved alias (the anchor must be set before the alias): ",
                        source,
                    ),
                ));
            }
            return Ok(concat(&[
                &"*".into(),
                source,
                &if implicit { " " } else { "" }.into(),
            ]));
        }
        let mut prefix = JsString::default();
        if props {
            if let Some(anchor) = &n.anchor {
                crate::doc::anchors::anchor_is_valid(anchor)?;
                self.anchors.push(id);
                prefix = concat(&[&"&".into(), anchor]);
            }
            if let Some(tag) = &n.tag {
                if !prefix.is_empty() {
                    append(&mut prefix, &" ".into());
                }
                append(&mut prefix, &self.tag_string(tag));
            }
        }
        if !prefix.is_empty() {
            indent_at_start = Some(indent_at_start.unwrap_or(0) + prefix.len_utf16() + 1);
        }
        let body = match &n.kind {
            NodeKind::Scalar(v) => self.scalar(id, v, indent, implicit, indent_at_start),
            NodeKind::Seq(items) => self.flow_collection(
                items.iter().copied().map(FlowItem::Node).collect(),
                ("[", "]"),
                indent,
                indent + 4,
                false,
                indent_at_start,
            )?,
            NodeKind::Map(pairs) | NodeKind::Set(pairs) => {
                let all_null = pairs.iter().all(|p| {
                    p.value.is_none()
                        || (matches!(n.kind, NodeKind::Set(_))
                            && p.value.is_some_and(|id| {
                                matches!(
                                    self.doc.nodes[id].kind,
                                    NodeKind::Scalar(ScalarValue::Null)
                                )
                            }))
                });
                self.flow_collection(
                    pairs.iter().cloned().map(FlowItem::Pair).collect(),
                    ("{", "}"),
                    indent,
                    indent + 2,
                    all_null,
                    indent_at_start,
                )?
            }
            NodeKind::Pairs(pairs) | NodeKind::OMap(pairs) => self.flow_collection(
                pairs.iter().cloned().map(FlowItem::Pair).collect(),
                ("[", "]"),
                indent,
                indent + 4,
                false,
                indent_at_start,
            )?,
            NodeKind::Alias(_) => unreachable!(),
        };
        Ok(if prefix.is_empty() {
            body
        } else {
            concat(&[&prefix, &" ".into(), &body])
        })
    }
    fn tag_string(&self, tag: &JsString) -> JsString {
        for (handle, prefix) in &self.doc.directives.tags {
            if tag.units().starts_with(prefix.units()) {
                let suffix = slice(tag, prefix.len_utf16(), tag.len_utf16());
                let mut encoded = Vec::new();
                for c in suffix.units() {
                    let escape = match c {
                        33 => Some("%21"),
                        44 => Some("%2C"),
                        91 => Some("%5B"),
                        93 => Some("%5D"),
                        123 => Some("%7B"),
                        125 => Some("%7D"),
                        _ => None,
                    };
                    if let Some(s) = escape {
                        encoded.extend(s.encode_utf16());
                    } else {
                        encoded.push(*c);
                    }
                }
                return concat(&[handle, &JsString::from_utf16(encoded)]);
            }
        }
        if tag.units().first() == Some(&33) {
            tag.clone()
        } else {
            concat(&[&"!<".into(), tag, &">".into()])
        }
    }
    fn pair(
        &mut self,
        pair: &Pair,
        indent: usize,
        all_null: bool,
        indent_at_start: Option<usize>,
    ) -> Result<JsString, ConversionError> {
        let key = &self.doc.nodes[pair.key];
        let explicit = !key.is_scalar() && !matches!(key.kind, NodeKind::Alias(_))
            || matches!(key.style, "BLOCK_FOLDED" | "BLOCK_LITERAL");
        let k = self.node(
            pair.key,
            indent + 2,
            !explicit && !all_null,
            true,
            indent_at_start,
        )?;
        if all_null || pair.value.is_none() {
            return Ok(if k.is_empty() {
                "?".into()
            } else if explicit {
                concat(&[&"? ".into(), &k])
            } else {
                k
            });
        }
        let value = pair.value.unwrap();
        let mut out = if explicit {
            concat(&[
                &"? ".into(),
                &k,
                &format!("\n{}:", " ".repeat(indent)).into(),
            ])
        } else {
            concat(&[&k, &":".into()])
        };
        let at_start = if !explicit && key.comment.is_empty() && self.doc.nodes[value].is_scalar() {
            Some(out.len_utf16() + 1)
        } else {
            indent_at_start
        };
        let v = self.node(value, indent + 2, false, true, at_start)?;
        let ws = if !explicit && !self.doc.nodes[value].is_scalar() && v.units().contains(&10) {
            format!("\n{}", " ".repeat(indent + 2))
        } else if v.is_empty() || v.units().first() == Some(&10) {
            String::new()
        } else {
            " ".into()
        };
        append(&mut out, &ws.into());
        append(&mut out, &v);
        Ok(out)
    }
    fn flow_collection(
        &mut self,
        items: Vec<FlowItem>,
        flow_chars: (&str, &str),
        indent: usize,
        item_indent: usize,
        all_null: bool,
        indent_at_start: Option<usize>,
    ) -> Result<JsString, ConversionError> {
        let (start, end) = flow_chars;
        let mut req_newline = false;
        let mut lines_at_value = 0;
        let mut lines = Vec::new();
        for (i, item) in items.iter().enumerate() {
            let mut comment = JsString::default();
            let key = match item {
                FlowItem::Node(id) => Some(*id),
                FlowItem::Pair(p) => Some(p.key),
            };
            if let Some(id) = key {
                let node = &self.doc.nodes[id];
                if node.space_before {
                    lines.push(JsString::default());
                }
                if !node.comment_before.is_empty() {
                    let c = indent_comment(&stringify_comment(&node.comment_before), indent);
                    lines.push(trim_start(&c));
                }
                match item {
                    FlowItem::Node(_) => comment = node.comment.clone(),
                    FlowItem::Pair(p) => {
                        if !node.comment.is_empty() {
                            req_newline = true;
                        }
                        if let Some(v) = p.value {
                            let v = &self.doc.nodes[v];
                            comment = v.comment.clone();
                            if !v.comment_before.is_empty() {
                                req_newline = true;
                            }
                        } else {
                            comment = node.comment.clone();
                        }
                    }
                }
            }
            if !comment.is_empty() {
                req_newline = true;
            }
            let mut str = match item {
                FlowItem::Node(id) => self.node(*id, item_indent, false, true, indent_at_start)?,
                FlowItem::Pair(pair) => self.pair(pair, item_indent, all_null, indent_at_start)?,
            };
            req_newline = req_newline || lines.len() > lines_at_value || str.units().contains(&10);
            if i + 1 < items.len() {
                append(&mut str, &",".into());
            }
            if !comment.is_empty() {
                let suffix = line_comment(&str, item_indent, &stringify_comment(&comment));
                append(&mut str, &suffix);
            }
            lines.push(str);
            lines_at_value = lines.len();
        }
        if lines.is_empty() {
            return Ok(format!("{start}{end}").into());
        }
        req_newline =
            req_newline || lines.iter().map(|s| s.len_utf16() + 2).sum::<usize>() + 2 > 80;
        let mut out: JsString = start.into();
        if req_newline {
            for line in lines {
                append(&mut out, &"\n".into());
                if !line.is_empty() {
                    append(&mut out, &" ".repeat(indent + 2).into());
                    append(&mut out, &line);
                }
            }
            append(&mut out, &format!("\n{}{end}", " ".repeat(indent)).into());
        } else {
            append(&mut out, &" ".into());
            for (i, line) in lines.iter().enumerate() {
                if i > 0 {
                    append(&mut out, &" ".into());
                }
                append(&mut out, line);
            }
            append(&mut out, &format!(" {end}").into());
        }
        Ok(out)
    }
    fn scalar(
        &self,
        id: NodeId,
        v: &ScalarValue,
        indent: usize,
        implicit: bool,
        indent_at_start: Option<usize>,
    ) -> JsString {
        let n = &self.doc.nodes[id];
        if let ScalarValue::String(value) = v
            && let Some(tag) = &n.tag
        {
            if tag.equals_str("tag:yaml.org,2002:null") {
                return "null".into();
            }
            if tag.equals_str("tag:yaml.org,2002:bool") {
                return if value.is_empty() { "false" } else { "true" }.into();
            }
            if tag.equals_str("tag:yaml.org,2002:int") || tag.equals_str("tag:yaml.org,2002:float")
            {
                let num = value
                    .to_utf8()
                    .ok()
                    .and_then(|s| s.parse::<f64>().ok())
                    .unwrap_or(f64::NAN);
                return if num.is_nan() {
                    ".nan".into()
                } else {
                    json(value)
                };
            }
        }
        match v {
            ScalarValue::Null => {
                if n.source.is_empty()
                    || ["~", "null", "Null", "NULL"]
                        .iter()
                        .any(|s| n.source.equals_str(s))
                {
                    n.source.clone()
                } else {
                    "null".into()
                }
            }
            ScalarValue::Bool(b) => {
                let valid = if self.doc.schema.yaml11 {
                    [
                        "Y", "y", "Yes", "yes", "YES", "True", "true", "TRUE", "On", "on", "ON",
                        "N", "n", "No", "no", "NO", "False", "false", "FALSE", "Off", "off", "OFF",
                    ]
                    .iter()
                    .any(|s| n.source.equals_str(s))
                } else {
                    ["True", "true", "TRUE", "False", "false", "FALSE"]
                        .iter()
                        .any(|s| n.source.equals_str(s))
                };
                if valid {
                    n.source.clone()
                } else {
                    if *b { "true" } else { "false" }.into()
                }
            }
            ScalarValue::Number(v) => {
                if v.is_nan() {
                    return ".nan".into();
                }
                if v.is_infinite() {
                    return if v.is_sign_negative() {
                        "-.inf"
                    } else {
                        ".inf"
                    }
                    .into();
                }
                let s = n.source.to_utf8().unwrap_or_default();
                if n.format == Some("EXP") {
                    let s = format!("{:e}", v);
                    let (a, b) = s.split_once('e').unwrap();
                    let exp = b.parse::<i32>().unwrap();
                    return format!("{a}e{exp:+}").into();
                }
                if matches!(n.format, Some("OCT" | "HEX" | "BIN"))
                    && v.is_finite()
                    && v.fract() == 0.0
                {
                    let neg = *v < 0.0;
                    let number = v.abs();
                    let (prefix, digits) = match n.format {
                        Some("OCT") => (
                            if self.doc.schema.yaml11 { "0" } else { "0o" },
                            radix_number(number, 3),
                        ),
                        Some("HEX") => ("0x", radix_number(number, 4)),
                        _ => ("0b", radix_number(number, 1)),
                    };
                    return format!("{}{prefix}{digits}", if neg { "-" } else { "" }).into();
                }
                if n.format == Some("TIME") {
                    return crate::schema::yaml_1_1::timestamp::stringify_sexagesimal(*v);
                }
                let mut out = if *v == 0.0 && v.is_sign_negative() {
                    "-0".to_owned()
                } else {
                    super::to_js::number_string(*v).to_utf8().unwrap()
                };
                if let Some((_, frac)) = s
                    .split_once('.')
                    .filter(|_| !s.contains(['e', 'E']) && s.ends_with('0'))
                {
                    let digits = frac.chars().filter(|c| *c != '_').count();
                    let have = out.split_once('.').map_or(0, |(_, f)| f.len());
                    if have == 0 {
                        out.push('.');
                    }
                    if digits > have {
                        out.push_str(&"0".repeat(digits - have));
                    }
                }
                out.into()
            }
            ScalarValue::String(s) => self.string(s, n.style, indent, implicit, indent_at_start),
            ScalarValue::Merge => "<<".into(),
            ScalarValue::Timestamp(ms) => {
                let text = super::to_js::date_string(*ms).to_utf8().unwrap();
                text.strip_suffix("T00:00:00.000Z")
                    .or_else(|| text.strip_suffix(".000Z"))
                    .unwrap_or(&text)
                    .into()
            }
            ScalarValue::Binary(data) => {
                let text = crate::schema::yaml_1_1::binary::encode(data);
                let mut text = text;
                if n.style != "QUOTE_DOUBLE" {
                    let width = 80usize.saturating_sub(indent).max(20);
                    text = text
                        .as_bytes()
                        .chunks(width)
                        .map(|c| std::str::from_utf8(c).unwrap())
                        .collect::<Vec<_>>()
                        .join(if n.style == "BLOCK_LITERAL" {
                            "\n"
                        } else {
                            " "
                        });
                }
                self.string(&text.into(), n.style, indent, implicit, indent_at_start)
            }
        }
    }
    fn string(
        &self,
        value: &JsString,
        style: &str,
        indent: usize,
        implicit: bool,
        indent_at_start: Option<usize>,
    ) -> JsString {
        let s = value.units();
        let control = s.iter().any(|c| matches!(c,0..=8|11..=31|127..=159));
        let unpaired = char::decode_utf16(s.iter().copied()).any(|c| c.is_err());
        if style == "QUOTE_DOUBLE" || control || unpaired {
            return double_quote(value, indent, implicit, indent_at_start);
        }
        if style == "QUOTE_SINGLE"
            && !((implicit && s.contains(&10))
                || s.windows(2).any(|w| {
                    (matches!(w[0], 32 | 9) && w[1] == 10) || (w[0] == 10 && matches!(w[1], 32 | 9))
                }))
        {
            let mut out = vec![39];
            let mut i = 0;
            while i < s.len() {
                out.push(s[i]);
                if s[i] == 39 {
                    out.push(39);
                }
                if s[i] == 10 && (i + 1 == s.len() || s[i + 1] != 10) {
                    out.push(10);
                    out.extend(vec![32; indent]);
                }
                i += 1;
            }
            out.push(39);
            return if implicit {
                JsString::from_utf16(out)
            } else {
                super::fold_flow_lines::fold_flow_lines(
                    &JsString::from_utf16(out),
                    indent,
                    false,
                    indent_at_start,
                )
            };
        }
        let forbidden = s.first().is_some_and(|c| {
            matches!(
                c,
                10 | 9
                    | 32
                    | 44
                    | 91
                    | 93
                    | 123
                    | 125
                    | 35
                    | 38
                    | 42
                    | 33
                    | 124
                    | 62
                    | 39
                    | 34
                    | 37
                    | 64
                    | 96
            )
        }) || s.last().is_some_and(|c| matches!(c, 10 | 9 | 32 | 58))
            || s.iter().any(|c| matches!(c, 91 | 93 | 123 | 125 | 44))
            || s.windows(2).any(|w| {
                (matches!(w[0], 10 | 58) && matches!(w[1], 32 | 9))
                    || (matches!(w[0], 32 | 9) && w[1] == 10)
                    || (matches!(w[0], 10 | 9 | 32) && w[1] == 35)
            })
            || matches!(s, [45] | [63])
            || (s.len() > 1 && matches!(s[0], 45 | 63) && matches!(s[1], 32 | 9));
        let typed = self
            .doc
            .schema
            .tags
            .iter()
            .any(|t| t.default && t.name != "tag:yaml.org,2002:str" && t.test(value));
        if style == "QUOTE_SINGLE"
            || style.starts_with("BLOCK_")
            || forbidden
            || typed
            || (implicit && s.contains(&10))
        {
            return if s.contains(&34) && !s.contains(&39) {
                self.string(value, "QUOTE_SINGLE", indent, implicit, indent_at_start)
            } else {
                double_quote(value, indent, implicit, indent_at_start)
            };
        }
        let mut out = Vec::new();
        for (i, c) in s.iter().enumerate() {
            out.push(*c);
            if *c == 10 && s.get(i + 1) != Some(&10) {
                out.push(10);
                out.extend(vec![32; indent]);
            }
        }
        let out = JsString::from_utf16(out);
        if implicit {
            out
        } else {
            super::fold_flow_lines::fold_flow_lines(&out, indent, false, indent_at_start)
        }
    }
}
#[derive(Clone)]
enum FlowItem {
    Node(NodeId),
    Pair(Pair),
}
fn stringify_comment(s: &JsString) -> JsString {
    let mut out = Vec::new();
    for (i, line) in s.units().split(|c| *c == 10).enumerate() {
        if i > 0 {
            out.push(10);
        }
        if !line.is_empty() {
            out.push(35);
            if line != [32] {
                out.extend_from_slice(line);
            }
        }
    }
    JsString::from_utf16(out)
}
fn indent_comment(s: &JsString, indent: usize) -> JsString {
    if !s.is_empty() && s.units().iter().all(|c| *c == 10) {
        return slice(s, 1, s.len_utf16());
    }
    let mut out = Vec::new();
    for (i, line) in s.units().split(|c| *c == 10).enumerate() {
        if i > 0 {
            out.push(10);
        }
        if line.iter().any(|c| *c != 32) {
            out.extend(vec![32; indent]);
        }
        out.extend_from_slice(line);
    }
    JsString::from_utf16(out)
}
fn trim_start(s: &JsString) -> JsString {
    let i = s
        .units()
        .iter()
        .position(|c| !crate::string::is_ws(*c))
        .unwrap_or(s.len_utf16());
    slice(s, i, s.len_utf16())
}
fn line_comment(s: &JsString, indent: usize, comment: &JsString) -> JsString {
    if s.units().last() == Some(&10) {
        indent_comment(comment, indent)
    } else if comment.units().contains(&10) {
        concat(&[&"\n".into(), &indent_comment(comment, indent)])
    } else {
        concat(&[
            &if s.units().last() == Some(&32) {
                ""
            } else {
                " "
            }
            .into(),
            comment,
        ])
    }
}
fn double_quote(
    value: &JsString,
    indent: usize,
    implicit: bool,
    indent_at_start: Option<usize>,
) -> JsString {
    let json = json(value);
    let s = json.units();
    let mut out = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < s.len() {
        let mut ch = s[i];
        if ch == 32 && s.get(i + 1) == Some(&92) && s.get(i + 2) == Some(&110) {
            out.extend_from_slice(&s[start..i]);
            out.extend_from_slice(&[92, 32]);
            i += 1;
            start = i;
            ch = 92;
        }
        if ch == 92 {
            match s.get(i + 1) {
                Some(117) => {
                    out.extend_from_slice(&s[start..i]);
                    let code = String::from_utf16(&s[i + 2..i + 6]).unwrap();
                    let esc = match code.as_str() {
                        "0000" => Some("\\0"),
                        "0007" => Some("\\a"),
                        "000b" => Some("\\v"),
                        "001b" => Some("\\e"),
                        "0085" => Some("\\N"),
                        "00a0" => Some("\\_"),
                        "2028" => Some("\\L"),
                        "2029" => Some("\\P"),
                        _ => None,
                    };
                    if let Some(esc) = esc {
                        out.extend(esc.encode_utf16());
                    } else if let Some(code) = code.strip_prefix("00") {
                        out.extend(format!("\\x{code}").encode_utf16());
                    } else {
                        out.extend_from_slice(&s[i..i + 6]);
                    }
                    i += 5;
                    start = i + 1;
                }
                Some(110) => {
                    if implicit || s.get(i + 2) == Some(&34) || s.len() < 40 {
                        i += 1;
                    } else {
                        out.extend_from_slice(&s[start..i]);
                        out.extend_from_slice(&[10, 10]);
                        while s.get(i + 2) == Some(&92)
                            && s.get(i + 3) == Some(&110)
                            && s.get(i + 4) != Some(&34)
                        {
                            out.push(10);
                            i += 2;
                        }
                        out.extend(vec![32; indent]);
                        if s.get(i + 2) == Some(&32) {
                            out.push(92);
                        }
                        i += 1;
                        start = i + 1;
                    }
                }
                _ => i += 1,
            }
        }
        i += 1;
    }
    let str = if start > 0 {
        out.extend_from_slice(&s[start..]);
        JsString::from_utf16(out)
    } else {
        json
    };
    if implicit {
        str
    } else {
        super::fold_flow_lines::fold_flow_lines(&str, indent, true, indent_at_start)
    }
}

// Number#toString for the power-of-two integer radices used by YAML tags.
fn radix_number(value: f64, width: usize) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i32 - 1023;
    let mantissa = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let mut binary = format!("{mantissa:b}");
    if exponent >= 52 {
        binary.push_str(&"0".repeat((exponent - 52) as usize));
    } else {
        binary.truncate((exponent + 1) as usize);
    }
    let mut digits = Vec::new();
    for chunk in binary.as_bytes().rchunks(width) {
        let digit = chunk.iter().fold(0u8, |n, c| n * 2 + c - b'0');
        digits.push(b"0123456789abcdef"[digit as usize] as char);
    }
    digits.into_iter().rev().collect()
}
