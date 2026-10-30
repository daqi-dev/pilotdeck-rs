#[allow(dead_code)]
#[path = "common/baseline.rs"]
mod baseline;

use std::{
    fs,
    io::{BufRead, BufReader},
    net::{SocketAddr, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver},
    thread::{self, JoinHandle},
    time::Duration,
};

struct Gateway {
    child: Child,
    lines: Receiver<String>,
    reader: Option<JoinHandle<()>>,
    home: PathBuf,
}

impl Gateway {
    fn start(listen: SocketAddr) -> Self {
        let home = std::env::temp_dir().join(format!("pdrs-1131-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&home).expect("#1131 create isolated home");
        let stderr =
            fs::File::create(home.join("stderr.log")).expect("#1131 create gateway stderr capture");
        let mut child = Command::new(env!("CARGO_BIN_EXE_pilotdeck-gateway"))
            .env_clear()
            .env("HOME", &home)
            .env("PILOT_HOME", home.join("pilot"))
            .env("PILOTDECK_PROXY_LISTEN", listen.to_string())
            .env("PILOTDECK_PROXY_UPSTREAM", "http://127.0.0.1:1")
            .current_dir(&home)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(stderr)
            .spawn()
            .expect("#1131 spawn real gateway binary");
        let stdout = child.stdout.take().expect("#1131 capture gateway stdout");
        let (send, lines) = mpsc::channel();
        let reader = thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                if reader
                    .read_line(&mut line)
                    .expect("#1131 read gateway stdout")
                    == 0
                {
                    break;
                }
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            lines,
            reader: Some(reader),
            home,
        }
    }

    fn line(&self) -> String {
        self.lines
            .recv_timeout(Duration::from_secs(15))
            .unwrap_or_else(|error| {
                panic!(
                    "#1131 gateway must publish address on stdout: {error}; stderr: {}",
                    fs::read_to_string(self.home.join("stderr.log"))
                        .expect("#1131 read startup diagnostics")
                )
            })
    }
}

impl Drop for Gateway {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let _ = fs::remove_dir_all(&self.home);
    }
}

fn check_address(listen: SocketAddr, expected: &[serde_json::Value]) -> SocketAddr {
    let gateway = Gateway::start(listen);
    let http = gateway.line();
    let address: SocketAddr = http
        .strip_prefix("PilotDeck server listening: http://")
        .expect("#1131 HTTP address prefix matches TS")
        .strip_suffix('\n')
        .expect("#1131 HTTP address line ends with newline")
        .parse()
        .expect("#1131 HTTP address must be a socket address");
    assert_ne!(address.port(), 0, "#1131 reported port must be nonzero");
    assert_eq!(address.ip(), listen.ip(), "#1131 reported loopback host");
    if listen.port() != 0 {
        assert_eq!(address, listen, "#1131 fixed address must be preserved");
    }
    let ws = gateway.line();
    let actual = [http, ws];
    for (line, template) in actual.iter().zip(expected) {
        assert_eq!(
            line,
            &(template
                .as_str()
                .expect("#1131 TS address template is a string")
                .replace("<PORT>", &address.port().to_string())
                + "\n"),
            "#1131 stdout address bytes match TS"
        );
    }
    TcpStream::connect_timeout(&address, Duration::from_secs(3))
        .expect("#1131 reported address must accept TCP connections");
    address
}

#[test]
fn reports_reachable_actual_address() {
    let oracle: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/proxy-listen-address/oracle.json"))
            .expect("#1131 parse TS oracle");
    let expected = oracle["addressLines"]
        .as_array()
        .expect("#1131 TS oracle contains address lines");
    assert_eq!(expected.len(), 2, "#1131 TS oracle has HTTP and WS lines");
    let ephemeral = check_address(
        "127.0.0.1:0"
            .parse()
            .expect("#1131 ephemeral input address"),
        expected,
    );
    // The first owned child has been killed and reaped; exercise the same
    // output contract with that port explicitly requested on the next start.
    check_address(ephemeral, expected);
}

#[test]
#[ignore = "requires the fixed TS checkout and Node v22.22.0"]
fn regenerates_identically() {
    baseline::regenerates_identically("proxy-listen-address");
}
