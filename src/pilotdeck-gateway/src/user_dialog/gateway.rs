use super::*;
use crate::runtime::connection::RequestError;
use std::{collections::HashMap, sync::Mutex};

/// None identifies an ephemeral session. The host resolves projectKey/fallback
/// and storage paths; the gateway never treats a caller's path as authoritative.
pub type DialogSessionResolver =
    Arc<dyn Fn(&JsValue) -> std::result::Result<Option<DialogSession>, RequestError> + Send + Sync>;
pub struct DialogSession {
    pub key: StoreKey,
    pub journal: Option<Arc<GatewayUserDialogJournal>>,
}
#[derive(Clone)]
pub enum DialogStore {
    Native(Arc<dyn LiveGatewayUserDialogStore>),
    Http(Arc<HttpGatewayUserDialogStore>),
}
impl DialogStore {
    async fn submit(
        &self,
        key: StoreKey,
        request: JsString,
        lease: Option<JsString>,
        answer: DialogAnswer,
    ) -> std::result::Result<bool, RequestError> {
        match self {
            Self::Native(store) => {
                let store = store.clone();
                tokio::task::spawn_blocking(move || {
                    store.submit_live_answer(&key, &request, lease.as_ref(), &answer)
                })
                .await
                .map_err(|error| RequestError::new(error.to_string()))?
            }
            Self::Http(store) => {
                store
                    .submit_live_answer(&key, &request, lease.as_ref(), &answer)
                    .await
            }
        }
        .map_err(|error| RequestError::new(error.to_string()))
    }
    async fn remove(
        &self,
        key: StoreKey,
        request: JsString,
    ) -> std::result::Result<(), RequestError> {
        match self {
            Self::Native(store) => {
                let store = store.clone();
                tokio::task::spawn_blocking(move || store.remove(&key, &request))
                    .await
                    .map_err(|error| RequestError::new(error.to_string()))?
            }
            Self::Http(store) => store.remove(&key, &request).await,
        }
        .map_err(|error| RequestError::new(error.to_string()))
    }
    async fn release(
        &self,
        key: StoreKey,
        request: JsString,
        lease: JsString,
    ) -> std::result::Result<bool, RequestError> {
        match self {
            Self::Native(store) => {
                let store = store.clone();
                tokio::task::spawn_blocking(move || store.release_live(&key, &request, &lease))
                    .await
                    .map_err(|error| RequestError::new(error.to_string()))?
            }
            Self::Http(store) => store.release_live(&key, &request, &lease).await,
        }
        .map_err(|error| RequestError::new(error.to_string()))
    }

    async fn claim(
        &self,
        key: StoreKey,
        request: JsString,
        ttl_ms: f64,
        lease: Option<JsString>,
    ) -> std::result::Result<JsValue, RequestError> {
        match self {
            Self::Native(store) => {
                let store = store.clone();
                tokio::task::spawn_blocking(move || {
                    store.claim_live(&key, &request, ttl_ms, lease.as_ref())
                })
                .await
                .map_err(|error| RequestError::new(error.to_string()))?
                .map(|claim| super::http_store::lease_claim_value(&claim))
            }
            Self::Http(store) => store
                .claim_live(&key, &request, ttl_ms, lease.as_ref())
                .await
                .map(|claim| claim.to_value()),
        }
        .map_err(|error| RequestError::new(error.to_string()))
    }

    async fn list(
        &self,
        key: StoreKey,
        live: bool,
    ) -> std::result::Result<Vec<StoredDialog>, RequestError> {
        match self {
            Self::Native(store) => {
                let store = store.clone();
                tokio::task::spawn_blocking(move || {
                    if live {
                        store.list_live(&key)
                    } else {
                        store.list(&key)
                    }
                })
                .await
                .map_err(|error| RequestError::new(error.to_string()))?
            }
            Self::Http(store) => {
                if live {
                    store.list_live(&key).await
                } else {
                    store.list(&key).await
                }
            }
        }
        .map_err(|error| RequestError::new(error.to_string()))
    }
}

type RecoveryLocks = Mutex<HashMap<(JsString, JsString), Arc<tokio::sync::Mutex<()>>>>;
type RecoveryReceiver =
    tokio::sync::watch::Receiver<Option<std::result::Result<bool, RequestError>>>;
type Recovering = Mutex<HashMap<(JsString, JsString, JsString), RecoveryReceiver>>;

pub struct DialogPersistence {
    pub store: Option<DialogStore>,
    pub resolve_session: DialogSessionResolver,
    recovered: Mutex<HashMap<(JsString, JsString), Vec<JsValue>>>,
    recovery_storage: Option<super::recovery::DialogRecoveryStorageResolver>,
    recovery_locks: RecoveryLocks,
    recovering: Recovering,
}
impl DialogPersistence {
    pub fn with_recovery_storage(
        mut self,
        resolve: super::recovery::DialogRecoveryStorageResolver,
    ) -> Self {
        self.recovery_storage = Some(resolve);
        self
    }
    pub(crate) async fn submit(
        &self,
        input: &JsValue,
        request: &JsString,
        lease: Option<&JsString>,
        answer: &DialogAnswer,
    ) -> std::result::Result<bool, RequestError> {
        let Some(session) = (self.resolve_session)(input)? else {
            return Ok(false);
        };
        let Some(store) = &self.store else {
            return Ok(false);
        };
        let dialogs = store.list(session.key.clone(), true).await?;
        let Some(dialog) = dialogs.iter().find(|dialog| {
            dialog.request.get("requestId").and_then(JsValue::as_string) == Some(request)
        }) else {
            return Ok(false);
        };
        if let DialogAnswer::Answered(value) = answer
            && !super::respond::accepts(&dialog.request, value)
        {
            return Err(super::respond::contract_error(&dialog.request, false));
        }
        store
            .submit(session.key, request.clone(), lease.cloned(), answer.clone())
            .await
    }
    async fn cleanup(
        &self,
        session: DialogSession,
        request: &JsString,
    ) -> std::result::Result<(), RequestError> {
        if let Some(journal) = session.journal {
            let request = request.clone();
            tokio::task::spawn_blocking(move || journal.remove(&request))
                .await
                .map_err(|error| RequestError::new(error.to_string()))?
                .map_err(|error| RequestError::new(error.to_string()))?;
        }
        if let Some(store) = &self.store {
            store.remove(session.key.clone(), request.clone()).await?;
        }
        let key = (session.key.project_root, session.key.session_id);
        let mut cache = self.recovered.lock().unwrap();
        if let Some(dialogs) = cache.get_mut(&key) {
            dialogs.retain(|dialog| {
                dialog
                    .get("request")
                    .and_then(|request| request.get("requestId"))
                    .and_then(JsValue::as_string)
                    != Some(request)
            });
            if dialogs.is_empty() {
                cache.remove(&key);
            }
        }
        Ok(())
    }
    pub(crate) async fn acknowledge(
        &self,
        input: &JsValue,
        request: &JsString,
    ) -> std::result::Result<bool, RequestError> {
        let Some(session) = (self.resolve_session)(input)? else {
            return Ok(false);
        };
        let dialogs = self.recovered(input).await?;
        if !dialogs.iter().any(|dialog| {
            dialog
                .get("request")
                .and_then(|request| request.get("requestId"))
                .and_then(JsValue::as_string)
                == Some(request)
        }) {
            return Ok(false);
        }
        self.cleanup(session, request).await?;
        Ok(true)
    }
    pub(crate) async fn recover(
        self: &Arc<Self>,
        input: &JsValue,
        request: &JsString,
        answer: &DialogAnswer,
    ) -> std::result::Result<bool, RequestError> {
        let Some(session) = (self.resolve_session)(input)? else {
            return Ok(false);
        };
        let key = (
            session.key.project_root.clone(),
            session.key.session_id.clone(),
            request.clone(),
        );
        let mut receiver = {
            let mut active = self.recovering.lock().unwrap();
            if let Some(receiver) = active.get(&key) {
                receiver.clone()
            } else {
                let (sender, receiver) = tokio::sync::watch::channel(None);
                active.insert(key.clone(), receiver.clone());
                let host = self.clone();
                let input = input.clone();
                let request = request.clone();
                let answer = answer.clone();
                tokio::spawn(async move {
                    let lock = host
                        .recovery_locks
                        .lock()
                        .unwrap()
                        .entry((key.0.clone(), key.1.clone()))
                        .or_default()
                        .clone();
                    let _guard = lock.lock().await;
                    let result = host
                        .write_recovered(&input, session, &request, &answer)
                        .await;
                    sender.send_replace(Some(result));
                    host.recovering.lock().unwrap().remove(&key);
                });
                receiver
            }
        };
        loop {
            if let Some(result) = receiver.borrow().clone() {
                return result;
            }
            receiver
                .changed()
                .await
                .map_err(|error| RequestError::new(error.to_string()))?;
        }
    }
    async fn write_recovered(
        &self,
        input: &JsValue,
        session: DialogSession,
        request: &JsString,
        answer: &DialogAnswer,
    ) -> std::result::Result<bool, RequestError> {
        let dialogs = self.recovered(input).await?;
        let Some(dialog) = dialogs.iter().find(|dialog| {
            dialog
                .get("request")
                .and_then(|request| request.get("requestId"))
                .and_then(JsValue::as_string)
                == Some(request)
        }) else {
            return Ok(false);
        };
        let event = dialog.get("request").unwrap();
        if let DialogAnswer::Answered(value) = answer
            && !super::respond::accepts(event, value)
        {
            return Err(super::respond::contract_error(event, true));
        }
        let resolve = self.recovery_storage.as_ref().ok_or_else(|| {
            RequestError::new("Persistent dialog recovery has no session transcript storage.")
        })?;
        let storage = resolve(&session)?;
        let session_id = session.key.session_id.clone();
        let event = event.clone();
        let answer = answer.clone();
        tokio::task::spawn_blocking(move || {
            super::recovery::persist(&storage, &session_id, &event, &answer)
        })
        .await
        .map_err(|error| RequestError::new(error.to_string()))??;
        self.cleanup(session, request).await?;
        Ok(true)
    }
    pub fn new(store: Option<DialogStore>, resolve_session: DialogSessionResolver) -> Self {
        Self {
            store,
            resolve_session,
            recovered: Mutex::new(HashMap::new()),
            recovery_storage: None,
            recovery_locks: Mutex::new(HashMap::new()),
            recovering: Mutex::new(HashMap::new()),
        }
    }
    pub(crate) async fn release(
        &self,
        input: &JsValue,
        request: &JsString,
        lease: &JsString,
    ) -> std::result::Result<bool, RequestError> {
        match ((self.resolve_session)(input)?, &self.store) {
            (Some(session), Some(store)) => {
                store
                    .release(session.key, request.clone(), lease.clone())
                    .await
            }
            _ => Ok(false),
        }
    }
    pub(crate) async fn claim(
        &self,
        input: &JsValue,
        request: &JsString,
        ttl_ms: f64,
        lease: Option<&JsString>,
    ) -> std::result::Result<JsValue, RequestError> {
        let session = (self.resolve_session)(input)?;
        match (session, &self.store) {
            (Some(session), Some(store)) => {
                store
                    .claim(session.key, request.clone(), ttl_ms, lease.cloned())
                    .await
            }
            _ => Ok(super::http_store::lease_claim_value(
                &LeaseClaim::NotPending,
            )),
        }
    }
    pub(crate) async fn hosted(
        &self,
        input: &JsValue,
    ) -> std::result::Result<Vec<JsValue>, RequestError> {
        let Some(session) = (self.resolve_session)(input)? else {
            return Ok(Vec::new());
        };
        match &self.store {
            Some(store) => Ok(store
                .list(session.key, true)
                .await?
                .into_iter()
                .map(|dialog| dialog.request)
                .collect()),
            None => Ok(Vec::new()),
        }
    }
    pub(crate) async fn recovered(
        &self,
        input: &JsValue,
    ) -> std::result::Result<Vec<JsValue>, RequestError> {
        let Some(session) = (self.resolve_session)(input)? else {
            return Ok(Vec::new());
        };
        let key = (
            session.key.project_root.clone(),
            session.key.session_id.clone(),
        );
        let mut candidates = if let Some(journal) = session.journal {
            tokio::task::spawn_blocking(move || journal.recover())
                .await
                .map_err(|error| RequestError::new(error.to_string()))?
                .map_err(|error| RequestError::new(error.to_string()))?
        } else {
            Vec::new()
        };
        if let Some(store) = &self.store {
            for dialog in store.list(session.key, false).await? {
                let event = object([
                    ("type", "user_dialog_terminated".into()),
                    ("request", dialog.request),
                    ("reason", "gateway_restarted".into()),
                    ("terminatedAt", JsValue::String(dialog.created_at)),
                    ("recovery", "next_turn_context".into()),
                ]);
                candidates.push(event);
            }
        }
        let mut recovered: Vec<JsValue> = Vec::new();
        for event in candidates {
            let id = event
                .get("request")
                .and_then(|request| request.get("requestId"));
            if let Some(index) = recovered.iter().position(|old| {
                old.get("request")
                    .and_then(|request| request.get("requestId"))
                    == id
            }) {
                recovered[index] = event;
            } else {
                recovered.push(event);
            }
        }
        let mut cache = self.recovered.lock().unwrap();
        if recovered.is_empty() {
            cache.remove(&key);
            return Ok(recovered);
        }
        if let Some(previous) = cache.get(&key) {
            for event in &mut recovered {
                let id = event
                    .get("request")
                    .and_then(|request| request.get("requestId"));
                if let Some(at) = previous
                    .iter()
                    .find(|old| {
                        old.get("request")
                            .and_then(|request| request.get("requestId"))
                            == id
                    })
                    .and_then(|old| old.get("terminatedAt"))
                    .filter(|value| value.as_string().is_some_and(|value| !value.is_empty()))
                {
                    set(event, "terminatedAt", at.clone());
                }
            }
        }
        cache.insert(key, recovered.clone());
        Ok(recovered)
    }
}

pub struct GatewayUserDialogs {
    pub bus: Arc<Mutex<GatewayUserDialogBus>>,
    pub persistence: Option<Arc<DialogPersistence>>,
}
impl Default for GatewayUserDialogs {
    fn default() -> Self {
        Self {
            bus: Arc::new(Mutex::new(GatewayUserDialogBus::new(
                DialogRuntime::default(),
                None,
            ))),
            persistence: None,
        }
    }
}
