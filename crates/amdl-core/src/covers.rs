//! covers — backfill missing Opus cover art (spec §3), as an ordered funnel from
//! cheapest+safest to riskiest. Implemented here (offline, always-correct passes):
//!   1. copy-from-source — the source file still has embedded art convert didn't need.
//!   2. cross-library copy — a `--reference` library already has the *same album*
//!      covered; copy it (free, guaranteed-correct).
//! Anything still uncovered becomes a **numbered straggler list** (album, artist,
//! track count, impact-sorted) for a human/agent to resolve. Covers are applied
//! per *album* (normalized title, so multi-disc sets and editions group), and
//! every embedded image is validated (decodes, min edge) and square-cropped.
//! Network sources (MusicBrainz/CAA/iTunes/Discogs) are planned (see WORKFLOWS).
//!
//! Safety: only ever embeds into a *coverless* track, validates bytes before
//! writing, and never touches the source. A blank cover beats a wrong one.
use crate::{tags, ui};
use lofty::picture::{MimeType, Picture, PictureType};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};

pub struct Opts {
    /// Read-only source tree to copy embedded art from (pass 1).
    pub source: Option<PathBuf>,
    /// Other output libraries to borrow matching-album covers from (pass 2).
    pub references: Vec<PathBuf>,
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

    // Group coverless tracks by normalized album.
    let mut albums: HashMap<String, Album> = HashMap::new();
    for o in &opus {
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
