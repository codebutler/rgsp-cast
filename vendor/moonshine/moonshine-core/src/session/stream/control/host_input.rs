//! Forwarding client input to the host.
//!
//! The control stream decrypts and length-checks the client's `InputData`
//! messages and would otherwise drop them. This hands the payload to the host,
//! which owns the decision of what an input event means on this device (see
//! `rgsp_host::input`); nothing here interprets the bytes.
//!
//! Kept in its own file, like the video and audio `host_source` modules, so the
//! surrounding upstream code stays mergeable.

use tokio::sync::mpsc;

/// Sender half handed to the control loop. Cloneable so a resumed session can
/// keep feeding the same host-side receiver.
#[derive(Clone)]
pub(crate) struct InputForwarder {
    tx: mpsc::Sender<Vec<u8>>,
}

impl InputForwarder {
    pub(crate) fn new(tx: mpsc::Sender<Vec<u8>>) -> Self {
        Self { tx }
    }

    /// Hand one input payload to the host.
    ///
    /// Drops rather than blocks when the queue is full. Input is a stream of
    /// absolute state - every packet carries the complete set of buttons held
    /// at that instant - so a dropped packet costs at most a few milliseconds
    /// of staleness, and the next one restores the truth. Blocking the control
    /// loop instead would stall pings and IDR requests, which is far worse.
    ///
    /// A release can be dropped as easily as a press, so the host must not
    /// treat the last packet it saw as authoritative forever; it releases
    /// everything when the session ends.
    pub(crate) fn forward(&self, payload: &[u8]) {
        if self.tx.try_send(payload.to_vec()).is_err() {
            tracing::trace!("input queue full or closed, dropping one packet");
        }
    }
}
