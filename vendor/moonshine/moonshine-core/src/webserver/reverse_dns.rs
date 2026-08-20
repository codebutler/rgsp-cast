//! Best-effort reverse-DNS labelling of a pairing peer.
//!
//! Every identifying field Moonlight sends during pairing is a hardcoded
//! protocol constant (`devicename=roth`, `uniqueid=0123456789ABCDEF`, even
//! the client cert CN) - the protocol itself tells us nothing about who is
//! pairing. The peer's IP address at least differs per machine, and on most
//! home networks the router serves PTR records from DHCP hostnames, so a
//! reverse lookup often turns that IP into something a human recognizes
//! (`192.168.180.115` -> `MacBookPro`).

use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

/// How long we'll wait for a reverse-DNS lookup of a pairing peer before
/// giving up and falling back to the bare IP.
///
/// A resolved hostname is a nicety for the on-device pairing UI, never
/// something pairing itself should be blocked on: a slow or unreachable DNS
/// server must not stall the Moonlight client waiting on this HTTP handler.
/// A second is generous for a LAN PTR lookup and short enough that a dead
/// resolver is barely noticeable.
const REVERSE_DNS_TIMEOUT: Duration = Duration::from_secs(1);

/// Resolve a display label for a pairing peer: a reverse-DNS hostname when
/// one exists, otherwise the bare IP address as a string. Returns `None`
/// only when there was no peer address to begin with.
///
/// `getnameinfo(3)` is a blocking libc call, so the lookup runs on a
/// blocking-pool thread via [`tokio::task::spawn_blocking`] and is bounded
/// by [`REVERSE_DNS_TIMEOUT`]. A timeout, a lookup error (including the
/// ordinary `NXDOMAIN` case - most networks have no PTR record for most
/// peers), or a panicked lookup task all fall back to the plain IP rather
/// than surfacing as an error: absent PTR records are the common case on
/// some networks, not a fault.
pub(crate) async fn resolve_label(peer_ip: Option<IpAddr>) -> Option<String> {
	let ip = peer_ip?;

	let lookup = tokio::task::spawn_blocking(move || reverse_lookup_blocking(ip));
	let hostname = match tokio::time::timeout(REVERSE_DNS_TIMEOUT, lookup).await {
		Ok(Ok(hostname)) => hostname,
		Ok(Err(join_error)) => {
			tracing::warn!("Reverse-DNS lookup task for {ip} panicked: {join_error}");
			None
		},
		Err(_) => {
			tracing::debug!("Reverse-DNS lookup for {ip} did not finish within {REVERSE_DNS_TIMEOUT:?}");
			None
		},
	};

	Some(hostname.unwrap_or_else(|| ip.to_string()))
}

/// Blocking `getnameinfo(3)` call. Must only be run via `spawn_blocking`.
fn reverse_lookup_blocking(ip: IpAddr) -> Option<String> {
	let sockaddr = socket2::SockAddr::from(SocketAddr::new(ip, 0));
	let mut host = [0 as libc::c_char; libc::NI_MAXHOST as usize];

	// SAFETY: `sockaddr` owns valid, correctly-sized storage for `ip` (IPv4
	// or IPv6), and `host` is a correctly-sized buffer we alone own for the
	// duration of this call.
	let rc = unsafe {
		libc::getnameinfo(
			sockaddr.as_ptr() as *const libc::sockaddr,
			sockaddr.len(),
			host.as_mut_ptr(),
			host.len() as libc::socklen_t,
			std::ptr::null_mut(),
			0,
			0,
		)
	};
	if rc != 0 {
		// No PTR record (NXDOMAIN) lands here on plenty of networks - this
		// handheld's own LAN included. Unremarkable, not a warning.
		return None;
	}

	// SAFETY: `getnameinfo` returned success, so `host` holds a
	// NUL-terminated C string written within the buffer we gave it.
	let hostname = unsafe { std::ffi::CStr::from_ptr(host.as_ptr()) }.to_str().ok()?;

	tidy_hostname(hostname, ip)
}

/// Tidy a resolved hostname for display.
///
/// Strips a trailing root-zone dot and a trailing `.lan`/`.local` suffix
/// (both common on home routers serving PTR records from DHCP hostnames),
/// and treats a "resolution" that just echoes the IP back as no result at
/// all - some resolvers do that instead of returning an error. Anything
/// else, such as a name with more structure like `nas.home.example.com`,
/// is left untouched.
fn tidy_hostname(hostname: &str, ip: IpAddr) -> Option<String> {
	let trimmed = hostname.trim_end_matches('.');
	let trimmed = trimmed
		.strip_suffix(".lan")
		.or_else(|| trimmed.strip_suffix(".local"))
		.unwrap_or(trimmed);

	if trimmed.is_empty() || trimmed == ip.to_string() {
		return None;
	}

	Some(trimmed.to_string())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn strips_trailing_dot() {
		let ip: IpAddr = "192.168.180.115".parse().unwrap();
		assert_eq!(tidy_hostname("MacBookPro.", ip).as_deref(), Some("MacBookPro"));
	}

	#[test]
	fn strips_trailing_lan_suffix() {
		let ip: IpAddr = "192.168.180.115".parse().unwrap();
		assert_eq!(tidy_hostname("MacBookPro.lan", ip).as_deref(), Some("MacBookPro"));
	}

	#[test]
	fn strips_trailing_local_suffix() {
		let ip: IpAddr = "192.168.180.115".parse().unwrap();
		assert_eq!(tidy_hostname("MacBookPro.local", ip).as_deref(), Some("MacBookPro"));
	}

	#[test]
	fn strips_trailing_dot_and_lan_suffix_together() {
		let ip: IpAddr = "192.168.180.115".parse().unwrap();
		assert_eq!(tidy_hostname("MacBookPro.lan.", ip).as_deref(), Some("MacBookPro"));
	}

	#[test]
	fn leaves_a_structured_name_alone() {
		let ip: IpAddr = "192.168.180.115".parse().unwrap();
		assert_eq!(
			tidy_hostname("nas.home.example.com", ip).as_deref(),
			Some("nas.home.example.com")
		);
	}

	#[test]
	fn treats_a_resolution_that_echoes_the_ip_as_no_result() {
		let ip: IpAddr = "192.168.180.115".parse().unwrap();
		assert_eq!(tidy_hostname("192.168.180.115", ip), None);
		assert_eq!(tidy_hostname("192.168.180.115.", ip), None);
	}

	#[tokio::test]
	async fn no_peer_address_resolves_to_no_label() {
		assert_eq!(resolve_label(None).await, None);
	}
}
