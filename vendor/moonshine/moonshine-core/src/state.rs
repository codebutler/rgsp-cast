use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};

use serde::{Deserialize, Serialize};

/// The human-readable label captured for a paired client at the moment it
/// paired: the reverse-DNS hostname if one resolved, otherwise the bare
/// peer IP, and Moonlight's (near-always absent, see
/// `PendingClient::name`'s doc comment) `devicename`. Both are DHCP/LAN
/// snapshots, not a stable identity — the fingerprint key this is stored
/// under is the only thing here that actually is one.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
struct PairedLabel {
	#[serde(default)]
	name: Option<String>,
	#[serde(default)]
	address: Option<String>,
}

/// `(fingerprint, name, address)` — [`PersistentState::paired_certs`]'s
/// return shape, named so its callers (and clippy) don't have to parse a
/// three-tuple type out of a function signature.
pub(crate) type PairedCertRow = (String, Option<String>, Option<String>);

#[derive(Clone, Debug, Serialize, Deserialize)]
struct StateData {
	unique_id: String,
	#[serde(default)]
	clients: HashSet<String>,
	#[serde(default)]
	paired_certs: HashSet<String>,
	/// Fingerprint -> display label, for the on-device "paired devices"
	/// list. A field new to this version: `#[serde(default)]`, like every
	/// other field here, so a `state.toml` written before it existed still
	/// loads (with an empty map, i.e. no labels) instead of failing to
	/// parse or wiping the pairings already on disk. A `BTreeMap`, not a
	/// `HashMap`, so the serialized file has a stable key order.
	#[serde(default)]
	paired_labels: BTreeMap<String, PairedLabel>,
}

impl StateData {
	fn new() -> Self {
		Self {
			unique_id: uuid::Uuid::new_v4().to_string(),
			clients: Default::default(),
			paired_certs: Default::default(),
			paired_labels: Default::default(),
		}
	}
}

#[derive(Clone)]
pub struct PersistentState {
	data: Arc<RwLock<StateData>>,
	path: PathBuf,
}

impl PersistentState {
	pub(crate) fn new() -> Result<Self, ()> {
		let path = dirs::data_dir()
			.ok_or_else(|| tracing::error!("Failed to get data directory."))?
			.join("moonshine")
			.join("state.toml");

		let data = if path.exists() {
			let serialized =
				std::fs::read_to_string(&path).map_err(|e| tracing::error!("Failed to read state file: {e}"))?;
			let data: StateData = toml::from_str(&serialized)
				.map_err(|e| tracing::error!("Failed to parse state file at '{}': {e}", path.display()))?;

			tracing::debug!("Successfully loaded state from {:?}", path);
			tracing::trace!("State: {data:?}");

			data
		} else {
			StateData::new()
		};

		let state = Self {
			data: Arc::new(RwLock::new(data)),
			path,
		};
		state.save()?;

		Ok(state)
	}

	pub fn get_uuid(&self) -> Result<String, ()> {
		let data = self
			.data
			.read()
			.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
		Ok(data.unique_id.clone())
	}

	/// Whether any client has completed pairing.
	///
	/// Read by the host binary to decide whether to show a pairing URL or a
	/// ready-to-stream message on the device's screen; `has_client` below
	/// answers a per-client question the host has no client id for.
	pub fn has_any_client(&self) -> bool {
		match self.data.read() {
			Ok(data) => !data.clients.is_empty(),
			Err(poison) => {
				tracing::error!("RwLock poisoned: {poison}");
				false
			},
		}
	}

	pub(crate) fn save(&self) -> Result<(), ()> {
		let data = self
			.data
			.read()
			.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
		let parent_dir = self
			.path
			.parent()
			.ok_or_else(|| tracing::warn!("Failed to get state dir for file {:?}", self.path))?;
		std::fs::create_dir_all(parent_dir).map_err(|e| tracing::warn!("Failed to create state dir: {e}"))?;

		std::fs::write(
			&self.path,
			toml::to_string_pretty(&*data).map_err(|e| tracing::warn!("Failed to serialize state: {e}"))?,
		)
		.map_err(|e| tracing::warn!("Failed to save state file: {e}"))
	}

	pub(crate) fn has_client(&self, client: String) -> Result<bool, ()> {
		let data = self
			.data
			.read()
			.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
		Ok(data.clients.contains(&client))
	}

	pub(crate) fn add_client(&self, client: String) -> Result<bool, ()> {
		let has_client = {
			let mut data = self
				.data
				.write()
				.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
			if data.clients.contains(&client) {
				tracing::warn!("Failed to add client ('{client}'), client already exists.");
				false
			} else {
				data.clients.insert(client);
				true
			}
		};
		if has_client {
			self.save()?;
		}
		Ok(has_client)
	}

	pub(crate) fn has_paired_cert(&self, fingerprint: String) -> Result<bool, ()> {
		let data = self
			.data
			.read()
			.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
		Ok(data.paired_certs.contains(&fingerprint))
	}

	/// `name`/`address` are the label to remember for this fingerprint (see
	/// [`PairedLabel`]); pass whatever the pending client had at the moment
	/// it paired, even if both are `None`. Always persists, even when the
	/// fingerprint was already paired — that's how a re-pair refreshes a
	/// stale label — so the returned `bool` (whether this fingerprint is
	/// newly paired) is a report, not a gate on whether anything was saved.
	pub(crate) fn add_paired_cert(&self, fingerprint: String, name: Option<String>, address: Option<String>) -> Result<bool, ()> {
		let inserted = {
			let mut data = self
				.data
				.write()
				.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
			let inserted = data.paired_certs.insert(fingerprint.clone());
			data.paired_labels.insert(fingerprint, PairedLabel { name, address });
			inserted
		};
		if !inserted {
			tracing::warn!("Adding a paired cert that was already paired; refreshing its label.");
		}
		self.save()?;
		Ok(inserted)
	}

	/// Every paired client, as `(fingerprint, name, address)` — the shape
	/// the on-device UI's paired-devices list needs, without exposing
	/// [`PairedLabel`] itself outside this module.
	pub(crate) fn paired_certs(&self) -> Result<Vec<PairedCertRow>, ()> {
		let data = self
			.data
			.read()
			.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
		Ok(data
			.paired_certs
			.iter()
			.map(|fingerprint| {
				let label = data.paired_labels.get(fingerprint).cloned().unwrap_or_default();
				(fingerprint.clone(), label.name, label.address)
			})
			.collect())
	}

	/// Remove a paired client by certificate fingerprint — the real
	/// per-machine identity (see the module-level doc comment in
	/// `clients.rs` on why `uniqueid` cannot be used for this). Drops both
	/// the fingerprint and its remembered label.
	///
	/// Also clears `clients` (the `uniqueid` bookkeeping `is_paired`/
	/// `server_info` reads) whenever this empties `paired_certs`
	/// completely: every real Moonlight client hardcodes the same
	/// `uniqueid`, so `clients` cannot be mapped back to a specific
	/// fingerprint and cleared per-unpair in the general case — but once
	/// zero certificates are paired, any `uniqueid` still marked paired is
	/// unconditionally stale (`verify_paired_client` would reject every
	/// connection regardless), so this is the one case where clearing it
	/// is unambiguously correct rather than a guess.
	pub(crate) fn remove_paired_cert(&self, fingerprint: &str) -> Result<bool, ()> {
		let removed = {
			let mut data = self
				.data
				.write()
				.map_err(|poison| tracing::error!("RwLock poisoned: {poison}"))?;
			let removed = data.paired_certs.remove(fingerprint);
			data.paired_labels.remove(fingerprint);
			if removed && data.paired_certs.is_empty() {
				data.clients.clear();
			}
			removed
		};
		if removed {
			self.save()?;
		}
		Ok(removed)
	}
}

#[cfg(test)]
mod state_data_tests {
	use super::*;

	/// Regression pin for the schema change: a `state.toml` written before
	/// `paired_labels` existed must still load, with no labels rather than
	/// a parse failure or (worse) silently losing `paired_certs`. Goes
	/// through `toml::from_str` directly on `StateData`, not
	/// `PersistentState::new()` — that reads/writes a fixed real path, and
	/// `#[test]`s across this crate share it.
	#[test]
	fn a_pre_labels_state_file_still_loads_its_paired_certs() {
		let old = r#"
			unique_id = "11111111-1111-1111-1111-111111111111"
			clients = ["0123456789ABCDEF"]
			paired_certs = ["aabbcc"]
		"#;
		let data: StateData = toml::from_str(old).expect("a state.toml predating paired_labels must still parse");
		assert!(data.paired_certs.contains("aabbcc"), "existing pairings must not be dropped");
		assert!(data.paired_labels.is_empty(), "no label existed to recover for a pre-existing pairing");
	}

	#[test]
	fn a_state_file_with_labels_round_trips() {
		let mut data = StateData::new();
		data.paired_certs.insert("aabbcc".to_string());
		data.paired_labels.insert(
			"aabbcc".to_string(),
			PairedLabel { name: None, address: Some("192.168.180.44".to_string()) },
		);

		let serialized = toml::to_string_pretty(&data).expect("serialize");
		let reloaded: StateData = toml::from_str(&serialized).expect("deserialize");
		assert_eq!(reloaded.paired_labels.get("aabbcc").and_then(|l| l.address.as_deref()), Some("192.168.180.44"));
	}
}
