//! TS RouterSessionCustomRouterPort.ts:29-85. Routers are constructed from
//! the session's retained plugin snapshot, never from the live generation.
use super::{CustomRouter, CustomRouterRegistry, trim_js};
use pilotdeck_protocol::JsString;
use std::sync::{
    Arc, Mutex, Weak,
    atomic::{AtomicBool, Ordering},
};

/// A native `PilotDeckCustomRouter`; its id may differ from its contribution id.
#[derive(Clone)]
pub struct CustomRouterInstance {
    pub id: JsString,
    pub router: Arc<dyn CustomRouter>,
}
impl CustomRouterInstance {
    pub fn new(id: impl Into<JsString>, router: Arc<dyn CustomRouter>) -> Self {
        Self {
            id: id.into(),
            router,
        }
    }
}
impl std::fmt::Debug for CustomRouterInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomRouterInstance")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

struct Registration {
    routers: Vec<(JsString, Arc<dyn CustomRouter>)>,
}
#[derive(Default)]
struct RegistryState {
    registrations: Vec<(JsString, Arc<Registration>)>,
    disposed: bool,
}

/// Project-owned index. Plugin leases, rather than this index, own disposal.
#[derive(Default)]
pub struct SessionCustomRouterRegistry {
    state: Arc<Mutex<RegistryState>>,
}

/// Explicit idempotent release. A retiring session cannot remove its replacement.
pub struct CustomRouterRegistration {
    state: Weak<Mutex<RegistryState>>,
    session: JsString,
    registration: Arc<Registration>,
    released: AtomicBool,
}
impl CustomRouterRegistration {
    pub fn release(&self) {
        if self.released.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(state) = self.state.upgrade() {
            let mut state = state.lock().expect("custom router registry");
            state.registrations.retain(|(session, registration)| {
                session != &self.session || !Arc::ptr_eq(registration, &self.registration)
            });
        }
    }
}

impl SessionCustomRouterRegistry {
    pub fn register_session(
        &self,
        session: JsString,
        routers: Vec<CustomRouterInstance>,
    ) -> Result<CustomRouterRegistration, JsString> {
        let mut state = self.state.lock().expect("custom router registry");
        if state.disposed {
            return Err("Router session custom-router registry is disposed.".into());
        }
        let mut mapped = Vec::with_capacity(routers.len());
        for router in routers {
            let id = trim_js(&router.id);
            if id.is_empty() {
                return Err(message(
                    "Cannot register a custom router with an empty id for session ",
                    &session,
                    ".",
                ));
            }
            if mapped.iter().any(|(key, _)| key == &id) {
                let mut error: Vec<u16> = "Duplicate custom router '".encode_utf16().collect();
                error.extend_from_slice(id.units());
                error.extend("' for session ".encode_utf16());
                error.extend_from_slice(session.units());
                error.push(b'.' as u16);
                return Err(JsString::from_utf16(error));
            }
            mapped.push((id, router.router));
        }
        let registration = Arc::new(Registration { routers: mapped });
        if let Some((_, existing)) = state
            .registrations
            .iter_mut()
            .find(|(id, _)| id == &session)
        {
            *existing = registration.clone();
        } else {
            state
                .registrations
                .push((session.clone(), registration.clone()));
        }
        Ok(CustomRouterRegistration {
            state: Arc::downgrade(&self.state),
            session,
            registration,
            released: AtomicBool::new(false),
        })
    }

    /// Compatibility seam for callers that register one native router.
    pub fn register(&self, extension: JsString, session: JsString, router: Arc<dyn CustomRouter>) {
        let mut state = self.state.lock().expect("custom router registry");
        assert!(
            !state.disposed,
            "Router session custom-router registry is disposed."
        );
        let mut routers = state
            .registrations
            .iter()
            .find(|(id, _)| id == &session)
            .map(|(_, entry)| entry.routers.clone())
            .unwrap_or_default();
        if let Some((_, existing)) = routers.iter_mut().find(|(id, _)| id == &extension) {
            *existing = router;
        } else {
            routers.push((extension, router));
        }
        let registration = Arc::new(Registration { routers });
        if let Some((_, existing)) = state
            .registrations
            .iter_mut()
            .find(|(id, _)| id == &session)
        {
            *existing = registration;
        } else {
            state.registrations.push((session, registration));
        }
    }

    pub fn clear(&self) {
        self.state
            .lock()
            .expect("custom router registry")
            .registrations
            .clear();
    }
    pub fn dispose(&self) {
        let mut state = self.state.lock().expect("custom router registry");
        state.registrations.clear();
        state.disposed = true;
    }
}

impl CustomRouterRegistry for SessionCustomRouterRegistry {
    fn lookup_router(
        &self,
        extension: &JsString,
        session: &JsString,
    ) -> Option<Arc<dyn CustomRouter>> {
        let state = self.state.lock().expect("custom router registry");
        if state.disposed || session.is_empty() {
            return None;
        }
        state
            .registrations
            .iter()
            .find(|(id, _)| id == session)
            .and_then(|(_, entry)| entry.routers.iter().find(|(id, _)| id == extension))
            .map(|(_, router)| router.clone())
    }
}

fn message(prefix: &str, session: &JsString, suffix: &str) -> JsString {
    let mut units: Vec<u16> = prefix.encode_utf16().collect();
    units.extend_from_slice(session.units());
    units.extend(suffix.encode_utf16());
    JsString::from_utf16(units)
}

#[cfg(test)]
#[path = "custom_registry_tests.rs"]
mod tests;
