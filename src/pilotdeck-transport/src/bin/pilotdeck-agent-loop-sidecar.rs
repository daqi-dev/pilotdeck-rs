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

/// One best-effort console line (#2558 transport twin, #3152 item 7): the
/// write error is dropped, like TS's `console.log` / `console.error`, while
/// std's `println!` / `eprintln!` panic when the write fails — a closed
/// stdout/stderr pipe would kill this process (exit 101) instead of letting
/// it serve or exit with its own receipt, and the gateway side of the same
/// contract is `pilotdeck-gateway`'s `native_process::console_log_line`
/// (`native_process/channels.rs`, #2301 / #2558), which this crate cannot
/// import (transport must not depend on the gateway). Same shape, locally:
/// one buffer, one `write_all`, so a concurrent writer can never interleave
/// between a line and its newline (#3152 item 11).
fn console_line(sink: &mut dyn io::Write, message: &str) {
    let mut line = String::with_capacity(message.len() + 1);
    line.push_str(message);
    line.push('\n');
    let _ = sink.write_all(line.as_bytes());
}

fn main() -> ExitCode {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    if arguments == ["--version"] {
        console_line(
            &mut io::stdout(),
            &format!(
                "pilotdeck-agent-loop-sidecar {} implementation=rust wire=2.0 source={}",
                env!("CARGO_PKG_VERSION"),
                env!("PILOTDECK_SOURCE_COMMIT")
            ),
        );
        return ExitCode::SUCCESS;
    }
    if !accepts_launch_arguments(&arguments) {
        console_line(
            &mut io::stderr(),
            "Usage: pilotdeck-agent-loop-sidecar [--stdio | --version | <sidecar-path>]",
        );
        return ExitCode::from(2);
    }
    for option in [
        "PILOTDECK_AGENT_LOOP_FACTORY",
        "PILOTDECK_AGENT_LOOP_TCP_PORT",
    ] {
        if std::env::var_os(option).is_some_and(|value| !value.is_empty()) {
            console_line(
                &mut io::stderr(),
                &format!("Rust candidate does not yet support the {option} deployment profile."),
            );
            return ExitCode::from(2);
        }
    }
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
                            console_line(&mut io::stderr(), message);
                            return ExitCode::from(2);
                        }
                    }
                    // The entry above only exists when the variable is set.
                    None => {
                        console_line(
                            &mut io::stderr(),
                            &format!(
                                "native loop stimulus could not resolve {}.",
                                pilotdeck_transport::loop_stimulus::STIMULUS_ENV
                            ),
                        );
                        return ExitCode::from(2);
                    }
                }
                console_line(
                    &mut io::stderr(),
                    &format!("native loop stimulus on {}", dir.display()),
                );
            }
            Err(message) => {
                console_line(&mut io::stderr(), &message);
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
            console_line(
                &mut io::stderr(),
                &format!("Sidecar initialization failed: {error}"),
            );
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
        console_line(
            &mut io::stderr(),
            &format!(
                "Sidecar shutdown incomplete: reader_joined={} writer_joined={} worker_joined={}",
                report.reader_joined, report.writer_joined, report.worker_joined
            ),
        );
        ExitCode::from(3)
    }
}

// A single nonempty path is only a launch placeholder for the TS gateway;
// it is ignored, and every accepted invocation uses stdio.
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
