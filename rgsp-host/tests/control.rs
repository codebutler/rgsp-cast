use jsonrpsee::core::client::{ClientT, SubscriptionClientT};
use jsonrpsee::core::params::ObjectParams;
use jsonrpsee::rpc_params;
use rgsp_host::control::{CastState, ControlHandle, PinResult};
use std::time::Duration;

fn socket_path(name: &str) -> String {
    format!("{}/rgsp-test-{}-{}.sock", std::env::temp_dir().display(), name, std::process::id())
}

#[tokio::test]
async fn subscribe_delivers_a_snapshot_immediately() {
    let path = socket_path("snapshot");
    let handle = ControlHandle::new();
    let server = handle.clone().serve(&path).await.expect("serve");

    let client = reth_ipc::client::IpcClientBuilder::default()
        .build(&path)
        .await
        .expect("connect");
    let mut sub = client
        .subscribe::<CastState, _>("state_subscribe", rpc_params![], "state_unsubscribe")
        .await
        .expect("subscribe");

    let first = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .expect("no snapshot within 2s")
        .expect("stream ended")
        .expect("decode");
    assert!(!first.casting, "a fresh handle is not casting");
    assert!(first.pending.is_empty());

    server.stop().unwrap();
}

#[tokio::test]
async fn a_change_pushes_without_polling() {
    let path = socket_path("push");
    let handle = ControlHandle::new();
    let server = handle.clone().serve(&path).await.expect("serve");

    let client = reth_ipc::client::IpcClientBuilder::default()
        .build(&path)
        .await
        .expect("connect");
    let mut sub = client
        .subscribe::<CastState, _>("state_subscribe", rpc_params![], "state_unsubscribe")
        .await
        .expect("subscribe");
    let _snapshot = sub.next().await.expect("stream ended").expect("decode");

    handle.set_casting(true);

    let pushed = tokio::time::timeout(Duration::from_secs(2), sub.next())
        .await
        .expect("no push within 2s")
        .expect("stream ended")
        .expect("decode");
    assert!(pushed.casting, "the change should have been pushed");

    server.stop().unwrap();
}

#[tokio::test]
async fn submit_pin_accepts_the_spec_object_shape() {
    // The UI client sends `{"id":..., "pin":...}` — a JSON object, not a
    // positional array. jsonrpsee defaults to array params, which would
    // reject this payload with "Invalid params" before it ever reached the
    // handler. This pins the wire shape by asserting the call gets *past*
    // decoding: a handle with no `ClientManager` wired in has nothing to
    // pair with, so the request must fail with "pairing not available", not
    // a params-decode error.
    let path = socket_path("submit-pin-shape");
    let handle = ControlHandle::new();
    let server = handle.clone().serve(&path).await.expect("serve");

    let client = reth_ipc::client::IpcClientBuilder::default()
        .build(&path)
        .await
        .expect("connect");

    let mut params = ObjectParams::new();
    params.insert("id", "some-client-id").expect("serialize id");
    params.insert("pin", "1234").expect("serialize pin");
    let result: Result<PinResult, _> = client.request("submit_pin", params).await;

    let err = result.expect_err("no ClientManager is wired in, so pairing cannot succeed");
    let message = err.to_string();
    assert!(
        message.contains("pairing not available"),
        "expected the pairing-not-available error, got a different failure \
         (possibly a params-decode error if the object shape were rejected): {message}"
    );

    server.stop().unwrap();
}

#[tokio::test]
async fn connect_fails_when_nothing_is_listening() {
    let path = socket_path("absent");
    let err = reth_ipc::client::IpcClientBuilder::default().build(&path).await;
    assert!(err.is_err(), "connecting to an unbound socket must fail — this is the liveness check");
}
