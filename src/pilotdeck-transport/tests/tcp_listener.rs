//! TCP production listener integration: real sockets and real ownership.
use pilotdeck_agent::ports::PortResult;
use pilotdeck_protocol::admission::SidecarBinding;
use pilotdeck_protocol::module::ModuleOutcome;
use pilotdeck_protocol::{JsObject, JsString, JsValue, parse_json};
use pilotdeck_transport::server::{ExecuteRequest, ExecutionClock, ExecutionContext, ExecutionFactory, ExecutionTerminal};
use pilotdeck_transport::tcp::TcpExecutionServer;
use std::{io::{BufRead, BufReader, Write}, net::{Shutdown, SocketAddr, TcpStream},
    sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, thread, time::{Duration, Instant}};

struct Clock;
impl ExecutionClock for Clock {
    fn now_epoch_ms(&self) -> f64 { 1_000.0 }
    fn now_iso(&self) -> JsString { "1970-01-01T00:00:01.000Z".into() }
    fn parse_date(&self, _: &JsString) -> Result<Option<f64>, JsString> { Ok(None) }
}
#[derive(Default)]
struct Factory {
    entered: Mutex<Vec<String>>,
    release: AtomicBool,
}
impl ExecutionFactory for Factory {
    fn execute(&self, request: &ExecuteRequest, ctx: &mut ExecutionContext) -> PortResult<ExecutionTerminal> {
        self.entered.lock().unwrap().push(request.operation_id.to_utf8().unwrap());
        while !self.release.load(Ordering::SeqCst) {
            ctx.control.check()?;
            thread::sleep(Duration::from_millis(5));
        }
        ctx.control.check()?;
        Ok(ExecutionTerminal { outcome: ModuleOutcome::Completed, payload: JsObject::new(), error: None })
    }
}
struct Harness {
    address: SocketAddr,
    stop: Arc<AtomicBool>,
    factory: Arc<Factory>,
    worker: Option<thread::JoinHandle<std::io::Result<()>>>,
}
impl Harness {
    fn new() -> Self {
        let factory = Arc::new(Factory::default());
        let binding = SidecarBinding {
            module_id: "pilotdeck-agent-loop".into(), module_instance_id: "fixture-instance".into(),
            connection_generation: "fixture-base".into(), capabilities_version: "1".into(),
        };
        let server = TcpExecutionServer::bind(("127.0.0.1", 0), binding, factory.clone(), Arc::new(Clock)).unwrap();
        let address = server.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let worker = thread::spawn(move || server.serve(signal, Duration::from_secs(2)));
        Self { address, stop, factory, worker: Some(worker) }
    }
    fn connect(&self) -> TcpStream {
        let socket = TcpStream::connect(self.address).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(4))).unwrap();
        socket.set_write_timeout(Some(Duration::from_secs(4))).unwrap();
        socket
    }
    fn wait_entered(&self, id: &str) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while !self.factory.entered.lock().unwrap().iter().any(|item| item == id) {
            assert!(Instant::now() < deadline, "factory never entered {id}");
            thread::sleep(Duration::from_millis(5));
        }
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.factory.release.store(true, Ordering::SeqCst);
        self.stop.store(true, Ordering::SeqCst);
        if let Some(worker) = self.worker.take() { assert!(worker.join().unwrap().is_ok()); }
    }
}
fn send(socket: &mut TcpStream, text: &str) { writeln!(socket, "{text}").unwrap(); }
fn read(socket: &TcpStream) -> JsValue {
    let mut reader = BufReader::with_capacity(1, socket);
    let mut line = String::new();
    assert_ne!(reader.read_line(&mut line).unwrap(), 0, "unexpected socket EOF");
    parse_json(&line).unwrap()
}
fn get<'a>(value: &'a JsValue, key: &str) -> &'a JsValue { value.get(key).unwrap() }
fn response(socket: &TcpStream, id: &str) -> JsValue {
    for _ in 0..40 {
        let value = read(socket);
        if get(&value, "kind") == &JsValue::from("response")
            && value.get("inReplyTo") == Some(&JsValue::from(id)) { return value; }
    }
    panic!("response not observed: {id}")
}
fn execute(tag: &str) -> String {
    format!(r#"{{"kind":"request","messageId":"execute-{tag}","method":"execute","runId":"run-{tag}","operationId":"op-{tag}","requestId":"request-{tag}","payload":{{}}}}"#)
}
fn status(socket: &mut TcpStream, id: &str, tag: &str) -> JsValue {
    send(socket, &format!(r#"{{"kind":"request","messageId":"{id}","method":"status","requestId":"request-{tag}"}}"#));
    response(socket, id)
}
fn hello(socket: &mut TcpStream, id: &str) -> JsValue {
    send(socket, &format!(r#"{{"kind":"request","messageId":"{id}","method":"hello","payload":{{}}}}"#));
    response(socket, id)
}

#[test]
fn tcp_owners_share_status_and_cancel_without_cross_retirement() {
    let h = Harness::new();
    let mut a = h.connect();
    let mut b = h.connect();
    let ha = hello(&mut a, "hello-a");
    let hb = hello(&mut b, "hello-b");
    assert_eq!(get(&ha, "moduleInstanceId"), get(&hb, "moduleInstanceId"));
    assert_ne!(get(&ha, "connectionGeneration"), get(&hb, "connectionGeneration"));
    send(&mut a, r#"{"kind":"request","messageId":"caps","method":"capabilities","payload":{}}"#);
    let caps = response(&a, "caps");
    let JsValue::Array(methods) = get(get(&caps, "payload"), "methods") else { panic!("methods"); };
    let execute_capability = methods.iter().find(|m| m.get("name") == Some(&JsValue::from("execute"))).unwrap();
    assert_eq!(get(execute_capability, "resumeSupport"), &JsValue::from("none"));

    send(&mut a, &execute("a"));
    assert_eq!(get(&response(&a, "execute-a"), "ok"), &JsValue::Bool(true));
    h.wait_entered("op-a");
    send(&mut b, &execute("b"));
    assert_eq!(get(&response(&b, "execute-b"), "ok"), &JsValue::Bool(true));
    h.wait_entered("op-b");
    // Same request ID, same bytes, same active connection: fixed TS rejects it.
    send(&mut a, &execute("a"));
    assert_eq!(get(&response(&a, "execute-a"), "code"), &JsValue::from("OPERATION_ALREADY_ACTIVE"));
    assert_eq!(h.factory.entered.lock().unwrap().iter().filter(|v| *v == "op-a").count(), 1);
    send(&mut b, &execute("a"));
    assert_eq!(get(&response(&b, "execute-a"), "code"), &JsValue::from("OPERATION_ALREADY_ACTIVE"));
    assert_eq!(h.factory.entered.lock().unwrap().iter().filter(|v| *v == "op-a").count(), 1);

    let cross = status(&mut b, "status-a", "a");
    assert_eq!(get(get(&cross, "payload"), "state"), &JsValue::from("running"));
    send(&mut b, r#"{"kind":"request","messageId":"cancel-a","method":"cancel","runId":"run-a","operationId":"op-a","requestId":"request-a","reason":"test"}"#);
    assert_eq!(get(&response(&b, "cancel-a"), "ok"), &JsValue::Bool(true));
    assert_eq!(get(get(&status(&mut b, "status-b", "b"), "payload"), "state"), &JsValue::from("running"));
    h.factory.release.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let value = status(&mut b, "finished-b", "b");
        if get(get(&value, "payload"), "state") == &JsValue::from("completed") { break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    a.shutdown(Shutdown::Both).unwrap();
    let mut later = h.connect();
    assert_eq!(get(&hello(&mut later, "later"), "ok"), &JsValue::Bool(true));
}

#[test]
fn tcp_disconnect_marks_only_its_operation_unknown_and_listener_survives() {
    let h = Harness::new();
    let mut a = h.connect();
    let mut b = h.connect();
    send(&mut a, &execute("a"));
    assert_eq!(get(&response(&a, "execute-a"), "ok"), &JsValue::Bool(true));
    h.wait_entered("op-a");
    send(&mut b, &execute("b"));
    assert_eq!(get(&response(&b, "execute-b"), "ok"), &JsValue::Bool(true));
    h.wait_entered("op-b");
    a.shutdown(Shutdown::Both).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let s = status(&mut b, "a-after-close", "a");
        if get(&s, "payload").get("outcome") == Some(&JsValue::from("result_unknown")) { break; }
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(get(get(&status(&mut b, "b-still-live", "b"), "payload"), "state"), &JsValue::from("running"));
    let mut later = h.connect();
    assert_eq!(get(&hello(&mut later, "later"), "ok"), &JsValue::Bool(true));
}

#[test]
fn tcp_oversize_input_does_not_stop_other_connections() {
    let h = Harness::new();
    let mut malformed = h.connect();
    let mut healthy = h.connect();
    assert_eq!(get(&hello(&mut healthy, "before"), "ok"), &JsValue::Bool(true));
    // TCP native input budget: one line over 8 MiB must be refused.
    let data = vec![b'x'; 8 * 1024 * 1024 + 1];
    let _ = malformed.write_all(&data);
    let _ = malformed.write_all(b"\n");
    let deadline = Instant::now() + Duration::from_secs(4);
    loop {
        let mut byte = [0u8; 1];
        match std::io::Read::read(&mut malformed, &mut byte) {
            Ok(0) => break,
            Err(error) if matches!(error.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted | std::io::ErrorKind::BrokenPipe) => break,
            Err(error) => panic!("oversized socket did not close: {error}"),
            Ok(_) => {}
        }
        assert!(Instant::now() < deadline, "oversized socket never terminated");
    }
    assert_eq!(get(&hello(&mut healthy, "after"), "ok"), &JsValue::Bool(true));
}
