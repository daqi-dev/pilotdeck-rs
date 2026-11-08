//! Node path/env/os semantics used by the CLI (path.resolve, os.homedir, String#trim).
use crate::Failure;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

pub struct Env {
    values: HashMap<OsString, OsString>,
    home_origin: HomeOrigin,
}
#[derive(Clone)]
enum HomeOrigin {
    Process,
    #[cfg(test)]
    Synthetic,
    #[cfg(test)]
    Injected(PathBuf),
}
/// Only HOME and its provenance are retained; no provider credentials are
/// copied when a consumer defers its home lookup until it actually needs it.
#[derive(Clone)]
pub struct HomeInput {
    value: Option<OsString>,
    origin: HomeOrigin,
}
impl HomeInput {
    pub fn capture(env: &Env) -> Self {
        Self {
            value: env.get("HOME").map(OsStr::to_os_string),
            origin: env.home_origin.clone(),
        }
    }
    pub fn resolve(&self) -> Result<PathBuf, Failure> {
        if let Some(value) = &self.value {
            return Ok(value.into());
        }
        match &self.origin {
            HomeOrigin::Process => process_account_home(),
            #[cfg(test)]
            HomeOrigin::Synthetic => Err(Failure::Todo("home directory lookup without $HOME")),
            #[cfg(test)]
            HomeOrigin::Injected(value) => Ok(value.clone()),
        }
    }
}

impl Env {
    pub fn from_process() -> Self {
        Self {
            values: std::env::vars_os().collect(),
            home_origin: HomeOrigin::Process,
        }
    }
    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self {
            values: pairs.iter().map(|(k, v)| (k.into(), v.into())).collect(),
            home_origin: HomeOrigin::Synthetic,
        }
    }
    #[cfg(test)]
    pub fn with_home_fallback(pairs: &[(&str, &str)], home: PathBuf) -> Self {
        let mut env = Self::from_pairs(pairs);
        env.home_origin = HomeOrigin::Injected(home);
        env
    }
    /// Present-but-empty is Some(""), matching `??` on process.env.
    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.values.get(OsStr::new(name)).map(OsString::as_os_str)
    }
    pub fn string(&self, name: &str) -> Option<String> {
        self.get(name).map(|v| v.to_string_lossy().into_owned())
    }
    /// Shared config loading needs the complete environment for provider keys
    /// and module substitutions; preserve the CLI's lossy UTF-8 convention.
    pub fn strings(&self) -> HashMap<String, String> {
        self.values
            .iter()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect()
    }
}

/// path.resolve(cwd, p): lexical normalization, no symlink resolution.
pub fn resolve(cwd: &Path, path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };
    let mut out = PathBuf::from("/");
    for component in joined.components() {
        match component {
            Component::RootDir | Component::Prefix(_) => out = PathBuf::from("/"),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => out.push(name),
        }
    }
    out
}

/// os.homedir(): an existing HOME (including empty/relative) always wins.
/// Only a real process snapshot may request the platform account fallback.
pub fn homedir(env: &Env) -> Result<PathBuf, Failure> {
    HomeInput::capture(env).resolve()
}
#[cfg(unix)]
fn account_home(
    real_uid: u32,
    effective_uid: u32,
    lookup: impl FnOnce() -> Option<PathBuf>,
) -> Result<PathBuf, Failure> {
    // std uses real UID; Node/libuv uses effective UID. Do not pretend the
    // same result has been established for setuid or a missing passwd entry.
    if real_uid != effective_uid {
        return Err(Failure::Todo(
            "home directory lookup with distinct real/effective user IDs",
        ));
    }
    let home = lookup().ok_or(Failure::Todo(
        "home directory lookup without an OS account home",
    ))?;
    if home.to_str().is_none() {
        return Err(Failure::Todo(
            "home directory lookup with a non-UTF8 OS account home",
        ));
    }
    Ok(home)
}
#[cfg(unix)]
fn process_account_home() -> Result<PathBuf, Failure> {
    let changed = || Failure::Todo("home directory lookup after the environment snapshot changed");
    if std::env::var_os("HOME").is_some() {
        return Err(changed());
    }
    let uid = rustix::process::getuid().as_raw();
    let euid = rustix::process::geteuid().as_raw();
    let home = account_home(uid, euid, std::env::home_dir)?;
    if std::env::var_os("HOME").is_some()
        || rustix::process::getuid().as_raw() != uid
        || rustix::process::geteuid().as_raw() != euid
    {
        return Err(changed());
    }
    // This is a string/path lookup only: never stat/read the returned home.
    Ok(home)
}
#[cfg(not(unix))]
fn process_account_home() -> Result<PathBuf, Failure> {
    Err(Failure::Todo("OS home directory lookup on this platform"))
}

/// resolvePilotHome(env) from src/pilot/paths.ts.
pub fn pilot_home(env: &Env, cwd: &Path) -> Result<PathBuf, Failure> {
    let raw = env.get("PILOT_HOME").unwrap_or(OsStr::new("~/.pilotdeck"));
    let bytes = raw.as_bytes();
    if bytes == b"~" {
        return Ok(resolve(cwd, &homedir(env)?));
    }
    if let Some(rest) = bytes.strip_prefix(b"~/") {
        return Ok(resolve(cwd, &homedir(env)?.join(OsStr::from_bytes(rest))));
    }
    Ok(resolve(cwd, Path::new(raw)))
}

/// resolveGatewayTokenPath (src/gateway/server/authToken.ts:12-15):
/// `resolve(pilotHome || "~/.pilotdeck", "server-token")`, the default unexpanded.
/// resolvePilotHome is "" only for PILOT_HOME="~" with an empty $HOME, because
/// normalizeHomePath returns os.homedir() for "~" without resolving it.
pub fn gateway_token_path(env: &Env, cwd: &Path) -> Result<PathBuf, Failure> {
    if env.get("PILOT_HOME") == Some(OsStr::new("~")) && homedir(env)?.as_os_str().is_empty() {
        return Ok(resolve(cwd, Path::new("~/.pilotdeck/server-token")));
    }
    Ok(pilot_home(env, cwd)?.join("server-token"))
}

/// ECMAScript WhiteSpace + LineTerminator, as used by String.prototype.trim.
fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\u{9}'..='\u{d}'
            | ' '
            | '\u{a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

pub fn js_trim(text: &str) -> &str {
    text.trim_matches(is_js_space)
}

/// Node fs error message `CODE: description, syscall 'path'`, formatted by the
/// gateway's libuv template (a descriptor read/write-stage error omits the
/// path; a staged error from `read_session_bytes` etc. carries its own stage).
pub fn node_fs_message(error: &std::io::Error, syscall: &str, path: &Path) -> Option<String> {
    let path = path.to_string_lossy();
    pilotdeck_gateway::node_fs_message(error, syscall, Some(path.as_ref()))
}

/// [`node_fs_message`] with libuv's `UV_UNKNOWN` fallback for errnos outside
/// the table, under the same stage rule: a descriptor read/write error
/// carries no path even in the fallback.
pub fn node_fs_message_or_unknown(error: &std::io::Error, syscall: &str, path: &Path) -> String {
    pilotdeck_gateway::node_fs_message_or_unknown(
        error,
        syscall,
        Some(path.to_string_lossy().as_ref()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn homedir_injected_fallback_never_uses_host_environment() {
        let fake = PathBuf::from("/injected-account-only");
        let env = Env::with_home_fallback(&[], fake.clone());
        assert_eq!(homedir(&env).unwrap(), fake);
        assert_eq!(
            pilot_home(&env, Path::new("/work")).unwrap(),
            fake.join(".pilotdeck")
        );
        assert_eq!(
            gateway_token_path(&env, Path::new("/work")).unwrap(),
            fake.join(".pilotdeck/server-token")
        );
        for value in ["", "relative-home", "/explicit-home"] {
            let env = Env::with_home_fallback(&[("HOME", value)], fake.clone());
            assert_eq!(homedir(&env).unwrap(), PathBuf::from(value));
        }
        // A synthetic unset Env is intentionally not the actual process Env.
        assert!(matches!(
            homedir(&Env::from_pairs(&[])),
            Err(Failure::Todo("home directory lookup without $HOME"))
        ));
        // Explicit literal overrides do not need any account lookup.
        assert_eq!(
            pilot_home(
                &Env::from_pairs(&[("PILOT_HOME", "local")]),
                Path::new("/work")
            )
            .unwrap(),
            PathBuf::from("/work/local")
        );
    }

    #[test]
    fn account_lookup_refuses_unestablished_uid_or_account_cases() {
        let called = std::cell::Cell::new(false);
        assert!(matches!(
            account_home(10, 11, || {
                called.set(true);
                Some(PathBuf::from("/fake"))
            }),
            Err(Failure::Todo(
                "home directory lookup with distinct real/effective user IDs"
            ))
        ));
        assert!(!called.get());
        assert!(matches!(
            account_home(10, 10, || None),
            Err(Failure::Todo(
                "home directory lookup without an OS account home"
            ))
        ));
        assert!(matches!(
            account_home(10, 10, || Some(PathBuf::from(OsStr::from_bytes(
                b"/bad/\xff"
            )))),
            Err(Failure::Todo(
                "home directory lookup with a non-UTF8 OS account home"
            ))
        ));
        assert_eq!(
            account_home(10, 10, || Some(PathBuf::from("/fake-account"))).unwrap(),
            PathBuf::from("/fake-account")
        );
    }

    /// Parent runs this explicitly in an isolated HOME-unset child. It only
    /// compares strings: no filesystem operation is performed on OS home.
    #[test]
    #[ignore = "requires HOME unset and PDRS_EXPECTED_OS_HOME from fixed Node os.homedir"]
    fn process_home_unset_string_probe() {
        assert!(std::env::var_os("HOME").is_none());
        let expected = std::env::var_os("PDRS_EXPECTED_OS_HOME").expect("Node string oracle");
        let env = Env::from_process();
        assert_eq!(homedir(&env).unwrap().as_os_str(), expected);
        assert_eq!(
            pilot_home(&env, Path::new("/isolated-unused-cwd")).unwrap(),
            PathBuf::from(expected).join(".pilotdeck")
        );
    }

    #[test]
    fn resolve_normalizes_like_node() {
        let cwd = Path::new("/a/b");
        assert_eq!(resolve(cwd, Path::new("")), PathBuf::from("/a/b"));
        assert_eq!(
            resolve(cwd, Path::new("../c/./d/")),
            PathBuf::from("/a/c/d")
        );
        assert_eq!(resolve(cwd, Path::new("/../../x")), PathBuf::from("/x"));
    }

    #[test]
    fn pilot_home_expands_tilde_and_keeps_empty_override() {
        let cwd = Path::new("/w");
        let env = Env::from_pairs(&[("HOME", "/h")]);
        assert!(matches!(pilot_home(&env, cwd), Ok(p) if p == Path::new("/h/.pilotdeck")));
        let env = Env::from_pairs(&[("HOME", "/h"), ("PILOT_HOME", "")]);
        assert!(matches!(pilot_home(&env, cwd), Ok(p) if p == Path::new("/w")));
        let env = Env::from_pairs(&[("PILOT_HOME", "rel/p")]);
        assert!(matches!(pilot_home(&env, cwd), Ok(p) if p == Path::new("/w/rel/p")));
    }

    #[test]
    fn empty_home_matches_node_homedir_and_token_fallback() {
        let cwd = Path::new("/w");
        let home = |pilot: Option<&str>| {
            let mut pairs = vec![("HOME", "")];
            pairs.extend(pilot.map(|p| ("PILOT_HOME", p)));
            Env::from_pairs(&pairs)
        };
        // resolve("", "x") and resolve("") both land on cwd.
        assert!(matches!(pilot_home(&home(None), cwd), Ok(p) if p == Path::new("/w/.pilotdeck")));
        assert!(matches!(pilot_home(&home(Some("~/x")), cwd), Ok(p) if p == Path::new("/w/x")));
        assert!(matches!(pilot_home(&home(Some("~")), cwd), Ok(p) if p == Path::new("/w")));
        assert!(matches!(pilot_home(&home(Some("~/")), cwd), Ok(p) if p == Path::new("/w")));
        assert!(matches!(gateway_token_path(&home(Some("~/")), cwd),
            Ok(p) if p == Path::new("/w/server-token")));
        assert!(matches!(gateway_token_path(&home(None), cwd),
            Ok(p) if p == Path::new("/w/.pilotdeck/server-token")));
        // Only the raw "" pilotHome falls back to the literal default path.
        assert!(matches!(gateway_token_path(&home(Some("~")), cwd),
            Ok(p) if p == Path::new("/w/~/.pilotdeck/server-token")));
        assert!(matches!(gateway_token_path(&home(Some("~/x")), cwd),
            Ok(p) if p == Path::new("/w/x/server-token")));
        assert!(matches!(gateway_token_path(&home(Some("")), cwd),
            Ok(p) if p == Path::new("/w/server-token")));
        let env = Env::from_pairs(&[("HOME", "/h"), ("PILOT_HOME", "~")]);
        assert!(
            matches!(gateway_token_path(&env, cwd), Ok(p) if p == Path::new("/h/server-token"))
        );
    }

    #[test]
    fn js_trim_uses_ecmascript_whitespace() {
        assert_eq!(js_trim("\u{feff} tok\n\u{3000}"), "tok");
        assert_eq!(js_trim("\u{85}tok"), "\u{85}tok");
    }

    fn node_message(errno: i32) -> String {
        node_fs_message(
            &std::io::Error::from_raw_os_error(errno),
            "stat",
            Path::new("/tmp/errno"),
        )
        .unwrap_or_else(|| panic!("errno {errno} must be in the libuv table"))
    }

    /// Every errno the previous hand-written table covered must keep the exact
    /// Node v22.22.0 / libuv 1.52.1 message.
    #[test]
    fn node_fs_message_matches_libuv_for_previous_table() {
        for (errno, expected) in [
            (libc::EPERM, "EPERM: operation not permitted"),
            (libc::ENOENT, "ENOENT: no such file or directory"),
            (libc::EACCES, "EACCES: permission denied"),
            (libc::EEXIST, "EEXIST: file already exists"),
            (libc::ENOTDIR, "ENOTDIR: not a directory"),
            (libc::EISDIR, "EISDIR: illegal operation on a directory"),
            (libc::ENOSPC, "ENOSPC: no space left on device"),
            (libc::EROFS, "EROFS: read-only file system"),
            (libc::EIO, "EIO: i/o error"),
            (libc::ELOOP, "ELOOP: too many symbolic links encountered"),
            (libc::ENAMETOOLONG, "ENAMETOOLONG: name too long"),
            (libc::EXDEV, "EXDEV: cross-device link not permitted"),
            (libc::ENOTEMPTY, "ENOTEMPTY: directory not empty"),
            (libc::EINVAL, "EINVAL: invalid argument"),
            (libc::EBUSY, "EBUSY: resource busy or locked"),
            (libc::EMFILE, "EMFILE: too many open files"),
            (libc::ENFILE, "ENFILE: file table overflow"),
            (libc::ENOMEM, "ENOMEM: not enough memory"),
            (libc::EFBIG, "EFBIG: file too large"),
            (libc::ENXIO, "ENXIO: no such device or address"),
            (libc::ENODEV, "ENODEV: no such device"),
            (libc::EBADF, "EBADF: bad file descriptor"),
            (libc::ETXTBSY, "ETXTBSY: text file is busy"),
            (libc::ENOTSUP, "ENOTSUP: operation not supported on socket"),
        ] {
            assert_eq!(
                node_message(errno),
                format!("{expected}, stat '/tmp/errno'"),
                "errno {errno}"
            );
        }
    }

    /// The CLI used to hand-maintain a subset; the shared libuv table also
    /// covers errnos that were previously reported as unknown.
    #[test]
    fn node_fs_message_uses_the_full_libuv_table() {
        assert_eq!(
            node_message(libc::EAGAIN),
            "EAGAIN: resource temporarily unavailable, stat '/tmp/errno'"
        );
        assert_eq!(
            node_fs_message(
                &std::io::Error::from_raw_os_error(123456),
                "stat",
                Path::new("/tmp/errno")
            ),
            None
        );
    }

    /// The formatter is the gateway's libuv template: a read-stage error omits
    /// the path, exactly like `node_io::read_file` (Node v22.22.0 renders a
    /// directory read as `EISDIR: illegal operation on a directory, read`).
    #[test]
    fn read_stage_errors_omit_the_path() {
        assert_eq!(
            node_fs_message(
                &std::io::Error::from_raw_os_error(libc::EISDIR),
                "read",
                Path::new("/tmp/errno")
            )
            .as_deref(),
            Some("EISDIR: illegal operation on a directory, read")
        );
    }

    /// Node inserts the path verbatim: quotes and newlines are not escaped
    /// (expected bytes: `tests/fixtures/node-io/errors.json`, EPERM row).
    #[test]
    fn path_is_inserted_verbatim_like_node() {
        assert_eq!(
            node_fs_message(
                &std::io::Error::from_raw_os_error(libc::EPERM),
                "stat",
                Path::new("/fixture/quote' and\nline")
            )
            .as_deref(),
            Some("EPERM: operation not permitted, stat '/fixture/quote' and\nline'")
        );
    }
}
