use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub fn default_launch_timeout() -> u64 {
	2
}

/// Configuration for a single application that can be launched in a session.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ApplicationConfig {
	/// Title of the application.
	pub title: String,

	/// Path to a boxart image.
	pub boxart: Option<PathBuf>,

	/// The command to run.
	pub command: Vec<String>,

	/// Commands to run before launching the application.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub pre_command: Vec<Vec<String>>,

	/// Commands to run after the streaming session ends.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub post_command: Vec<Vec<String>>,

	/// systemd StandardOutput value. If not set, defaults to "null".
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub stdout: Option<String>,

	/// systemd StandardError value. If not set, defaults to "null".
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub stderr: Option<String>,

	/// Seconds to wait for the application to reach an active state after launch.
	#[serde(default = "default_launch_timeout")]
	pub launch_timeout_secs: u64,
}

impl Default for ApplicationConfig {
	fn default() -> Self {
		Self {
			title: String::new(),
			boxart: None,
			command: Vec::new(),
			pre_command: Vec::new(),
			post_command: Vec::new(),
			stdout: None,
			stderr: None,
			launch_timeout_secs: default_launch_timeout(),
		}
	}
}

impl ApplicationConfig {
	pub fn id(&self) -> i32 {
		let mut hasher = DefaultHasher::new();
		self.title.hash(&mut hasher);
		hasher.finish() as i32
	}
}
