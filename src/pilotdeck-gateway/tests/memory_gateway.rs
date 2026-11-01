//! #1214: replay of the real-TS memory-gateway fixture. The committed
//! `oracle.json` runs the real `InProcessGateway` wrapper validation
//! (`InProcessGateway.ts:2794-2808`) over the unmodified
//! `createLocalGateway.ts` method bodies with recording seams; this test
//! replays every cell through the production `MemoryGateway` adapter
//! (`runtime/memory/gateway.rs`) and pins the wire result, the wire error and
//! the seam call order, cell by cell.
#[path = "common/baseline.rs"]
mod baseline;

use pilotdeck_gateway::runtime::memory::{
    MemoryManagementService, MemoryProjection, MemoryProviderError, ProjectMemoryRegistry,
    gateway::{MemoryDispatch, MemoryGateway},
};
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use std::sync::{Arc, Mutex};

const ORACLE: &str = include_str!("fixtures/memory-gateway/oracle.json");
const FALLBACK: &str = "/pdrs-fixture/fallback";

fn as_array(value: &JsValue) -> Option<&Vec<JsValue>> {
    match value {
        JsValue::Array(items) => Some(items),
        _ => None,
    }
}
fn text(value: Option<&JsValue>) -> Option<String> {
    value
        .and_then(JsValue::as_string)
        .and_then(|value| value.to_utf8().ok())
}
fn call_object(op: &str) -> JsObject {
    let mut object = JsObject::new();
    object.insert("op".into(), JsValue::from(op));
    object
}

struct GatewayRegistry {
    calls: Arc<Mutex<Vec<JsValue>>>,
    management: Option<Arc<dyn MemoryManagementService>>,
    /// `knownProjects` from the fixture: mimic the real registry's unit-exact
    /// match and `Unknown projectKey` rejection. `None` keeps the older mock's
    /// `resolved:<key>` echo.
    known_projects: Option<Vec<JsString>>,
}
impl ProjectMemoryRegistry for GatewayRegistry {
    fn resolve_project_key(&self, project_key: &JsString) -> Result<JsString, MemoryProviderError> {
        let mut call = call_object("resolveProjectKey");
        call.insert("key".into(), JsValue::String(project_key.clone()));
        self.calls
            .lock()
            .expect("#1214 calls mutex")
            .push(JsValue::Object(call));
        match &self.known_projects {
            Some(known) => match known
                .iter()
                .find(|known| known.units() == project_key.units())
            {
                Some(known) => Ok(known.clone()),
                None => Err(MemoryProviderError::unknown_project_key(
                    project_key.units(),
                )),
            },
            None => {
                let mut units = "resolved:".encode_utf16().collect::<Vec<_>>();
                units.extend_from_slice(project_key.units());
                Ok(JsString::from_utf16(units))
            }
        }
    }
    fn memory_management(
        &self,
        project_key: &JsString,
    ) -> Result<Option<Arc<dyn MemoryManagementService>>, MemoryProviderError> {
        let mut call = call_object("resolve");
        call.insert("key".into(), JsValue::String(project_key.clone()));
        self.calls
            .lock()
            .expect("#1214 calls mutex")
            .push(JsValue::Object(call));
        Ok(self.management.clone())
    }
}

struct GatewayManagement {
    calls: Arc<Mutex<Vec<JsValue>>>,
    items: Vec<JsValue>,
}
impl MemoryManagementService for GatewayManagement {
    fn list(&self, limit: usize) -> Result<Vec<JsValue>, MemoryProviderError> {
        let mut call = call_object("list");
        call.insert("limit".into(), JsValue::Number(limit as f64));
        self.calls
            .lock()
            .expect("#1214 calls mutex")
            .push(JsValue::Object(call));
        Ok(self.items.clone())
    }
    fn clear(&self, scope: &str) -> Result<(), MemoryProviderError> {
        let mut call = call_object("clear");
        call.insert("scope".into(), JsValue::from(scope));
        self.calls
            .lock()
            .expect("#1214 calls mutex")
            .push(JsValue::Object(call));
        Ok(())
    }
    fn clear_session(&self, session_key: &JsString) -> Result<(), MemoryProviderError> {
        let mut call = call_object("clearSession");
        call.insert("sessionKey".into(), JsValue::String(session_key.clone()));
        self.calls
            .lock()
            .expect("#1214 calls mutex")
            .push(JsValue::Object(call));
        Ok(())
    }
}

#[test]
fn gateway_replays_real_ts_wrapper_oracle() {
    let oracle = baseline::oracle(ORACLE);
    let cases = oracle
        .get("gatewayCases")
        .and_then(as_array)
        .expect("#1214 gatewayCases must be an array");
    assert_eq!(
        cases.len(),
        10,
        "#1214 the committed gateway case corpus is pinned"
    );
    let mut replayed_steps = 0;
    for case in cases {
        let name = text(case.get("name")).expect("#1214 each case is named");
        let capability =
            text(case.get("capability")).expect("#1214 each case records its capability");
        let calls: Arc<Mutex<Vec<JsValue>>> = Arc::new(Mutex::new(Vec::new()));
        let management: Option<Arc<dyn MemoryManagementService>> =
            if text(case.get("management")).as_deref() == Some("available") {
                Some(Arc::new(GatewayManagement {
                    calls: Arc::clone(&calls),
                    items: case
                        .get("items")
                        .and_then(as_array)
                        .expect("#1214 each available case records its items")
                        .clone(),
                }))
            } else {
                None
            };
        let registry = Arc::new(GatewayRegistry {
            calls: Arc::clone(&calls),
            management,
            known_projects: case.get("knownProjects").map(|projects| {
                as_array(projects)
                    .expect("#1214 knownProjects must be an array")
                    .iter()
                    .map(|project| {
                        project
                            .as_string()
                            .expect("#1214 knownProjects entries are strings")
                            .clone()
                    })
                    .collect::<Vec<_>>()
            }),
        });
        let gateway = if capability == "available" {
            MemoryGateway::new(
                Arc::clone(&registry) as Arc<dyn ProjectMemoryRegistry>,
                FALLBACK,
            )
        } else {
            MemoryGateway::disabled()
        };
        let steps = case
            .get("steps")
            .and_then(as_array)
            .expect("#1214 each case carries steps");
        for (i, step) in steps.iter().enumerate() {
            registry.calls.lock().expect("#1214 calls mutex").clear();
            let op = text(step.get("op")).expect("#1214 each step records its op");
            let method = match op.as_str() {
                "memoryList" => "memory_list",
                "memoryWipe" => "memory_wipe",
                other => panic!("#1214 {name} step {i}: unknown op {other:?}"),
            };
            let params_kind =
                text(step.get("params")).expect("#1214 each step records its params kind");
            let params = match params_kind.as_str() {
                "undefined" => JsValue::Undefined,
                "null" => JsValue::Null,
                _ => step
                    .get("input")
                    .cloned()
                    .expect("#1214 each step records its input"),
            };
            let dispatch = gateway
                .dispatch(method, &params)
                .unwrap_or_else(|| panic!("#1214 {name} step {i}: {method} must dispatch locally"));
            let actual = match dispatch {
                MemoryDispatch::Response(value) => value,
                MemoryDispatch::Forward => {
                    panic!("#1214 {name} step {i}: this corpus never hits a host boundary")
                }
            };
            if let Some(result) = step.get("resultJson") {
                let expected = parse_json(
                    text(Some(result))
                        .expect("#1214 resultJson is a string")
                        .as_str(),
                )
                .expect("#1214 resultJson must parse");
                assert_eq!(
                    actual, expected,
                    "#1214 {name} step {i} ({method}): wire result must match the TS fixture"
                );
            } else {
                let expected = step
                    .get("error")
                    .expect("#1214 each step records a result or an error")
                    .clone();
                assert_eq!(
                    actual, expected,
                    "#1214 {name} step {i} ({method}): wire error must match the TS fixture"
                );
            }
            let recorded =
                JsValue::Array(registry.calls.lock().expect("#1214 calls mutex").clone());
            let expected_calls = step
                .get("calls")
                .expect("#1214 each step records its seam calls")
                .clone();
            assert_eq!(
                recorded, expected_calls,
                "#1214 {name} step {i} ({method}): seam calls must match the TS fixture"
            );
            replayed_steps += 1;
        }
    }
    assert_eq!(
        replayed_steps, 74,
        "#1214 the committed gateway step corpus is pinned"
    );
}

#[test]
#[ignore = "#280 run with PDRS_BASELINE; regenerates the committed oracle byte-for-byte"]
fn memory_gateway_fixture_regenerates_identically() {
    baseline::regenerates_identically("memory-gateway");
}

/// A registry whose provider hits the R19 host boundary: every projection
/// outcome must surface as `Forward`, not a synthesized error response.
struct HostBoundaryRegistry;
impl ProjectMemoryRegistry for HostBoundaryRegistry {
    fn resolve_project_key(
        &self,
        _project_key: &JsString,
    ) -> Result<JsString, MemoryProviderError> {
        Err(MemoryProviderError::host_required())
    }
    fn memory_management(
        &self,
        _project_key: &JsString,
    ) -> Result<Option<Arc<dyn MemoryManagementService>>, MemoryProviderError> {
        Ok(None)
    }
}

#[test]
fn dispatch_forwards_requires_host_boundary() {
    let gateway = MemoryGateway::new(
        Arc::new(HostBoundaryRegistry) as Arc<dyn ProjectMemoryRegistry>,
        FALLBACK,
    );
    let list = gateway.dispatch(
        "memory_list",
        &parse_json(r#"{"projectKey":"/p"}"#).expect("#1214 Forward test JSON must parse"),
    );
    assert_eq!(
        list,
        Some(MemoryDispatch::Forward),
        "#1214 memory_list must forward a requires_host boundary to the Node host"
    );
    let wipe = gateway.dispatch(
        "memory_wipe",
        &parse_json(r#"{"projectKey":"/p"}"#).expect("#1214 Forward test JSON must parse"),
    );
    assert_eq!(
        wipe,
        Some(MemoryDispatch::Forward),
        "#1214 memory_wipe must forward a requires_host boundary to the Node host"
    );
}

#[test]
fn from_projection_serves_both_methods() {
    let calls: Arc<Mutex<Vec<JsValue>>> = Arc::new(Mutex::new(Vec::new()));
    let projection = MemoryProjection::new(
        Arc::new(GatewayRegistry {
            calls: Arc::clone(&calls),
            management: Some(Arc::new(GatewayManagement {
                calls: Arc::clone(&calls),
                items: Vec::new(),
            })),
            known_projects: None,
        }) as Arc<dyn ProjectMemoryRegistry>,
        FALLBACK,
    );
    let gateway = MemoryGateway::from_projection(projection);
    let list = gateway.dispatch(
        "memory_list",
        &parse_json(r#"{"projectKey":"/p"}"#).expect("#1214 from_projection JSON must parse"),
    );
    assert_eq!(
        list,
        Some(MemoryDispatch::Response(
            parse_json(r#"{"items":[]}"#).expect("#1214 from_projection expected must parse")
        )),
        "#1214 from_projection must serve memory_list through the wired projection"
    );
    let wipe = gateway.dispatch(
        "memory_wipe",
        &parse_json(r#"{"projectKey":"/p"}"#).expect("#1214 from_projection JSON must parse"),
    );
    assert_eq!(
        wipe,
        Some(MemoryDispatch::Response(
            parse_json(r#"{"wiped":true,"scope":"project"}"#)
                .expect("#1214 from_projection expected must parse")
        )),
        "#1214 from_projection must serve memory_wipe through the wired projection"
    );
}
