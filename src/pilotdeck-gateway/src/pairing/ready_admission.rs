//! Private explicit-envelope prototype, no production hook.
use super::*;
use std::io::{BufRead, BufReader, Write};
#[path = "ready_audit.rs"]
mod ready_audit;

const WIRE_CAPACITY: usize = 1024 * 1024;
const HOST_RECORD_LIMIT: usize = 256 * 1024;

fn read_host_record(input: &mut impl BufRead) -> std::io::Result<Option<String>> {
    let mut line = String::new();
    let count = std::io::Read::take(input, (HOST_RECORD_LIMIT + 1) as u64).read_line(&mut line)?;
    if count > HOST_RECORD_LIMIT {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "bounded pipe record",
        ));
    }
    Ok((count != 0).then_some(line))
}

#[test]
fn host_record_exact_limit_preserves_next_record() {
    let mut bytes = vec![b'x'; HOST_RECORD_LIMIT - 1];
    bytes.extend_from_slice(b"\nnext\n");
    let mut input = std::io::Cursor::new(bytes);
    assert_eq!(
        read_host_record(&mut input).unwrap().unwrap().len(),
        HOST_RECORD_LIMIT
    );
    assert_eq!(
        read_host_record(&mut input).unwrap().as_deref(),
        Some("next\n")
    );
    assert_eq!(read_host_record(&mut input).unwrap(), None);
}

#[test]
fn host_record_first_excess_byte_stops_consumption() {
    let mut input = std::io::Cursor::new(vec![b'x'; HOST_RECORD_LIMIT * 4]);
    assert_eq!(
        read_host_record(&mut input).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(input.position(), (HOST_RECORD_LIMIT + 1) as u64);
}

#[test]
fn host_record_empty_eof_and_unterminated_limit() {
    assert_eq!(
        read_host_record(&mut std::io::Cursor::new(b"")).unwrap(),
        None
    );
    let mut input = std::io::Cursor::new(vec![b'x'; HOST_RECORD_LIMIT]);
    assert_eq!(
        read_host_record(&mut input).unwrap().unwrap().len(),
        HOST_RECORD_LIMIT
    );
    assert_eq!(read_host_record(&mut input).unwrap(), None);
}

const HELPERS: &[&str] = &[
    "ready-host.mjs",
    "ready-owner.mjs",
    "acceptance-gate.mjs",
    "domain-commit.mjs",
    "fixture.mjs",
    "protocol.mjs",
    "guard.mjs",
    "offline.mjs",
];

struct Host {
    process: Process,
    input: std::process::ChildStdin,
    output: BufReader<std::process::ChildStdout>,
    events: Vec<Value>,
    commits: Vec<Value>,
}
impl Host {
    fn send(&mut self, value: Value) {
        serde_json::to_writer(&mut self.input, &value).unwrap();
        self.input.write_all(b"\n").unwrap();
        self.input.flush().unwrap();
    }
    fn until(&mut self, event: &str) -> Value {
        loop {
            let line = read_host_record(&mut self.output)
                .unwrap()
                .expect("host EOF");
            let row: Value = serde_json::from_str(&line).unwrap();
            if row["event"] == "Commit" {
                self.commits.push(row["record"].clone());
            }
            self.events.push(row.clone());
            if row["event"] == event {
                return row;
            }
        }
    }
    fn responses(&mut self) {
        while self.commits.len() < 2 {
            self.until("Commit");
        }
    }
}

fn request(id: &str, method: &str) -> (Value, frame::Frame) {
    let json = json!({"type":"request","id":id,"method":method});
    let payload = serde_json::to_vec(&json).unwrap();
    let wire = owned_frame(1, &payload);
    let mut decoder = Decoder::default();
    decoder.buffer.extend_from_slice(&wire);
    (json, decoder.next().unwrap().unwrap())
}
fn local_item(token: &str, list: bool) -> Value {
    json!({"kind":"NativeSlot","token":token,"readinessClass":if list {"JoinConsumed"} else {"ReadyAtAdmission"}})
}

async fn acquire(
    input: frame::Frame,
    list: bool,
    bypass: bool,
    metadata: Arc<crate::metadata::Metadata<TracedFiles>>,
    observer: &Observer,
) -> (String, Vec<u8>, Value) {
    let value = dispatch::parse(&input.payload).unwrap();
    let id = crate::metadata::text_field(&value, "id").unwrap();
    let facts = json!({"requestId":id,"inputSha256":hash(&input.wire)});
    let route = if list {
        let mut jobs = tokio::task::JoinSet::new();
        let observed = observer.clone();
        let frame_sha = hash(&input.wire);
        let (tx, rx) = tokio::sync::oneshot::channel();
        jobs.spawn_blocking(move || {
            let route = dispatch::route_with_metadata(&value, Some(metadata.as_ref()));
            observed.event("RouteReturned", json!({"inputSha256":frame_sha}));
            if bypass {
                tx.send(route).ok();
                None
            } else {
                Some(route)
            }
        });
        if bypass {
            let route = rx.await.unwrap();
            observer.event("MutantHit", json!({"mutation":"X1_REAL_JOIN_BYPASS"}));
            // The closure-owned result really leaves before join_next; no fake R2.
            route
        } else {
            let route = jobs.join_next().await.unwrap().unwrap().unwrap();
            observer.event("JoinConsumed", facts.clone());
            route
        }
    } else {
        dispatch::route_with_metadata(&value, Some(metadata.as_ref()))
    };
    let Route::LocalResponse(payload) = route else {
        observer.event(
            "UnmappedRoute",
            json!({"route":"Forward","inputWireHex":hex(&input.wire)}),
        );
        panic!("UNMAPPED_FORWARD_ROUTE");
    };
    observer.event(
        "RouteEvaluated",
        json!({"route":"LocalResponse","inputWireHex":hex(&input.wire),"payloadHex":hex(&payload)}),
    );
    let result: Value = serde_json::from_slice(&payload).unwrap();
    assert_eq!(result["ok"], true, "UNMAPPED_ERROR_RESPONSE");
    let binding = json!({"run":std::env::var("PDRS_READY_RUN").unwrap(),"connection":std::env::var("PDRS_READY_CASE").unwrap(),"generation":1,"requestId":id,"inputSha256":hash(&input.wire),"resultSha256":hash(&payload),"routeSha256":hash([b"LocalResponse:".as_slice(),payload.as_slice()].concat())});
    let token = hash(serde_json::to_vec(&binding).unwrap());
    observer.event(
        "OutcomeOwned",
        json!({"token":token,"binding":binding,"list":list}),
    );
    (token, frame::server_frame(1, &payload), binding)
}

fn consume(
    records: &[Value],
    native: &BTreeMap<String, Vec<u8>>,
    observer: &Observer,
    arrival: bool,
) -> Vec<Vec<u8>> {
    assert_eq!(records.len(), 2, "exact two decided records");
    for record in records {
        assert_eq!(
            record["connection"],
            std::env::var("PDRS_READY_CASE").unwrap()
        );
        assert_eq!(record["generation"], 1);
    }
    let mut transport = records.to_vec();
    transport.swap(0, 1);
    observer.event("TransportSwapped",json!({"seqs":transport.iter().map(|r|r["seq"].clone()).collect::<Vec<_>>(),"arrivalWriter":arrival}));
    let mut output = Vec::new();
    if arrival {
        observer.event("MutantHit",json!({"mutation":"X2_ARRIVAL_WRITER","transportSeqs":transport.iter().map(|r|r["seq"].clone()).collect::<Vec<_>>()}));
    }
    let mut pending = BTreeMap::new();
    let mut next = 1_u64;
    let mut used = std::collections::BTreeSet::new();
    for record in transport {
        observer.event("CommitReceived", record.clone());
        if arrival {
            pending.insert(next, record);
        } else {
            let seq = record["seq"].as_u64().unwrap();
            assert!(seq >= next);
            assert!(pending.insert(seq, record).is_none());
        }
        while let Some(record) = pending.remove(&next) {
            let seq = record["seq"].as_u64().unwrap();
            let bytes = if record["kind"] == "NativeSlot" {
                let token = record["token"].as_str().unwrap();
                assert!(used.insert(token.to_string()));
                native.get(token).expect("unknown committed token").clone()
            } else {
                unhex(record["wireHex"].as_str().unwrap())
            };
            // This is the actual candidate writer's consume path, before downstream.
            // X2 performs the wrong write order; independent wire verdict runs later.
            observer.event(
                "CommitConsumed",
                json!({"seq":seq,"expected":next,"wireSha256":hash(&bytes),"record":record}),
            );
            output.push(bytes);
            next += 1;
        }
    }
    assert_eq!(pending.len(), 0);
    output
}
fn unhex(text: &str) -> Vec<u8> {
    assert_eq!(text.len() % 2, 0);
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ready_wire_exact_capacity_round_trip() {
    let (mut client, mut downstream) = tokio::io::duplex(WIRE_CAPACITY);
    let payload: Vec<u8> = (0..WIRE_CAPACITY)
        .map(|index| (index % 251) as u8)
        .collect();
    // No reader is polled until the exact-capacity write has completed.
    timeout(LIMIT, downstream.write_all(&payload))
        .await
        .expect("exact-capacity write-before-read watchdog")
        .expect("exact-capacity duplex write");
    let mut received = vec![0_u8; WIRE_CAPACITY];
    timeout(LIMIT, client.read_exact(&mut received))
        .await
        .expect("exact-capacity read watchdog")
        .expect("exact-capacity duplex read");
    assert_eq!(received, payload);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "PREPARED_NOT_RUN; root fresh exact Cargo artifact and bounded execution window"]
async fn ready_admission_real_payload() {
    let case = std::env::var("PDRS_READY_CASE").unwrap();
    assert!(["D1+", "D1-", "A+", "A-", "X1", "X2"].contains(&case.as_str()));
    let (node, node_sha) = pinned("NODE");
    let (git, git_sha) = pinned("GIT");
    let (loader, loader_sha) = pinned("LOADER");
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let workspace = manifest
        .ancestors()
        .find(|p| p.join(".git").exists())
        .unwrap();
    let read_git = |args: &[&str]| {
        let result = Command::new(&git)
            .arg("-C")
            .arg(workspace)
            .args(args)
            .output()
            .unwrap();
        assert!(result.status.success());
        String::from_utf8(result.stdout).unwrap().trim().to_string()
    };
    let head = read_git(&["rev-parse", "HEAD"]);
    assert_eq!(
        option_env!("PDRS_ORDER_BUILD_SOURCE_HEAD"),
        Some(head.as_str()),
        "compile-time source HEAD binding required for this opt-in probe"
    );
    assert!(read_git(&["status", "--porcelain", "--", "src/pilotdeck-gateway"]).is_empty());
    assert_eq!(
        hash(fs::read(std::env::current_exe().unwrap()).unwrap()),
        std::env::var("PDRS_ORDER_BINARY_SHA256").unwrap()
    );
    let output = PathBuf::from(std::env::var("PDRS_ORDER_OUTPUT").unwrap());
    assert!(output.is_absolute() && !output.exists());
    let common = PathBuf::from(read_git(&[
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ]));
    assert!(
        fs::canonicalize(output.parent().unwrap())
            .unwrap()
            .starts_with(common.parent().unwrap().join("_tmp"))
    );
    fs::create_dir(&output).unwrap();
    let prereg = fs::canonicalize(std::env::var("PDRS_READY_PREREG").unwrap()).unwrap();
    let mut preregistration = BTreeMap::new();
    for (name, key) in [
        ("task.md", "PDRS_READY_TASK_SHA256"),
        ("matrix.json", "PDRS_READY_MATRIX_SHA256"),
    ] {
        let digest = hash(fs::read(prereg.join(name)).unwrap());
        assert_eq!(digest, std::env::var(key).unwrap());
        preregistration.insert(name, digest);
    }
    fs::write(output.join("identity.json"),serde_json::to_vec_pretty(&json!({"sourceHead":head,"binarySha256":std::env::var("PDRS_ORDER_BINARY_SHA256").unwrap(),"preregistration":preregistration,"case":case,"status":"RUN_STARTED_NOT_ACCEPTED"})).unwrap()).unwrap();
    let scripts_root = manifest.join("tests/order");
    let scripts: BTreeMap<_, _> = HELPERS
        .iter()
        .map(|name| (*name, hash(fs::read(scripts_root.join(name)).unwrap())))
        .collect();
    let scratch = output.join("scratch");
    fs::create_dir(&scratch).unwrap();
    for dir in ["home", "config", "cache", "data", "tmp", "pilot"] {
        fs::create_dir(scratch.join(dir)).unwrap();
    }
    let baseline = fs::canonicalize(std::env::var("PDRS_ORDER_BASELINE").unwrap()).unwrap();
    let observer = Observer::new();
    let _failure = super::super::result_acquisition::FailureTrace::new(
        observer.clone(),
        output.join("failure-trace.json"),
    );
    let mut oracle = Vec::new();
    for mode in ["oracle", "candidate"] {
        let spec = json!({"mode":mode,"connection":case,"scratch":scratch,"baseline":baseline,"scripts":scripts,"scriptRoot":scripts_root,"report":output.join(format!("{mode}-host.json")),"tools":{"node":{"path":node,"sha256":node_sha},"git":{"path":git,"sha256":git_sha},"loader":{"path":loader,"sha256":loader_sha}}});
        let spec_path = output.join(format!("{mode}-spec.json"));
        fs::write(&spec_path, serde_json::to_vec_pretty(&spec).unwrap()).unwrap();
        let mut command = Command::new(&node);
        command
            .args(["--import"])
            .arg(&loader)
            .arg("--import")
            .arg(scripts_root.join("offline.mjs"))
            .arg(scripts_root.join("ready-host.mjs"))
            .arg(&spec_path)
            .current_dir(&scratch)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("PDRS_READY_PROBE", "owned-ready-admission");
        for (key, dir) in [
            ("HOME", "home"),
            ("XDG_CONFIG_HOME", "config"),
            ("XDG_CACHE_HOME", "cache"),
            ("XDG_DATA_HOME", "data"),
            ("TMPDIR", "tmp"),
            ("PILOT_HOME", "pilot"),
        ] {
            command.env(key, scratch.join(dir));
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let process_directory = output.join(mode);
        fs::create_dir(&process_directory).unwrap();
        let mut process = Process::new(
            command.spawn().unwrap(),
            process_directory.join("stderr.log"),
        );
        let input = process.child.stdin.take().unwrap();
        let stdout = process.child.stdout.take().unwrap();
        let mut host = Host {
            process,
            input,
            output: BufReader::new(stdout),
            events: Vec::new(),
            commits: Vec::new(),
        };
        host.until("Boot");
        let list = case.starts_with('A') || case == "X1";
        let reverse = case == "D1-";
        let methods = if list {
            ["list_projects", "supported_agents"]
        } else if reverse {
            ["supported_agents", "describe_server"]
        } else {
            ["describe_server", "supported_agents"]
        };
        let metadata = Arc::new(crate::metadata::Metadata {
            home: scratch.join("pilot"),
            cwd: scratch.join("project"),
            files: TracedFiles(observer.clone()),
        });
        let mut items = Vec::new();
        let mut native = BTreeMap::new();
        let mut ready_token = None;
        for (i, method) in methods.iter().enumerate() {
            let (_, frame) = request(&format!("{case}-{i}"), method);
            if mode == "oracle" || *method == "describe_server" {
                items.push(json!({"kind":"HostFrame","wireHex":hex(&frame.wire)}));
            } else {
                let (token, wire, _) = acquire(
                    frame,
                    *method == "list_projects",
                    case == "X1",
                    metadata.clone(),
                    &observer,
                )
                .await;
                native.insert(token.clone(), wire);
                if *method != "list_projects" {
                    observer.event("ReadySent",json!({"token":token,"readinessClass":"ReadyAtAdmission","transport":"explicit-envelope"}));
                }
                if *method == "list_projects" {
                    ready_token = Some(token.clone());
                }
                items.push(local_item(&token, *method == "list_projects"));
            }
        }
        if list {
            host.send(json!({"kind":"Input","label":"list","items":[items[0]]}));
            host.until("CallbackReturned");
            let oracle_token = if mode == "oracle" {
                Some(host.until("Owned")["token"].clone())
            } else {
                None
            };
            if case == "A-" {
                host.send(json!({"kind":"Input","label":"successor","items":[items[1]]}));
                host.until("CallbackReturned");
            }
            let after_accept = if case == "A+" || case == "X1" {
                Some(vec![items[1].clone()])
            } else {
                None
            };
            if let Some(token) = oracle_token {
                host.send(json!({"kind":"OracleRelease","token":token,"afterAccept":after_accept}));
            } else {
                let token = ready_token.unwrap();
                observer.event("ReadySent", json!({"token":token}));
                // X1 sends Ready despite the real closure bypass.
                // The independent Rust causal ledger, not this declaration, is
                // the verdict source after actual receipt/commit/wire collection.
                host.send(json!({"kind":"Ready","token":token,"readinessClass":"JoinConsumed","afterAccept":after_accept}));
            }
            host.until("Accepted");
            if case == "A+" || case == "X1" {
                host.until("CallbackReturned");
            }
        } else {
            host.send(json!({"kind":"Input","label":"pair","items":items}));
            host.until("CallbackReturned");
        }
        host.responses();
        host.send(json!({"kind":"Finish"}));
        host.until("Finished");
        let (status, stderr_bytes) = host
            .process
            .finish()
            .await
            .expect("Ready host exit and complete bounded stderr capture");
        assert!(status.success());
        observer.event(
            "HostStderrCaptured",
            json!({"mode":mode,"bytes":stderr_bytes,"complete":true}),
        );
        let host_report: Value =
            serde_json::from_slice(&fs::read(output.join(format!("{mode}-host.json"))).unwrap())
                .unwrap();
        let host_events = host_report["events"].as_array().unwrap();
        assert_eq!(host_report["cleanup"]["disposed"], true);
        assert_eq!(host_report["cleanup"]["closed"], true);
        if mode == "candidate" && (case == "A+" || case == "A-") {
            let received = host_events
                .iter()
                .position(|e| {
                    e["event"] == "ReadyReceived" && e["readinessClass"] == "JoinConsumed"
                })
                .unwrap();
            let returned = host_events
                .iter()
                .position(|e| e["event"] == "CallbackReturn" && e["label"] == "successor")
                .unwrap();
            let token = items[0]["token"].clone();
            let committed = host_events
                .iter()
                .position(|e| e["event"] == "commit" && e["token"] == token)
                .unwrap();
            assert!(
                returned < committed,
                "continuation must remain after successor callback return"
            );
            assert!(
                if case == "A+" {
                    received < returned
                } else {
                    returned < received
                },
                "explicit callback/Ready envelope mapping"
            );
        }
        fs::write(
            output.join(format!("{mode}-pipe.json")),
            serde_json::to_vec_pretty(&host.events).unwrap(),
        )
        .unwrap();
        let consume_start = observer.snapshot().len();
        let actual = consume(
            &host.commits,
            &native,
            &observer,
            mode == "candidate" && case == "X2",
        );
        if mode == "oracle" {
            oracle = actual;
        } else {
            let total_wire_bytes = actual
                .iter()
                .try_fold(0_usize, |total, bytes| total.checked_add(bytes.len()))
                .expect("UNMAPPED_WIRE_SIZE_LIMIT: total wire length overflow");
            assert!(
                total_wire_bytes <= WIRE_CAPACITY,
                "UNMAPPED_WIRE_SIZE_LIMIT: total wire exceeds duplex capacity"
            );
            observer.event(
                "WireCapacityChecked",
                json!({"totalWireBytes":total_wire_bytes,"capacityBytes":WIRE_CAPACITY}),
            );
            let (mut client, mut downstream) = tokio::io::duplex(WIRE_CAPACITY);
            for bytes in &actual {
                downstream.write_all(bytes).await.unwrap();
                observer.event("W0", json!({"wireHex":hex(bytes)}));
            }
            let mut decoder = Decoder::default();
            for bytes in &actual {
                let read = next_frame(&mut client, &mut decoder).await;
                observer.event("C0", json!({"wireHex":hex(&read.wire)}));
                assert_eq!(&read.wire, bytes);
            }
            fs::write(
                output.join("actual-wire.json"),
                serde_json::to_vec_pretty(
                    &actual.iter().map(|bytes| hex(bytes)).collect::<Vec<_>>(),
                )
                .unwrap(),
            )
            .unwrap();
            if case == "X1" {
                let events = observer.snapshot();
                assert!(
                    events.iter().any(
                        |e| e["event"] == "MutantHit" && e["mutation"] == "X1_REAL_JOIN_BYPASS"
                    ),
                    "actual bypass not hit"
                );
                assert!(
                    host_events
                        .iter()
                        .any(|e| e["event"] == "ReadyReceived"
                            && e["readinessClass"] == "JoinConsumed"),
                    "early Ready was not actually received"
                );
            }
            if case == "X2" {
                assert!(
                    observer.snapshot()[consume_start..]
                        .iter()
                        .any(|e| e["event"] == "MutantHit" && e["mutation"] == "X2_ARRIVAL_WRITER"),
                    "actual arrival writer not hit"
                );
            }
            let events = observer.snapshot();
            let transport_events = &events[consume_start..];
            let ready = ready_audit::audit_ready_chain(
                &events,
                host_events,
                &host.events,
                &host.commits,
                transport_events,
                &native,
            );
            let wire = ready_audit::audit_commit_wire(
                &host.commits,
                transport_events,
                &native,
                &actual,
                &oracle,
            );
            let rejection = ready
                .as_ref()
                .err()
                .or_else(|| wire.as_ref().err())
                .cloned();
            let ready_report = match ready {
                Ok(value) => json!({"Ok":value}),
                Err(value) => json!({"Err":value}),
            };
            let wire_report = match wire {
                Ok(value) => json!({"Ok":value}),
                Err(value) => json!({"Err":value}),
            };
            let audit = json!({"status":if rejection.is_some(){"REJECTED"}else{"ACCEPTED"},"readyChain":ready_report,"commitWire":wire_report,"rejection":rejection});
            let file = fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(output.join("audit.json"))
                .unwrap();
            serde_json::to_writer_pretty(file, &audit).unwrap();
            assert!(
                rejection.is_none(),
                "COMMON_AUDIT_REJECTED: {}",
                rejection.unwrap_or(Value::Null)
            );
        }
    }
    fs::write(
        output.join("events.json"),
        serde_json::to_vec_pretty(&observer.snapshot()).unwrap(),
    )
    .unwrap();
}
