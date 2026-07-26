//! gamdl orchestration: run it per URL, trying storefronts until one yields
//! tracks (Apple Music availability is region-specific). gamdl does the actual
//! download + decrypt; we just drive it and collect the .m4a it produces.
use crate::ui;
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub struct Opts {
    pub cookies: PathBuf,
    pub storefronts: Vec<String>, // primary first, then fallbacks
    pub output: PathBuf,          // gamdl --output-path (work dir)
    pub artist_auto: bool,        // add --artist-auto-select main-albums for /artist/ URLs
}

/// Swap the storefront (country) token in an Apple Music URL:
/// https://music.apple.com/us/album/... -> .../dk/album/...
fn swap_storefront(url: &str, sf: &str) -> String {
    if let Some(idx) = url.find("music.apple.com/") {
        let start = idx + "music.apple.com/".len();
        if let Some(rel_end) = url[start..].find('/') {
            let end = start + rel_end;
            return format!("{}{}{}", &url[..start], sf, &url[end..]);
        }
    }
    url.to_string()
}

/// Run gamdl for one URL. Returns the .m4a files it produced under `output`.
pub fn download(url: &str, opts: &Opts) -> Result<Vec<PathBuf>> {
    if which("gamdl").is_none() {
        bail!("gamdl not found on PATH — `brew install gamdl`");
    }
    for sf in &opts.storefronts {
        let attempt = swap_storefront(url, sf);
        let before = list_ext(&opts.output, "m4a");
        let pb = ui::spinner(&format!("gamdl · storefront={sf}"));
        let mut cmd = Command::new("gamdl");
        cmd.arg("--output-path").arg(&opts.output)
            .arg("--cookies-path").arg(&opts.cookies);
        if opts.artist_auto {
            cmd.arg("--artist-auto-select").arg("main-albums");
        }
        cmd.arg(&attempt);
        let result = cmd.output();
        pb.finish_and_clear();

        let after = list_ext(&opts.output, "m4a");
        let fresh: Vec<PathBuf> = after.into_iter().filter(|p| !before.contains(p)).collect();
        match result {
            Ok(o) if o.status.success() && !fresh.is_empty() => {
                ui::ok(&format!("downloaded {} track(s) (storefront={sf})", fresh.len()));
                return Ok(fresh);
            }
            Ok(_) => ui::warn(&format!("storefront={sf}: no tracks — trying next")),
            Err(e) => ui::warn(&format!("storefront={sf}: gamdl error ({e})")),
        }
    }
    bail!("all storefronts failed for {url}");
}

fn list_ext(dir: &Path, ext: &str) -> Vec<PathBuf> {
    crate::scan::with_exts(dir, &[ext])
}


fn which(bin: &str) -> Option<PathBuf> {
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths).map(|d| d.join(bin)).find(|p| p.is_file())
    })
}
