//! The fixture is the real StaffDeckSopControlPlane -> SopStateStore call,
//! including read-time migration between requests. Physical session identity is
//! a string here; the four Node Buffer.from nonstring session probes are evidence
//! only and are not silently converted into a different native session key.
use super::*;
use crate::module_host::approvals::StoreResumeCommand;
fn array(value: &JsValue) -> &[JsValue] {
    let JsValue::Array(rows) = value else {
        panic!("oracle array")
    };
    rows
}
fn same(actual: &JsValue, expected: &JsValue, name: &str) {
    assert_eq!(
        stringify_json(actual).unwrap(),
        stringify_json(expected).unwrap(),
        "{name}"
    );
}
struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
#[tokio::test]
async fn forty_four_real_control_cases_preserve_resume_order_spread_and_receipts() {
    let oracle = parse_json(include_str!(
        "../../../tests/fixtures/sop-resume-wire/oracle.json"
    ))
    .unwrap();
    assert_eq!(
        prop(&oracle, "baseline"),
        &JsValue::from("cc7c4e653b15c8b2cec7bcf2d15d41d69396ae85")
    );
    assert_eq!(prop(&oracle, "networkAttempts"), &JsValue::Number(0.0));
    let rows = array(prop(&oracle, "cases"));
    assert_eq!(rows.len(), 49);
    let mut count = 0;
    let mut steps = 0;
    for row in rows {
        let name = prop(row, "name").as_string().unwrap().to_utf8().unwrap();
        if matches!(
            name.as_str(),
            "disabled-invalid" | "session-7" | "session-null" | "session-{}" | "session-[\"s1\"]"
        ) {
            continue;
        }
        count += 1;
        let root = std::env::temp_dir().join(format!("pdrs-sop-wire-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&root).unwrap();
        let scratch = Scratch(root);
        let fixed = prop(&oracle, "fixed")
            .as_string()
            .unwrap()
            .to_utf8()
            .unwrap();
        let store = DiskSopStore::with_clock(scratch.0.clone(), Arc::new(move || fixed.clone()));
        let session: JsString = "s1".into();
        let path = store.path(&session);
        if let Some(initial) = row.get("initial") {
            std::fs::write(
                &path,
                format!("{}\n", stringify_json(initial).unwrap().unwrap()),
            )
            .unwrap();
        }
        for step in array(prop(row, "steps")) {
            steps += 1;
            let input = prop(step, "input");
            let before = std::fs::read(&path).ok();
            let result = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                store.resume_command(StoreResumeCommand::wire(session.clone(), input)),
            )
            .await
            .unwrap();
            let expected = prop(step, "outcome");
            if prop(expected, "ok") == &JsValue::Bool(true) {
                same(&result.unwrap(), prop(expected, "result"), &name);
            } else {
                let error = result.unwrap_err();
                assert_eq!(
                    &JsValue::from(error.code.as_str()),
                    prop(expected, "code"),
                    "{name}"
                );
                assert_eq!(
                    &JsValue::from(error.message.as_str()),
                    prop(expected, "message"),
                    "{name}"
                );
            }
            let after = std::fs::read(&path).ok();
            assert_eq!(
                before == after,
                prop(step, "unchanged") == &JsValue::Bool(true),
                "{name}"
            );
            match step.get("after") {
                Some(expected) => same(
                    &parse_json(std::str::from_utf8(after.as_ref().unwrap()).unwrap()).unwrap(),
                    expected,
                    &name,
                ),
                None => assert!(after.is_none(), "{name}"),
            }
        }
    }
    assert_eq!(count, 44);
    assert_eq!(steps, 49);
}
