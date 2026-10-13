//! Native stdio entry point. stdout contains only Module2 NDJSON.
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
            option_env!("PILOTDECK_SOURCE_COMMIT").unwrap_or("unregistered-build")
        );
        return ExitCode::SUCCESS;
    }
    if !accepts_stdio_arguments(&arguments) {
        eprintln!("Usage: pilotdeck-agent-loop-sidecar [--stdio | --version | <sidecar-path>]");
        return ExitCode::from(2);
    }
    for option in [
        "PILOTDECK_AGENT_LOOP_FACTORY",
        "PILOTDECK_AGENT_LOOP_TCP_PORT",
    ] {
        if std::env::var_os(option).is_some_and(|value| !value.is_empty()) {
            eprintln!("Rust candidate does not yet support the {option} deployment profile.");
            return ExitCode::from(2);
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

fn accepts_stdio_arguments(arguments: &[String]) -> bool {
    arguments.is_empty()
        || (arguments.len() == 1
            && !arguments[0].is_empty()
            && (arguments[0] == "--stdio" || !arguments[0].starts_with("--")))
}

#[cfg(test)]
mod tests {
    use super::accepts_stdio_arguments;

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
            assert!(accepts_stdio_arguments(&arguments), "{arguments:?}");
        }
        for arguments in [
            vec![""],
            vec!["--unknown"],
            vec!["--stdio", "sidecar.js"],
            vec!["sidecar.js", "another.js"],
            vec!["--version", "sidecar.js"],
        ] {
            let arguments = arguments.into_iter().map(String::from).collect::<Vec<_>>();
            assert!(!accepts_stdio_arguments(&arguments), "{arguments:?}");
        }
    }
}
