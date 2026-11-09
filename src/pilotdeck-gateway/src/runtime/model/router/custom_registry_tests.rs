use super::*;
use crate::runtime::model::router::RouterFuture;
use pilotdeck_protocol::JsValue;

struct Router;
impl CustomRouter for Router {
    fn decide(&self, _input: JsValue) -> RouterFuture<JsValue> {
        Box::pin(async { Ok(JsValue::Undefined) })
    }
}
fn router(id: impl Into<JsString>) -> CustomRouterInstance {
    CustomRouterInstance::new(id, Arc::new(Router))
}

#[test]
fn replacement_release_is_idempotent_and_cannot_remove_the_new_session_generation() {
    let registry = SessionCustomRouterRegistry::default();
    let old = router(" extension ");
    let replacement = router("extension");
    let old_registration = registry
        .register_session("s".into(), vec![old.clone()])
        .unwrap();
    assert!(Arc::ptr_eq(
        &registry
            .lookup_router(&"extension".into(), &"s".into())
            .unwrap(),
        &old.router
    ));
    assert!(
        registry
            .lookup_router(&"extension".into(), &"other".into())
            .is_none()
    );
    let new_registration = registry
        .register_session("s".into(), vec![replacement.clone()])
        .unwrap();
    old_registration.release();
    old_registration.release();
    assert!(Arc::ptr_eq(
        &registry
            .lookup_router(&"extension".into(), &"s".into())
            .unwrap(),
        &replacement.router
    ));
    new_registration.release();
    assert!(
        registry
            .lookup_router(&"extension".into(), &"s".into())
            .is_none()
    );
}

#[test]
fn invalid_batch_preserves_existing_mapping_and_error_keeps_raw_utf16() {
    let registry = SessionCustomRouterRegistry::default();
    let session = JsString::from_utf16(vec![0xd800]);
    registry
        .register_session(session.clone(), vec![router("original")])
        .unwrap();
    let duplicate = JsString::from_utf16(vec![0xd801]);
    let mut padded = vec![0xfeff, 0xd801, 0x20];
    let error = registry
        .register_session(
            session.clone(),
            vec![
                router(duplicate.clone()),
                router(JsString::from_utf16(std::mem::take(&mut padded))),
            ],
        )
        .err()
        .unwrap();
    let mut expected: Vec<u16> = "Duplicate custom router '".encode_utf16().collect();
    expected.push(0xd801);
    expected.extend("' for session ".encode_utf16());
    expected.extend([0xd800, b'.' as u16]);
    assert_eq!(error.units(), expected);
    assert!(
        registry
            .lookup_router(&"original".into(), &session)
            .is_some()
    );
    let error = registry
        .register_session(session.clone(), vec![router("\u{feff}\u{2028}")])
        .err()
        .unwrap();
    assert_eq!(
        error,
        message(
            "Cannot register a custom router with an empty id for session ",
            &session,
            "."
        )
    );
    assert!(
        registry
            .lookup_router(&"original".into(), &session)
            .is_some()
    );
}

#[test]
fn empty_batch_replaces_session_clear_remains_active_dispose_closes_admission() {
    let registry = SessionCustomRouterRegistry::default();
    registry
        .register_session("s".into(), vec![router("r")])
        .unwrap();
    registry.register_session("s".into(), vec![]).unwrap();
    assert!(registry.lookup_router(&"r".into(), &"s".into()).is_none());
    registry.clear();
    registry
        .register_session("s".into(), vec![router("r")])
        .unwrap();
    registry.dispose();
    registry.dispose();
    assert!(registry.lookup_router(&"r".into(), &"s".into()).is_none());
    assert_eq!(
        registry
            .register_session("late".into(), vec![router("r")])
            .err()
            .unwrap(),
        "Router session custom-router registry is disposed.".into()
    );
    // TS lookupRouter treats an empty sessionId as absent.
    let active = SessionCustomRouterRegistry::default();
    active
        .register_session("".into(), vec![router("r")])
        .unwrap();
    assert!(active.lookup_router(&"r".into(), &"".into()).is_none());
}
