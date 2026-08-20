//! Integration tests for the control socket client against a minimal
//! `reth_ipc`/`jsonrpsee` server modeled on `rgsp-host/tests/control.rs`
//! (Task 2) and `vendor/reth-ipc/src/server/mod.rs`'s own test module.
//! `rgsp-ui` does not depend on `rgsp-host`, so the server here is built
//! directly from a plain `RpcModule` rather than the daemon's real handle.

use jsonrpsee::{PendingSubscriptionSink, RpcModule, SubscriptionMessage};
use rgsp_ui::rpc::{CastState, Control, PinOutcome, PinResult};
use serde::Deserialize;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

fn socket_path(name: &str) -> String {
    format!("{}/rgsp-ui-test-{}-{}.sock", std::env::temp_dir().display(), name, std::process::id())
}

#[test]
fn connect_fails_when_the_daemon_is_not_running() {
    let path = format!("{}/rgsp-absent-{}.sock", std::env::temp_dir().display(), std::process::id());
    assert!(
        Control::connect(&path).is_err(),
        "a refused connect is how the UI learns the service is stopped"
    );
}

#[derive(Deserialize)]
struct SubmitPinParams {
    #[expect(dead_code)]
    id: String,
    #[expect(dead_code)]
    pin: String,
}

/// Starts a server exposing `state_subscribe`/`state_unsubscribe` (pushing
/// `initial`, then nothing further unless the caller pokes the returned
/// sink registration) and `submit_pin` returning `{"paired":true}`.
async fn start_server(path: &str, initial: CastState) -> jsonrpsee::server::ServerHandle {
    let mut module = RpcModule::new(initial);

    module
        .register_subscription(
            "state_subscribe",
            "state_subscribe",
            "state_unsubscribe",
            |_params, pending: PendingSubscriptionSink, state: Arc<CastState>, _| async move {
                let sink = pending.accept().await?;
                let raw = serde_json::value::to_raw_value(&*state)?;
                sink.send(SubscriptionMessage::from(raw)).await?;
                Ok::<(), jsonrpsee::core::SubscriptionError>(())
            },
        )
        .expect("register state_subscribe");

    module
        .register_method("submit_pin", |params, _ctx, _| {
            // The UI sends the spec's object shape `{"id":..., "pin":...}`;
            // parsing into a named-field struct proves the object shape
            // decodes, mirroring rgsp-host's `submit_pin_accepts_the_spec_object_shape`.
            let _: SubmitPinParams = params.parse().expect("object-shaped params");
            Ok::<PinResult, jsonrpsee::types::ErrorObjectOwned>(PinResult { paired: true })
        })
        .expect("register submit_pin");

    let server = reth_ipc::server::Builder::default().build(path.to_string());
    server.start(module).await.expect("start server")
}

#[test]
fn snapshot_arrives_on_subscribe_and_submit_pin_round_trips() {
    let path = socket_path("snapshot-and-pin");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let _server = runtime.block_on(start_server(
        &path,
        CastState { casting: false, client: None, pending: Vec::new() },
    ));

    let mut control = Control::connect(&path).expect("connect");
    assert!(control.is_connected());

    // Poll until the snapshot lands; poll_state is non-blocking so a single
    // call may race the server's first send.
    let mut snapshot = None;
    for _ in 0..200 {
        if let Some(state) = control.poll_state() {
            snapshot = Some(state);
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let snapshot = snapshot.expect("snapshot should arrive within 2s");
    assert!(!snapshot.casting);
    assert!(snapshot.pending.is_empty());

    let outcome = control.submit_pin("some-client-id", "1234").expect("submit_pin");
    assert_eq!(outcome, PinOutcome::Paired);
    assert!(control.is_connected());
}

// Mutation check: every other test in this file only ever exercises the
// `connected == true` side. Delete both `self.connected = false;` lines in
// `rpc.rs` and this is the only test that would notice — everything else
// still passes. `poll_state`'s stream-ended/errored arm and `submit_pin`'s
// non-`Call` error arm both set it; this pins the latter, which is easier to
// trigger deterministically than waiting on the subscription stream to end.
#[test]
fn a_transport_failure_marks_the_connection_dead() {
    let path = socket_path("transport-failure");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let server = runtime.block_on(start_server(
        &path,
        CastState { casting: false, client: None, pending: Vec::new() },
    ));

    let mut control = connected_control(&path);
    assert!(control.is_connected(), "a fresh connection to a live server must report connected");

    // Tear the server down out from under the already-connected client —
    // this is the transport failure `submit_pin` should detect, as opposed
    // to the RPC-level errors covered elsewhere in this file.
    server.stop().expect("stop server");
    runtime.block_on(server.stopped());

    let result = control.submit_pin("some-id", "0000");
    assert!(result.is_err(), "a dead server must not look like a successful (or even a rejected) pairing");
    assert!(!control.is_connected(), "a transport failure, unlike an RPC error, must mark the client disconnected");
}

/// Starts a server whose `submit_pin` always fails with the given wire code
/// (mirroring the daemon's real codes: `-32000` "pairing not available",
/// `-32001` "unknown client or bad pin" — see `rgsp_host::control::PinApiServer`).
async fn start_pin_error_server(
    path: &str,
    code: i32,
    message: &'static str,
    called: Arc<AtomicBool>,
) -> jsonrpsee::server::ServerHandle {
    let mut module = RpcModule::new(());
    module
        .register_subscription(
            "state_subscribe",
            "state_subscribe",
            "state_unsubscribe",
            |_params, pending: PendingSubscriptionSink, _state: Arc<()>, _| async move {
                let sink = pending.accept().await?;
                let state = CastState { casting: false, client: None, pending: Vec::new() };
                let raw = serde_json::value::to_raw_value(&state)?;
                sink.send(SubscriptionMessage::from(raw)).await?;
                Ok::<(), jsonrpsee::core::SubscriptionError>(())
            },
        )
        .expect("register state_subscribe");
    module
        .register_method("submit_pin", move |_params, _ctx, _| {
            called.store(true, Ordering::SeqCst);
            Err::<PinResult, _>(jsonrpsee::types::ErrorObjectOwned::owned(code, message, None::<()>))
        })
        .expect("register submit_pin");

    let server = reth_ipc::server::Builder::default().build(path.to_string());
    server.start(module).await.expect("start server")
}

/// Connects and drains the initial snapshot so the caller starts from a
/// known-clean subscription state. The server this dials must already be
/// running on a runtime that outlives this call.
fn connected_control(path: &str) -> Control {
    let mut control = Control::connect(path).expect("connect");
    for _ in 0..200 {
        if control.poll_state().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    control
}

// Pins the distinction the daemon's two wire codes must keep: -32000 means
// "not ready yet, retry" and -32001 means "rejected, re-enter the PIN". A
// client that collapsed both into one case (or matched on message text)
// would not catch these two codes drifting back together.
#[test]
fn submit_pin_distinguishes_not_ready_from_rejected() {
    let not_ready_path = socket_path("pin-not-ready");
    let rejected_path = socket_path("pin-rejected");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let not_ready_called = Arc::new(AtomicBool::new(false));
    let rejected_called = Arc::new(AtomicBool::new(false));

    let _not_ready_server = runtime.block_on(start_pin_error_server(
        &not_ready_path,
        -32000,
        "pairing not available",
        not_ready_called.clone(),
    ));
    let _rejected_server = runtime.block_on(start_pin_error_server(
        &rejected_path,
        -32001,
        "unknown client or bad pin",
        rejected_called.clone(),
    ));

    let mut not_ready_control = connected_control(&not_ready_path);
    let outcome = not_ready_control.submit_pin("some-id", "0000").expect("not-ready is not a transport error");
    assert_eq!(outcome, PinOutcome::NotReady);
    assert!(not_ready_called.load(Ordering::SeqCst));
    assert!(not_ready_control.is_connected(), "a live daemon's answer must not look like a dead connection");

    let mut rejected_control = connected_control(&rejected_path);
    let outcome = rejected_control.submit_pin("some-id", "9999").expect("rejected is not a transport error");
    assert_eq!(outcome, PinOutcome::Rejected);
    assert!(rejected_called.load(Ordering::SeqCst));
    assert!(rejected_control.is_connected(), "a live daemon's answer must not look like a dead connection");
}
