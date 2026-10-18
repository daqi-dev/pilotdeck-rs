//! Node path/env/os semantics used by the CLI (path.resolve, os.homedir, String#trim).
use crate::Failure;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};

pub struct Env(HashMap<OsString, OsString>);

impl Env {
    pub fn from_process() -> Self {
        Self(std::env::vars_os().collect())
    }
    #[cfg(test)]
    pub fn from_pairs(pairs: &[(&str, &str)]) -> Self {
        Self(pairs.iter().map(|(k, v)| (k.into(), v.into())).collect())
    }
    /// Present-but-empty is Some(""), matching `??` on process.env.
    pub fn get(&self, name: &str) -> Option<&OsStr> {
        self.0.get(OsStr::new(name)).map(OsString::as_os_str)
    }
    pub fn string(&self, name: &str) -> Option<String> {
        self.get(name).map(|v| v.to_string_lossy().into_owned())
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

/// os.homedir(): libuv returns $HOME whenever it is set, even if empty.
fn homedir(env: &Env) -> Result<PathBuf, Failure> {
    env.get("HOME")
        .map(PathBuf::from)
        .ok_or(Failure::Todo("home directory lookup without $HOME"))
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

/// Node fs error message `CODE: description, syscall 'path'` for common errnos.
pub fn node_fs_message(error: &std::io::Error, syscall: &str, path: &Path) -> Option<String> {
    let (code, description) = match error.raw_os_error()? {
        1 => ("EPERM", "operation not permitted"),
        2 => ("ENOENT", "no such file or directory"),
        13 => ("EACCES", "permission denied"),
        17 => ("EEXIST", "file already exists"),
        20 => ("ENOTDIR", "not a directory"),
        21 => ("EISDIR", "illegal operation on a directory"),
        28 => ("ENOSPC", "no space left on device"),
        30 => ("EROFS", "read-only file system"),
        _ => return None,
    };
    Some(format!(
        "{code}: {description}, {syscall} '{}'",
        path.display()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

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
}
