//! #280: lexical masks for two captures that intentionally retain raw clock/PID
//! observations. The checked-in table is the complete allowlist, never inferred
//! from regenerated data. Everything outside those scalar spans stays byte exact.
use std::{collections::BTreeMap, ops::Range};

const TABLE: &str = include_str!("../fixtures/_shared/regeneration-masks.tsv");

pub(crate) fn rules(fixture: &str, file: &str) -> BTreeMap<&'static str, &'static str> {
    TABLE
        .lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| {
            let columns: Vec<_> = line.split('\t').collect();
            assert_eq!(columns.len(), 4, "#280 mask table has four columns");
            (columns[0] == fixture && columns[1] == file).then_some((columns[2], columns[3]))
        })
        .fold(BTreeMap::new(), |mut rules, (pointer, kind)| {
            // One kind per (fixture, file, pointer): a second row must not
            // silently relax an existing mask (GLM #280 r5 P1).
            let previous = rules.insert(pointer, kind);
            assert!(
                previous.is_none(),
                "#280 mask table repeats {fixture} {file} {pointer}"
            );
            rules
        })
}

pub(crate) fn pid(oracle: &[u8]) -> Result<u32, String> {
    let text = std::str::from_utf8(oracle)
        .map_err(|error| format!("#280 registry PID manifest UTF-8: {error}"))?;
    let json = pilotdeck_protocol::parse_json(text)
        .map_err(|error| format!("#280 registry PID manifest JSON: {error:?}"))?;
    json.get("generatorPid")
        .and_then(pilotdeck_protocol::JsValue::as_number)
        .filter(|pid| {
            pid.is_finite() && pid.fract() == 0.0 && *pid > 0.0 && *pid <= u32::MAX as f64
        })
        .map(|pid| pid as u32)
        .ok_or_else(|| "#280 registry generatorPid must be a positive u32".into())
}

// This scanner collects complete scalar spans, not a rewritten JSON tree. Some
// companions deliberately contain invalid UTF-8, lone surrogates or a cut-off
// final string. On those tails scanning stops; all remaining bytes stay intact.
// The completeness flag lets a truncated prefix report *where* it stopped
// instead of looking like a valid document with a missing masked field.
fn scan_all(bytes: &[u8]) -> (Vec<(String, Range<usize>)>, bool, usize) {
    let mut spans = Vec::new();
    let mut at = 0;
    let complete = scan(bytes, &mut at, "", &mut spans).is_some();
    (spans, complete, at)
}
fn scan(
    bytes: &[u8],
    at: &mut usize,
    path: &str,
    spans: &mut Vec<(String, Range<usize>)>,
) -> Option<()> {
    whitespace(bytes, at);
    let start = *at;
    match *bytes.get(*at)? {
        b'{' => {
            *at += 1;
            whitespace(bytes, at);
            if bytes.get(*at) == Some(&b'}') {
                *at += 1;
                return Some(());
            }
            loop {
                whitespace(bytes, at);
                let key_start = *at;
                string_end(bytes, at)?;
                // Only ASCII keys occur in the allowlist. Unrepresentable keys
                // cannot match it, but their original bytes are still compared.
                let key =
                    serde_json::from_slice::<String>(&bytes[key_start..*at]).unwrap_or_else(|_| {
                        String::from_utf8_lossy(&bytes[key_start..*at]).into_owned()
                    });
                whitespace(bytes, at);
                if bytes.get(*at) != Some(&b':') {
                    return None;
                }
                *at += 1;
                let key = key.replace('~', "~0").replace('/', "~1");
                scan(bytes, at, &format!("{path}/{key}"), spans)?;
                whitespace(bytes, at);
                match bytes.get(*at)? {
                    b'}' => {
                        *at += 1;
                        return Some(());
                    }
                    b',' => *at += 1,
                    _ => return None,
                }
            }
        }
        b'[' => {
            *at += 1;
            whitespace(bytes, at);
            if bytes.get(*at) == Some(&b']') {
                *at += 1;
                return Some(());
            }
            for index in 0usize.. {
                scan(bytes, at, &format!("{path}/{index}"), spans)?;
                whitespace(bytes, at);
                match bytes.get(*at)? {
                    b']' => {
                        *at += 1;
                        return Some(());
                    }
                    b',' => *at += 1,
                    _ => return None,
                }
            }
        }
        b'"' => string_end(bytes, at)?,
        _ => {
            while bytes
                .get(*at)
                .is_some_and(|byte| !byte.is_ascii_whitespace() && !b",]}:".contains(byte))
            {
                *at += 1;
            }
            if *at == start {
                return None;
            }
        }
    }
    spans.push((path.to_owned(), start..*at));
    Some(())
}

fn whitespace(bytes: &[u8], at: &mut usize) {
    while bytes.get(*at).is_some_and(u8::is_ascii_whitespace) {
        *at += 1;
    }
}

fn string_end(bytes: &[u8], at: &mut usize) -> Option<()> {
    if bytes.get(*at) != Some(&b'"') {
        return None;
    }
    *at += 1;
    loop {
        match bytes.get(*at)? {
            b'"' => {
                *at += 1;
                return Some(());
            }
            b'\\' => *at += 2,
            _ => *at += 1,
        }
    }
}

pub(crate) fn scalar_spans(bytes: &[u8]) -> Vec<(String, Range<usize>)> {
    scan_all(bytes).0
}

fn mask_paths(remaining: &BTreeMap<&'static str, &'static str>) -> String {
    let mut paths: Vec<String> = remaining
        .keys()
        .take(4)
        .map(|path| format!("{path:?}"))
        .collect();
    if remaining.len() > paths.len() {
        paths.push(format!("+{} more", remaining.len() - paths.len()));
    }
    paths.join(", ")
}

pub(crate) fn normalized(
    fixture: &str,
    file: &str,
    bytes: &[u8],
    generator_pid: Option<u32>,
) -> Result<Vec<u8>, String> {
    if !matches!(fixture, "provider-timing" | "run-registry") {
        return Ok(bytes.to_vec());
    }
    let mut remaining = rules(fixture, file);
    if remaining.is_empty() {
        return Ok(bytes.to_vec());
    }
    let generator_pid = if fixture == "run-registry" && file == "oracle.json" {
        Some(pid(bytes)?)
    } else {
        generator_pid
    };
    let mut normalized = Vec::new();
    let mut copied = 0;
    let mut seen = std::collections::BTreeSet::new();
    let (spans, complete, stopped) = scan_all(bytes);
    for (pointer, span) in spans {
        if seen.contains(&pointer) {
            return Err(format!("#280 duplicate masked path {file}{pointer}"));
        }
        let Some(kind) = remaining.remove(pointer.as_str()) else {
            continue;
        };
        seen.insert(pointer.clone());
        let token = &bytes[span.clone()];
        let valid = match kind {
            "timer" => {
                !token.is_empty()
                    && token.iter().all(u8::is_ascii_digit)
                    && (token.len() == 1 || token[0] != b'0')
                    && std::str::from_utf8(token)
                        .ok()
                        .and_then(|text| text.parse::<u64>().ok())
                        .is_some()
            }
            "pid" => generator_pid.is_some_and(|pid| token == pid.to_string().as_bytes()),
            "clock" => {
                token.len() == 26
                    && token.iter().enumerate().all(|(index, byte)| match index {
                        0 | 25 => *byte == b'"',
                        5 | 8 => *byte == b'-',
                        11 => *byte == b'T',
                        14 | 17 => *byte == b':',
                        20 => *byte == b'.',
                        24 => *byte == b'Z',
                        _ => byte.is_ascii_digit(),
                    })
            }
            _ => false,
        };
        if !valid {
            if kind == "pid" {
                let actual = String::from_utf8_lossy(token);
                let expected = generator_pid
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "unavailable".into());
                return Err(format!(
                    "#280 foreign owner PID {actual} at {file}{pointer}; expected this run's generator PID {expected}"
                ));
            }
            return Err(format!("#280 invalid {kind} value at {file}{pointer}"));
        }
        normalized.extend_from_slice(&bytes[copied..span.start]);
        normalized.extend_from_slice(b"0");
        copied = span.end;
    }
    if !remaining.is_empty() {
        if !complete {
            let position = if stopped >= bytes.len() {
                format!("input ends at byte {}", bytes.len())
            } else {
                format!("is malformed at byte {stopped} of {}", bytes.len())
            };
            return Err(format!(
                "#280 {file} {position} before masked path(s) [{}]",
                mask_paths(&remaining)
            ));
        }
        return Err(format!(
            "#280 missing masked paths in {file}: [{}]",
            mask_paths(&remaining)
        ));
    }
    normalized.extend_from_slice(&bytes[copied..]);
    Ok(normalized)
}

pub(crate) fn compare(
    fixture: &str,
    file: &str,
    actual: &[u8],
    expected: &[u8],
    pids: (Option<u32>, Option<u32>),
) -> Result<(), String> {
    if normalized(fixture, file, actual, pids.0)? != normalized(fixture, file, expected, pids.1)? {
        return Err(format!(
            "#280 {fixture}/{file} differs from committed bytes outside the exact masks"
        ));
    }
    // Absolute starts are volatile, but their deadline ordering defines the
    // timing input. A masked start must not silently change that input.
    if fixture == "provider-timing" && file == "oracle.json" {
        validate_provider_timing(actual)?;
        validate_provider_timing(expected)?;
    }
    Ok(())
}

pub(crate) fn validate_provider_timing(bytes: &[u8]) -> Result<(), String> {
    let error = |id: &str| format!("#280 provider-timing actual deadline order: {id}");
    let oracle: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| error("invalid JSON"))?;
    let rows = oracle["cases"]
        .as_array()
        .ok_or_else(|| error("missing cases"))?;
    for row in rows {
        let id = row["id"].as_str().ok_or_else(|| error("missing id"))?;
        let order = row["order"].as_str().ok_or_else(|| error(id))?;
        if order == "synchronous" || row["reachable"] == false {
            continue;
        }
        let timers = row["timers"].as_array().ok_or_else(|| error(id))?;
        let mut previous_start = 0;
        for timer in timers {
            let start = timer["start"].as_u64().ok_or_else(|| error(id))?;
            if start < previous_start {
                return Err(error(id));
            }
            previous_start = start;
        }
        let target = if row["event"] != "idle" {
            "completion"
        } else if row["phase"] == "response_text" && row["protocol"] != "google" {
            "body-timeout"
        } else {
            "watchdog"
        };
        let timer = |kind: &str| {
            let mut found = timers.iter().enumerate().filter(|(_, t)| t["kind"] == kind);
            let (index, value) = found.next().ok_or_else(|| error(id))?;
            if found.next().is_some() {
                return Err(error(id));
            }
            let start = value["start"].as_u64().ok_or_else(|| error(id))?;
            let delay = value["delay"].as_u64().ok_or_else(|| error(id))?;
            Ok((index, start.checked_add(delay).ok_or_else(|| error(id))?))
        };
        let (parent_index, parent_deadline) = timer("parent")?;
        let (target_index, target_deadline) = timer(target)?;
        let ordered = match order {
            "continuation-first" => parent_deadline > target_deadline,
            "parent-first" => parent_deadline < target_deadline,
            "tie-parent-first" | "tie-parent-last" => parent_deadline == target_deadline,
            _ => false,
        };
        if !ordered || (parent_index < target_index) != (order != "tie-parent-last") {
            return Err(error(id));
        }
    }
    Ok(())
}
