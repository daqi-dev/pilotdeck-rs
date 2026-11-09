use super::*;
#[test]
fn empty_query_returns_before_io() {
    let result = search(SearchOptions {
        pilot_home: "/definitely/missing".into(),
        query: " ".into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(result.get("sessionsScanned"), Some(&JsValue::Number(0.0)));
}

struct Fixture(std::path::PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "pdrs-shared-search-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir_all(path.join("projects/p/chats")).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, text: &str) {
        std::fs::write(self.0.join("projects/p/chats").join(name), text).unwrap();
    }
    fn options(&self) -> SearchOptions {
        SearchOptions {
            pilot_home: self.0.clone(),
            query: "needle".into(),
            ..Default::default()
        }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn rows(value: &JsValue) -> &[JsValue] {
    match value.get("matches").unwrap() {
        JsValue::Array(rows) => rows,
        _ => panic!("matches must be an array"),
    }
}
const USER: &str = r#"{"type":"accepted_input","sessionId":"s","messages":[{"content":[{"type":"text","text":"needle"}]}]}"#;
#[test]
fn fractional_and_nan_limits_keep_provider_comparison_semantics() {
    let fixture = Fixture::new();
    fixture.write("s.jsonl", &[USER, USER, USER].join("\n"));
    let mut options = fixture.options();
    options.limit = 1.5;
    let result = search(options).unwrap();
    assert_eq!(rows(&result).len(), 2);
    assert_eq!(result.get("truncated"), Some(&JsValue::Bool(true)));
    let mut options = fixture.options();
    options.limit = f64::NAN;
    let result = search(options).unwrap();
    assert_eq!(rows(&result).len(), 3);
    assert_eq!(result.get("truncated"), Some(&JsValue::Bool(false)));
}
#[test]
fn physical_lines_unknown_role_and_internal_sessions() {
    let fixture = Fixture::new();
    fixture.write("s.jsonl", &format!("invalid\r\n{USER}\r{USER}\n"));
    fixture.write("always-on-report-hidden.jsonl", USER);
    let result = search(fixture.options()).unwrap();
    assert_eq!(result.get("sessionsScanned"), Some(&JsValue::Number(1.0)));
    assert_eq!(
        rows(&result)[0].get("lineNumber"),
        Some(&JsValue::Number(2.0))
    );
    assert_eq!(
        rows(&result)[1].get("lineNumber"),
        Some(&JsValue::Number(3.0))
    );
    let mut options = fixture.options();
    options.role = "unknown".into();
    assert!(rows(&search(options).unwrap()).is_empty());
}
#[test]
fn utf16_query_and_result_do_not_replace_lone_surrogates() {
    let fixture = Fixture::new();
    fixture.write("s.jsonl", r#"{"type":"accepted_input","sessionId":"s","messages":[{"content":[{"type":"text","text":"a\ud800b"}]}]}"#);
    let mut options = fixture.options();
    options.query = JsString::from_utf16(vec![0xd800]);
    let result = search(options).unwrap();
    assert_eq!(
        result.get("query"),
        Some(&JsValue::String(JsString::from_utf16(vec![0xd800])))
    );
    assert_eq!(rows(&result).len(), 1);
    assert_eq!(
        rows(&result)[0].get("text"),
        Some(&JsValue::String(JsString::from_utf16(vec![97, 0xd800, 98])))
    );
}
#[test]
fn limit_does_not_skip_late_same_file_read_failure() {
    let fixture = Fixture::new();
    fixture.write("s.jsonl", &format!("{USER}\nnull\n"));
    let mut options = fixture.options();
    options.limit = 1.0;
    let error = search(options).unwrap_err();
    assert!(error.to_string().contains("Cannot read properties of null"));
}
