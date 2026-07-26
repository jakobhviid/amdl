//! Lazy config at `~/.config/amdl/config.toml`. Everything is optional and flags
//! always override; the file only exists to hold durable defaults — your usual
//! source/output dirs, the Opus quality, and low-sensitivity API keys (AcoustID
//! app key, Discogs token). Account credentials (cookies) are never stored here.
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub paths: Paths,
    #[serde(default)]
    pub convert: Convert,
    #[serde(default)]
    pub keys: Keys,
    #[serde(default)]
    pub lyrics: Lyrics,
}

#[derive(Debug, Default, Deserialize)]
pub struct Paths {
    /// Default read-only source (input) library.
    pub source: Option<PathBuf>,
    /// Default derived (output) library.
    pub output: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Convert {
    /// Default Opus bitrate/quality (e.g. "192k", "256k", "128k").
    pub bitrate: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Keys {
    /// AcoustID *application* key (from acoustid.org/new-application), for `identify`.
    pub acoustid: Option<String>,
    /// Discogs personal token (optional), improves compilation cover coverage.
    pub discogs: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Lyrics {
    /// Optional fallback lyrics server speaking the LrcApi protocol
    /// (HisAtri/LrcApi): `GET {url}/jsonapi?title=&artist=&album=`. Consulted by
    /// `lyrics` only when lrclib.net has no synced match. Needs `lrcapi_key`.
    pub lrcapi_url: Option<String>,
    /// API key for `lrcapi_url`, sent verbatim as the `Authorization` header.
    pub lrcapi_key: Option<String>,
    /// Flip the source priority: query the LrcApi server *first* and fall back to
    /// lrclib.net. Default (false) keeps lrclib.net primary. Either way a synced
    /// hit still beats a plain one; this only decides which source wins a tie and
    /// is consulted first.
    #[serde(default)]
    pub lrcapi_first: bool,
    /// Optional forced-alignment service (amdl-aligner) URL, e.g.
    /// "http://192.168.1.6:8790". Enables `lyrics --align` to generate *synced* lyrics
    /// from plain ones by listening to the track. Left unset, `--align` just
    /// prints setup instructions. See github.com/jakobhviid/amdl-aligner.
    pub aligner_url: Option<String>,
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

/// A starter config, written by `amdl config --init`. Values are illustrative
/// formats only — never real credentials.
pub const EXAMPLE: &str = "\
# ~/.config/amdl/config.toml — amdl defaults. All optional; command flags override.

[paths]
# The read-only input library (originals) and the derived Opus output library.
# source = \"/mnt/music/originals\"
# output = \"/mnt/music/library\"

[convert]
# Opus quality. Common values: \"128k\" (small), \"192k\" (default), \"256k\" (high).
# bitrate = \"192k\"

[keys]
# AcoustID — needed by `identify`. This must be the APPLICATION key, NOT your
# account/user API key (the account key is rejected as \"invalid API key\").
# Create one (application type: \"personal\") here:
#   https://acoustid.org/new-application
# It's a short token, roughly 10 chars, e.g.:
# acoustid = \"AbCdEfGhIj\"
#
# Discogs — optional, improves cover art for obscure/physical compilations.
# Create a personal token here:
#   https://www.discogs.com/settings/developers
# It's a ~40-char string, e.g.:
# discogs = \"aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789aBcD\"

[lyrics]
# Optional fallback lyrics server, tried by `lyrics` only when lrclib.net has no
# synced match — a synced hit from either source still wins. Must speak the
# LrcApi protocol (https://github.com/HisAtri/LrcApi): GET {url}/jsonapi with
# title/artist/album query params. The key is sent verbatim as the Authorization
# header. Both fields are required to enable the fallback.
# lrcapi_url = \"https://lyrics.example.cloud\"
# lrcapi_key = \"your-api-key-here\"
# Flip priority to query the LrcApi server first and fall back to lrclib.net.
# Default is lrclib.net first. (A synced match still beats a plain one either way.)
# lrcapi_first = false
#
# Optional forced-alignment service for `lyrics --align`: generates *synced*
# lyrics from plain ones by listening to the track, when no source has timed
# lyrics. Run the service yourself (GPU recommended) and point at it here.
# See https://github.com/jakobhviid/amdl-aligner
# aligner_url = \"http://192.168.1.6:8790\"
";
