//! covers — backfill missing Opus cover art (spec §3), as an ordered funnel from
//! cheapest+safest to riskiest:
//!   1. copy-from-source — the source file still has embedded art convert didn't need.
//!   2. cross-library copy — a `--reference` library already has the *same album*
//!      covered; copy it (free, guaranteed-correct).
//!   3. online waterfall (`--online`) — MusicBrainz/CAA → iTunes → Discogs, gated
//!      on artist+album agreeing (a blank cover beats a wrong one).
//!   4. paste-a-URL tail (`--paste`) — whatever's still uncovered is a numbered,
//!      most-tracks-first straggler list; the operator pastes one image/Spotify URL
//!      per album and it's embedded across every track of that album.
//! Covers are applied per *album* (normalized title, so multi-disc sets and
//! editions group), and every embedded image is validated (decodes, min edge)
//! and square-cropped.
//!
//! Safety: only ever embeds into a *coverless* track, validates bytes before
//! writing, and never touches the source. A blank cover beats a wrong one.
use crate::{tags, ui};
use lofty::picture::{MimeType, Picture, PictureType};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};

pub struct Opts {
    /// Read-only source tree to copy embedded art from (pass 1).
    pub source: Option<PathBuf>,
    /// Other output libraries to borrow matching-album covers from (pass 2).
    pub references: Vec<PathBuf>,
    /// Enable the online waterfall (pass 3): MusicBrainz/CAA → iTunes → Discogs.
    pub online: bool,
    /// Discogs personal token (optional) — adds a Discogs pass to the waterfall.
    pub discogs: Option<String>,
    /// Report what would change, but don't write.
    pub dry_run: bool,
    /// Minimum acceptable cover edge in px (guards against tiny/placeholder art).
    pub min_dim: u32,
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub coverless_albums: usize,
    pub filled_from_source: usize,
    pub filled_from_reference: usize,
    pub filled_online: usize,
    pub albums_filled: usize,
    pub dry_run: bool,
    pub stragglers: Vec<Straggler>,
}

#[derive(Debug, Serialize)]
pub struct Straggler {
    pub n: usize,
    pub album: String,
    pub artist: String,
    pub tracks: usize,
}

struct Album {
    display: String,
    artist: String,
    coverless: Vec<PathBuf>,
}

/// Normalize an album title so multi-disc sets / editions group together.
pub fn norm_album(s: &str) -> String {
    let mut out = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '[' | '(' => depth += 1,
            ']' | ')' => depth = (depth - 1).max(0),
            _ if depth == 0 && c.is_alphanumeric() => out.extend(c.to_lowercase()),
            _ => {}
        }
    }
    out
}

pub fn backfill(output: &Path, opts: &Opts) -> Report {
    let mut report = Report { dry_run: opts.dry_run, ..Default::default() };
    let opus = list_opus(output);
    if opus.is_empty() {
        return report;
    }

    // Group coverless tracks by normalized album (reading every opus's cover
    // state — worth a bar on a large library).
    let scan = ui::bar(opus.len() as u64, "Scanning");
    let mut albums: HashMap<String, Album> = HashMap::new();
    for o in &opus {
        scan.inc(1);
        if tags::has_cover(o) {
            continue;
        }
        let b = tags::read_basic(o);
        let display = b.album.clone().unwrap_or_else(|| "(unknown album)".into());
        let artist = b.album_artist.or(b.artist).unwrap_or_else(|| "(unknown)".into());
        let key = norm_album(&display);
        albums
            .entry(key)
            .or_insert_with(|| Album { display, artist, coverless: Vec::new() })
            .coverless
            .push(o.clone());
    }
    scan.finish_and_clear();
    report.coverless_albums = albums.len();
    if albums.is_empty() {
        return report;
    }

    // Build a reference index: normalized album -> path of a covered opus.
    let ref_index = build_reference_index(&opts.references);

    let pb = ui::bar(albums.len() as u64, "Covers");
    let mut stragglers: Vec<Straggler> = Vec::new();
    for (key, album) in &albums {
        // Pass 1: copy-from-source (per album — first track whose source has art).
        let mut cover = opts
            .source
            .as_deref()
            .and_then(|src| album_cover_from_source(&album.coverless, output, src, opts.min_dim));
        let mut from = From::Source;

        // Pass 2: cross-library copy.
        if cover.is_none() {
            if let Some(ref_opus) = ref_index.get(key) {
                cover = tags::read_cover(ref_opus).and_then(|p| validate_and_square(&p, opts.min_dim));
                from = From::Reference;
            }
        }

        // Pass 3: online waterfall (MusicBrainz/CAA → iTunes → Discogs), gated by
        // an artist+album match so a wrong cover is never auto-embedded.
        if cover.is_none() && opts.online {
            cover = online_cover(&album.display, &album.artist, opts.min_dim, opts.discogs.as_deref());
            from = From::Online;
        }

        match cover {
            Some(c) => {
                let mut n = 0;
                if !opts.dry_run {
                    for t in &album.coverless {
                        if tags::finalize_opus(t, Some(&c)).unwrap_or(false) {
                            n += 1;
                        }
                    }
                } else {
                    n = album.coverless.len();
                }
                report.albums_filled += 1;
                match from {
                    From::Source => report.filled_from_source += n,
                    From::Reference => report.filled_from_reference += n,
                    From::Online => report.filled_online += n,
                }
            }
            None => stragglers.push(Straggler {
                n: 0,
                album: album.display.clone(),
                artist: album.artist.clone(),
                tracks: album.coverless.len(),
            }),
        }
        pb.inc(1);
    }
    pb.finish_and_clear();

    // Impact-sorted, then numbered.
    stragglers.sort_by(|a, b| b.tracks.cmp(&a.tracks).then(a.album.cmp(&b.album)));
    for (i, s) in stragglers.iter_mut().enumerate() {
        s.n = i + 1;
    }
    report.stragglers = stragglers;
    report
}

enum From {
    Source,
    Reference,
    Online,
}

/// Find a validated cover for an album by reading the first source file (of any
/// of its coverless tracks) that still carries embedded art.
fn album_cover_from_source(
    coverless: &[PathBuf],
    output: &Path,
    source: &Path,
    min_dim: u32,
) -> Option<Picture> {
    for t in coverless {
        let rel = t.strip_prefix(output).ok()?;
        for ext in ["m4a", "mp3"] {
            let cand = source.join(rel).with_extension(ext);
            if cand.exists() {
                if let Some(pic) = tags::read_cover(&cand).and_then(|p| validate_and_square(&p, min_dim)) {
                    return Some(pic);
                }
            }
        }
    }
    None
}

/// normalized-album -> a covered opus in one of the reference libraries.
fn build_reference_index(references: &[PathBuf]) -> HashMap<String, PathBuf> {
    let mut index: HashMap<String, PathBuf> = HashMap::new();
    for r in references {
        let opus = list_opus(r);
        // Read tags in parallel, then insert (first covered wins).
        let covered: Vec<(String, PathBuf)> = opus
            .par_iter()
            .filter_map(|o| {
                let b = tags::read_basic(o);
                if b.has_cover {
                    b.album.map(|a| (norm_album(&a), o.clone()))
                } else {
                    None
                }
            })
            .collect();
        for (k, p) in covered {
            index.entry(k).or_insert(p);
        }
    }
    index
}

/// Decode + validate a cover, rejecting art below `min_dim`; center-crop to
/// square if needed (re-encoded as JPEG). Returns None for undecodable/too-small
/// art so we never embed junk.
pub fn validate_and_square(pic: &Picture, min_dim: u32) -> Option<Picture> {
    let img = image::load_from_memory(pic.data()).ok()?;
    let (w, h) = (img.width(), img.height());
    if w.min(h) < min_dim {
        return None;
    }
    if w == h {
        return Some(pic.clone());
    }
    let side = w.min(h);
    let (x, y) = ((w - side) / 2, (h - side) / 2);
    let square = img.crop_imm(x, y, side, side);
    let mut buf = Vec::new();
    square
        .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Jpeg)
        .ok()?;
    Some(Picture::new_unchecked(
        PictureType::CoverFront,
        Some(MimeType::Jpeg),
        None,
        buf,
    ))
}

// ---- pass 3: online waterfall ----------------------------------------------

const UA: &str = concat!("amdl/", env!("CARGO_PKG_VERSION"), " (https://github.com/jakobhviid/amdl)");

/// Strict normalized equality (album titles: brackets/editions/case already
/// stripped by `norm_album`). Used to gate every online match on BOTH artist and
/// album agreeing — a wrong cover is worse than a blank one.
fn matches(query: &str, candidate: &str) -> bool {
    let q = norm_album(query);
    !q.is_empty() && q == norm_album(candidate)
}

/// Try each source in order; the first validated, artist+album-matched cover
/// wins. Never returns a mismatched cover.
fn online_cover(album: &str, artist: &str, min_dim: u32, discogs: Option<&str>) -> Option<Picture> {
    if album.starts_with("(unknown") || artist.starts_with("(unknown") {
        return None;
    }
    musicbrainz_caa(album, artist, min_dim)
        .or_else(|| itunes(album, artist, min_dim))
        .or_else(|| discogs.and_then(|t| discogs_cover(album, artist, min_dim, t)))
}

fn get_json(req: ureq::Request) -> Option<serde_json::Value> {
    let text = req.set("User-Agent", UA).call().ok()?.into_string().ok()?;
    serde_json::from_str(&text).ok()
}

/// Fetch an image URL and validate + square-crop it. `None` on any failure.
fn fetch_cover(url: &str, min_dim: u32) -> Option<Picture> {
    let resp = ureq::get(url).set("User-Agent", UA).call().ok()?;
    let mut bytes = Vec::new();
    resp.into_reader().take(20_000_000).read_to_end(&mut bytes).ok()?;
    let mime = if bytes.starts_with(&[0x89, b'P', b'N', b'G']) { MimeType::Png } else { MimeType::Jpeg };
    let pic = Picture::new_unchecked(PictureType::CoverFront, Some(mime), None, bytes);
    validate_and_square(&pic, min_dim)
}

/// MusicBrainz release search → Cover Art Archive front image (MB asks for
/// ≤1 req/sec, so we pace it).
fn musicbrainz_caa(album: &str, artist: &str, min_dim: u32) -> Option<Picture> {
    std::thread::sleep(std::time::Duration::from_millis(1100));
    let q = format!("release:\"{album}\" AND artist:\"{artist}\"");
    let v = get_json(
        ureq::get("https://musicbrainz.org/ws/2/release")
            .query("query", &q)
            .query("fmt", "json")
            .query("limit", "8"),
    )?;
    for rel in v.get("releases")?.as_array()? {
        let title = rel.get("title").and_then(|x| x.as_str()).unwrap_or("");
        let r_artist = rel
            .get("artist-credit")
            .and_then(|a| a.as_array())
            .and_then(|a| a.first())
            .and_then(|c| c.get("name"))
            .and_then(|x| x.as_str())
            .unwrap_or("");
        if !matches(album, title) || !matches(artist, r_artist) {
            continue;
        }
        let Some(id) = rel.get("id").and_then(|x| x.as_str()) else { continue };
        for endpoint in ["front-500", "front"] {
            if let Some(p) = fetch_cover(&format!("https://coverartarchive.org/release/{id}/{endpoint}"), min_dim) {
                return Some(p);
            }
        }
    }
    None
}

/// iTunes Search API album art (100×100 → 1400×1400), paced to dodge 403/429.
fn itunes(album: &str, artist: &str, min_dim: u32) -> Option<Picture> {
    std::thread::sleep(std::time::Duration::from_millis(200));
    let v = get_json(
        ureq::get("https://itunes.apple.com/search")
            .query("term", &format!("{artist} {album}"))
            .query("entity", "album")
            .query("limit", "12"),
    )?;
    for r in v.get("results")?.as_array()? {
        let c_artist = r.get("artistName").and_then(|x| x.as_str()).unwrap_or("");
        let c_album = r.get("collectionName").and_then(|x| x.as_str()).unwrap_or("");
        if !matches(album, c_album) || !matches(artist, c_artist) {
            continue;
        }
        if let Some(art) = r.get("artworkUrl100").and_then(|x| x.as_str()) {
            if let Some(p) = fetch_cover(&art.replace("100x100bb", "1400x1400bb"), min_dim) {
                return Some(p);
            }
        }
    }
    None
}

/// Discogs search (needs a token) — best for obscure/physical compilations.
fn discogs_cover(album: &str, artist: &str, min_dim: u32, token: &str) -> Option<Picture> {
    std::thread::sleep(std::time::Duration::from_millis(300));
    let v = get_json(
        ureq::get("https://api.discogs.com/database/search")
            .query("release_title", album)
            .query("artist", artist)
            .query("type", "release")
            .query("per_page", "10")
            .query("token", token),
    )?;
    let want = norm_album(album);
    if want.is_empty() {
        return None;
    }
    for r in v.get("results")?.as_array()? {
        let title = r.get("title").and_then(|x| x.as_str()).unwrap_or("");
        // Discogs "title" is "Artist - Album"; require the album to appear.
        if !norm_album(title).contains(&want) {
            continue;
        }
        if let Some(img) = r.get("cover_image").and_then(|x| x.as_str()) {
            if let Some(p) = fetch_cover(img, min_dim) {
                return Some(p);
            }
        }
    }
    None
}

// ---- paste-a-URL human tail -------------------------------------------------

/// A coverless album (for the interactive/scripted paste flow): display title,
/// artist, and the actual track paths to embed into.
pub struct AlbumGroup {
    pub display: String,
    pub artist: String,
    pub tracks: Vec<PathBuf>,
}

/// Coverless albums under `output`, grouped by normalized title and sorted
/// most-tracks-first — so the operator gets the most coverage per URL pasted.
pub fn coverless_albums(output: &Path) -> Vec<AlbumGroup> {
    let mut albums: HashMap<String, AlbumGroup> = HashMap::new();
    for o in list_opus(output) {
        if tags::has_cover(&o) {
            continue;
        }
        let b = tags::read_basic(&o);
        let display = b.album.clone().unwrap_or_else(|| "(unknown album)".into());
        let artist = b.album_artist.or(b.artist).unwrap_or_else(|| "(unknown)".into());
        albums
            .entry(norm_album(&display))
            .or_insert_with(|| AlbumGroup { display, artist, tracks: Vec::new() })
            .tracks
            .push(o);
    }
    let mut v: Vec<AlbumGroup> = albums.into_values().collect();
    v.sort_by(|a, b| b.tracks.len().cmp(&a.tracks.len()).then(a.display.cmp(&b.display)));
    v
}

/// Embed the cover at `url` (a direct image, or a Spotify album page whose
/// og:image is extracted) across **every track** of an album — cover art is an
/// album-level property. Validated + square-cropped; replaces any existing
/// cover. Returns how many tracks were set.
pub fn embed_from_url(tracks: &[PathBuf], url: &str, min_dim: u32) -> anyhow::Result<usize> {
    let img_url = resolve_image_url(url)?;
    let cover = fetch_cover(&img_url, min_dim)
        .ok_or_else(|| anyhow::anyhow!("no valid image (>={min_dim}px, decodable) at {img_url}"))?;
    let mut n = 0;
    for t in tracks {
        if tags::set_cover(t, &cover).is_ok() {
            n += 1;
        }
    }
    Ok(n)
}

/// A Spotify album link → its og:image; anything else is treated as a direct URL.
fn resolve_image_url(url: &str) -> anyhow::Result<String> {
    if url.contains("spotify.com") {
        let html = ureq::get(url).set("User-Agent", UA).call()?.into_string()?;
        return extract_og_image(&html).ok_or_else(|| anyhow::anyhow!("no og:image on {url}"));
    }
    Ok(url.to_string())
}

fn extract_og_image(html: &str) -> Option<String> {
    let idx = html.find("og:image")?;
    let rest = &html[idx..];
    let c = rest.find("content=")?;
    let rest = &rest[c + "content=".len()..];
    let quote = rest.chars().next()?;
    let rest = &rest[quote.len_utf8()..];
    let end = rest.find(quote)?;
    Some(rest[..end].to_string())
}

fn list_opus(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(dir, &mut out);
    out.retain(|p| p.extension().and_then(|e| e.to_str()) == Some("opus"));
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
    use super::*;

    #[test]
    fn albums_group_across_discs_and_case() {
        // multi-disc + editions + case + punctuation all collapse to one key
        let a = norm_album("The Very Best of Pop 1989-90");
        assert_eq!(a, norm_album("The Very Best of Pop 1989-90 [Disc 2]"));
        assert_eq!(a, norm_album("the  very best of pop 1989-90 (Deluxe Edition)"));
        assert_ne!(a, norm_album("The Very Best of Pop 1991-92"));
    }

    fn jpeg_of(w: u32, h: u32) -> Picture {
        let img = image::RgbImage::from_pixel(w, h, image::Rgb([120, 30, 30]));
        let mut buf = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut buf), image::ImageFormat::Jpeg)
            .unwrap();
        Picture::new_unchecked(PictureType::CoverFront, Some(MimeType::Jpeg), None, buf)
    }

    #[test]
    fn cover_too_small_is_rejected() {
        assert!(validate_and_square(&jpeg_of(100, 100), 250).is_none());
    }

    #[test]
    fn wide_cover_is_center_cropped_square() {
        let out = validate_and_square(&jpeg_of(600, 400), 250).expect("valid");
        let img = image::load_from_memory(out.data()).unwrap();
        assert_eq!(img.width(), img.height(), "cropped to square");
        assert_eq!(img.width(), 400, "square side = min edge");
    }

    #[test]
    fn square_cover_passes_through() {
        assert!(validate_and_square(&jpeg_of(500, 500), 250).is_some());
    }
}
