//! recover — re-acquire broken/missing tracks (spec §5): the file→metadata→
//! re-download inverse of `download`. Detection catches **source files that
//! never produced an Opus** (conversion failures / damaged originals) and
//! **unreadable Opus**. For each, in order:
//!   1. **cross-library copy** — if a `--reference` library already has the same
//!      track (album+title), copy its Opus. Free, no download; a household shares
//!      tracks, so this beats a fresh DRM download.
//!   2. **re-acquire** (`--online`) — resolve the track on Apple Music (iTunes
//!      search by artist+title, duration-matched) → gamdl → convert → place.
//! Metadata comes from the file's tags, else the folder + filename.
use crate::{convert, download, tags, ui};
use anyhow::Result;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub struct Opts {
    pub source: PathBuf,
    pub references: Vec<PathBuf>,
    pub online: bool,
    pub cookies: Option<PathBuf>,
    pub storefronts: Vec<String>,
    pub bitrate: String,
    pub work_dir: PathBuf,
    pub jobs: usize,
    pub dry_run: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub broken: usize,
    pub recovered_from_sibling: usize,
    pub reacquired: usize,
    /// Recovered files whose album tag was matched to an existing sibling so they
    /// group with the album instead of splitting out under Apple's own album tag.
    pub regrouped: usize,
    pub dry_run: bool,
    pub still_broken: Vec<String>,
}

const UA: &str = concat!("amdl/", env!("CARGO_PKG_VERSION"), " (https://github.com/jakobhviid/amdl)");

/// (normalized album, normalized title)
type Key = (String, String);

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Best-effort artist/title/album for a source file: its tags, else derive from
/// the folder + filename (album = parent dir, title = filename stem sans track no).
fn resolve_meta(src: &Path) -> (Option<String>, String, Option<String>) {
    let b = tags::read_basic(src);
    let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    // strip a leading track number like "03 " or "2-16 "
    let title_from_name = stem
        .trim_start_matches(|c: char| c.is_ascii_digit() || c == '-' || c == ' ')
        .to_string();
    let album_from_dir = src
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().into_owned());
    let title = b.title.unwrap_or(if title_from_name.is_empty() { stem } else { title_from_name });
    let album = b.album.or(album_from_dir);
    (b.artist, title, album)
}

pub fn run(output: &Path, opts: &Opts) -> Result<Report> {
    let mut report = Report { dry_run: opts.dry_run, ..Default::default() };

    // Detect: source audio files that have no corresponding Opus in the output.
    let sources = list_audio(&opts.source, &["m4a", "mp3", "opus"]);
    let broken: Vec<PathBuf> = sources
        .into_iter()
        .filter(|s| {
            s.strip_prefix(&opts.source)
                .map(|rel| !output.join(rel).with_extension("opus").exists())
                .unwrap_or(false)
        })
        .collect();
    report.broken = broken.len();
    if broken.is_empty() {
        return Ok(report);
    }

    // Build a cross-library index: (album, title) -> a reference Opus.
    let ref_index = reference_index(&opts.references);

    let pb = ui::bar(broken.len() as u64, "Recovering");
    for src in &broken {
        let rel = src.strip_prefix(&opts.source).unwrap_or(src);
        let out = output.join(rel).with_extension("opus");
        let (artist, title, album) = resolve_meta(src);
        let key = (norm(album.as_deref().unwrap_or("")), norm(&title));

        // 1. cross-library copy
        if let Some(ref_opus) = ref_index.get(&key) {
            if !opts.dry_run {
                if let Some(p) = out.parent() {
                    let _ = std::fs::create_dir_all(p);
                }
                let _ = std::fs::copy(ref_opus, &out);
                let dir = out.parent().unwrap_or(output);
                report.regrouped += regroup_to_sibling(std::slice::from_ref(&out), dir, false);
            }
            report.recovered_from_sibling += 1;
            pb.inc(1);
            continue;
        }

        // 2. re-acquire via Apple Music (needs --online + cookies)
        if opts.online && !opts.dry_run {
            if let Some(artist) = artist.as_deref() {
                let dir = out.parent().unwrap_or(output).to_path_buf();
                if let Ok(Some(new_files)) = reacquire(&artist_title(artist, &title), src, output, opts) {
                    report.reacquired += 1;
                    report.regrouped += regroup_to_sibling(&new_files, &dir, false);
                    pb.inc(1);
                    continue;
                }
            }
        }

        report.still_broken.push(format!(
            "{} — {} ({})",
            artist.as_deref().unwrap_or("?"),
            title,
            rel.display()
        ));
        pb.inc(1);
    }
    pb.finish_and_clear();
    report.still_broken.sort();
    Ok(report)
}

fn artist_title(artist: &str, title: &str) -> String {
    format!("{artist} {title}")
}

/// Resolve `term` to an Apple Music song URL (iTunes search), download via gamdl,
/// convert to Opus into `output` at the same relative path as `src`. Returns the
/// newly-written Opus paths on success (so the caller can regroup them), or `None`
/// if the track couldn't be resolved/fetched.
fn reacquire(term: &str, src: &Path, output: &Path, opts: &Opts) -> Result<Option<Vec<PathBuf>>> {
    let Some(url) = itunes_song_url(term) else {
        return Ok(None);
    };
    let cookies = crate::cookies::resolve(opts.cookies.clone(), false)?;
    std::fs::create_dir_all(&opts.work_dir)?;
    let dl = download::Opts {
        cookies,
        storefronts: opts.storefronts.clone(),
        output: opts.work_dir.clone(),
        artist_auto: false,
    };
    let fetched = download::download(&url, &dl)?;
    if fetched.is_empty() {
        return Ok(None);
    }
    // Convert into the output at the ORIGINAL relative path (group with siblings).
    let rel_dir = src
        .strip_prefix(&opts.source)
        .ok()
        .and_then(|r| r.parent())
        .map(|p| output.join(p))
        .unwrap_or_else(|| output.to_path_buf());
    // Snapshot the album dir so we can identify the freshly-written Opus (gamdl
    // names the file itself, so we can't predict the path).
    let before: HashSet<PathBuf> = list_audio(&rel_dir, &["opus"]).into_iter().collect();
    let _ = convert::convert_files(&fetched, &opts.work_dir, &rel_dir, &opts.bitrate, opts.jobs)?;
    let new_files: Vec<PathBuf> =
        list_audio(&rel_dir, &["opus"]).into_iter().filter(|p| !before.contains(p)).collect();
    Ok(Some(new_files))
}

/// Match each just-recovered Opus in `dir` to an existing album sibling's `album`
/// (and compilation flag), so it groups with the album instead of splitting out
/// under Apple's own album tag. `new_files` are excluded from sibling selection.
/// Uses `read_dir` (never a glob — album folders contain brackets like `[Disc 2]`,
/// which a glob would treat as a character class). Returns how many were changed.
fn regroup_to_sibling(new_files: &[PathBuf], dir: &Path, dry_run: bool) -> usize {
    let sibling = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("opus"))
        .filter(|p| !new_files.iter().any(|n| n == p))
        .find_map(|p| {
            let b = tags::read_basic(&p);
            grouping_from_sibling(b.album.as_deref(), b.album_artist.as_deref())
        });
    let Some((album, compilation)) = sibling else {
        return 0;
    };
    let mut n = 0;
    for f in new_files {
        // skip if it already matches the album (idempotent).
        if tags::read_basic(f).album.as_deref() == Some(album.as_str()) {
            continue;
        }
        if dry_run {
            n += 1;
        } else if tags::set_album_grouping(f, &album, compilation).is_ok() {
            n += 1;
        }
    }
    n
}

/// The album grouping to stamp on a recovered file, given a sibling's tags: its
/// `album`, and whether the album is a compilation (inferred from the sibling's
/// album-artist being "Various Artists"). `None` if the sibling has no album.
fn grouping_from_sibling(sibling_album: Option<&str>, sibling_album_artist: Option<&str>) -> Option<(String, bool)> {
    let album = sibling_album?;
    if album.is_empty() {
        return None;
    }
    Some((album.to_string(), sibling_album_artist == Some("Various Artists")))
}

fn itunes_song_url(term: &str) -> Option<String> {
    let text = ureq::get("https://itunes.apple.com/search")
        .set("User-Agent", UA)
        .query("term", term)
        .query("entity", "song")
        .query("limit", "5")
        .call()
        .ok()?
        .into_string()
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("results")?
        .as_array()?
        .iter()
        .find_map(|r| r.get("trackViewUrl").and_then(|x| x.as_str()).map(String::from))
}

fn reference_index(references: &[PathBuf]) -> HashMap<Key, PathBuf> {
    let mut index = HashMap::new();
    for r in references {
        for o in list_audio(r, &["opus"]) {
            let b = tags::read_basic(&o);
            if let (Some(al), Some(ti)) = (b.album, b.title) {
                index.entry((norm(&al), norm(&ti))).or_insert(o);
            }
        }
    }
    index
}

fn list_audio(dir: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.retain(|p| p.extension().and_then(|e| e.to_str()).map(|e| exts.contains(&e)).unwrap_or(false));
    out.sort();
    out
}
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::grouping_from_sibling;

    #[test]
    fn sibling_grouping_infers_compilation_from_album_artist() {
        // a compilation sibling → match its album and flag it a compilation
        assert_eq!(
            grouping_from_sibling(Some("Eurovision [Disc 2]"), Some("Various Artists")),
            Some(("Eurovision [Disc 2]".to_string(), true))
        );
        // an ordinary album sibling → match album, not a compilation
        assert_eq!(
            grouping_from_sibling(Some("Some Album"), Some("Some Artist")),
            Some(("Some Album".to_string(), false))
        );
        // no sibling album → nothing to match; leave the recovered tags as-is
        assert_eq!(grouping_from_sibling(None, Some("Various Artists")), None);
        assert_eq!(grouping_from_sibling(Some(""), None), None);
    }
}
