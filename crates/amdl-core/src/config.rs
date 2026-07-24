//! Lazy config at `~/.config/amdl/config.toml`. Everything is optional and flags
//! always override; the file only exists to hold durable defaults (e.g. your
//! usual source/output dirs) and, later, low-sensitivity API keys (AcoustID app
//! key, Discogs token). Account credentials (cookies) are never stored here.
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub paths: Paths,
}

#[derive(Debug, Default, Deserialize)]
pub struct Paths {
    /// Default read-only source (input) library.
    pub source: Option<PathBuf>,
    /// Default derived (output) library.
    pub output: Option<PathBuf>,
}

/// Path to the config file (honours `$XDG_CONFIG_HOME`).
pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("amdl").join("config.toml")
}

/// Load config if present; otherwise defaults (all `None`). Never errors — a
/// malformed file just falls back to defaults.
pub fn load() -> Config {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// A starter config, written by `amdl config --init`.
pub const EXAMPLE: &str = "\
# ~/.config/amdl/config.toml — amdl defaults (all optional; flags override)
[paths]
# source = \"/path/to/originals\"   # read-only input library (never written)
# output = \"/path/to/library\"     # derived Opus output library
";
