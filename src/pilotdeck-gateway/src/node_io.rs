//! Node v22 libuv diagnostics. Unknown errno (including non-OS Rust errors) is
//! an explicit host boundary, never a Rust Display string on the wire.
mod generated;
use std::io;

pub(crate) fn lookup(error: &io::Error) -> Option<(&'static str, &'static str)> {
    let errno = stage(error).map_or_else(|| error.raw_os_error(), |e| e.source.raw_os_error())?;
    generated::ERRORS
        .iter()
        .find(|(number, _, _)| *number == errno)
        .map(|(_, code, description)| (*code, *description))
}
pub(crate) fn diagnostic(
    error: &io::Error,
    syscall: &str,
    path: Option<&str>,
) -> Option<(&'static str, String)> {
    let (code, description) = lookup(error)?;
    let (syscall, path) = stage(error).map_or((syscall, path), |e| (e.syscall, None));
    let suffix = path
        .filter(|p| !p.is_empty())
        .map_or(String::new(), |p| format!(" '{p}'"));
    Some((code, format!("{code}: {description}, {syscall}{suffix}")))
}
/// readFile errors before descriptor acquisition include the path. Node's
/// fstat/read descriptor errors have no path. EISDIR fixtures without stage
/// annotations also denote read (opening a directory succeeds on POSIX).
pub(crate) fn read_file(error: &io::Error, path: &str) -> Option<(&'static str, String)> {
    if stage(error).is_none() && lookup(error)?.0 == "EISDIR" {
        diagnostic(error, "read", None)
    } else {
        diagnostic(error, "open", Some(path))
    }
}
#[derive(Debug)]
struct Stage {
    source: io::Error,
    syscall: &'static str,
}
impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.source.fmt(f)
    }
}
impl std::error::Error for Stage {}
fn stage(error: &io::Error) -> Option<&Stage> {
    error.get_ref()?.downcast_ref()
}
pub(crate) fn descriptor_error(source: io::Error, syscall: &'static str) -> io::Error {
    io::Error::new(source.kind(), Stage { source, syscall })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_generated_errno_and_format_matches_node_uv_exception() {
        let oracle: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/node-io/errors.json")).unwrap();
        for (errno, code, _) in generated::ERRORS {
            let error = io::Error::from_raw_os_error(*errno);
            let rows: Vec<_> = oracle["formatting"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|row| row["code"] == *code)
                .collect();
            assert_eq!(rows.len(), 18, "{code}");
            for row in rows {
                let actual = diagnostic(
                    &error,
                    row["syscall"].as_str().unwrap(),
                    row["path"].as_str(),
                )
                .unwrap();
                assert_eq!(actual.0, *code);
                assert_eq!(actual.1, row["message"].as_str().unwrap());
            }
        }
        #[cfg(target_os = "macos")]
        for entry in oracle["all"].as_array().unwrap() {
            let uv = entry["uv"].as_i64().unwrap();
            if (-1024..0).contains(&uv) {
                assert_eq!(
                    lookup(&io::Error::from_raw_os_error(-uv as i32)).unwrap().0,
                    entry["code"].as_str().unwrap()
                );
            }
        }
    }
    #[test]
    fn unknown_errno_and_rust_errors_require_host_including_descriptor_errors() {
        for e in [
            io::Error::from_raw_os_error(123456),
            io::Error::other("not an errno"),
            io::Error::from_raw_os_error(libc::EDQUOT),
        ] {
            assert!(diagnostic(&e, "stat", Some("/file")).is_none());
            assert!(read_file(&e, "/file").is_none());
            assert!(read_file(&descriptor_error(e, "read"), "/file").is_none());
        }
    }
    #[test]
    fn descriptor_errno_preserves_the_actual_syscall_without_path() {
        let e = descriptor_error(io::Error::from_raw_os_error(libc::EIO), "read");
        assert_eq!(
            read_file(&e, "/file").unwrap(),
            ("EIO", "EIO: i/o error, read".into())
        );
    }
}
