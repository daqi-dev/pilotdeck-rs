use super::super::{Result, timestamp};
use super::http::failure;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

pub const STATUS_STARTING: &str = "starting";
pub const STATUS_CONNECTED: &str = "connected";
pub const STATUS_WAITING_FOR_LOGIN: &str = "waiting_for_login";
pub const STATUS_EXPIRED: &str = "expired";
pub const STATUS_FAILED: &str = "failed";
pub const STATUS_STOPPED: &str = "stopped";

pub const MSG_STARTING: &str = "weixin: starting in background";
pub const MSG_CONNECTED: &str = "weixin: connected";
pub const MSG_WAITING_FOR_LOGIN: &str = "微信等待扫码登录";
pub const MSG_EXPIRED: &str = "微信登录已过期，请重新扫码登录";
pub const MSG_FAILED: &str = "weixin: QR login failed";
pub const MSG_STOPPED: &str = "weixin: stopped";

#[derive(Clone)]
pub(super) struct Storage {
    pub home: PathBuf,
    mapper_save: Arc<Mutex<Option<Arc<dyn Fn(Value) + Send + Sync>>>>,
}

impl Storage {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            mapper_save: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_debounced_mapper_save(&self, saver: Arc<dyn Fn(Value) + Send + Sync>) {
        *self.mapper_save.lock().unwrap() = Some(saver);
    }

    /// TS WeixinChannel.ts:978-988 loadCredentials():
    /// Returns None if the credentials file does not exist, is corrupted JSON,
    /// or is missing required fields (baseUrl, botToken, accountId).
    pub fn load_credentials(&self) -> Option<Value> {
        let path = self.home.join("weixin-credentials.json");
        let bytes = fs::read(&path).ok()?;
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        if ["baseUrl", "botToken", "accountId"]
            .iter()
            .all(|k| text(&value, k).is_some())
        {
            Some(value)
        } else {
            None
        }
    }

    /// Aligns with TS loadCredentials(): returns Ok(None) instead of Err when
    /// the file is missing, corrupt JSON, or missing required fields, allowing
    /// channel startup to smoothly proceed to QR login.
    pub fn credentials(&self) -> Result<Option<Value>> {
        Ok(self.load_credentials())
    }

    /// TS WeixinChannel.ts:990-997 saveCredentials():
    /// Persists baseUrl, botToken, accountId, and optional cursor to weixin-credentials.json
    /// with 0o600 permissions, creating parent directories if needed.
    pub fn save_credentials(&self, creds: &Value) -> Result<()> {
        let owner = self.credential_owner();
        let _guard = owner.lock().unwrap();
        self.save_credentials_locked(creds)
    }

    fn save_credentials_locked(&self, creds: &Value) -> Result<()> {
        let mut to_save = json!({
            "baseUrl": creds.get("baseUrl").and_then(Value::as_str).unwrap_or(""),
            "botToken": creds.get("botToken").and_then(Value::as_str).unwrap_or(""),
            "accountId": creds.get("accountId").and_then(Value::as_str).unwrap_or(""),
        });
        if let Some(cursor) = creds.get("cursor").and_then(Value::as_str) {
            to_save["cursor"] = cursor.into();
        }
        write(&self.home.join("weixin-credentials.json"), &to_save)
    }

    /// TS WeixinChannel.ts:990-997 saveCredentials():
    /// Persists credentials (baseUrl, botToken, accountId, optional cursor) to weixin-credentials.json
    /// with 0o600 permissions under the credential_owner lock, overwriting any previous
    /// credentials so new logins from QR confirmation are not discarded.
    pub fn save_login(&self, value: Value) -> Result<Value> {
        let owner = self.credential_owner();
        let _guard = owner.lock().unwrap();
        self.save_credentials_locked(&value)?;
        Ok(value)
    }

    /// TS WeixinChannel.ts:1000-1007 saveCursor():
    /// Updates cursor in stored credentials and persists to disk.
    /// If cursor is empty, does not write to disk.
    pub fn save_cursor(&self, cursor: &str) -> Result<Option<Value>> {
        if cursor.is_empty() {
            return Ok(self.load_credentials());
        }
        let owner = self.credential_owner();
        let _guard = owner.lock().unwrap();
        let Some(mut current) = self.credentials()? else {
            return Ok(None);
        };
        current["cursor"] = cursor.into();
        self.save_credentials_locked(&current)?;
        Ok(Some(current))
    }

    pub fn advance_cursor(&self, expected: &Value, cursor: &str) -> Result<Option<Value>> {
        if cursor.is_empty() {
            return Ok(self.load_credentials());
        }
        let owner = self.credential_owner();
        let _guard = owner.lock().unwrap();
        let Some(mut current) = self.credentials()? else {
            return Ok(None);
        };
        if !same_generation(&current, expected) {
            return Ok(None);
        }
        current["cursor"] = cursor.into();
        self.save_credentials_locked(&current)?;
        Ok(Some(current))
    }

    // shortcut: external CLI writers do not share this native lock; stop native
    // polling before handing off credentials, until every writer uses one owner.
    pub(super) fn credential_owner(&self) -> Arc<Mutex<()>> {
        static OWNERS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
        let key = fs::canonicalize(&self.home).unwrap_or_else(|_| self.home.clone());
        let mut owners = OWNERS.get_or_init(Default::default).lock().unwrap();
        owners.retain(|_, owner| owner.strong_count() > 0);
        if let Some(owner) = owners.get(&key).and_then(Weak::upgrade) {
            return owner;
        }
        let owner = Arc::new(Mutex::new(()));
        owners.insert(key, Arc::downgrade(&owner));
        owner
    }

    pub fn load_mapper(&self) -> Result<Value> {
        let mapper = read_checked(&self.home.join("channels/weixin.state.json"))?
            .unwrap_or_else(|| json!({"activeByChatId":{},"projectByChatId":{}}));
        validate_mapper(&mapper)?;
        Ok(mapper)
    }

    pub fn save_mapper(&self, mapper: &Value) -> Result<()> {
        validate_mapper(mapper)?;
        if let Some(save) = self.mapper_save.lock().unwrap().clone() {
            save(mapper.clone());
            return Ok(());
        }
        write(&self.home.join("channels/weixin.state.json"), mapper)
    }

    /// TS WeixinChannel.ts:341-347 reportStatus():
    /// Reports runtime state to channels/runtime-status.json
    /// State types: "starting", "connected", "waiting_for_login", "expired", "failed", "stopped"
    /// Details: { accountId?: string, error?: string, qrUrl?: string }
    pub fn report_status(&self, state: &str, message: &str, details: Value) -> Result<()> {
        self.status(state, message, details)
    }

    pub fn status(&self, state: &str, message: &str, details: Value) -> Result<()> {
        let path = self.home.join("channels/runtime-status.json");
        let mut snapshot = read(&path)
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        if !snapshot["channels"].is_object() {
            snapshot["channels"] = json!({});
        }
        let now = timestamp();
        let mut channel = json!({
            "channelKey": "weixin",
            "state": state,
            "updatedAt": now,
        });
        if !message.is_empty() {
            channel["message"] = message.into();
        }
        if let Some(details) = details.as_object() {
            for (key, value) in details {
                if !value.is_null() {
                    channel[key] = value.clone();
                }
            }
        }
        snapshot["updatedAt"] = now.into();
        snapshot["channels"]["weixin"] = channel;
        write(&path, &snapshot)
    }
}

pub(super) fn text<'a>(value: &'a Value, key: &str) -> Option<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

fn read(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

pub(super) fn write(path: &Path, value: &Value) -> Result<()> {
    let operation = || -> std::io::Result<()> {
        use std::io::Write;
        fs::create_dir_all(path.parent().unwrap())?;
        let temp = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = options.open(&temp)?;
            file.write_all(serde_json::to_string_pretty(value)?.as_bytes())?;
            file.write_all(b"\n")?;
            file.sync_all()?;
            fs::rename(&temp, path)?;
            fs::File::open(path.parent().unwrap())?.sync_all()
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    };
    operation().map_err(|_| failure("Cannot persist Weixin channel state"))
}

pub(super) fn same_login(a: &Value, b: &Value) -> bool {
    ["baseUrl", "accountId", "botToken"]
        .iter()
        .all(|k| a.get(k) == b.get(k))
}

pub(super) fn same_generation(a: &Value, b: &Value) -> bool {
    same_login(a, b) && a.get("cursor") == b.get("cursor")
}

fn validate_mapper(mapper: &Value) -> Result<()> {
    if !mapper.is_object()
        || !mapper.get("activeByChatId").is_some_and(Value::is_object)
        || mapper
            .get("projectByChatId")
            .is_some_and(|v| !v.is_object())
    {
        return Err(failure("Invalid Weixin mapper; original retained"));
    }
    Ok(())
}

pub(super) fn read_checked(path: &Path) -> Result<Option<Value>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|_| failure("Invalid Weixin state JSON; original retained")),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(failure("Cannot read Weixin state; original retained")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestHome(PathBuf);
    impl TestHome {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!("test-storage-{}", uuid::Uuid::new_v4()));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for TestHome {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn test_credentials_returns_none_when_file_not_found() {
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());
        assert_eq!(store.credentials().unwrap(), None);
        assert_eq!(store.load_credentials(), None);
    }

    #[test]
    fn test_credentials_returns_none_on_corrupt_json() {
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());
        fs::write(home.0.join("weixin-credentials.json"), b"{\ninvalid json :(").unwrap();
        // TS: catch error and return null to trigger QR flow
        assert_eq!(store.credentials().unwrap(), None);
        assert_eq!(store.load_credentials(), None);
    }

    #[test]
    fn test_credentials_returns_none_when_required_fields_missing_or_empty() {
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());

        // Missing accountId
        let no_account = json!({"baseUrl": "https://ilink.example.com", "botToken": "token123"});
        fs::write(home.0.join("weixin-credentials.json"), no_account.to_string()).unwrap();
        assert_eq!(store.credentials().unwrap(), None);
        assert_eq!(store.load_credentials(), None);

        // Missing baseUrl
        let no_base = json!({"botToken": "token123", "accountId": "acc456"});
        fs::write(home.0.join("weixin-credentials.json"), no_base.to_string()).unwrap();
        assert_eq!(store.credentials().unwrap(), None);
        assert_eq!(store.load_credentials(), None);

        // Missing botToken
        let no_token = json!({"baseUrl": "https://ilink.example.com", "accountId": "acc456"});
        fs::write(home.0.join("weixin-credentials.json"), no_token.to_string()).unwrap();
        assert_eq!(store.credentials().unwrap(), None);
        assert_eq!(store.load_credentials(), None);

        // Empty string in accountId
        let empty_account = json!({"baseUrl": "https://ilink.example.com", "botToken": "token123", "accountId": ""});
        fs::write(home.0.join("weixin-credentials.json"), empty_account.to_string()).unwrap();
        assert_eq!(store.credentials().unwrap(), None);
        assert_eq!(store.load_credentials(), None);
    }

    #[test]
    fn test_credentials_loads_valid_credentials() {
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());
        let valid = json!({
            "baseUrl": "https://ilink.example.com",
            "botToken": "token123",
            "accountId": "acc456",
            "cursor": "cur_001"
        });
        fs::write(home.0.join("weixin-credentials.json"), valid.to_string()).unwrap();
        let loaded = store.credentials().unwrap().expect("should be Some");
        assert_eq!(loaded["baseUrl"], "https://ilink.example.com");
        assert_eq!(loaded["botToken"], "token123");
        assert_eq!(loaded["accountId"], "acc456");
        assert_eq!(loaded["cursor"], "cur_001");
        assert_eq!(store.load_credentials().unwrap(), loaded);
    }

    #[test]
    fn test_save_credentials_persists_to_disk() {
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());
        let creds = json!({
            "baseUrl": "https://ilink.example.com",
            "botToken": "token-xyz",
            "accountId": "acc-xyz",
            "cursor": "initial-cur"
        });
        store.save_credentials(&creds).unwrap();

        let path = home.0.join("weixin-credentials.json");
        assert!(path.exists());
        let on_disk: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(on_disk["baseUrl"], "https://ilink.example.com");
        assert_eq!(on_disk["botToken"], "token-xyz");
        assert_eq!(on_disk["accountId"], "acc-xyz");
        assert_eq!(on_disk["cursor"], "initial-cur");
    }

    #[test]
    fn test_save_cursor_updates_existing_credentials() {
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());
        let initial = json!({
            "baseUrl": "https://ilink.example.com",
            "botToken": "tok",
            "accountId": "acc",
            "cursor": "c1"
        });
        store.save_credentials(&initial).unwrap();

        let updated = store.save_cursor("c2").unwrap().expect("should return updated");
        assert_eq!(updated["cursor"], "c2");

        let reloaded = store.credentials().unwrap().unwrap();
        assert_eq!(reloaded["cursor"], "c2");
        assert_eq!(reloaded["baseUrl"], "https://ilink.example.com");
        assert_eq!(reloaded["botToken"], "tok");
        assert_eq!(reloaded["accountId"], "acc");
    }

    #[test]
    fn test_save_cursor_returns_none_when_no_credentials() {
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());
        assert_eq!(store.save_cursor("c1").unwrap(), None);
    }

    #[test]
    fn test_save_cursor_empty_buf_does_not_modify_disk() {
        // Aligns with TS: empty buf/cursor does not write to disk
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());
        let initial = json!({
            "baseUrl": "https://ilink.example.com",
            "botToken": "tok",
            "accountId": "acc",
            "cursor": "c1"
        });
        store.save_credentials(&initial).unwrap();

        let res = store.save_cursor("").unwrap();
        assert!(res.is_some());
        assert_eq!(res.unwrap()["cursor"], "c1");

        let on_disk = store.credentials().unwrap().unwrap();
        assert_eq!(on_disk["cursor"], "c1");
    }

    #[test]
    fn test_report_status_aligns_with_ts() {
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());

        // Test starting status
        store
            .report_status(STATUS_STARTING, MSG_STARTING, json!({}))
            .unwrap();

        // Test connected status
        store
            .report_status(
                STATUS_CONNECTED,
                MSG_CONNECTED,
                json!({"accountId": "test-account"}),
            )
            .unwrap();

        let status_path = home.0.join("channels/runtime-status.json");
        assert!(status_path.exists());
        let status_json: Value = serde_json::from_slice(&fs::read(&status_path).unwrap()).unwrap();

        assert!(status_json["updatedAt"].is_string());
        let weixin = &status_json["channels"]["weixin"];
        assert_eq!(weixin["channelKey"], "weixin");
        assert_eq!(weixin["state"], "connected");
        assert_eq!(weixin["message"], "weixin: connected");
        assert_eq!(weixin["accountId"], "test-account");

        // Test waiting_for_login with qrUrl
        store
            .report_status(
                STATUS_WAITING_FOR_LOGIN,
                MSG_WAITING_FOR_LOGIN,
                json!({"qrUrl": "https://login.weixin.qq.com/qrcode/123"}),
            )
            .unwrap();
        let status_json2: Value = serde_json::from_slice(&fs::read(&status_path).unwrap()).unwrap();
        let weixin2 = &status_json2["channels"]["weixin"];
        assert_eq!(weixin2["state"], "waiting_for_login");
        assert_eq!(weixin2["message"], "微信等待扫码登录");
        assert_eq!(weixin2["qrUrl"], "https://login.weixin.qq.com/qrcode/123");

        // Test failed with error
        store
            .report_status(
                STATUS_FAILED,
                MSG_FAILED,
                json!({"error": "network timeout"}),
            )
            .unwrap();
        let status_json3: Value = serde_json::from_slice(&fs::read(&status_path).unwrap()).unwrap();
        let weixin3 = &status_json3["channels"]["weixin"];
        assert_eq!(weixin3["state"], "failed");
        assert_eq!(weixin3["message"], "weixin: QR login failed");
        assert_eq!(weixin3["error"], "network timeout");

        // Test stopped
        store.report_status(STATUS_STOPPED, MSG_STOPPED, json!({})).unwrap();
        let status_json4: Value = serde_json::from_slice(&fs::read(&status_path).unwrap()).unwrap();
        let weixin4 = &status_json4["channels"]["weixin"];
        assert_eq!(weixin4["state"], "stopped");
        assert_eq!(weixin4["message"], "weixin: stopped");

        // Test expired
        store.report_status(STATUS_EXPIRED, MSG_EXPIRED, json!({})).unwrap();
        let status_json5: Value = serde_json::from_slice(&fs::read(&status_path).unwrap()).unwrap();
        let weixin5 = &status_json5["channels"]["weixin"];
        assert_eq!(weixin5["state"], "expired");
        assert_eq!(weixin5["message"], "微信登录已过期，请重新扫码登录");
    }

    #[test]
    fn test_advance_cursor_generation_check() {
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());
        let a = json!({"baseUrl": "https://ilink.example.com", "botToken": "b", "accountId": "a"});
        store.save_credentials(&a).unwrap();

        // Advance cursor succeeds when expected generation matches
        let advanced = store.advance_cursor(&a, "cur-2").unwrap().unwrap();
        assert_eq!(advanced["cursor"], "cur-2");

        // Advance cursor fails with None when expected generation does not match (cursor mismatch)
        assert_eq!(store.advance_cursor(&a, "cur-3").unwrap(), None);
    }

    #[test]
    fn test_save_login_overwrites_existing_credentials() {
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());
        let old_creds = json!({
            "baseUrl": "https://old.example.com",
            "botToken": "old-token-123",
            "accountId": "old-acc-456",
            "cursor": "old-cur-789"
        });
        store.save_credentials(&old_creds).unwrap();

        let initial_on_disk = store.credentials().unwrap().expect("old creds must exist");
        assert_eq!(initial_on_disk["botToken"], "old-token-123");
        assert_eq!(initial_on_disk["cursor"], "old-cur-789");

        // Aligns with TS WeixinChannel.ts:990-997 (ts-align item 47):
        // QR login confirmation persists the new credentials and overwrites stale ones.
        let new_login = json!({
            "baseUrl": "https://new.example.com",
            "botToken": "new-token-abc",
            "accountId": "new-acc-def"
        });
        let result = store.save_login(new_login).unwrap();
        assert_eq!(result["baseUrl"], "https://new.example.com");
        assert_eq!(result["botToken"], "new-token-abc");
        assert_eq!(result["accountId"], "new-acc-def");

        let reloaded = store.credentials().unwrap().expect("new creds must exist");
        assert_eq!(reloaded["baseUrl"], "https://new.example.com");
        assert_eq!(reloaded["botToken"], "new-token-abc");
        assert_eq!(reloaded["accountId"], "new-acc-def");
        // Fresh QR login without cursor removes old cursor
        assert!(reloaded.get("cursor").is_none());

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let meta = fs::metadata(home.0.join("weixin-credentials.json")).unwrap();
            assert_eq!(meta.permissions().mode() & 0o777, 0o600);
        }
    }

    #[test]
    fn test_save_cursor_empty_does_not_modify_disk_and_does_not_lock() {
        // Aligns with TS WeixinChannel.ts:1000-1007 (ts-align item 48):
        // Empty buf / empty cursor directly returns without writing to disk or locking.
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());
        let initial = json!({
            "baseUrl": "https://ilink.example.com",
            "botToken": "tok",
            "accountId": "acc",
            "cursor": "c1"
        });
        store.save_credentials(&initial).unwrap();

        // Hold the credential_owner lock in this thread.
        // If save_cursor("") attempted to acquire the mutex, it would deadlock.
        let owner = store.credential_owner();
        let _guard = owner.lock().unwrap();

        let res = store.save_cursor("").unwrap();
        assert!(res.is_some());
        assert_eq!(res.unwrap()["cursor"], "c1");

        let on_disk = store.load_credentials().unwrap();
        assert_eq!(on_disk["cursor"], "c1");
    }

    #[test]
    fn test_save_cursor_non_empty_acquires_credential_owner_lock() {
        let home = TestHome::new();
        let store = Storage::new(home.0.clone());
        let initial = json!({
            "baseUrl": "https://ilink.example.com",
            "botToken": "tok",
            "accountId": "acc",
            "cursor": "c1"
        });
        store.save_credentials(&initial).unwrap();

        let owner = store.credential_owner();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();

        let owner_clone = owner.clone();
        let blocker = std::thread::spawn(move || {
            let _guard = owner_clone.lock().unwrap();
            acquired_tx.send(()).unwrap();
            release_rx.recv().unwrap();
        });

        acquired_rx.recv().unwrap();

        let store_clone = store.clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let updater = std::thread::spawn(move || {
            let res = store_clone.save_cursor("c2").unwrap();
            done_tx.send(res).unwrap();
        });

        // Ensure updater is blocked while lock is held
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(done_rx.try_recv().is_err());

        // Release the lock
        release_tx.send(()).unwrap();
        blocker.join().unwrap();

        let result = done_rx
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("updater should finish after lock release");
        assert_eq!(result.unwrap()["cursor"], "c2");
        updater.join().unwrap();

        let reloaded = store.credentials().unwrap().unwrap();
        assert_eq!(reloaded["cursor"], "c2");
    }
}
