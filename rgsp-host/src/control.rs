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
use std::time::Duration;
use tokio::sync::Notify;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingEntry {
    pub id: String,
    pub name: Option<String>,
    pub address: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CastState {
    pub casting: bool,
    pub client: Option<String>,
    pub pending: Vec<PendingEntry>,
}

/// `paired` is the *pairing outcome*, not "the PIN was accepted for
/// delivery": see [`PinApiServer::submit_pin`], which waits for the
/// handshake to finish before answering.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PinResult {
    pub paired: bool,
}

/// How long [`PinApiServer::submit_pin`] waits for Moonlight to finish the
/// handshake before calling the PIN wrong.
///
/// Steps 4 and 5 of the pairing exchange are two HTTP round trips on the
/// same LAN — sub-second in practice — so anything past this is a client
/// that gave up, and the overwhelmingly common reason for that is a wrong
/// PIN. It must also stay comfortably under the UI's own 5s
/// `SUBMIT_PIN_TIMEOUT` (`rgsp-ui/src/rpc.rs`): if the UI gave up first, a
/// wrong PIN would surface there as a transport error rather than as
/// "PIN rejected". The UI shows a "Pairing..." frame for the duration, so
/// the wait reads as progress rather than as a hang.
const PAIRING_TIMEOUT: Duration = Duration::from_secs(3);

#[rpc(server, namespace = "state")]
pub trait ControlApi {
    #[subscription(name = "subscribe", unsubscribe = "unsubscribe", item = CastState)]
    async fn subscribe(&self) -> SubscriptionResult;
}

#[rpc(server)]
pub trait PinApi {
    // `param_kind = map`: the spec's UI client sends `{"id":..., "pin":...}`,
    // a JSON object. The server-side decoder jsonrpsee 0.26 generates
    // actually branches on `params.is_object()` at request time regardless
    // of this attribute (verified: `tests/control.rs` passes the object
    // shape even with the attribute removed), so this alone doesn't gate
    // acceptance. It's kept because `param_kind` *does* control what shape
    // the generated `PinApiClient` trait would encode if this crate ever
    // called `submit_pin` through it instead of a raw `request(...)` — and
    // because it documents, at the trait definition, the shape the spec
    // actually promises callers.
    #[method(name = "submit_pin", param_kind = map)]
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

        // Register interest before reading anything: `Notify::notify_waiters`
        // stores no permit, it only wakes waiters already registered, so a
        // change landing between reading the snapshot and this call would
        // otherwise be silently dropped. `enable()` (not just `notified()`)
        // is what actually registers us — `notified()` alone only registers
        // once polled.
        let changed = self.changed.clone();
        let notified = changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        // Snapshot first: a late subscriber must not wait for the next change.
        let msg: jsonrpsee::server::SubscriptionMessage = to_json_raw_value(&self.snapshot())?.into();
        sink.send(msg).await?;

        loop {
            tokio::select! {
                _ = notified.as_mut() => {
                    // Re-register immediately, before the `send` below can
                    // suspend for a while: two `notify_waiters()` calls can
                    // land back-to-back (e.g. `set_client` then
                    // `set_casting`), and `CastState` is a full replacement,
                    // not a diff, so missing the second one here would leave
                    // the UI showing a stale mix indefinitely, not just late.
                    notified.set(changed.notified());
                    notified.as_mut().enable();

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

/// Waits until `still_pending()` reports false, or `timeout` elapses.
/// Returns whether it resolved (`true`) rather than timing out.
///
/// `changed` must be a `Notify` that fires on every mutation of whatever
/// `still_pending` reads. Kept generic over both so it can be tested
/// without fabricating a pending client — `ClientManager::add_pending` is
/// `pub(crate)` in the vendored moonshine-core and stays that way.
///
/// The ordering here is the one this codebase keeps having to relearn:
/// `Notify::notify_waiters` stores no permit, and `notified()` only
/// registers a waiter once polled, so the future is built and `enable()`d
/// *before* every read of the state. Otherwise a change landing between the
/// read and the await is dropped and this waits out the full timeout on an
/// outcome that already happened.
async fn wait_until_resolved(
    changed: &Notify,
    still_pending: impl Fn() -> bool,
    timeout: Duration,
) -> bool {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let notified = changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();

        if !still_pending() {
            return true;
        }
        if tokio::time::timeout_at(deadline, notified).await.is_err() {
            // One last read: the state can have changed in the instant
            // between the timer firing and us giving up.
            return !still_pending();
        }
    }
}

#[async_trait]
impl PinApiServer for ControlHandle {
    /// Answers with the *pairing outcome*, not with "the PIN was stored".
    ///
    /// `ClientManager::register_pin` does not validate anything: it looks the
    /// client up, derives a key from the PIN and returns `Ok`, so a wrong PIN
    /// registers exactly as happily as a right one (`clients.rs`). The real
    /// check is `verify_pairing_secret`, two protocol steps later, inside
    /// `check_client_pairing_secret` — and that is also where a client is
    /// removed from the pending set. So the pending set is the honest signal:
    /// the id disappearing from it means pairing genuinely completed, and its
    /// still being there once [`PAIRING_TIMEOUT`] has passed means the
    /// handshake failed, which on this path means the PIN was wrong.
    ///
    /// Answering `paired: true` on `register_pin` alone was actively harmful,
    /// not merely optimistic: the user was told a wrong PIN worked, and their
    /// retry from the still-listed pending row did nothing at all — the pair
    /// handler awaiting `pin_notify` had already returned, and a fresh
    /// `Notify` is only created by a new `/pair` POST.
    async fn submit_pin(&self, id: String, pin: String) -> RpcResult<PinResult> {
        let Some(client_manager) = self.client_manager.get() else {
            return Err(ErrorObjectOwned::owned(-32000, "pairing not available", None::<()>));
        };
        if client_manager.register_pin(&id, &pin).is_err() {
            // `register_pin`'s only failure is an id it has never heard of
            // (or a poisoned lock). Nothing about the PIN itself is checked
            // here, so the message must not claim otherwise.
            return Err(ErrorObjectOwned::owned(-32001, "unknown client", None::<()>));
        }

        let changed = client_manager.pending_changed();
        let paired = wait_until_resolved(
            &changed,
            || client_manager.pending_clients().iter().any(|p| p.id == id),
            PAIRING_TIMEOUT,
        )
        .await;

        if paired {
            Ok(PinResult { paired: true })
        } else {
            Err(ErrorObjectOwned::owned(-32001, "pairing did not complete; check the PIN", None::<()>))
        }
    }
}

#[cfg(test)]
mod wait_until_resolved_tests {
    use super::{Notify, wait_until_resolved};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn an_already_resolved_state_answers_without_waiting() {
        let changed = Notify::new();
        let began = std::time::Instant::now();
        assert!(wait_until_resolved(&changed, || false, Duration::from_secs(5)).await);
        assert!(began.elapsed() < Duration::from_secs(1), "it must not wait for a notification it does not need");
    }

    #[tokio::test]
    async fn a_state_that_never_resolves_times_out() {
        let changed = Notify::new();
        assert!(!wait_until_resolved(&changed, || true, Duration::from_millis(100)).await);
    }

    #[tokio::test]
    async fn resolving_while_waiting_is_seen() {
        let changed = Arc::new(Notify::new());
        let pending = Arc::new(AtomicBool::new(true));

        let flipper = {
            let changed = changed.clone();
            let pending = pending.clone();
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(50)).await;
                pending.store(false, Ordering::SeqCst);
                changed.notify_waiters();
            })
        };

        let pending_read = pending.clone();
        assert!(
            wait_until_resolved(&changed, move || pending_read.load(Ordering::SeqCst), Duration::from_secs(5)).await
        );
        flipper.await.expect("flipper");
    }

    #[tokio::test]
    async fn an_unrelated_change_does_not_end_the_wait() {
        // The pending set churns during the handshake (four `notify_waiters`
        // in a row is normal), so a fire that leaves the client pending must
        // re-arm and keep waiting rather than being read as "paired".
        let changed = Arc::new(Notify::new());
        let noise = {
            let changed = changed.clone();
            tokio::spawn(async move {
                for _ in 0..4 {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                    changed.notify_waiters();
                }
            })
        };

        assert!(!wait_until_resolved(&changed, || true, Duration::from_millis(150)).await);
        noise.await.expect("noise");
    }
}
