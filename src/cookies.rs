//! Resolve a gamdl-readable Netscape cookies file, with a validity check.
//! Priority:
//!   1. --cookies <path> (must exist)
//!   2. gamdl's own default (~/.gamdl/cookies.txt) — if not expired
//!   3. auto-extract Apple Music cookies from an installed browser
//!      (Chrome, Chromium, Firefox, Brave, Vivaldi)
//!   4. if the existing/extracted cookies look expired, or none are found,
//!      warn the user to log in at music.apple.com and retry.
//!
//! amdl never overwrites ~/.gamdl — extracted cookies go to its own cache and are
//! passed to gamdl via --cookies-path.
use crate::ui;
use anyhow::{anyhow, bail, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const BROWSER_LIST: &str = "Chrome, Chromium, Firefox, Brave, or Vivaldi";

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
        if attempt < 2 {
            ui::warn(&format!("Log in at https://music.apple.com in {BROWSER_LIST}, then continue."));
            ui::ask("→ Press Enter after logging in (Ctrl-C to cancel):");
        }
    }
    bail!("could not obtain valid Apple Music cookies — log in at https://music.apple.com and retry");
}

/// `amdl cookies`: report what we'd use and whether the browser extraction works.
/// Does not download anything.
pub fn diagnose() -> Result<()> {
    let default = gamdl_default();
    if default.is_file() {
        let state = if expired(&default) { "looks EXPIRED" } else { "looks valid" };
        ui::info(&format!("gamdl cookies: {} ({state})", default.display()));
    } else {
        ui::info("gamdl cookies: none");
    }
    match extract_from_browser() {
        Some((browser, netscape, n)) => {
            let out = write_cache(&netscape)?;
            ui::ok(&format!("browser: {n} apple.com cookie(s) from {browser} → wrote {}", out.display()));
            if expired(&out) {
                ui::warn("  …but they look EXPIRED — log in again at https://music.apple.com");
            } else {
                ui::info("  extracted cookies look valid");
            }
        }
        None => {
            ui::warn(&format!("browser: no apple.com cookies found in {BROWSER_LIST}"));
            ui::warn("→ log in at https://music.apple.com in one of those browsers");
        }
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
    let tries = [
        ("Chrome", rookie::chrome(domains.clone())),
        ("Chromium", rookie::chromium(domains.clone())),
        ("Firefox", rookie::firefox(domains.clone())),
        ("Brave", rookie::brave(domains.clone())),
        ("Vivaldi", rookie::vivaldi(domains.clone())),
    ];
    for (name, res) in tries {
        if let Ok(cookies) = res {
            if !cookies.is_empty() {
                return Some((name.to_string(), to_netscape(&cookies), cookies.len()));
            }
        }
    }
    None
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
