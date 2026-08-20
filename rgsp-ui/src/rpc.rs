//! Control socket client.
//!
//! A socket rather than a state file on purpose: a file outlives the process
//! that wrote it, so a killed daemon would leave a stale "casting" behind and
//! the UI would report Running for a corpse. A failed [`Control::connect`] is
//! the authoritative answer that the service is stopped.

use anyhow::Context;
use jsonrpsee::core::client::{ClientT, Subscription, SubscriptionClientT};
use jsonrpsee::core::params::ObjectParams;
use jsonrpsee::rpc_params;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Mirrors `rgsp_host::control::PendingEntry`. Redeclared locally (not
/// depended on) so `rgsp-ui` doesn't pull in the daemon crate; field names
/// must match for serde compatibility.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PendingEntry {
    pub id: String,
    pub name: Option<String>,
}

/// Mirrors `rgsp_host::control::CastState`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CastState {
    /// A Moonlight client is **actively streaming** right now — not "the
    /// daemon is running". `rgsp-host` sets this when a session starts and
    /// clears it when the session ends, so it is false for a healthy idle
    /// daemon. Liveness is a separate question, answered by whether the
    /// control socket connects at all ([`Control::is_connected`]).
    ///
    /// Conflating the two shipped a bug: the home screen showed "Stopped"
    /// for a running-but-idle daemon, so the service could never be stopped
    /// from the UI and every press spawned a doomed second daemon.
    pub casting: bool,
    pub client: Option<String>,
    pub pending: Vec<PendingEntry>,
}

/// Mirrors `rgsp_host::control::PinResult`. `paired` is the real pairing
/// outcome: the daemon waits for the handshake to finish before answering.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PinResult {
    pub paired: bool,
}

const SUBMIT_PIN_TIMEOUT: Duration = Duration::from_secs(5);

/// The daemon's control socket uses distinct JSON-RPC error codes for the
/// two ways `submit_pin` can fail while the daemon is alive and answering
/// (`rgsp_host::control::PinApiServer::submit_pin`):
/// - `-32000` "pairing not available" — the daemon's `ClientManager` isn't
///   wired up yet (its startup window). The caller should wait and retry.
/// - `-32001` — the pairing did not complete: either the id is one the
///   daemon has never heard of, or the handshake failed to finish within
///   the daemon's own pairing timeout, which is what a wrong PIN looks
///   like from here (the daemon only learns a PIN was wrong two protocol
///   steps after it was submitted, when the client's pairing secret fails
///   to verify). The caller should have the user re-enter the PIN.
///
/// A transport-level failure (dropped connection, timeout) is a third,
/// separate case — it surfaces as `Err` from [`Control::submit_pin`], not as
/// a variant here, since it means the daemon never answered at all.
const NOT_READY_CODE: i32 = -32000;
const REJECTED_CODE: i32 = -32001;

/// The daemon's answer to `submit_pin`, once it has actually answered (as
/// opposed to a transport failure, which is a plain `Err`). See the module
/// constants above for the wire codes this distinguishes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PinOutcome {
    /// Pairing succeeded.
    Paired,
    /// The daemon isn't ready to pair yet — retry.
    NotReady,
    /// The daemon rejected the id/pin — have the user re-enter it.
    Rejected,
}

/// A live connection to the daemon's control socket.
///
/// Owns a current-thread `tokio` runtime so screens (which live on a plain
/// synchronous frame loop) can drive it without becoming async themselves.
///
/// There is no reconnect logic: once a transport failure latches
/// `connected` to `false` (see [`Control::is_connected`]), it stays `false`
/// for the rest of this instance's life. That is by design, not an
/// oversight — a fresh [`Control::connect`] *is* how the UI retries, since
/// the connect itself is the liveness check this type exists to provide. A
/// caller that sees `is_connected() == false` must construct a new
/// `Control` to try again; this one will not heal itself.
pub struct Control {
    runtime: tokio::runtime::Runtime,
    client: jsonrpsee::async_client::Client,
    sub: Subscription<CastState>,
    connected: bool,
}

impl Control {
    /// Connects to the daemon's control socket at `path` and subscribes to
    /// state changes. A failed connect means the daemon is not running —
    /// that is expected, not exceptional, and callers should treat it as the
    /// liveness answer rather than logging it as an error.
    pub fn connect(path: &str) -> anyhow::Result<Self> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .context("building control socket runtime")?;

        let (client, sub) = runtime.block_on(async {
            let client = reth_ipc::client::IpcClientBuilder::default()
                .build(path)
                .await
                .context("connecting to control socket")?;
            let sub = client
                .subscribe::<CastState, _>("state_subscribe", rpc_params![], "state_unsubscribe")
                .await
                .context("subscribing to state")?;
            anyhow::Ok((client, sub))
        })?;

        Ok(Self { runtime, client, sub, connected: true })
    }

    /// True once `connect` succeeded and no subsequent call has observed the
    /// connection drop. Latches to `false` permanently once a transport
    /// failure is observed — there is no reconnect, so a `false` here means
    /// this `Control` is done; the caller must `connect` a new one.
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Non-blocking: returns the next state update, or `None` if there isn't
    /// one this frame. The UI redraws continuously, so this must never block
    /// waiting on the daemon — it drives the runtime with a zero timeout.
    ///
    /// The server sends a full snapshot immediately on subscribe, then one
    /// message per change, so the first call after `connect` surfaces that
    /// snapshot.
    pub fn poll_state(&mut self) -> Option<CastState> {
        self.runtime.block_on(async {
            match tokio::time::timeout(Duration::ZERO, self.sub.next()).await {
                Ok(Some(Ok(state))) => Some(state),
                Ok(Some(Err(_))) | Ok(None) => {
                    self.connected = false;
                    None
                }
                Err(_) => None, // nothing pending this frame
            }
        })
    }

    /// Submits a pairing PIN. Returns a [`PinOutcome`] once the daemon has
    /// answered — success, not-ready-yet, or rejected are all legitimate,
    /// expected responses, not crashes, and none of them mark the
    /// connection dead. Only a transport-level failure (dropped connection,
    /// timeout) does that, and surfaces as `Err`.
    pub fn submit_pin(&mut self, id: &str, pin: &str) -> anyhow::Result<PinOutcome> {
        let mut params = ObjectParams::new();
        params.insert("id", id).context("encoding id")?;
        params.insert("pin", pin).context("encoding pin")?;

        let outcome = self.runtime.block_on(async {
            tokio::time::timeout(SUBMIT_PIN_TIMEOUT, self.client.request::<PinResult, _>("submit_pin", params))
                .await
        });

        match outcome {
            Ok(Ok(PinResult { paired: true })) => Ok(PinOutcome::Paired),
            Ok(Ok(PinResult { paired: false })) => {
                // The daemon only ever returns `Ok` on success (see
                // `PinApiServer::submit_pin`) — a false `paired` here would
                // be a wire-contract surprise, not one of the two documented
                // failure codes, so it doesn't fit `PinOutcome`.
                Err(anyhow::anyhow!("submit_pin succeeded but reported paired: false"))
            }
            Ok(Err(jsonrpsee::core::client::Error::Call(err))) => match err.code() {
                NOT_READY_CODE => Ok(PinOutcome::NotReady),
                REJECTED_CODE => Ok(PinOutcome::Rejected),
                // An RPC error with a code the client doesn't recognize is
                // still an answer from a live daemon, not a dropped
                // connection — but it's not one of the two documented
                // outcomes either, so it surfaces as a plain error rather
                // than being forced into `PinOutcome`.
                _ => Err(err.into()),
            },
            Ok(Err(err)) => {
                self.connected = false;
                Err(err.into())
            }
            Err(_) => {
                self.connected = false;
                Err(anyhow::anyhow!("submit_pin timed out after {SUBMIT_PIN_TIMEOUT:?}"))
            }
        }
    }
}
