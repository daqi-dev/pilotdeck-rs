//! Native stdio/TCP entry point. stdout contains only stdio Module2 NDJSON.
use pilotdeck_protocol::admission::SidecarBinding;
use pilotdeck_transport::default_factory::OriginalAgentFactory;
use pilotdeck_transport::server::ExecutionServer;
use pilotdeck_transport::stdio::ConnectionOwner;
use pilotdeck_transport::system_clock::SystemExecutionClock;
use std::io::{self, BufReader};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments == ["--version"] {
        println!(
            "pilotdeck-agent-loop-sidecar {} implementation=rust wire=2.0 source={}",
            env!("CARGO_PKG_VERSION"),
            env!("PILOTDECK_SOURCE_COMMIT")
        );
        return ExitCode::SUCCESS;
    }
    if !accepts_launch_arguments(&arguments) {
        eprintln!("Usage: pilotdeck-agent-loop-sidecar [--stdio | --version | <sidecar-path>]");
        return ExitCode::from(2);
    }
    if std::env::var_os("PILOTDECK_AGENT_LOOP_FACTORY").is_some_and(|value| !value.is_empty()) {
        eprintln!("Rust sidecar cannot load a JavaScript PILOTDECK_AGENT_LOOP_FACTORY.");
        return ExitCode::from(2);
    }
    let port_value = std::env::var_os("PILOTDECK_AGENT_LOOP_TCP_PORT");
    let port_text = port_value.as_ref().map(|value| value.to_string_lossy());
    let tcp_port = match pilotdeck_transport::tcp::optional_port(port_text.as_deref()) {
        Ok(port) => port,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::from(2);
        }
    };
    // #1371: an armed controlled loop resolves and validates its plan before
    // the server advertises readiness, so an invalid control directory fails
    // closed here exactly like the gateway assembly.
    if let Ok(value) = std::env::var(pilotdeck_transport::loop_stimulus::STIMULUS_ENV) {
        match pilotdeck_transport::loop_stimulus::validate_value(&value) {
            Ok(dir) => {
                // Resolve and freeze the plan here, at startup, not on the
                // first turn: the gateway resolved its own copy before it
                // launched this process, so a lazy read would let the two
                // disagree if `loop.json` changes in between and silently
                // record an uncontrolled turn under a planned cell.
                match pilotdeck_transport::loop_stimulus::LoopStimulus::global() {
                    Some(stimulus) => {
                        if let Err(message) = stimulus.plan() {
                            eprintln!("{message}");
                            return ExitCode::from(2);
                        }
                    }
                    // The entry above only exists when the variable is set.
                    None => {
                        eprintln!(
                            "native loop stimulus could not resolve {}.",
                            pilotdeck_transport::loop_stimulus::STIMULUS_ENV
                        );
                        return ExitCode::from(2);
                    }
                }
                eprintln!("native loop stimulus on {}", dir.display());
            }
            Err(message) => {
                eprintln!("{message}");
                return ExitCode::from(2);
            }
        }
    }
    let process_cwd = match std::env::current_dir().and_then(|path| {
        path.into_os_string().into_string().map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "Process cwd is not valid UTF8.")
        })
    }) {
        Ok(cwd) => cwd.into(),
        Err(error) => {
            eprintln!("Sidecar initialization failed: {error}");
            return ExitCode::from(2);
        }
    };
    let module_instance = format!("pilotdeck-agent-loop-instance-{}", uuid::Uuid::new_v4());
    let binding = SidecarBinding {
        module_id: "pilotdeck-agent-loop".into(),
        module_instance_id: module_instance.clone().into(),
        connection_generation: format!("{module_instance}-{}", uuid::Uuid::new_v4()).into(),
        capabilities_version: "1".into(),
    };
    let clock = Arc::new(SystemExecutionClock);
    let factory = Arc::new(OriginalAgentFactory {
        clock: clock.clone(),
        process_cwd,
    });
    if let Some(port) = tcp_port {
        let host_value = std::env::var_os("PILOTDECK_AGENT_LOOP_TCP_HOST")
            .map(|value| value.to_string_lossy().into_owned());
        let host = pilotdeck_transport::tcp::listen_host(host_value.as_deref());
        let listener = match pilotdeck_transport::tcp::TcpExecutionServer::bind(
            (host, port),
            binding,
            factory,
            clock,
        ) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("Sidecar TCP initialization failed: {error}");
                return ExitCode::from(2);
            }
        };
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        #[cfg(unix)]
        let _signals = match Signals::new(stop.clone()) {
            Ok(signals) => signals,
            Err(error) => {
                eprintln!("Sidecar signal initialization failed: {error}");
                return ExitCode::from(2);
            }
        };
        // No readiness line on stdout: the protocol begins on accepted sockets.
        return match listener.serve(stop, Duration::from_secs(5)) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("Sidecar TCP shutdown failed: {error}");
                ExitCode::from(3)
            }
        };
    }
    let server = ExecutionServer::new(ConnectionOwner::new(binding, 64), factory, clock, 32);
    let report = server.serve(
        BufReader::new(io::stdin()),
        io::stdout(),
        Duration::from_secs(5),
    );
    if report.complete() {
        ExitCode::SUCCESS
    } else {
        // The OS process is the final supervisor for a stuck input/output/host
        // worker. Do not report graceful thread cleanup which did not happen.
        eprintln!(
            "Sidecar shutdown incomplete: reader_joined={} writer_joined={} worker_joined={}",
            report.reader_joined, report.writer_joined, report.worker_joined
        );
        ExitCode::from(3)
    }
}

#[cfg(unix)]
struct Signals(Vec<signal_hook::SigId>);
#[cfg(unix)]
impl Signals {
    fn new(stop: Arc<std::sync::atomic::AtomicBool>) -> io::Result<Self> {
        let mut signals = Self(Vec::new());
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            signals
                .0
                .push(signal_hook::flag::register(signal, stop.clone())?);
        }
        Ok(signals)
    }
}
#[cfg(unix)]
impl Drop for Signals {
    fn drop(&mut self) {
        for signal in self.0.drain(..) {
            signal_hook::low_level::unregister(signal);
        }
    }
}
// A single nonempty path is only a launch placeholder for the TS gateway;
// it is ignored. Environment selects TCP, otherwise the invocation uses stdio.
fn accepts_launch_arguments(arguments: &[String]) -> bool {
    arguments.is_empty()
        || (arguments.len() == 1
            && !arguments[0].is_empty()
            && (arguments[0] == "--stdio" || !arguments[0].starts_with("--")))
}

#[cfg(test)]
mod tests {
    use super::accepts_launch_arguments;

    #[test]
    fn stdio_accepts_one_ignored_path_and_rejects_invalid_arguments() {
        for arguments in [
            vec![],
            vec!["--stdio"],
            vec!["/tmp/PilotDeck/dist/agent-loop-sidecar.js"],
            vec!["relative-sidecar.js"],
            vec!["-sidecar.js"],
        ] {
            let arguments = arguments.into_iter().map(String::from).collect::<Vec<_>>();
            assert!(accepts_launch_arguments(&arguments), "{arguments:?}");
        }
        for arguments in [
            vec![""],
            vec!["--unknown"],
            vec!["--stdio", "sidecar.js"],
            vec!["sidecar.js", "another.js"],
            vec!["--version", "sidecar.js"],
        ] {
            let arguments = arguments.into_iter().map(String::from).collect::<Vec<_>>();
            assert!(!accepts_launch_arguments(&arguments), "{arguments:?}");
        }
    }
}
