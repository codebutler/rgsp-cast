use jsonrpsee::core::client::SubscriptionClientT;
use jsonrpsee::rpc_params;
use rgsp_host::control::{CastState, ControlHandle};
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
async fn connect_fails_when_nothing_is_listening() {
    let path = socket_path("absent");
    let err = reth_ipc::client::IpcClientBuilder::default().build(&path).await;
    assert!(err.is_err(), "connecting to an unbound socket must fail — this is the liveness check");
}
