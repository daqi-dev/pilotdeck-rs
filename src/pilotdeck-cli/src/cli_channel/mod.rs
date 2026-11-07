//! Legacy default channel. Local fallback still needs the shared native host.
mod remote;
mod render;

use super::{
    Failure,
    paths::{self, Env},
    stderr_line,
};
use std::{
    io::{self, BufRead, Write},
    path::Path,
};

pub(crate) fn run(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Failure::Error(error.to_string()))?;
    runtime.block_on(run_async(argv, env, cwd))
}

async fn run_async(argv: &[String], env: &Env, cwd: &Path) -> Result<u8, Failure> {
    let Some(mut remote) = remote::connect(env, cwd).await else {
        stderr_line("[pilotdeck] gateway server unavailable; using in-process mode.");
        return Err(Failure::Todo("default CLI in-process Gateway"));
    };
    let session = format!("cli:project={}:default", cwd.to_string_lossy());
    let joined = argv.join(" ");
    let prompt = paths::js_trim(&joined);
    let outcome = if !prompt.is_empty() {
        remote.submit(&session, prompt).await
    } else {
        // Detached reader keeps blocking stdin off the runtime thread. Dropping
        // the receiver releases a blocked sender; process exit owns any read
        // still waiting on an open interactive stdin.
        let (send, mut receive) = tokio::sync::mpsc::channel(32);
        std::thread::spawn(move || {
            let stdin = io::stdin();
            let mut input = Lines::new(stdin.lock());
            loop {
                let next = input.next_line();
                let done = !matches!(&next, Ok(Some(_)));
                if send.blocking_send(next).is_err() || done {
                    break;
                }
            }
        });
        let mut disconnected = false;
        loop {
            let next = tokio::select! {
                next = receive.recv() => next,
                result = remote.idle(), if !disconnected => {
                    if result.is_err() { disconnected = true; }
                    continue;
                }
            };
            let line = match next {
                Some(Ok(Some(line))) => line,
                None | Some(Ok(None)) => break Ok(()),
                Some(Err(error)) => break Err(Failure::Error(error.to_string())),
            };
            let text = String::from_utf8_lossy(&line);
            let text = paths::js_trim(&text);
            if text.is_empty() {
                continue;
            }
            if disconnected {
                break Err(Failure::Error("Gateway WebSocket closed.".into()));
            }
            if let Err(error) = remote.submit(&session, text).await {
                break Err(error);
            }
            let mut stdout = io::stdout().lock();
            if let Err(error) = stdout.write_all(b"\n").and_then(|()| stdout.flush()) {
                break Err(Failure::Error(error.to_string()));
            }
        }
    };
    remote.close().await;
    outcome.map(|()| 0)
}

/// Node readline accepts LF, CRLF, and bare CR, including delimiters split
/// across underlying reads. An EOF without a delimiter still yields a line.
struct Lines<R> {
    input: R,
    after_cr: bool,
}
impl<R: BufRead> Lines<R> {
    fn new(input: R) -> Self {
        Self {
            input,
            after_cr: false,
        }
    }
    fn next_line(&mut self) -> io::Result<Option<Vec<u8>>> {
        let mut output = Vec::new();
        loop {
            let available = self.input.fill_buf()?;
            if available.is_empty() {
                return Ok((!output.is_empty()).then_some(output));
            }
            if self.after_cr {
                self.after_cr = false;
                if available[0] == b'\n' {
                    self.input.consume(1);
                    continue;
                }
            }
            let found = available.iter().position(|b| matches!(*b, b'\r' | b'\n'));
            match found {
                Some(index) => {
                    output.extend_from_slice(&available[..index]);
                    self.after_cr = available[index] == b'\r';
                    self.input.consume(index + 1);
                    return Ok(Some(output));
                }
                None => {
                    let length = available.len();
                    output.extend_from_slice(available);
                    self.input.consume(length);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Lines;
    use std::io::{BufReader, Cursor};

    #[test]
    fn line_reader_handles_split_crlf_bare_cr_and_unterminated_tail() {
        let input = BufReader::with_capacity(1, Cursor::new(b"a\r\nb\rc\n\nlast"));
        let mut lines = Lines::new(input);
        assert_eq!(lines.next_line().unwrap().as_deref(), Some(&b"a"[..]));
        assert_eq!(lines.next_line().unwrap().as_deref(), Some(&b"b"[..]));
        assert_eq!(lines.next_line().unwrap().as_deref(), Some(&b"c"[..]));
        assert_eq!(lines.next_line().unwrap().as_deref(), Some(&b""[..]));
        assert_eq!(lines.next_line().unwrap().as_deref(), Some(&b"last"[..]));
        assert_eq!(lines.next_line().unwrap(), None);
    }
}
