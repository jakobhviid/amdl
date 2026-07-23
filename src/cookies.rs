//! Resolve a gamdl-readable Netscape cookies file. Priority:
//!   1. --cookies <path> (must exist)
//!   2. gamdl's own default (~/.gamdl/cookies.txt)
//!   3. auto-extract Apple Music cookies from an installed browser
//!      (Chrome, Chromium, Firefox, Brave, Vivaldi)
//!   4. prompt the user to log in at music.apple.com, then retry (3)
use crate::ui;
use anyhow::{anyhow, bail, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub fn resolve(explicit: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(p) = explicit {
        if p.is_file() {
            return Ok(p);
        }
        bail!("--cookies {} does not exist", p.display());
    }
    let default = home().join(".gamdl/cookies.txt");
    if default.is_file() {
        ui::info(&format!("Using existing cookies: {}", default.display()));
        return Ok(default);
    }
    ui::info("No cookies file — reading your Apple Music login from an installed browser…");
    if let Some(path) = extract_to_file()? {
        return Ok(path);
    }
    ui::warn("No Apple Music login found in Chrome, Chromium, Firefox, Brave, or Vivaldi.");
    ui::warn("Log in at https://music.apple.com in one of those browsers, then continue.");
    for _ in 0..3 {
        ui::ask("→ Logged in? Press Enter to retry (Ctrl-C to cancel):");
        if let Some(path) = extract_to_file()? {
            return Ok(path);
        }
        ui::warn("Still no Apple Music cookies found.");
    }
    bail!("could not obtain Apple Music cookies");
}

fn extract_to_file() -> Result<Option<PathBuf>> {
    let netscape = match extract_apple_cookies() {
        Some(s) => s,
        None => return Ok(None),
    };
    let out = cache_dir().join("cookies.txt");
    fs::create_dir_all(out.parent().unwrap())?;
    fs::write(&out, netscape).map_err(|e| anyhow!(e))?;
    ui::ok(&format!("Wrote cookies → {}", out.display()));
    Ok(Some(out))
}

/// Try each supported browser; return the cookies in Netscape format, or None.
fn extract_apple_cookies() -> Option<String> {
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
                ui::info(&format!("  found Apple Music cookies in {name}"));
                return Some(to_netscape(&cookies));
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

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap_or_default())
}
fn cache_dir() -> PathBuf {
    std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| home().join(".cache"))
        .join("amdl")
}

#[allow(dead_code)]
fn _touch(_: &Path) {}
