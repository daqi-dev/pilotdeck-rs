//! Long-lived listener; each socket retains its own host-call authority.
use crate::{
    server::{ExecutionClock, ExecutionFactory, ExecutionServer, SharedExecutionLedger},
    stdio::ConnectionOwner,
};
use pilotdeck_protocol::admission::SidecarBinding;
use std::{
    io::{self, BufReader},
    net::{Shutdown, SocketAddr, TcpListener, TcpStream, ToSocketAddrs},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

const MAX_CONNECTIONS: usize = 32;
/// TS sidecar optionalPort: JS trim/Number, including exponent and radix forms.
pub fn optional_port(value: Option<&str>) -> Result<Option<u16>, &'static str> {
    let Some(value) = value
        .map(|v| v.trim_matches(crate::server::js_whitespace))
        .filter(|v| !v.is_empty())
    else {
        return Ok(None);
    };
    let radix = [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ]
    .into_iter()
    .find_map(|(prefix, radix)| value.strip_prefix(prefix).map(|digits| (digits, radix)));
    let number = if let Some((digits, radix)) = radix {
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            f64::NAN
        } else {
            u32::from_str_radix(digits.trim_start_matches('0'), radix)
                .map(f64::from)
                .unwrap_or(f64::NAN)
        }
    } else {
        value.parse::<f64>().unwrap_or(f64::NAN)
    };
    if number.is_finite() && number.fract() == 0.0 && (1.0..=65535.0).contains(&number) {
        Ok(Some(number as u16))
    } else {
        Err("PILOTDECK_AGENT_LOOP_TCP_PORT must be an integer between 1 and 65535.")
    }
}

pub fn listen_host(value: Option<&str>) -> &str {
    value
        .map(|v| v.trim_matches(crate::server::js_whitespace))
        .filter(|v| !v.is_empty())
        .unwrap_or("127.0.0.1")
}
// Only known transient listener failures are retried. Unknown or permanent
// failures must return to the supervisor rather than loop as a dead listener.
fn retryable_accept_error(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::OutOfMemory
            | io::ErrorKind::NetworkDown
            | io::ErrorKind::NetworkUnreachable
            | io::ErrorKind::HostUnreachable
    ) {
        return true;
    }
    // POSIX resource pressure; use this target's actual errno constants.
    #[cfg(unix)]
    if matches!(
        error.raw_os_error(),
        Some(libc::ENOMEM | libc::ENFILE | libc::EMFILE | libc::ENOBUFS)
    ) {
        return true;
    }
    false
}

struct ConnectionTask {
    socket: TcpStream,
    server: Arc<ExecutionServer>,
    worker: thread::JoinHandle<bool>,
}
/// A listener owns accepted sockets, never the connecting gateway process.
pub struct TcpExecutionServer {
    listener: TcpListener,
    binding: SidecarBinding,
    factory: Arc<dyn ExecutionFactory>,
    clock: Arc<dyn ExecutionClock>,
    ledger: SharedExecutionLedger,
    budget: Arc<crate::server::TransportBudget>,
    #[cfg(test)]
    panic_closer: bool,
    #[cfg(test)]
    fail_after_clones: bool,
}
impl TcpExecutionServer {
    pub fn bind(
        address: impl ToSocketAddrs,
        binding: SidecarBinding,
        factory: Arc<dyn ExecutionFactory>,
        clock: Arc<dyn ExecutionClock>,
    ) -> io::Result<Self> {
        let listener = TcpListener::bind(address)?;
        listener.set_nonblocking(true)?;
        Ok(Self {
            listener,
            binding,
            factory,
            clock,
            ledger: SharedExecutionLedger::default(),
            budget: Arc::default(),
            #[cfg(test)]
            panic_closer: false,
            #[cfg(test)]
            fail_after_clones: false,
        })
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// The process supervisor must exit if any connection cannot be reaped.
    /// Closing one socket never retires another socket's operations or listener.
    pub fn serve(self, stop: Arc<AtomicBool>, grace: Duration) -> io::Result<()> {
        self.serve_accept(stop, grace, TcpListener::accept)
    }
    fn serve_accept(
        self,
        stop: Arc<AtomicBool>,
        grace: Duration,
        mut accept: impl FnMut(&TcpListener) -> io::Result<(TcpStream, SocketAddr)>,
    ) -> io::Result<()> {
        let mut tasks: Vec<ConnectionTask> = Vec::new();
        let mut failure = None;
        while !stop.load(Ordering::SeqCst) {
            let mut index = 0;
            while index < tasks.len() {
                if tasks[index].worker.is_finished() {
                    let task = tasks.swap_remove(index);
                    if !task.worker.join().unwrap_or(false) {
                        failure = Some(io::Error::other(
                            "TCP sidecar connection cleanup was incomplete.",
                        ));
                        break;
                    }
                } else {
                    index += 1;
                }
            }
            if failure.is_some() {
                break;
            }
            match accept(&self.listener) {
                Ok((socket, _)) => {
                    if tasks.len() >= MAX_CONNECTIONS {
                        let _ = socket.shutdown(Shutdown::Both);
                        continue;
                    }
                    match self.connection(socket, grace) {
                        Ok(task) => tasks.push(task),
                        // A failed socket setup never acquired execution
                        // authority. Its owned descriptors drop here; retain
                        // every already admitted connection.
                        Err(_) => thread::sleep(Duration::from_millis(25)),
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                // Aborted pending peers and OS descriptor/resource pressure
                // are retryable accept failures, with bounded backoff.
                Err(error) if retryable_accept_error(&error) => {
                    thread::sleep(Duration::from_millis(25));
                }
                Err(error) => {
                    failure = Some(error);
                    break;
                }
            }
        }
        drop(self.listener);
        for task in &tasks {
            let _ = task.socket.shutdown(Shutdown::Both);
            task.server.retire();
        }
        let deadline = Instant::now() + grace + Duration::from_millis(100);
        for task in tasks {
            while !task.worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            if !task.worker.is_finished() || !task.worker.join().unwrap_or(false) {
                failure.get_or_insert_with(|| {
                    io::Error::other("TCP sidecar shutdown was incomplete.")
                });
            }
        }
        failure.map_or(Ok(()), Err)
    }
    fn connection(&self, socket: TcpStream, grace: Duration) -> io::Result<ConnectionTask> {
        // BSD accept inherits the listener's O_NONBLOCK; each connection uses
        // independent blocking reader/writer threads after nonblocking accept.
        socket.set_nonblocking(false)?;
        socket.set_nodelay(true)?;
        socket.set_write_timeout(Some(grace))?;
        let input = socket.try_clone()?;
        let output = socket.try_clone()?;
        let shutdown = socket.try_clone()?;
        #[cfg(test)]
        if self.fail_after_clones {
            return Err(io::Error::other("injected setup failure after socket clones"));
        }
        let mut binding = self.binding.clone();
        binding.connection_generation = format!(
            "{}-{}",
            String::from_utf16_lossy(binding.connection_generation.units()),
            uuid::Uuid::new_v4()
        )
        .into();
        let server = ExecutionServer::with_shared_ledger(
            ConnectionOwner::with_budget(binding, 64, self.budget.clone()),
            self.factory.clone(),
            self.clock.clone(),
            32,
            self.ledger.clone(),
        );
        let serving = server.clone();
        #[cfg(test)]
        let panic_closer = self.panic_closer;
        // An output failure must interrupt the independent blocking reader too.
        let worker = match thread::Builder::new()
            .name("sidecar-tcp".into())
            .spawn(move || {
                let watcher = serving.clone();
                let closer = match thread::Builder::new()
                    .name("sidecar-tcp-close".into())
                    .spawn(move || {
                        while !watcher.is_retired() {
                            thread::sleep(Duration::from_millis(5));
                        }
                        #[cfg(test)]
                        if panic_closer {
                            panic!("injected closer panic");
                        }
                        let _ = shutdown.shutdown(Shutdown::Both);
                    }) {
                    Ok(closer) => closer,
                    Err(_) => {
                        serving.retire();
                        return false;
                    }
                };
                let report = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    serving.serve(BufReader::new(input), output, grace)
                }));
                serving.retire();
                let closed = closer.join().is_ok();
                closed && report.is_ok_and(|report| report.complete())
            }) {
            Ok(worker) => worker,
            Err(error) => {
                server.retire();
                let _ = socket.shutdown(Shutdown::Both);
                return Err(error);
            }
        };
        Ok(ConnectionTask {
            socket,
            server,
            worker,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn transient_accept_errors_preserve_the_real_listener() {
        use crate::server::{ExecuteRequest, ExecutionContext, ExecutionTerminal};
        use pilotdeck_protocol::{JsString, JsValue};
        use std::io::{BufRead, Write};
        struct Fixture;
        impl ExecutionClock for Fixture {
            fn now_epoch_ms(&self) -> f64 {
                1000.0
            }
            fn now_iso(&self) -> JsString {
                "1970-01-01T00:00:01.000Z".into()
            }
            fn parse_date(&self, _: &JsString) -> Result<Option<f64>, JsString> {
                Ok(None)
            }
        }
        impl ExecutionFactory for Fixture {
            fn execute(
                &self,
                _: &ExecuteRequest,
                _: &mut ExecutionContext,
            ) -> pilotdeck_agent::ports::PortResult<ExecutionTerminal> {
                unreachable!("hello cannot execute a model")
            }
        }
        let binding = SidecarBinding {
            module_id: "test".into(),
            module_instance_id: "instance".into(),
            connection_generation: "base".into(),
            capabilities_version: "1".into(),
        };
        let listener = TcpExecutionServer::bind(
            ("127.0.0.1", 0),
            binding,
            Arc::new(Fixture),
            Arc::new(Fixture),
        )
        .unwrap();
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let worker = thread::spawn(move || {
            let mut attempts = 0;
            listener.serve_accept(signal, Duration::from_secs(1), move |listener| {
                attempts += 1;
                match attempts {
                    1 => Err(io::Error::from(io::ErrorKind::ConnectionAborted)),
                    2 => Err(io::Error::from_raw_os_error(24)), // POSIX EMFILE
                    _ => listener.accept(),
                }
            })
        });
        let observed = (|| -> io::Result<JsValue> {
            let mut socket = TcpStream::connect(address)?;
            socket.set_read_timeout(Some(Duration::from_secs(2)))?;
            socket.set_write_timeout(Some(Duration::from_secs(2)))?;
            writeln!(
                socket,
                "{{\"kind\":\"request\",\"messageId\":\"after-errors\",\"method\":\"hello\",\"payload\":{{}}}}"
            )?;
            let mut text = String::new();
            BufReader::new(&socket).read_line(&mut text)?;
            pilotdeck_protocol::parse_json(&text).map_err(|e| io::Error::other(e.to_string()))
        })();
        stop.store(true, Ordering::SeqCst);
        assert!(worker.join().unwrap().is_ok());
        assert_eq!(observed.unwrap().get("ok"), Some(&JsValue::Bool(true)));
    }

    #[cfg(unix)]
    #[test]
    fn fatal_accept_errors_return_to_supervisor_without_retry() {
        use crate::server::{ExecuteRequest, ExecutionContext, ExecutionTerminal};
        use pilotdeck_protocol::JsString;
        use std::sync::atomic::AtomicUsize;
        struct Fixture;
        impl ExecutionClock for Fixture {
            fn now_epoch_ms(&self) -> f64 { 1000.0 }
            fn now_iso(&self) -> JsString { "1970-01-01T00:00:01.000Z".into() }
            fn parse_date(&self, _: &JsString) -> Result<Option<f64>, JsString> { Ok(None) }
        }
        impl ExecutionFactory for Fixture {
            fn execute(&self, _: &ExecuteRequest, _: &mut ExecutionContext)
                -> pilotdeck_agent::ports::PortResult<ExecutionTerminal> {
                unreachable!("accept error cannot execute")
            }
        }
        for errno in [libc::EPERM, libc::EBADF, libc::EACCES, libc::EFAULT, libc::EINVAL] {
            let binding = SidecarBinding {
                module_id: "test".into(), module_instance_id: "instance".into(),
                connection_generation: "base".into(), capabilities_version: "1".into(),
            };
            let listener = TcpExecutionServer::bind(("127.0.0.1", 0), binding,
                Arc::new(Fixture), Arc::new(Fixture)).unwrap();
            let stop = Arc::new(AtomicBool::new(false));
            let attempts = Arc::new(AtomicUsize::new(0));
            let worker_stop = stop.clone();
            let worker_attempts = attempts.clone();
            let worker = thread::spawn(move || listener.serve_accept(
                worker_stop, Duration::from_secs(1), move |_| {
                    worker_attempts.fetch_add(1, Ordering::SeqCst);
                    Err(io::Error::from_raw_os_error(errno))
                }
            ));
            // A broken retry loop cannot hang the test: stop it, then require Err.
            let deadline = Instant::now() + Duration::from_secs(1);
            while !worker.is_finished() && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(5));
            }
            stop.store(true, Ordering::SeqCst);
            assert_eq!(worker.join().unwrap().unwrap_err().raw_os_error(), Some(errno));
            assert_eq!(attempts.load(Ordering::SeqCst), 1, "errno {errno}");
        }
    }

    #[test]
    fn failed_setup_drops_every_socket_clone_and_keeps_listener() {
        use crate::server::{ExecuteRequest, ExecutionContext, ExecutionTerminal};
        use pilotdeck_protocol::JsString;
        use std::io::Read;
        struct Fixture;
        impl ExecutionClock for Fixture {
            fn now_epoch_ms(&self) -> f64 { 1000.0 }
            fn now_iso(&self) -> JsString { "1970-01-01T00:00:01.000Z".into() }
            fn parse_date(&self, _: &JsString) -> Result<Option<f64>, JsString> { Ok(None) }
        }
        impl ExecutionFactory for Fixture {
            fn execute(&self, _: &ExecuteRequest, _: &mut ExecutionContext)
                -> pilotdeck_agent::ports::PortResult<ExecutionTerminal> {
                unreachable!("failed setup cannot execute")
            }
        }
        let binding = SidecarBinding {
            module_id: "test".into(), module_instance_id: "instance".into(),
            connection_generation: "base".into(), capabilities_version: "1".into(),
        };
        let mut listener = TcpExecutionServer::bind(("127.0.0.1", 0), binding,
            Arc::new(Fixture), Arc::new(Fixture)).unwrap();
        listener.fail_after_clones = true;
        let address = listener.local_addr().unwrap();
        let mut client = TcpStream::connect(address).unwrap();
        client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        let accepted = loop {
            match listener.listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock && Instant::now() < deadline =>
                    thread::sleep(Duration::from_millis(5)),
                Err(error) => panic!("accept failed: {error}"),
            }
        };
        assert!(listener.connection(accepted, Duration::from_secs(1)).is_err());
        let mut data = [0u8; 1];
        assert_eq!(client.read(&mut data).unwrap(), 0, "all server socket clones must be closed");
        // The failed connection never owns the listener itself.
        let second = TcpStream::connect(address).unwrap();
        drop(second);
    }

    #[test]
    fn closer_panic_reports_listener_failure() {
        use crate::server::{ExecuteRequest, ExecutionContext, ExecutionTerminal};
        use pilotdeck_protocol::JsString;
        use std::io::{BufRead, Write};
        struct Fixture;
        impl ExecutionClock for Fixture {
            fn now_epoch_ms(&self) -> f64 { 1000.0 }
            fn now_iso(&self) -> JsString { "1970-01-01T00:00:01.000Z".into() }
            fn parse_date(&self, _: &JsString) -> Result<Option<f64>, JsString> { Ok(None) }
        }
        impl ExecutionFactory for Fixture {
            fn execute(&self, _: &ExecuteRequest, _: &mut ExecutionContext)
                -> pilotdeck_agent::ports::PortResult<ExecutionTerminal> {
                unreachable!("hello cannot execute")
            }
        }
        let binding = SidecarBinding {
            module_id: "test".into(), module_instance_id: "instance".into(),
            connection_generation: "base".into(), capabilities_version: "1".into(),
        };
        let mut listener = TcpExecutionServer::bind(("127.0.0.1", 0), binding,
            Arc::new(Fixture), Arc::new(Fixture)).unwrap();
        listener.panic_closer = true;
        let address = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let signal = stop.clone();
        let worker = thread::spawn(move || listener.serve(signal, Duration::from_secs(1)));
        let mut socket = TcpStream::connect(address).unwrap();
        socket.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        writeln!(socket, "{{\"kind\":\"request\",\"messageId\":\"hello\",\"method\":\"hello\",\"payload\":{{}}}}").unwrap();
        let mut response = String::new();
        BufReader::new(&socket).read_line(&mut response).unwrap();
        assert!(response.contains("\"ok\":true"));
        drop(socket);
        let deadline = Instant::now() + Duration::from_secs(3);
        while !worker.is_finished() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(5));
        }
        stop.store(true, Ordering::SeqCst);
        let error = worker.join().unwrap().expect_err("panic must reach supervisor as failure");
        assert!(error.to_string().contains("cleanup was incomplete"));
    }

    #[test]
    fn env_port_matches_fixed_typescript_number_cases() {
        for (value, expected) in [
            ("4321", 4321),
            ("04321", 4321),
            ("0x10e1", 4321),
            ("4.321e3", 4321),
            ("\u{a0}4321\u{a0}", 4321),
            ("0b1", 1),
            ("0o10", 8),
        ] {
            assert_eq!(optional_port(Some(value)), Ok(Some(expected)), "{value:?}");
        }
        for value in ["0", "65536", "1.5", "Infinity", "0x", "-1", "\u{85}4321"] {
            assert!(optional_port(Some(value)).is_err(), "{value:?}");
        }
        assert_eq!(optional_port(None), Ok(None));
        assert_eq!(optional_port(Some("\u{feff} ")), Ok(None));
        assert_eq!(listen_host(Some("\u{a0} localhost ")), "localhost");
        assert_eq!(listen_host(Some(" ")), "127.0.0.1");
    }
}
