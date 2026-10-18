//! Node 22 fs.validatePath -> ERR_INVALID_ARG_VALUE, including util.inspect's
//! primitive string quoting and the error constructor's 128 UTF-16 unit limit.
use pilotdeck_protocol::JsString;

fn quoted(units: &[u16]) -> Vec<u16> {
    let quote = if units.contains(&39) && !units.contains(&34) {
        34
    } else if units.contains(&39)
        && units.contains(&34)
        && !units.contains(&96)
        && !units.windows(2).any(|w| w == [36, 123])
    {
        96
    } else {
        39
    };
    let mut out = vec![quote];
    let mut i = 0;
    while i < units.len() {
        let u = units[i];
        let escape = match u {
            8 => Some("\\b".into()),
            9 => Some("\\t".into()),
            10 => Some("\\n".into()),
            12 => Some("\\f".into()),
            13 => Some("\\r".into()),
            92 => Some("\\\\".into()),
            39 if quote == 39 => Some("\\'".into()),
            0..=31 | 127..=159 => Some(format!("\\x{u:02X}")),
            0xd800..=0xdbff
                if units
                    .get(i + 1)
                    .is_some_and(|v| (0xdc00..=0xdfff).contains(v)) =>
            {
                out.extend_from_slice(&units[i..i + 2]);
                i += 2;
                continue;
            }
            0xd800..=0xdfff => Some(format!("\\u{u:04x}")),
            _ => None,
        };
        if let Some(s) = escape {
            out.extend(s.encode_utf16());
        } else {
            out.push(u);
        }
        i += 1;
    }
    out.push(quote);
    out
}
pub(crate) fn nul_message(path: &JsString) -> JsString {
    nul_message_for(path, "path")
}

/// Same `ERR_INVALID_ARG_VALUE` text with the syscall's own argument name
/// (`path` for mkdir/readFile/writeFile, `target` for symlink's first
/// parameter, `oldPath` for readlink).
pub(crate) fn nul_message_for(path: &JsString, argument: &str) -> JsString {
    let units = path.units();
    let displayed = &units[..units.len().min(10_000)];
    let mut inspected = vec![];
    if displayed.len() > 76 {
        for (index, line) in displayed.split_inclusive(|u| *u == 10).enumerate() {
            if index > 0 {
                inspected.extend(" +\n  ".encode_utf16());
            }
            inspected.extend(quoted(line));
        }
    } else {
        inspected = quoted(displayed);
    }
    if units.len() > displayed.len() {
        let n = units.len() - displayed.len();
        inspected.extend(
            format!("... {n} more character{}", if n > 1 { "s" } else { "" }).encode_utf16(),
        );
    }
    if inspected.len() > 128 {
        inspected.truncate(128);
        inspected.extend("...".encode_utf16());
    }
    let mut message: Vec<_> = format!(
        "The argument '{argument}' must be a string, Uint8Array, or URL without null bytes. Received "
    )
    .encode_utf16()
    .collect();
    message.extend(inspected);
    JsString::from_utf16(message)
}
