use pilotdeck_gateway::storage::{
    JsonlTranscriptStore, TranscriptKey, TranscriptRead, TranscriptStore, read_transcript,
};
use pilotdeck_protocol::stringify_json;
use std::{fs, io::Write};

struct Fixture {
    store: JsonlTranscriptStore,
    key: TranscriptKey,
}
impl Fixture {
    fn new(bytes: &[u8]) -> Self {
        let root = std::env::temp_dir().join(format!("pdrs-incremental-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let path = root.join("chat.jsonl");
        fs::write(&path, bytes).unwrap();
        Self {
            store: JsonlTranscriptStore::new(),
            key: TranscriptKey {
                project_root: root.clone(),
                pilot_home: root,
                session_id: "test".into(),
                transcript_path: path,
            },
        }
    }
    fn read(&self) -> TranscriptRead {
        self.store.read(&self.key, None).unwrap()
    }
    fn append(&self, bytes: &[u8]) {
        fs::OpenOptions::new()
            .append(true)
            .open(&self.key.transcript_path)
            .unwrap()
            .write_all(bytes)
            .unwrap();
    }
    fn assert_matches_full(&self) -> TranscriptRead {
        let cached = self.read();
        let full = read_transcript(&self.key.transcript_path).unwrap();
        let entries = |read: &TranscriptRead| {
            read.entries
                .iter()
                .map(|v| stringify_json(v).unwrap())
                .collect::<Vec<_>>()
        };
        assert_eq!(entries(&cached), entries(&full));
        let diagnostics = |read: &TranscriptRead| {
            read.diagnostics
                .iter()
                .map(|d| (d.code, d.severity, d.line, d.message.clone()))
                .collect::<Vec<_>>()
        };
        assert_eq!(diagnostics(&cached), diagnostics(&full));
        cached
    }
}
fn entry(sequence: u64, timestamp: &str, text: &str) -> String {
    format!(
        r#"{{"type":"test","sessionId":"test","turnId":"turn","createdAt":"{timestamp}","sequence":{sequence},"text":"{text}"}}"#
    )
}

#[test]
fn appended_invalid_lines_keep_absolute_line_numbers() {
    let f = Fixture::new(format!("{}\n\n", entry(1, "a", "first")).as_bytes());
    assert!(f.read().diagnostics.is_empty());
    f.append(b"invalid\n{}\n");
    let read = f.assert_matches_full();
    assert_eq!(
        read.diagnostics.iter().map(|d| d.line).collect::<Vec<_>>(),
        vec![Some(3), Some(4)]
    );
}

#[test]
fn completing_unterminated_record_removes_previous_parse_error() {
    let line = entry(2, "b", "completed");
    let split = line.len() - 3;
    let f = Fixture::new(&line.as_bytes()[..split]);
    assert_eq!(f.read().diagnostics.len(), 1);
    f.append(&line.as_bytes()[split..]);
    f.append(b"\n");
    let read = f.assert_matches_full();
    assert_eq!(read.entries.len(), 1);
    assert!(read.diagnostics.is_empty());
}

#[test]
fn growing_rewrite_replaces_cached_prefix() {
    let f = Fixture::new(format!("{}\n", entry(9, "z", "old")).as_bytes());
    f.read();
    let replacement = format!(
        "{}\n{}\n",
        entry(1, "a", "replacement with longer text"),
        entry(2, "b", "new")
    );
    fs::write(&f.key.transcript_path, replacement).unwrap();
    let read = f.assert_matches_full();
    assert_eq!(read.entries.len(), 2);
    assert!(
        !stringify_json(&read.entries[0])
            .unwrap()
            .unwrap()
            .contains("old")
    );
}

#[test]
fn appended_entries_are_globally_sorted_including_sequence_ties() {
    let f = Fixture::new(format!("{}\n", entry(3, "z", "last")).as_bytes());
    f.read();
    f.append(
        format!(
            "{}\n{}\n{}\n",
            entry(2, "b", "middle-b"),
            entry(1, "a", "first"),
            entry(2, "a", "middle-a")
        )
        .as_bytes(),
    );
    let read = f.assert_matches_full();
    let texts = read
        .entries
        .iter()
        .map(|v| stringify_json(v).unwrap().unwrap())
        .collect::<Vec<_>>();
    for (actual, expected) in texts.iter().zip(["first", "middle-a", "middle-b", "last"]) {
        assert!(actual.contains(expected), "{actual}");
    }
}

#[test]
fn utf8_split_across_appends_is_redecoded_after_completion() {
    let line = entry(1, "a", "中文🙂");
    let split = line.find('中').unwrap() + 1;
    let f = Fixture::new(&line.as_bytes()[..split]);
    assert!(!f.read().diagnostics.is_empty());
    f.append(&line.as_bytes()[split..]);
    f.append(b"\n");
    let read = f.assert_matches_full();
    assert!(read.diagnostics.is_empty());
    assert!(
        stringify_json(&read.entries[0])
            .unwrap()
            .unwrap()
            .contains("中文🙂")
    );
}

#[test]
fn invalid_utf8_in_appended_string_matches_full_lossy_decode() {
    let f = Fixture::new(format!("{}\n", entry(1, "a", "first")).as_bytes());
    f.read();
    let mut suffix = entry(2, "b", "X").into_bytes();
    let index = suffix.iter().rposition(|b| *b == b'X').unwrap();
    suffix[index] = 0xff;
    suffix.push(b'\n');
    f.append(&suffix);
    let read = f.assert_matches_full();
    assert!(
        stringify_json(&read.entries[1])
            .unwrap()
            .unwrap()
            .contains('�')
    );
}
