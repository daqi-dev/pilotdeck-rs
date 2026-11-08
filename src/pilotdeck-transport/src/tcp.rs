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
        })
    }
    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.listener.local_addr()
    }

    /// The process supervisor must exit if any connection cannot be reaped.
    /// Closing one socket never retires another socket's operations or listener.
    pub fn serve(self, stop: Arc<AtomicBool>, grace: Duration) -> io::Result<()> {
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
            match self.listener.accept() {
                Ok((socket, _)) => {
                    if tasks.len() >= MAX_CONNECTIONS {
                        let _ = socket.shutdown(Shutdown::Both);
                        continue;
                    }
                    match self.connection(socket, grace) {
                        Ok(task) => tasks.push(task),
                        Err(error) => {
                            failure = Some(error);
                            break;
                        }
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
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
        socket.set_nodelay(true)?;
        socket.set_write_timeout(Some(grace))?;
        let input = socket.try_clone()?;
        let output = socket.try_clone()?;
        let shutdown = socket.try_clone()?;
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
