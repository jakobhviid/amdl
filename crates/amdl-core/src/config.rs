//! Lazy config at `~/.config/amdl/config.toml`. Everything is optional and flags
//! always override; the file only exists to hold durable defaults — your usual
//! source/output dirs, the Opus quality, and low-sensitivity API keys (AcoustID
//! app key, Discogs token). Account credentials (cookies) are never stored here.
//!
//! Settings can be edited by hand or managed programmatically with
//! `amdl configure set/unset/get/list` (see [`set_value`]/[`render`]). Every
//! programmatic write re-renders the whole file from [`render`], so the inline
//! help is always preserved regardless of which values are set.
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
    /// "http://192.168.1.6:8790". Enables `lyrics` alignment: generate *synced*
    /// lyrics from plain ones by listening to the track. Alignment runs by
    /// default once this is set. See github.com/jakobhviid/amdl-aligner.
    pub aligner_url: Option<String>,
    /// Silence the one-line tip `lyrics` prints (when no `aligner_url` is set)
    /// suggesting an alignment server. Default false (tip shown).
    #[serde(default)]
    pub hide_aligner_hint: bool,
}

/// Path to the config file (honours `$XDG_CONFIG_HOME`).
pub fn path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config"));
    base.join("amdl").join("config.toml")
}

/// Load config if present; otherwise defaults (all `None`). Never errors — a
/// malformed file just falls back to defaults. Used by the read path of every
/// command; the `configure` subcommand uses [`load_strict`] so it never silently
/// clobbers a hand-edited file it couldn't parse.
pub fn load() -> Config {
    std::fs::read_to_string(path())
        .ok()
        .and_then(|s| toml::from_str(&s).ok())
        .unwrap_or_default()
}

/// Like [`load`] but distinguishes "no file" (→ defaults) from "file present but
/// unparseable" (→ `Err`). `configure` uses this so a `set`/`unset` refuses to
/// overwrite a config it can't round-trip rather than wiping unknown settings.
pub fn load_strict() -> Result<Config, String> {
    let p = path();
    match std::fs::read_to_string(&p) {
        Ok(s) => toml::from_str(&s)
            .map_err(|e| format!("{} is not valid TOML: {e}\nfix it by hand (or `amdl config --init` a fresh one) before using `configure`.", p.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Config::default()),
        Err(e) => Err(format!("cannot read {}: {e}", p.display())),
    }
}

/// Write `cfg` to the config path, re-rendering the full annotated file (so the
/// inline help survives every programmatic edit). Creates the parent dir.
pub fn save(cfg: &Config) -> std::io::Result<()> {
    let p = path();
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&p, render(cfg))
}

/// Every settable key (dotted `section.field`) with a one-line description.
/// Drives `configure keys`/`list`, key validation, and keeps the CLI in lockstep
/// with the struct — add a field here and in [`get_value`]/[`set_value`]/
/// [`unset_value`]/[`render`] and the whole surface follows.
pub const KEYS: &[(&str, &str)] = &[
    ("paths.source", "read-only source (input) library path"),
    ("paths.output", "derived Opus output library path"),
    ("convert.bitrate", "default Opus bitrate/quality, e.g. 192k"),
    ("keys.acoustid", "AcoustID application key (needed by `identify`)"),
    ("keys.discogs", "Discogs personal token (optional, better cover art)"),
    ("lyrics.lrcapi_url", "fallback LrcApi lyrics server URL"),
    ("lyrics.lrcapi_key", "LrcApi key, sent as the Authorization header"),
    ("lyrics.lrcapi_first", "query LrcApi before lrclib.net (true/false)"),
    ("lyrics.aligner_url", "amdl-aligner service URL (enables lyrics alignment)"),
    ("lyrics.hide_aligner_hint", "hide the 'set up an aligner' tip in lyrics runs (true/false)"),
];

/// Read one setting as a display string. `Ok(None)` = valid key but unset;
/// `Err` = unknown key. Booleans always return `Some` ("true"/"false").
pub fn get_value(cfg: &Config, key: &str) -> Result<Option<String>, String> {
    let s = |o: &Option<String>| o.clone();
    let p = |o: &Option<PathBuf>| o.as_ref().map(|v| v.display().to_string());
    Ok(match key {
        "paths.source" => p(&cfg.paths.source),
        "paths.output" => p(&cfg.paths.output),
        "convert.bitrate" => s(&cfg.convert.bitrate),
        "keys.acoustid" => s(&cfg.keys.acoustid),
        "keys.discogs" => s(&cfg.keys.discogs),
        "lyrics.lrcapi_url" => s(&cfg.lyrics.lrcapi_url),
        "lyrics.lrcapi_key" => s(&cfg.lyrics.lrcapi_key),
        "lyrics.lrcapi_first" => Some(cfg.lyrics.lrcapi_first.to_string()),
        "lyrics.aligner_url" => s(&cfg.lyrics.aligner_url),
        "lyrics.hide_aligner_hint" => Some(cfg.lyrics.hide_aligner_hint.to_string()),
        _ => return Err(unknown_key(key)),
    })
}

/// Set (or update) one setting from a string value. Validates the key and, for
/// typed fields, the value (booleans must be `true`/`false`). Empty values are
/// rejected — use [`unset_value`] to clear a setting.
pub fn set_value(cfg: &mut Config, key: &str, value: &str) -> Result<(), String> {
    if value.is_empty() {
        return Err(format!("empty value for `{key}` — use `amdl configure unset {key}` to clear it"));
    }
    match key {
        "paths.source" => cfg.paths.source = Some(PathBuf::from(value)),
        "paths.output" => cfg.paths.output = Some(PathBuf::from(value)),
        "convert.bitrate" => cfg.convert.bitrate = Some(value.to_string()),
        "keys.acoustid" => cfg.keys.acoustid = Some(value.to_string()),
        "keys.discogs" => cfg.keys.discogs = Some(value.to_string()),
        "lyrics.lrcapi_url" => cfg.lyrics.lrcapi_url = Some(value.to_string()),
        "lyrics.lrcapi_key" => cfg.lyrics.lrcapi_key = Some(value.to_string()),
        "lyrics.lrcapi_first" => cfg.lyrics.lrcapi_first = parse_bool(value)?,
        "lyrics.aligner_url" => cfg.lyrics.aligner_url = Some(value.to_string()),
        "lyrics.hide_aligner_hint" => cfg.lyrics.hide_aligner_hint = parse_bool(value)?,
        _ => return Err(unknown_key(key)),
    }
    Ok(())
}

/// Clear one setting: optionals go back to unset, booleans to their default.
pub fn unset_value(cfg: &mut Config, key: &str) -> Result<(), String> {
    match key {
        "paths.source" => cfg.paths.source = None,
        "paths.output" => cfg.paths.output = None,
        "convert.bitrate" => cfg.convert.bitrate = None,
        "keys.acoustid" => cfg.keys.acoustid = None,
        "keys.discogs" => cfg.keys.discogs = None,
        "lyrics.lrcapi_url" => cfg.lyrics.lrcapi_url = None,
        "lyrics.lrcapi_key" => cfg.lyrics.lrcapi_key = None,
        "lyrics.lrcapi_first" => cfg.lyrics.lrcapi_first = false,
        "lyrics.aligner_url" => cfg.lyrics.aligner_url = None,
        "lyrics.hide_aligner_hint" => cfg.lyrics.hide_aligner_hint = false,
        _ => return Err(unknown_key(key)),
    }
    Ok(())
}

fn unknown_key(key: &str) -> String {
    format!("unknown setting `{key}` — run `amdl configure keys` for the full list")
}

fn parse_bool(v: &str) -> Result<bool, String> {
    match v.to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => Err(format!("expected `true` or `false`, got `{v}`")),
    }
}

/// The starter config `amdl config --init` writes: the full annotated template
/// with every value commented out. Identical to rendering a default `Config`.
pub fn template() -> String {
    render(&Config::default())
}

/// Render the complete, self-documenting `config.toml` for `cfg`: every section
/// and every help comment is always present; each value is emitted as an active
/// `key = value` line when set, or a commented `# key = "example"` line when not.
/// This is the single source of truth for the file format — `--init` and every
/// `configure` write go through here, so the help never gets lost.
pub fn render(cfg: &Config) -> String {
    let mut o = String::new();
    o.push_str("# ~/.config/amdl/config.toml — amdl defaults. All optional; command flags override.\n");
    o.push_str("# Edit by hand, or manage programmatically: `amdl configure set/unset/get/list`.\n");
    o.push_str("# Every `configure` write re-renders this file, so the help below is always kept.\n\n");

    o.push_str("[paths]\n");
    o.push_str("# The read-only input library (originals).\n");
    line_path(&mut o, "source", &cfg.paths.source, "/mnt/music/originals");
    o.push_str("# The derived Opus output library.\n");
    line_path(&mut o, "output", &cfg.paths.output, "/mnt/music/library");
    o.push('\n');

    o.push_str("[convert]\n");
    o.push_str("# Opus quality. Common values: \"128k\" (small), \"192k\" (default), \"256k\" (high).\n");
    line_str(&mut o, "bitrate", &cfg.convert.bitrate, "192k");
    o.push('\n');

    o.push_str("[keys]\n");
    o.push_str("# AcoustID — needed by `identify`. This must be the APPLICATION key, NOT your\n");
    o.push_str("# account/user API key (the account key is rejected as \"invalid API key\").\n");
    o.push_str("# Create one (application type: \"personal\") at https://acoustid.org/new-application\n");
    o.push_str("# It's a short token, roughly 10 chars.\n");
    line_str(&mut o, "acoustid", &cfg.keys.acoustid, "AbCdEfGhIj");
    o.push_str("# Discogs — optional, improves cover art for obscure/physical compilations.\n");
    o.push_str("# Create a personal token at https://www.discogs.com/settings/developers\n");
    o.push_str("# It's a ~40-char string.\n");
    line_str(&mut o, "discogs", &cfg.keys.discogs, "aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789aBcD");
    o.push('\n');

    o.push_str("[lyrics]\n");
    o.push_str("# Optional fallback lyrics server, tried by `lyrics` only when lrclib.net has no\n");
    o.push_str("# synced match — a synced hit from either source still wins. Must speak the LrcApi\n");
    o.push_str("# protocol (https://github.com/HisAtri/LrcApi): GET {url}/jsonapi with title/artist/\n");
    o.push_str("# album query params. Both url + key are required to enable the fallback.\n");
    line_str(&mut o, "lrcapi_url", &cfg.lyrics.lrcapi_url, "https://lyrics.example.cloud");
    o.push_str("# API key for lrcapi_url, sent verbatim as the Authorization header.\n");
    line_str(&mut o, "lrcapi_key", &cfg.lyrics.lrcapi_key, "your-api-key-here");
    o.push_str("# Flip priority to query the LrcApi server first, falling back to lrclib.net.\n");
    o.push_str("# Default (false) keeps lrclib.net primary. A synced match still beats a plain one.\n");
    line_bool(&mut o, "lrcapi_first", cfg.lyrics.lrcapi_first);
    o.push_str("# Optional forced-alignment service (amdl-aligner) for `lyrics`: generates *synced*\n");
    o.push_str("# lyrics from plain ones by listening to the track when no source has timed lyrics.\n");
    o.push_str("# Alignment runs by default once this is set (`--no-align` opts out per run).\n");
    o.push_str("# See https://github.com/jakobhviid/amdl-aligner\n");
    line_str(&mut o, "aligner_url", &cfg.lyrics.aligner_url, "http://192.168.1.6:8790");
    o.push_str("# When no aligner_url is set, `lyrics` prints a one-line tip suggesting one.\n");
    o.push_str("# Set this true to silence that tip.\n");
    line_bool(&mut o, "hide_aligner_hint", cfg.lyrics.hide_aligner_hint);
    o
}

fn toml_escape(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn line_str(o: &mut String, key: &str, val: &Option<String>, example: &str) {
    match val {
        Some(v) => o.push_str(&format!("{key} = \"{}\"\n", toml_escape(v))),
        None => o.push_str(&format!("# {key} = \"{example}\"\n")),
    }
}

fn line_path(o: &mut String, key: &str, val: &Option<PathBuf>, example: &str) {
    line_str(o, key, &val.as_ref().map(|p| p.display().to_string()), example);
}

fn line_bool(o: &mut String, key: &str, val: bool) {
    // `false` is the default, so render it as the commented example; only an
    // explicit `true` becomes an active line.
    if val {
        o.push_str(&format!("{key} = true\n"));
    } else {
        o.push_str(&format!("# {key} = false\n"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_is_fully_commented_and_documents_every_key() {
        let t = template();
        // A fresh template must parse to all-defaults (nothing active).
        let cfg: Config = toml::from_str(&t).expect("template is valid TOML");
        assert!(cfg.paths.source.is_none() && cfg.lyrics.aligner_url.is_none());
        assert!(!cfg.lyrics.lrcapi_first);
        // Every section header and every key's help/example line is present.
        for section in ["[paths]", "[convert]", "[keys]", "[lyrics]"] {
            assert!(t.contains(section), "missing section {section}");
        }
        for (key, _) in KEYS {
            let field = key.split('.').next_back().unwrap();
            assert!(t.contains(&format!("# {field} = ")), "template lost help for {key}");
        }
    }

    #[test]
    fn set_get_unset_round_trip_through_the_rendered_file() {
        let mut cfg = Config::default();
        set_value(&mut cfg, "lyrics.aligner_url", "http://192.168.1.6:8790").unwrap();
        set_value(&mut cfg, "paths.output", "/mnt/music/library").unwrap();
        set_value(&mut cfg, "lyrics.lrcapi_first", "true").unwrap();

        assert_eq!(get_value(&cfg, "lyrics.aligner_url").unwrap().as_deref(), Some("http://192.168.1.6:8790"));
        assert_eq!(get_value(&cfg, "lyrics.lrcapi_first").unwrap().as_deref(), Some("true"));

        // Rendered file must still carry the full help AND parse back to the same values.
        let rendered = render(&cfg);
        assert!(rendered.contains("# Optional forced-alignment service"), "help dropped after set");
        assert!(rendered.contains("aligner_url = \"http://192.168.1.6:8790\""));
        assert!(rendered.contains("lrcapi_first = true"));
        let back: Config = toml::from_str(&rendered).unwrap();
        assert_eq!(back.paths.output, Some(PathBuf::from("/mnt/music/library")));
        assert_eq!(back.lyrics.aligner_url.as_deref(), Some("http://192.168.1.6:8790"));
        assert!(back.lyrics.lrcapi_first);

        unset_value(&mut cfg, "lyrics.aligner_url").unwrap();
        assert!(get_value(&cfg, "lyrics.aligner_url").unwrap().is_none());
        assert!(render(&cfg).contains("# aligner_url = "), "unset should re-comment the line");
    }

    #[test]
    fn rejects_unknown_keys_and_bad_values() {
        let mut cfg = Config::default();
        assert!(set_value(&mut cfg, "paths.nope", "x").is_err());
        assert!(get_value(&cfg, "paths.nope").is_err());
        assert!(unset_value(&mut cfg, "paths.nope").is_err());
        assert!(set_value(&mut cfg, "lyrics.lrcapi_first", "maybe").is_err());
        assert!(set_value(&mut cfg, "paths.source", "").is_err()); // empty → use unset
    }

    #[test]
    fn values_with_quotes_survive_a_render_round_trip() {
        let mut cfg = Config::default();
        set_value(&mut cfg, "paths.source", r#"/mnt/we"ird\path"#).unwrap();
        let back: Config = toml::from_str(&render(&cfg)).unwrap();
        assert_eq!(back.paths.source, Some(PathBuf::from(r#"/mnt/we"ird\path"#)));
    }
}
