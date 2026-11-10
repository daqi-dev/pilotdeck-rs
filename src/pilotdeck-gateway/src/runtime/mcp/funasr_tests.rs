use super::*;
#[test]
fn srt_retains_timestamp_and_multiline_contract() {
    let s = parse_srt(
        "1\r\n00:00:01,250 --> 00:00:02.50\r\n hello \r\n world\r\n\r\n2\n99:99:99.1 --> 00:00:00,0\nreverse allowed\n",
    );
    assert_eq!(s.len(), 2);
    assert_eq!(
        s[0],
        Segment {
            start: 1.25,
            end: 2.5,
            text: "hello world".into()
        }
    );
    assert_eq!(s[1].start, 362439.1);
    assert_eq!(s[1].end, 0.0);
}
#[test]
fn srt_ignores_unparseable_and_empty_blocks() {
    assert!(
        parse_srt(
            "log\n\n1\n0:00:00,000 --> 00:00:01,000\nno\n\n2\n00:00:00,000 --> 00:00:01,000\n"
        )
        .is_empty()
    );
}
#[tokio::test]
async fn runtime_configuration_precedes_argument_validation() {
    let (_tx, rx) = watch::channel(false);
    let result = FunasrOptions::default()
        .transcribe(object([("language", "bad".into())]), rx)
        .await;
    let text = stringify_json(&result).unwrap().unwrap();
    assert!(text.contains("runtime is not configured"));
    assert!(!text.contains("language only"));
}
#[tokio::test]
async fn language_validation_precedes_project_path_lookup() {
    let (_tx, rx) = watch::channel(false);
    let options = FunasrOptions {
        runtime_root: Some("missing".into()),
        ..Default::default()
    };
    let result = options
        .transcribe(object([("language", JsValue::Null)]), rx)
        .await;
    assert!(
        stringify_json(&result)
            .unwrap()
            .unwrap()
            .contains("language only")
    );
}
#[test]
fn helper_role_is_explicit_and_preserves_paths() {
    assert!(parse_helper_args(&["--native".into()]).is_none());
    let parsed = parse_helper_args(&[
        "--builtin-mcp".into(),
        "funasr".into(),
        "--project-root".into(),
        "/tmp/project space".into(),
    ])
    .unwrap();
    assert_eq!(
        parsed.project_root,
        Some(PathBuf::from("/tmp/project space"))
    );
}

#[cfg(unix)]
struct Fixture {
    root: PathBuf,
    options: FunasrOptions,
}
#[cfg(unix)]
impl Fixture {
    fn new(script: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root =
            std::env::temp_dir().join(format!("pdrs-funasr-test-{}-{n}", std::process::id()));
        std::fs::create_dir(&root).unwrap();
        let project = root.join("project");
        std::fs::create_dir(&project).unwrap();
        std::fs::write(project.join("audio.wav"), b"mock-audio").unwrap();
        let runtime = root.join("asr");
        let bin = runtime.join("runtime/v0.2.0").join(runtime_key());
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::create_dir(runtime.join("models")).unwrap();
        for file in ["sensevoice-small-q8.gguf", "fsmn-vad.gguf"] {
            std::fs::write(runtime.join("models").join(file), b"mock-model").unwrap();
        }
        let binary = bin.join("llama-funasr-sensevoice");
        std::fs::write(&binary, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let options = FunasrOptions {
            project_root: Some(project),
            runtime_root: Some(runtime),
            install_command: "mock install".into(),
        };
        Self { root, options }
    }
    async fn run(&self, input: JsValue) -> JsValue {
        let (_tx, rx) = watch::channel(false);
        self.options.transcribe(input, rx).await
    }
}
#[cfg(unix)]
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
#[cfg(unix)]
#[tokio::test]
async fn real_command_srt_success_uses_actual_audio_and_model_argv() {
    let fixture = Fixture::new(
        "test \"$1\" = -m && test -f \"$2\" && test \"$3\" = --vad && test -f \"$4\" && test \"$5\" = -a && test -f \"$6\" && test \"$7\" = --srt || exit 23\nprintf '1\\n00:00:01,250 --> 00:00:02,500\\nhello world\\n'",
    );
    let result = fixture
        .run(object([("audio_path", "audio.wav".into())]))
        .await;
    assert_eq!(
        f(f(&result, "structuredContent"), "text"),
        &JsValue::from("hello world")
    );
    assert!(f(&result, "isError").is_undefined());
    assert!(
        stringify_json(&result)
            .unwrap()
            .unwrap()
            .contains("[1.250 - 2.500] hello world")
    );
}
#[cfg(unix)]
#[tokio::test]
async fn actual_failure_and_empty_srt_are_business_errors() {
    let fixture = Fixture::new("printf 'real mock diagnostic' >&2; exit 9");
    let result = fixture
        .run(object([("audio_path", "audio.wav".into())]))
        .await;
    assert!(
        stringify_json(&result)
            .unwrap()
            .unwrap()
            .contains("FunASR transcription failed (exit 9): real mock diagnostic")
    );
    let fixture = Fixture::new("printf 'not an srt'; printf 'empty diagnostic' >&2");
    let result = fixture
        .run(object([("audio_path", "audio.wav".into())]))
        .await;
    assert!(
        stringify_json(&result)
            .unwrap()
            .unwrap()
            .contains("FunASR completed but did not produce SRT segments. empty diagnostic")
    );
}
#[cfg(unix)]
#[tokio::test]
async fn canonical_project_boundary_rejects_symlink_escape() {
    let fixture = Fixture::new("exit 72");
    let outside = fixture.root.join("outside.wav");
    std::fs::write(&outside, b"outside").unwrap();
    std::os::unix::fs::symlink(
        &outside,
        fixture
            .options
            .project_root
            .as_ref()
            .unwrap()
            .join("escape.wav"),
    )
    .unwrap();
    let result = fixture
        .run(object([("audio_path", "escape.wav".into())]))
        .await;
    assert!(
        stringify_json(&result)
            .unwrap()
            .unwrap()
            .contains("audio_path must resolve inside the current project root")
    );
}
#[cfg(unix)]
#[tokio::test]
async fn cancellation_reaps_real_owned_runtime_before_returning() {
    let fixture = Fixture::new("printf 'started\\n'; exec /bin/sleep 60");
    let started = fixture.root.join("started");
    let binary = fixture
        .options
        .runtime_root
        .as_ref()
        .unwrap()
        .join("runtime/v0.2.0")
        .join(runtime_key())
        .join("llama-funasr-sensevoice");
    std::fs::write(
        &binary,
        format!(
            "#!/bin/sh\nprintf started > '{}'\nexec /bin/sleep 60\n",
            started.display()
        ),
    )
    .unwrap();
    let (tx, rx) = watch::channel(false);
    let options = fixture.options.clone();
    let task = tokio::spawn(async move {
        options
            .transcribe(object([("audio_path", "audio.wav".into())]), rx)
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        while !started.exists() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tx.send_replace(true);
    let result = tokio::time::timeout(Duration::from_secs(4), task)
        .await
        .unwrap()
        .unwrap();
    assert!(
        stringify_json(&result)
            .unwrap()
            .unwrap()
            .contains("FunASR transcription cancelled.")
    );
}

#[tokio::test]
async fn real_duplex_protocol_matches_fixed_ts_initialize_list_and_error() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    let oracle = parse_json(include_str!("funasr_oracle.json")).unwrap();
    let JsValue::Array(cases) = f(&oracle, "cases") else {
        panic!("cases")
    };
    let (client, server) = tokio::io::duplex(65536);
    let (read, write) = tokio::io::split(server);
    let task = tokio::spawn(serve(
        tokio::io::BufReader::new(read),
        write,
        FunasrOptions {
            install_command: "mock install".into(),
            ..Default::default()
        },
    ));
    let (read, mut write) = tokio::io::split(client);
    let mut lines = tokio::io::BufReader::new(read).lines();
    for name in [
        "initialize",
        "tools-list",
        "no-runtime-precedes-language",
        "unknown-tool",
        "unknown-method",
    ] {
        let case = cases.iter().find(|c| eq(f(c, "case"), name)).unwrap();
        let request = stringify_json(f(case, "request")).unwrap().unwrap() + "\n";
        write.write_all(request.as_bytes()).await.unwrap();
        let line = tokio::time::timeout(Duration::from_secs(2), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(&parse_json(&line).unwrap(), f(case, "response"), "{name}");
    }
    write.shutdown().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
