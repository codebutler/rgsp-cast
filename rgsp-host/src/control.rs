//! JSON-RPC control socket for the on-device UI.
//!
//! A socket rather than a state file on purpose: a file outlives the process
//! that wrote it, so a killed daemon leaves a stale "casting" behind and the UI
//! reports Running for a corpse. A failed connect is the liveness answer.

use jsonrpsee::core::{RpcResult, SubscriptionResult, async_trait, to_json_raw_value};
use jsonrpsee::proc_macros::rpc;
use jsonrpsee::types::ErrorObjectOwned;
use jsonrpsee::PendingSubscriptionSink;
use moonshine_core::clients::ClientManager;
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Mutex, OnceLock};
use tokio::sync::Notify;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingEntry {
    pub id: String,
    pub name: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CastState {
    pub casting: bool,
    pub client: Option<String>,
    pub pending: Vec<PendingEntry>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PinResult {
    pub paired: bool,
}

#[rpc(server, namespace = "state")]
pub trait ControlApi {
    #[subscription(name = "subscribe", unsubscribe = "unsubscribe", item = CastState)]
    async fn subscribe(&self) -> SubscriptionResult;
}

#[rpc(server)]
pub trait PinApi {
    #[method(name = "submit_pin")]
    async fn submit_pin(&self, id: String, pin: String) -> RpcResult<PinResult>;
}

/// Shared, cheap to clone. Owns the state the UI observes.
#[derive(Clone)]
pub struct ControlHandle {
    inner: Arc<Mutex<CastState>>,
    changed: Arc<Notify>,
    /// Filled in by [`Self::set_client_manager`] once the daemon has one to
    /// hand over. Shared (not rebuilt) because by the time it's available the
    /// socket is already serving — a clone of `self` is already inside the
    /// running `RpcModule`, so only a shared cell reaches it.
    /// `submit_pin` has nothing to call without it, which is otherwise only
    /// true of tests that exercise the subscription path alone.
    client_manager: Arc<OnceLock<ClientManager>>,
}

impl Default for ControlHandle {
    fn default() -> Self {
        Self::new()
    }
}

impl ControlHandle {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(CastState {
                casting: false,
                client: None,
                pending: Vec::new(),
            })),
            changed: Arc::new(Notify::new()),
            client_manager: Arc::new(OnceLock::new()),
        }
    }

    /// Set once, early in startup. Ignored if already set.
    pub fn set_client_manager(&self, client_manager: ClientManager) {
        let _ = self.client_manager.set(client_manager);
    }

    pub fn snapshot(&self) -> CastState {
        self.inner.lock().expect("control state poisoned").clone()
    }

    pub fn set_casting(&self, casting: bool) {
        self.inner.lock().expect("control state poisoned").casting = casting;
        self.changed.notify_waiters();
    }

    pub fn set_client(&self, client: Option<String>) {
        self.inner.lock().expect("control state poisoned").client = client;
        self.changed.notify_waiters();
    }

    pub fn set_pending(&self, pending: Vec<PendingEntry>) {
        self.inner.lock().expect("control state poisoned").pending = pending;
        self.changed.notify_waiters();
    }

    pub async fn serve(
        self,
        path: &str,
    ) -> anyhow::Result<jsonrpsee::server::ServerHandle> {
        let _ = std::fs::remove_file(path);
        let server = reth_ipc::server::Builder::default().build(path.to_string());
        let mut methods = ControlApiServer::into_rpc(self.clone());
        methods.merge(PinApiServer::into_rpc(self))?;
        Ok(server.start(methods).await?)
    }
}

#[async_trait]
impl ControlApiServer for ControlHandle {
    async fn subscribe(&self, pending: PendingSubscriptionSink) -> SubscriptionResult {
        let sink = pending.accept().await?;
        // Snapshot first: a late subscriber must not wait for the next change.
        let msg: jsonrpsee::server::SubscriptionMessage = to_json_raw_value(&self.snapshot())?.into();
        sink.send(msg).await?;

        let changed = self.changed.clone();
        loop {
            tokio::select! {
                _ = changed.notified() => {
                    let msg: jsonrpsee::server::SubscriptionMessage = to_json_raw_value(&self.snapshot())?.into();
                    if sink.send(msg).await.is_err() {
                        break;
                    }
                }
                _ = sink.closed() => break,
            }
        }
        Ok(())
    }
}

#[async_trait]
impl PinApiServer for ControlHandle {
    async fn submit_pin(&self, id: String, pin: String) -> RpcResult<PinResult> {
        let Some(client_manager) = self.client_manager.get() else {
            return Err(ErrorObjectOwned::owned(-32000, "pairing not available", None::<()>));
        };
        match client_manager.register_pin(&id, &pin) {
            Ok(()) => Ok(PinResult { paired: true }),
            Err(()) => Err(ErrorObjectOwned::owned(-32000, "unknown client or bad pin", None::<()>)),
        }
    }
}
