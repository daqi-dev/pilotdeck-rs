//! One owned detached argv process. No shell interpolation or detached RPC work.
use super::*;
use crate::process_group::{GroupGuard, wait_exit_unreaped};
use rustix::process::{Pid, Signal};
use std::{collections::VecDeque, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
};

pub(super) struct Request {
    pub executable: String,
    pub args: Vec<String>,
    pub cwd: String,
    pub env: Vec<(String, String)>,
    pub stdin: Option<Vec<u8>>,
    pub timeout: Duration,
    pub stop: Option<crate::runtime::network::Cancellation>,
}
#[derive(Default)]
pub(super) struct ResultData {
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
}
struct HeadTail {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
}
impl HeadTail {
    fn new() -> Self {
        Self {
            head: Vec::new(),
            tail: VecDeque::new(),
            total: 0,
        }
    }
    fn push(&mut self, data: &[u8]) {
        self.total = self.total.saturating_add(data.len() as u64);
        let prefix = (20_000 - self.head.len()).min(data.len());
        self.head.extend_from_slice(&data[..prefix]);
        for byte in &data[prefix..] {
            if self.tail.len() == 30_000 {
                self.tail.pop_front();
            }
            self.tail.push_back(*byte);
        }
    }
    fn finish(self) -> String {
        let head = String::from_utf8_lossy(&self.head);
        let tail_bytes: Vec<_> = self.tail.into_iter().collect();
        let tail = String::from_utf8_lossy(&tail_bytes);
        if self.total > 50_000 && !tail.is_empty() {
            // Node measures the two independently decoded UTF-8 fragments,
            // including any replacement characters at the truncation edges.
            let omitted = self.total.saturating_sub((head.len() + tail.len()) as u64);
            format!(
                "{head}\n\n... [OUTPUT TRUNCATED - {} bytes omitted out of {} total] ...\n\n{tail}",
                commas(omitted),
                commas(self.total)
            )
        } else {
            format!("{head}{tail}")
        }
    }
}
fn commas(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}
pub(super) async fn run(
    request: Request,
    context: &ToolRunContext,
) -> Result<ResultData, ToolError> {
    let mut command = Command::new(&request.executable);
    command
        .args(&request.args)
        .current_dir(&request.cwd)
        .env_clear()
        .envs(request.env)
        .stdin(if request.stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    pilotdeck_child_signals::install_child_detach_session(command.as_std_mut());
    pilotdeck_child_signals::install_child_signal_reset(command.as_std_mut());
    let mut child = command
        .spawn()
        .map_err(|e| ToolError::failed(e.to_string()))?;
    let group = child.id().and_then(|id| Pid::from_raw(id as i32));
    let mut guard = GroupGuard(group);
    let mut stdout = child
        .stdout
        .take()
        .ok_or_else(|| ToolError::failed("Python stdout is unavailable."))?;
    let mut stderr = child
        .stderr
        .take()
        .ok_or_else(|| ToolError::failed("Python stderr is unavailable."))?;
    let stdin = child.stdin.take();
    let extra_stop = request.stop;
    let write_input = async move {
        if let (Some(mut stdin), Some(bytes)) = (stdin, request.stdin) {
            stdin.write_all(&bytes).await?;
            stdin.shutdown().await?;
        }
        Ok::<(), std::io::Error>(())
    };
    tokio::pin!(write_input);
    let mut input_done = false;
    let mut out = HeadTail::new();
    let mut err = Vec::new();
    let mut obuf = vec![0; 8192];
    let mut ebuf = vec![0; 8192];
    let mut out_done = false;
    let mut err_done = false;
    let mut exited = false;
    let mut exit_code = None;
    let deadline = tokio::time::Instant::now() + request.timeout;
    let mut stopped = None;
    let mut timed_out = false;
    let mut cancelled = false;
    let mut io_error = None;
    // The leader remains unreaped until the final group signal. Its PGID
    // cannot be reused while descendants still own inherited pipes.
    while !exited || !out_done || !err_done {
        tokio::select! {
            biased;
            _=context.cancelled(),if !cancelled && stopped.is_none()=>{
                cancelled=true;guard.signal(Signal::TERM);
                stopped=Some(tokio::time::Instant::now());
            },
            _=async {
                if let Some(signal)=&extra_stop {signal.cancelled().await}
                else {std::future::pending::<()>().await}
            },if stopped.is_none()=>{
                cancelled=true;guard.signal(Signal::TERM);
                stopped=Some(tokio::time::Instant::now());
            },
            _=tokio::time::sleep_until(deadline),if stopped.is_none()=>{
                timed_out=true;guard.signal(Signal::TERM);
                stopped=Some(tokio::time::Instant::now());
            },
            value=&mut write_input,if !input_done=>{
                input_done=true;
                if let Err(error)=value && error.kind()!=std::io::ErrorKind::BrokenPipe {
                        io_error=Some(error.to_string());guard.signal(Signal::TERM);
                        stopped.get_or_insert_with(tokio::time::Instant::now);
                }
            },
            value=stdout.read(&mut obuf),if !out_done=>{
                match value {
                    Ok(0)=>out_done=true,
                    Ok(n)=>out.push(&obuf[..n]),
                    Err(error)=>{io_error=Some(error.to_string());out_done=true;}
                }
            },
            value=stderr.read(&mut ebuf),if !err_done=>{
                match value {
                    Ok(0)=>err_done=true,
                    Ok(n)=>{let keep=n.min(10_000-err.len());err.extend_from_slice(&ebuf[..keep]);},
                    Err(error)=>{io_error=Some(error.to_string());err_done=true;}
                }
            },
            value=async { wait_exit_unreaped(group.expect("spawned Python has a PID")).await },if !exited=>{
                match value {
                    Ok(Some(status))=>{exited=true;exit_code=status.code;},
                    Ok(None)=>return Err(ToolError::new("unsupported_tool","Python process exit observation is unavailable on this platform.")),
                    Err(error)=>return Err(ToolError::failed(error.to_string())),
                }
                // A completed script cannot leave a pipe-holding background
                // process or authenticated RPC client behind.
                stopped.get_or_insert_with(tokio::time::Instant::now);
            },
            _=async {
                if let Some(at)=stopped {tokio::time::sleep_until(at+Duration::from_millis(500)).await}
                else {std::future::pending::<()>().await}
            }=>{
                guard.signal(Signal::KILL);
                break;
            }
        }
        if io_error.is_some() && stopped.is_none() {
            guard.signal(Signal::TERM);
            stopped = Some(tokio::time::Instant::now());
        }
    }
    guard.signal(Signal::KILL);
    // Owned cleanup is bounded even for a descendant holding a dead pipe.
    let status = tokio::time::timeout(Duration::from_secs(2), child.wait())
        .await
        .map_err(|_| ToolError::failed("Python process termination could not be confirmed."))?;
    // No further group signal after reaping: the PGID may now be reused.
    guard.disarm();
    let status = status.map_err(|e| ToolError::failed(e.to_string()))?;
    if !exited {
        exit_code = status.code();
    }
    let drain = async {
        loop {
            if !out_done {
                match stdout.read(&mut obuf).await {
                    Ok(0) => out_done = true,
                    Ok(n) => out.push(&obuf[..n]),
                    Err(error) => {
                        io_error = Some(error.to_string());
                        out_done = true;
                    }
                }
            }
            if !err_done {
                match stderr.read(&mut ebuf).await {
                    Ok(0) => err_done = true,
                    Ok(n) => {
                        let keep = n.min(10_000 - err.len());
                        err.extend_from_slice(&ebuf[..keep]);
                    }
                    Err(error) => {
                        io_error = Some(error.to_string());
                        err_done = true;
                    }
                }
            }
            if out_done && err_done {
                break;
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(2), drain)
        .await
        .map_err(|_| {
            ToolError::failed("Python process streams did not close after termination.")
        })?;
    if let Some(error) = io_error {
        return Err(ToolError::failed(error));
    }
    Ok(ResultData {
        exit_code,
        stdout: out.finish(),
        stderr: String::from_utf8_lossy(&err).into_owned(),
        timed_out,
        cancelled,
    })
}
