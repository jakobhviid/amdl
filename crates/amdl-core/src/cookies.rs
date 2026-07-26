//! Resolve a gamdl-readable Netscape cookies file, with a validity check.
//! Priority:
//!   1. --cookies <path> (must exist)
//!   2. gamdl's own default (~/.gamdl/cookies.txt) — if not expired
//!   3. auto-extract Apple Music cookies from an installed browser
//!      (Chrome, Chromium, Firefox, Brave, Brave Origin, Vivaldi)
//!   4. if the existing/extracted cookies look expired, or none are found,
//!      warn the user to log in at music.apple.com and retry.
//!
//! amdl never overwrites ~/.gamdl — extracted cookies go to its own cache and are
//! passed to gamdl via --cookies-path.
use crate::ui;
use serde::Serialize;
use anyhow::{anyhow, bail, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(target_os = "macos")]
const BROWSER_LIST: &str = "Safari, Chrome, Firefox, Brave, Brave Origin, Edge, Arc, or Vivaldi";
#[cfg(not(target_os = "macos"))]
const BROWSER_LIST: &str = "Chrome, Chromium, Firefox, Brave, Brave Origin, Vivaldi, Edge, or Arc";

pub fn resolve(explicit: Option<PathBuf>, refresh: bool) -> Result<PathBuf> {
    if !refresh {
        if let Some(p) = explicit {
            if !p.is_file() {
                bail!("--cookies {} does not exist", p.display());
            }
            if expired(&p) {
                ui::warn(&format!("{} looks expired — it may not work", p.display()));
            }
            return Ok(p);
        }
        // $AMDL_COOKIES = raw cookie text (for headless/CI secrets, no file on disk).
        if let Some(raw) = env_cookies() {
            ui::info("Using cookies from $AMDL_COOKIES");
            if let Some(path) = store_pasted(&raw)? {
                return Ok(path);
            }
            ui::warn("$AMDL_COOKIES held no apple.com cookies — falling back");
        }
        let default = gamdl_default();
        if default.is_file() && !expired(&default) {
            ui::info(&format!("Using existing cookies: {}", default.display()));
            return Ok(default);
        }
        if default.is_file() {
            ui::warn("Existing gamdl cookies look expired — refreshing from your browser…");
        } else {
            ui::info("No cookies file — reading your login from an installed browser…");
        }
    } else {
        ui::info("Refreshing cookies from your browser…");
    }

    for attempt in 0..3 {
        match extract_from_browser() {
            Some((browser, netscape, n)) => {
                let out = write_cache(&netscape)?;
                if !expired(&out) {
                    ui::ok(&format!("Got {n} valid cookies from {browser} → {}", out.display()));
                    return Ok(out);
                }
                ui::warn(&format!("Cookies from {browser} look expired."));
            }
            None => ui::warn(&format!("No Apple Music login found in {BROWSER_LIST}.")),
        }
        // Headless / server fallback: let the user paste a cookies.txt.
        if let Some(path) = prompt_paste()? {
            return Ok(path);
        }
        if attempt < 2 {
            ui::warn(&format!("Log in at https://music.apple.com in {BROWSER_LIST}, then continue."));
            ui::ask("→ Press Enter after logging in (Ctrl-C to cancel):");
        }
    }
    bail!("could not obtain valid Apple Music cookies — log in at https://music.apple.com and retry, or pass --cookies <file>");
}

/// On a terminal with no usable browser (e.g. a server), offer to paste a
/// Netscape cookies.txt directly. Returns the cache path if one was stored.
fn prompt_paste() -> Result<Option<PathBuf>> {
    if !ui::stdin_tty() {
        // Non-interactive (piped/headless with no TTY): can't prompt.
        // On a server, pass `--cookies -` to pipe a cookies file instead.
        return Ok(None);
    }
    let ans = ui::ask("No browser cookies. Paste a Netscape cookies.txt instead? [y/N]:");
    if !ans.eq_ignore_ascii_case("y") {
        return Ok(None);
    }
    ui::info("Paste the cookies now; finish with an empty line (or Ctrl-D):");
    store_pasted(&ui::read_block())
}

/// Parse pasted/piped text as a Netscape cookie file, keep the apple.com lines,
/// normalise to tab-delimited, and write it to amdl's cache. Returns the path,
/// or None if it held no apple.com cookies.
fn store_pasted(raw: &str) -> Result<Option<PathBuf>> {
    let (clean, count, has_token) = clean_netscape(raw);
    if count == 0 {
        ui::warn("no apple.com cookie lines found in that input");
        return Ok(None);
    }
    let out = write_cache(&clean)?;
    if !has_token {
        ui::warn("pasted cookies have no 'media-user-token' — gamdl may not authenticate");
    }
    if expired(&out) {
        ui::warn("pasted cookies look expired — you may need to log in again at https://music.apple.com");
    }
    ui::ok(&format!("saved {count} apple.com cookie(s) → {}", out.display()));
    Ok(Some(out))
}

/// Parse cookies from whatever a user is likely to paste, and re-emit a clean,
/// tab-delimited Netscape file. Returns (text, count, has media-user-token).
///
/// Tolerated inputs (tried in order):
///   1. Netscape `cookies.txt` rows — tab OR space separated, incl. the
///      `#HttpOnly_` line prefix Chrome/curl use (media-user-token is HttpOnly).
///   2. `document.cookie` / `Cookie:` header text — `name=value; name=value`,
///      on one line or one pair per line. Domain/expiry aren't in that form, so
///      we scope to `.apple.com` (sent to every apple subdomain) as a session
///      cookie. A regex isn't used on purpose: cookie values contain `=`, `+`,
///      `/` and base64 `==`, which delimiter-splitting handles cleanly.
fn clean_netscape(raw: &str) -> (String, usize, bool) {
    let row = |domain: &str, sub: &str, path: &str, sec: &str, exp: &str, name: &str, val: &str| {
        (
            format!("{domain}\t{sub}\t{path}\t{sec}\t{exp}\t{name}\t{val}"),
            name.eq_ignore_ascii_case("media-user-token"),
        )
    };
    let mut lines = vec!["# Netscape HTTP Cookie File".to_string()];
    let mut has_token = false;

    // Pass 1: Netscape rows.
    for line in raw.lines() {
        let mut l = line.trim();
        if l.is_empty() {
            continue;
        }
        if let Some(rest) = l.strip_prefix("#HttpOnly_") {
            l = rest.trim_start(); // keep HttpOnly cookies (drop only the marker)
        } else if l.starts_with('#') {
            continue;
        }
        let f: Vec<&str> = l.split_whitespace().collect();
        if f.len() >= 7 && f[0].contains("apple.com") {
            let (r, tok) = row(f[0], f[1], f[2], f[3], f[4], f[5], &f[6..].join(" "));
            lines.push(r);
            has_token |= tok;
        }
    }
    if lines.len() > 1 {
        return (lines.join("\n") + "\n", lines.len() - 1, has_token);
    }

    // Pass 2: `name=value; …` cookie-header / document.cookie style.
    for pair in raw.split([';', '\n', '\r']) {
        let pair = pair.trim();
        if pair.is_empty() || pair.starts_with('#') {
            continue;
        }
        if let Some((name, value)) = pair.split_once('=') {
            let (name, value) = (name.trim(), value.trim());
            // Cookie names/values are unescaped tokens — neither contains whitespace.
            if name.is_empty()
                || value.is_empty()
                || name.contains(char::is_whitespace)
                || value.contains(char::is_whitespace)
            {
                continue;
            }
            let (r, tok) = row(".apple.com", "TRUE", "/", "TRUE", "0", name, value);
            lines.push(r);
            has_token |= tok;
        }
    }
    (lines.join("\n") + "\n", lines.len() - 1, has_token)
}

/// `amdl cookies`: report what we'd use and whether the browser extraction works.
/// Does not download anything.
/// Machine-readable result of [`diagnose`] (the `--json` shape).
#[derive(Debug, Default, Serialize)]
pub struct Diagnosis {
    /// `$AMDL_COOKIES` was set in the environment.
    pub env_cookies_set: bool,
    /// When `env_cookies_set`: whether it held usable apple.com cookies.
    pub env_cookies_usable: Option<bool>,
    /// gamdl's own cookies file path, if present.
    pub gamdl_cookies: Option<String>,
    /// `"valid"` / `"expired"` for the gamdl cookies, if present.
    pub gamdl_state: Option<String>,
    /// Browser apple.com cookies were extracted from, if any.
    pub browser: Option<String>,
    pub browser_cookie_count: Option<usize>,
    /// Where the extracted cookies were cached.
    pub browser_cache_path: Option<String>,
    /// `"valid"` / `"expired"` for the extracted cookies.
    pub browser_state: Option<String>,
    /// Whether usable (non-expired) apple.com cookies are available anywhere.
    pub usable_cookies_available: bool,
}

pub fn diagnose(json: bool) -> Result<()> {
    let mut d = Diagnosis::default();
    if let Some(raw) = env_cookies() {
        d.env_cookies_set = true;
        if !json {
            ui::info("$AMDL_COOKIES is set — validating its contents:");
        }
        match store_pasted(&raw)? {
            Some(p) => {
                d.env_cookies_usable = Some(true);
                if !json {
                    ui::ok(&format!("  usable → {}", p.display()));
                }
            }
            None => {
                d.env_cookies_usable = Some(false);
                if !json {
                    ui::warn("  no apple.com cookies in $AMDL_COOKIES");
                }
            }
        }
    }
    let default = gamdl_default();
    if default.is_file() {
        let exp = expired(&default);
        d.gamdl_cookies = Some(default.display().to_string());
        d.gamdl_state = Some(if exp { "expired" } else { "valid" }.into());
        if !json {
            ui::info(&format!("gamdl cookies: {} ({})", default.display(), if exp { "looks EXPIRED" } else { "looks valid" }));
        }
    } else if !json {
        ui::info("gamdl cookies: none");
    }
    match extract_from_browser() {
        Some((browser, netscape, n)) => {
            let out = write_cache(&netscape)?;
            let exp = expired(&out);
            d.browser = Some(browser.clone());
            d.browser_cookie_count = Some(n);
            d.browser_cache_path = Some(out.display().to_string());
            d.browser_state = Some(if exp { "expired" } else { "valid" }.into());
            if !json {
                ui::ok(&format!("browser: {n} apple.com cookie(s) from {browser} → wrote {}", out.display()));
                if exp {
                    ui::warn("  …but they look EXPIRED — log in again at https://music.apple.com");
                } else {
                    ui::info("  extracted cookies look valid");
                }
            }
        }
        None => {
            if !json {
                ui::warn(&format!("browser: no apple.com cookies found in {BROWSER_LIST}"));
                ui::warn("→ log in at https://music.apple.com in one of those browsers");
                ui::info("  on a headless server: pipe a cookies file with `--cookies -`,");
                ui::info("  or run `download` and paste it when prompted.");
            }
        }
    }
    d.usable_cookies_available = d.env_cookies_usable == Some(true)
        || d.gamdl_state.as_deref() == Some("valid")
        || d.browser_state.as_deref() == Some("valid");
    if json {
        println!("{}", serde_json::to_string_pretty(&d)?);
    }
    Ok(())
}

/// True if the cookie file's newest real (non-session) expiry is in the past.
fn expired(path: &Path) -> bool {
    let txt = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return true,
    };
    let now = now_secs();
    let mut max_exp: i64 = -1;
    for line in txt.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() >= 5 {
            if let Ok(e) = f[4].parse::<i64>() {
                if e > 0 {
                    max_exp = max_exp.max(e);
                }
            }
        }
    }
    if max_exp < 0 {
        return false; // only session cookies / unparseable — can't tell, assume usable
    }
    max_exp < now
}

/// Try each supported browser; return (browser, netscape-text, count) or None.
fn extract_from_browser() -> Option<(String, String, usize)> {
    let domains = Some(vec!["apple.com".to_string()]);
    #[allow(unused_mut)]
    let mut tries = vec![
        ("Chrome", rookie::chrome(domains.clone())),
        ("Chromium", rookie::chromium(domains.clone())),
        ("Firefox", rookie::firefox(domains.clone())),
        ("Brave", rookie::brave(domains.clone())),
        ("Vivaldi", rookie::vivaldi(domains.clone())),
        ("Edge", rookie::edge(domains.clone())),
        ("Arc", rookie::arc(domains.clone())),
    ];
    // Safari is macOS-only (its API is cfg'd to macos in rookie).
    #[cfg(target_os = "macos")]
    tries.push(("Safari", rookie::safari(domains.clone())));
    // "Brave Origin" is a standalone Brave build whose profile lives in
    // ~/.config/BraveSoftware/Brave-Origin — a *sibling* of Brave-Browser, not a
    // channel suffix of it — so rookie::brave() never looks there. Feed each of its
    // profile DBs to rookie by path; the on-disk format and keyring key match Brave.
    for db in brave_origin_cookie_dbs() {
        if let Some(p) = db.to_str() {
            tries.push(("Brave Origin", rookie::any_browser(p, domains.clone(), None)));
        }
    }
    for (name, res) in tries {
        if let Ok(cookies) = res {
            if !cookies.is_empty() {
                return Some((name.to_string(), to_netscape(&cookies), cookies.len()));
            }
        }
    }
    None
}

/// Candidate Cookies-DB paths for "Brave Origin" — the standalone Brave build
/// rookie has no config for. Covers the native and Flatpak locations, and the
/// Default plus any numbered profiles. Only paths that exist are returned.
fn brave_origin_cookie_dbs() -> Vec<PathBuf> {
    let bases = [
        home().join(".config/BraveSoftware/Brave-Origin"),
        home().join(".var/app/com.brave.Browser/config/BraveSoftware/Brave-Origin"),
    ];
    let mut out = Vec::new();
    for base in bases {
        let default = base.join("Default/Cookies");
        if default.is_file() {
            out.push(default);
        }
        // Extra profiles are directories named "Profile 1", "Profile 2", …
        if let Ok(entries) = fs::read_dir(&base) {
            for e in entries.flatten() {
                if e.file_name().to_string_lossy().starts_with("Profile ") {
                    let db = e.path().join("Cookies");
                    if db.is_file() {
                        out.push(db);
                    }
                }
            }
        }
    }
    out
}

fn to_netscape(cookies: &[rookie::enums::Cookie]) -> String {
    let mut lines = vec!["# Netscape HTTP Cookie File".to_string()];
    for c in cookies {
        let subdomains = if c.domain.starts_with('.') { "TRUE" } else { "FALSE" };
        let secure = if c.secure { "TRUE" } else { "FALSE" };
        let expires = c.expires.unwrap_or(0);
        lines.push(format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            c.domain, subdomains, c.path, secure, expires, c.name, c.value
        ));
    }
    lines.join("\n") + "\n"
}

fn write_cache(netscape: &str) -> Result<PathBuf> {
    let out = cache_dir().join("cookies.txt");
    fs::create_dir_all(out.parent().unwrap())?;
    fs::write(&out, netscape).map_err(|e| anyhow!(e))?;
    Ok(out)
}

/// Raw cookie text from $AMDL_COOKIES, if set and non-empty.
fn env_cookies() -> Option<String> {
    std::env::var("AMDL_COOKIES").ok().filter(|s| !s.trim().is_empty())
}

fn now_secs() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}
fn gamdl_default() -> PathBuf {
    home().join(".gamdl/cookies.txt")
}
fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}
fn cache_dir() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".cache"))
        .join("amdl")
}
