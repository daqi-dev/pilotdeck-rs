//! Shared verdicts for every candidate, independent of case/mutation labels.
use super::*;
type Audit = Result<Value, Value>;
fn rejected(code: &str, facts: Value) -> Value {
    json!({"code":code,"facts":facts})
}
fn matching<'a>(events: &'a [Value], name: &str, token: &Value) -> Vec<(usize, &'a Value)> {
    events
        .iter()
        .enumerate()
        .filter(|(_, e)| e["event"] == name && e["token"] == *token)
        .collect()
}

pub(super) fn audit_ready_chain(
    rust: &[Value],
    host: &[Value],
    receipts: &[Value],
    records: &[Value],
    transport: &[Value],
    native: &BTreeMap<String, Vec<u8>>,
) -> Audit {
    let owned: Vec<_> = rust
        .iter()
        .enumerate()
        .filter(|(_, e)| e["event"] == "OutcomeOwned")
        .collect();
    if owned.len() != native.len() || owned.is_empty() {
        return Err(rejected(
            "OWNED_CARDINALITY",
            json!({"owned":owned.len(),"native":native.len()}),
        ));
    }
    let mut checked = Vec::new();
    let mut unique = std::collections::BTreeSet::new();
    for (owned_index, event) in owned {
        let token = &event["token"];
        let Some(token_text) = token.as_str() else {
            return Err(rejected("INVALID_TOKEN", event.clone()));
        };
        if !unique.insert(token_text) || !native.contains_key(token_text) {
            return Err(rejected("TOKEN_OWNERSHIP", event.clone()));
        }
        let binding = &event["binding"];
        if hash(serde_json::to_vec(binding).unwrap()) != token_text {
            return Err(rejected("TOKEN_BINDING", binding.clone()));
        }
        let sent = matching(rust, "ReadySent", token);
        if sent.len() != 1 || owned_index >= sent[0].0 {
            return Err(rejected(
                "READY_BEFORE_OWNERSHIP",
                json!({"token":token,"sentCount":sent.len()}),
            ));
        }
        let routes: Vec<_> = rust
            .iter()
            .filter(|e| {
                e["event"] == "RouteEvaluated"
                    && e["inputWireHex"]
                        .as_str()
                        .is_some_and(|wire| hash(unhex(wire)) == binding["inputSha256"])
            })
            .collect();
        if routes.len() != 1 {
            return Err(rejected(
                "INPUT_ROUTE_BINDING",
                json!({"token":token,"matchingRoutes":routes.len()}),
            ));
        }
        let mut input_decoder = Decoder::default();
        input_decoder
            .buffer
            .extend_from_slice(&unhex(routes[0]["inputWireHex"].as_str().unwrap()));
        let Some(input) = input_decoder
            .next()
            .map_err(|_| rejected("INPUT_FRAME_INVALID", json!({"token":token})))?
        else {
            return Err(rejected("INPUT_FRAME_MISSING", json!({"token":token})));
        };
        let input_json: Value = serde_json::from_slice(&input.payload)
            .map_err(|_| rejected("INPUT_JSON_INVALID", json!({"token":token})))?;
        let list = match input_json["method"].as_str() {
            Some("list_projects") => true,
            Some("supported_agents") => false,
            _ => return Err(rejected("UNMAPPED_METHOD", input_json)),
        };
        if !input_decoder.buffer.is_empty()
            || input_json["id"] != binding["requestId"]
            || event["list"] != list
            || hash(unhex(routes[0]["payloadHex"].as_str().unwrap())) != binding["resultSha256"]
        {
            return Err(rejected("INPUT_RESULT_BINDING", json!({"token":token})));
        }
        let joins: Vec<_> = rust
            .iter()
            .enumerate()
            .filter(|(_, e)| {
                e["event"] == "JoinConsumed"
                    && e["requestId"] == binding["requestId"]
                    && e["inputSha256"] == binding["inputSha256"]
            })
            .collect();
        let received = matching(host, "ReadyReceived", token);
        let accepted = matching(host, "AdmissionAccepted", token);
        let committed = matching(host, "commit", token);
        if received.len() != 1 || accepted.len() != 1 || committed.len() != 1 {
            return Err(rejected(
                "HOST_CHAIN_CARDINALITY",
                json!({"token":token,"received":received.len(),"accepted":accepted.len(),"committed":committed.len()}),
            ));
        }
        let expected_class = if list {
            "JoinConsumed"
        } else {
            "ReadyAtAdmission"
        };
        if received[0].1["readinessClass"] != expected_class
            || !(received[0].0 < accepted[0].0 && accepted[0].0 < committed[0].0)
        {
            return Err(rejected(
                "HOST_ADMISSION_ORDER",
                json!({"token":token,"readinessClass":received[0].1["readinessClass"]}),
            ));
        }
        if matching(receipts, "Accepted", token).len() != 1 {
            return Err(rejected("ACCEPTANCE_RECEIPT", json!({"token":token})));
        }
        let owner: Vec<_> = records
            .iter()
            .filter(|r| r["kind"] == "NativeSlot" && r["token"] == *token)
            .collect();
        let arrivals = matching(transport, "CommitReceived", token);
        let consumed: Vec<_> = transport
            .iter()
            .filter(|r| r["event"] == "CommitConsumed" && r["record"]["token"] == *token)
            .collect();
        if owner.len() != 1 || arrivals.len() != 1 || consumed.len() != 1 {
            return Err(rejected(
                "TOKEN_COMMIT_CARDINALITY",
                json!({"token":token,"owner":owner.len(),"arrivals":arrivals.len(),"consumed":consumed.len()}),
            ));
        }
        if owner[0]["connection"] != binding["connection"]
            || owner[0]["generation"] != binding["generation"]
            || owner[0]["seq"] != committed[0].1["seq"]
            || consumed[0]["record"] != *owner[0]
        {
            return Err(rejected("TOKEN_COMMIT_IDENTITY", json!({"token":token})));
        }
        let mut decoder = Decoder::default();
        decoder.buffer.extend_from_slice(&native[token_text]);
        let Some(frame) = decoder
            .next_server()
            .map_err(|_| rejected("NATIVE_FRAME_INVALID", json!({"token":token})))?
        else {
            return Err(rejected("NATIVE_FRAME_MISSING", json!({"token":token})));
        };
        if !decoder.buffer.is_empty()
            || hash(&frame.payload) != binding["resultSha256"]
            || hash([b"LocalResponse:".as_slice(), frame.payload.as_slice()].concat())
                != binding["routeSha256"]
        {
            return Err(rejected("NATIVE_RESULT_BINDING", json!({"token":token})));
        }
        if list && (joins.len() != 1 || joins[0].0 >= owned_index || joins[0].0 >= sent[0].0) {
            return Err(rejected(
                "R2_REQUIRED",
                json!({"token":token,"requestId":binding["requestId"],"inputSha256":binding["inputSha256"],"matchingJoinCount":joins.len(),"ownedRustIndex":owned_index,"readySentRustIndex":sent[0].0,"matchingJoinRustIndices":joins.iter().map(|(i,_)|*i).collect::<Vec<_>>(),"hostReadyIndex":received[0].0,"hostAcceptedIndex":accepted[0].0,"hostCommitIndex":committed[0].0,"ownerSeq":owner[0]["seq"],"transportReceived":true,"transportConsumed":true,"nativePayloadBound":true}),
            ));
        }
        checked.push(json!({"token":token,"requestId":binding["requestId"],"inputSha256":binding["inputSha256"],"readinessClass":expected_class,"matchingJoinCount":joins.len(),"ownerSeq":owner[0]["seq"]}));
    }
    Ok(
        json!({"tokens":checked,"clockRule":"indices compared only inside one observer; cross-end links use tokens"}),
    )
}

pub(super) fn audit_commit_wire(
    records: &[Value],
    transport: &[Value],
    native: &BTreeMap<String, Vec<u8>>,
    actual: &[Vec<u8>],
    oracle: &[Vec<u8>],
) -> Audit {
    let consumed: Vec<_> = transport
        .iter()
        .filter(|e| e["event"] == "CommitConsumed")
        .collect();
    let received: Vec<_> = transport
        .iter()
        .filter(|e| e["event"] == "CommitReceived")
        .collect();
    if records.len() != actual.len()
        || records.len() != consumed.len()
        || records.len() != received.len()
    {
        return Err(rejected(
            "COMMIT_CARDINALITY",
            json!({"owner":records.len(),"actual":actual.len(),"consumed":consumed.len(),"received":received.len()}),
        ));
    }
    let writes: Vec<_> = transport
        .iter()
        .filter(|e| e["event"] == "W0")
        .map(|e| e["wireHex"].clone())
        .collect();
    let reads: Vec<_> = transport
        .iter()
        .filter(|e| e["event"] == "C0")
        .map(|e| e["wireHex"].clone())
        .collect();
    let expected: Vec<_> = actual
        .iter()
        .map(|bytes| Value::String(hex(bytes)))
        .collect();
    if writes != expected || reads != expected {
        return Err(rejected(
            "W0_C0_WIRE_MISMATCH",
            json!({"writes":writes.len(),"reads":reads.len(),"frames":actual.len()}),
        ));
    }
    for (i, (record, used)) in records.iter().zip(&consumed).enumerate() {
        let seq = (i + 1) as u64;
        if record["seq"] != seq || used["seq"] != seq || used["record"]["seq"] != seq {
            return Err(rejected(
                "COMMIT_SEQUENCE_VIOLATION",
                json!({"expected":seq,"ownerSeq":record["seq"],"consumedSeq":used["seq"],"recordSeq":used["record"]["seq"]}),
            ));
        }
        let arrivals: Vec<_> = received
            .iter()
            .filter(|e| e["seq"] == record["seq"])
            .collect();
        // Observer adds event metadata; compare every original commit field.
        if arrivals.len() != 1
            || record
                .as_object()
                .unwrap()
                .iter()
                .any(|(k, v)| arrivals[0][k] != *v)
            || used["record"] != *record
        {
            return Err(rejected("COMMIT_RECORD_IDENTITY", json!({"seq":seq})));
        }
        let bytes = if record["kind"] == "NativeSlot" {
            let Some(token) = record["token"].as_str() else {
                return Err(rejected("TOKEN_MISSING", json!({"seq":seq})));
            };
            let Some(bytes) = native.get(token) else {
                return Err(rejected("TOKEN_UNKNOWN", json!({"seq":seq,"token":token})));
            };
            bytes.clone()
        } else if record["kind"] == "HostFrame" {
            unhex(
                record["wireHex"]
                    .as_str()
                    .ok_or_else(|| rejected("HOST_FRAME_MISSING", json!({"seq":seq})))?,
            )
        } else {
            return Err(rejected("COMMIT_KIND", record.clone()));
        };
        if bytes != actual[i] || used["wireSha256"] != hash(&bytes) {
            return Err(rejected("COMMIT_PAYLOAD_MISMATCH", json!({"seq":seq})));
        }
    }
    if actual != oracle {
        return Err(rejected(
            "FULL_WIRE_MISMATCH",
            json!({"actualSha256":hash(actual.concat()),"oracleSha256":hash(oracle.concat())}),
        ));
    }
    Ok(
        json!({"commits":records.len(),"seqContinuous":true,"wireMatchesOracle":true,"w0C0Matches":true}),
    )
}
