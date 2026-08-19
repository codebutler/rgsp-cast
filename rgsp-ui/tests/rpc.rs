//! Integration tests for the control socket client against a minimal
//! `reth_ipc`/`jsonrpsee` server modeled on `rgsp-host/tests/control.rs`
//! (Task 2) and `vendor/reth-ipc/src/server/mod.rs`'s own test module.
//! `rgsp-ui` does not depend on `rgsp-host`, so the server here is built
//! directly from a plain `RpcModule` rather than the daemon's real handle.

use jsonrpsee::{PendingSubscriptionSink, RpcModule, SubscriptionMessage};
use rgsp_ui::rpc::{CastState, Control, PinResult};
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

    let paired = control.submit_pin("some-client-id", "1234").expect("submit_pin");
    assert!(paired);
    assert!(control.is_connected());
}

#[test]
fn submit_pin_surfaces_an_rpc_error_without_marking_the_connection_dead() {
    let path = socket_path("pin-error");
    let runtime = tokio::runtime::Runtime::new().expect("runtime");
    let called = Arc::new(AtomicBool::new(false));
    let called_in_handler = called.clone();

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
            called_in_handler.store(true, Ordering::SeqCst);
            Err::<PinResult, _>(jsonrpsee::types::ErrorObjectOwned::owned(
                -32000,
                "pairing not available",
                None::<()>,
            ))
        })
        .expect("register submit_pin");

    let server = reth_ipc::server::Builder::default().build(path.clone());
    let _server = runtime.block_on(server.start(module)).expect("start server");

    let mut control = Control::connect(&path).expect("connect");
    for _ in 0..200 {
        if control.poll_state().is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let err = control.submit_pin("some-id", "0000").expect_err("pairing not available");
    assert!(err.to_string().contains("pairing not available"), "unexpected error: {err}");
    assert!(called.load(Ordering::SeqCst));
    assert!(control.is_connected(), "an RPC error from a live daemon must not look like a dead connection");
}
