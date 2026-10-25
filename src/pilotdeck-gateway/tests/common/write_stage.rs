//! Shared child-process fixture for write-stage (descriptor write) error
//! tests (#649/#651/#656). A write past RLIMIT_FSIZE surfaces as an `EFBIG`
//! error only when SIGXFSZ is ignored — the gateway binary installs that
//! disposition at startup (`ensure_sigxfsz_ignored`), and the child half
//! registers the same one before writing. The parent half spawns this test
//! binary under `ulimit -f <blocks>` running an `--ignored` child test that
//! prints one `WRITE_ERROR=` line, so every port that splits the open from
//! the write can be judged against Node's `EFBIG: file too large, write`
//! (node v22.22.0, `_tmp/20261025-issue-656/node-probe/write-stages.out`).

/// Environment variable carrying the child's write target from the parent.
pub(crate) const CHILD_PATH_ENV: &str = "PDRS_WRITE_STAGE_PATH";

/// Child half setup: the gateway's SIGXFSZ disposition, so the write past
/// RLIMIT_FSIZE returns `EFBIG` instead of killing the child.
pub(crate) fn ignore_sigxfsz_like_gateway() {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("child test runtime")
        .block_on(async { crate::ensure_sigxfsz_ignored() });
}

/// Parent half: spawn `child` (an `--ignored` test, run with `--exact`) under
/// `ulimit -f limit_blocks`, with [`CHILD_PATH_ENV`] pointing at `path`, and
/// return the child's single `WRITE_ERROR=` line. RLIMIT_FSIZE counts in
/// 512-byte blocks, so `1` fails any 4 KiB write and `0` fails every write.
pub(crate) fn child_write_error(child: &str, limit_blocks: u32, path: &std::path::Path) -> String {
    let output = std::process::Command::new("/bin/sh")
        .arg("-c")
        .arg(format!(
            "ulimit -f {limit_blocks}; exec \"$0\" --exact {child} --ignored --nocapture"
        ))
        .arg(std::env::current_exe().unwrap())
        .env(CHILD_PATH_ENV, path)
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        output.status.code(),
        Some(0),
        "child killed or failed: {stdout}"
    );
    stdout
        .lines()
        .find_map(|line| line.strip_prefix("WRITE_ERROR="))
        .unwrap_or_else(|| panic!("no WRITE_ERROR line: {stdout}"))
        .to_owned()
}
