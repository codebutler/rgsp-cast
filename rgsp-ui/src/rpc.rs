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
    pub casting: bool,
    pub client: Option<String>,
    pub pending: Vec<PendingEntry>,
}

/// Mirrors `rgsp_host::control::PinResult`.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct PinResult {
    pub paired: bool,
}

const SUBMIT_PIN_TIMEOUT: Duration = Duration::from_secs(5);

/// A live connection to the daemon's control socket.
///
/// Owns a current-thread `tokio` runtime so screens (which live on a plain
/// synchronous frame loop) can drive it without becoming async themselves.
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
    /// connection drop.
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

    /// Submits a pairing PIN. Returns `Ok(true)`/`Ok(false)` for
    /// `PinResult::paired`. An RPC error such as `-32000 "pairing not
    /// available"` (the daemon's startup window before its `ClientManager`
    /// exists) is a legitimate, expected response — it surfaces as `Err`,
    /// same as any other pairing failure, without marking the connection
    /// dead. Only a transport-level failure does that.
    pub fn submit_pin(&mut self, id: &str, pin: &str) -> anyhow::Result<bool> {
        let mut params = ObjectParams::new();
        params.insert("id", id).context("encoding id")?;
        params.insert("pin", pin).context("encoding pin")?;

        let outcome = self.runtime.block_on(async {
            tokio::time::timeout(SUBMIT_PIN_TIMEOUT, self.client.request::<PinResult, _>("submit_pin", params))
                .await
        });

        match outcome {
            Ok(Ok(PinResult { paired })) => Ok(paired),
            Ok(Err(err @ jsonrpsee::core::client::Error::Call(_))) => {
                // A well-formed RPC error (e.g. "pairing not available")
                // means the daemon is alive and answered — not a dropped
                // connection.
                Err(err.into())
            }
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
