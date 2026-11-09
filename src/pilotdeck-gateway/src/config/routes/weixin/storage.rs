use super::super::{Result, timestamp};
use super::http::failure;
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

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
    pub fn credentials(&self) -> Result<Option<Value>> {
        let Some(value) = read_checked(&self.home.join("weixin-credentials.json"))? else {
            return Ok(None);
        };
        if ["baseUrl", "botToken", "accountId"]
            .iter()
            .all(|k| text(&value, k).is_some())
        {
            Ok(Some(value))
        } else {
            Err(failure(
                "Invalid Weixin credentials file; original retained",
            ))
        }
    }
    pub fn save_login(&self, value: Value) -> Result<Value> {
        let owner = self.credential_owner();
        let _guard = owner.lock().unwrap();
        // A login already saved by this owner takes precedence over QR completion.
        if let Some(current) = self.credentials()? {
            return Ok(current);
        }
        write(&self.home.join("weixin-credentials.json"), &value)?;
        Ok(value)
    }
    pub fn advance_cursor(&self, expected: &Value, cursor: &str) -> Result<Option<Value>> {
        let owner = self.credential_owner();
        let _guard = owner.lock().unwrap();
        let Some(mut current) = self.credentials()? else {
            return Ok(None);
        };
        if !same_generation(&current, expected) {
            return Ok(None);
        }
        current["cursor"] = cursor.into();
        write(&self.home.join("weixin-credentials.json"), &current)?;
        Ok(Some(current))
    }
    // shortcut: external CLI writers do not share this native lock; stop native
    // polling before handing off credentials, until every writer uses one owner.
    fn credential_owner(&self) -> Arc<Mutex<()>> {
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
    pub fn status(&self, state: &str, message: &str, details: Value) -> Result<()> {
        let path = self.home.join("channels/runtime-status.json");
        let mut snapshot = read(&path)
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        if !snapshot["channels"].is_object() {
            snapshot["channels"] = json!({});
        }
        let now = timestamp();
        let mut channel =
            json!({"channelKey":"weixin","state":state,"updatedAt":now,"message":message});
        if let Some(details) = details.as_object() {
            for (key, value) in details {
                channel[key] = value.clone();
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
