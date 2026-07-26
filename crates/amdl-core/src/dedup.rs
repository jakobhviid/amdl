//! dedup — **surface** redundant recordings in a derived library so the operator
//! can prune them. It never deletes: deleting media is the operator's call, so
//! this only reports (with exact paths and a suggested keeper), or emits `rm`
//! lines to stdout for review (`--print-rm`). Two findings, both tag-level (a
//! shared folder can hold correctly- and mis-tagged tracks, so folder identity
//! isn't enough):
//!   - **exact duplicates** — files sharing the same (artist, *normalized* album,
//!     title). The normalized album is the guard against false positives: a song
//!     that appears on both a studio album *and* a compilation has two different
//!     albums, so it is **not** flagged — that cross-release membership is wanted.
//!   - **subset editions** — a raw edition whose track set is a strict subset of
//!     another edition of the *same* release (e.g. Standard ⊂ Deluxe). Multi-disc
//!     sets are safe: their discs have disjoint track sets, so neither is a subset.
//!
//! This tier is heuristic; the report labels it so.
use crate::covers::norm_album;
use crate::{tags, ui};
use rayon::prelude::*;
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub total: usize,
    pub exact_duplicates: Vec<DupCluster>,
    pub subset_editions: Vec<SubsetEdition>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.exact_duplicates.is_empty() && self.subset_editions.is_empty()
    }
    /// The `rm` candidates across both findings (the copies to remove), in the
    /// same impact order as the report — for `--print-rm`.
    pub fn removals(&self) -> Vec<&String> {
        self.exact_duplicates
            .iter()
            .flat_map(|d| d.remove.iter())
            .chain(self.subset_editions.iter().flat_map(|s| s.remove.iter()))
            .collect()
    }
}

/// Two or more files that are the same recording (same artist + normalized album
/// + title). `keep` is the copy in the most-complete edition; `remove` the rest.
#[derive(Debug, Serialize)]
pub struct DupCluster {
    pub artist: String,
    pub album: String,
    pub title: String,
    pub keep: String,
    pub remove: Vec<String>,
    /// Durations across the cluster differ by > 10 s — the "duplicates" may be
    /// distinct versions (radio edit vs album). Left for the operator to judge.
    pub durations_diverge: bool,
}

/// A raw edition wholly contained in another edition of the same release.
#[derive(Debug, Serialize)]
pub struct SubsetEdition {
    pub artist: String,
    pub album: String,
    pub covered_by: String,
    pub tracks: usize,
    pub remove: Vec<String>,
}

struct Track {
    path: PathBuf,
    artist: String,
    album: String,
    title: String,
    dur: Option<u64>,
}

pub fn run(output: &Path) -> Report {
    let opus = list_opus(output);
    let mut report = Report { total: opus.len(), ..Default::default() };
    if opus.is_empty() {
        return report;
    }
    let pb = ui::bar(opus.len() as u64, "Scanning");
    let tracks: Vec<Track> = opus
        .par_iter()
        .filter_map(|p| {
            pb.inc(1);
            let b = tags::read_basic(p);
            // Need all three to match safely — a blank field beats a wrong merge.
            match (b.artist, b.album, b.title) {
                (Some(artist), Some(album), Some(title)) if !artist.is_empty() && !album.is_empty() && !title.is_empty() => {
                    Some(Track { path: p.clone(), artist, album, title, dur: tags::duration_secs(p) })
                }
                _ => None,
            }
        })
        .collect();
    ui::finish_done(&pb);

    report.exact_duplicates = exact_duplicates(&tracks);
    report.subset_editions = subset_editions(&tracks);
    report
}

fn norm(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).flat_map(char::to_lowercase).collect()
}

/// Recording identity: normalized (artist, album, title). Using `norm_album`
/// (which strips edition/disc brackets) collapses editions, so the same song in
/// Standard and Deluxe shares a key — but a different album (a compilation) does
/// not, keeping cross-release copies out of the match.
fn key(t: &Track) -> (String, String, String) {
    (norm(&t.artist), norm_album(&t.album), norm(&t.title))
}

fn exact_duplicates(tracks: &[Track]) -> Vec<DupCluster> {
    // Track count per raw album, to pick the most-complete edition as the keeper.
    let mut album_size: HashMap<&str, usize> = HashMap::new();
    for t in tracks {
        *album_size.entry(t.album.as_str()).or_default() += 1;
    }
    let mut groups: HashMap<(String, String, String), Vec<&Track>> = HashMap::new();
    for t in tracks {
        groups.entry(key(t)).or_default().push(t);
    }
    let mut out = Vec::new();
    for g in groups.values_mut() {
        if g.len() < 2 {
            continue;
        }
        // keeper: the copy in the album with the most tracks; tie → path order.
        g.sort_by(|a, b| {
            album_size[b.album.as_str()]
                .cmp(&album_size[a.album.as_str()])
                .then_with(|| a.path.cmp(&b.path))
        });
        let durs: Vec<u64> = g.iter().filter_map(|t| t.dur).collect();
        let diverge = match (durs.iter().min(), durs.iter().max()) {
            (Some(mn), Some(mx)) => mx - mn > 10,
            _ => false,
        };
        let keep = g[0];
        out.push(DupCluster {
            artist: keep.artist.clone(),
            album: keep.album.clone(),
            title: keep.title.clone(),
            keep: keep.path.display().to_string(),
            remove: g[1..].iter().map(|t| t.path.display().to_string()).collect(),
            durations_diverge: diverge,
        });
    }
    // Impact-sorted (most copies first), deterministic tiebreak.
    out.sort_by(|a, b| {
        b.remove.len().cmp(&a.remove.len()).then_with(|| a.album.cmp(&b.album)).then_with(|| a.title.cmp(&b.title))
    });
    out
}

fn subset_editions(tracks: &[Track]) -> Vec<SubsetEdition> {
    // Group by release (normalized artist + album), then by raw edition.
    type Edition<'a> = (Vec<&'a Track>, HashSet<String>);
    let mut releases: HashMap<(String, String), HashMap<String, Edition>> = HashMap::new();
    for t in tracks {
        let editions = releases.entry((norm(&t.artist), norm_album(&t.album))).or_default();
        let e = editions.entry(t.album.clone()).or_default();
        e.0.push(t);
        e.1.insert(norm(&t.title));
    }
    let mut out = Vec::new();
    for editions in releases.values() {
        if editions.len() < 2 {
            continue;
        }
        for (name, (etracks, titles)) in editions {
            // Is this edition a strict subset of some other edition of the release?
            if let Some(superset) =
                editions.iter().find(|(other, (_, ot))| *other != name && is_strict_subset(titles, ot))
            {
                let mut remove: Vec<String> = etracks.iter().map(|t| t.path.display().to_string()).collect();
                remove.sort();
                out.push(SubsetEdition {
                    artist: etracks[0].artist.clone(),
                    album: name.clone(),
                    covered_by: superset.0.clone(),
                    tracks: etracks.len(),
                    remove,
                });
            }
        }
    }
    out.sort_by(|a, b| b.tracks.cmp(&a.tracks).then_with(|| a.album.cmp(&b.album)));
    out
}

/// `a` is a non-empty strict subset of `b` (every title of `a` is in `b`, and
/// `b` has strictly more). Equal sets are *not* subsets (that's exact-dup land);
/// disjoint sets (multi-disc) are not either.
fn is_strict_subset(a: &HashSet<String>, b: &HashSet<String>) -> bool {
    !a.is_empty() && a.len() < b.len() && a.iter().all(|x| b.contains(x))
}

fn list_opus(dir: &Path) -> Vec<PathBuf> {
    crate::scan::with_exts(dir, &["opus"])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(path: &str, artist: &str, album: &str, title: &str) -> Track {
        Track { path: PathBuf::from(path), artist: artist.into(), album: album.into(), title: title.into(), dur: Some(180) }
    }

    fn set(items: &[&str]) -> HashSet<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn strict_subset_rules() {
        assert!(is_strict_subset(&set(&["a"]), &set(&["a", "b"])));
        assert!(!is_strict_subset(&set(&["a", "b"]), &set(&["a", "b"]))); // equal
        assert!(!is_strict_subset(&set(&[]), &set(&["a"]))); // empty
        assert!(!is_strict_subset(&set(&["a"]), &set(&["b"]))); // disjoint (multi-disc)
    }

    #[test]
    fn cross_release_copy_is_not_a_duplicate() {
        // Same song on a studio album and a compilation → different albums → not a dup.
        let tracks = vec![
            t("/lib/Artist/Studio Album/01 Hit.opus", "Artist", "Studio Album", "Hit"),
            t("/lib/Compilations/Now 45/12 Hit.opus", "Artist", "Now 45", "Hit"),
        ];
        assert!(exact_duplicates(&tracks).is_empty());
    }

    #[test]
    fn same_song_across_editions_is_a_duplicate() {
        // Editions normalize to the same album → the shared track is flagged.
        let tracks = vec![
            t("/lib/Artist/Album/01 Hit.opus", "Artist", "Album", "Hit"),
            t("/lib/Artist/Album (Deluxe)/01 Hit.opus", "Artist", "Album (Deluxe)", "Hit"),
        ];
        let dups = exact_duplicates(&tracks);
        assert_eq!(dups.len(), 1);
        assert_eq!(dups[0].remove.len(), 1);
    }

    #[test]
    fn orphan_edition_is_a_strict_subset() {
        // A lone "Album" track fully covered by "Album (Deluxe)" → subset edition.
        let tracks = vec![
            t("/lib/Artist/Album/01 One.opus", "Artist", "Album", "One"),
            t("/lib/Artist/Album (Deluxe)/01 One.opus", "Artist", "Album (Deluxe)", "One"),
            t("/lib/Artist/Album (Deluxe)/02 Two.opus", "Artist", "Album (Deluxe)", "Two"),
        ];
        let subs = subset_editions(&tracks);
        assert_eq!(subs.len(), 1);
        assert_eq!(subs[0].album, "Album");
        assert_eq!(subs[0].covered_by, "Album (Deluxe)");
        assert_eq!(subs[0].tracks, 1);
    }

    #[test]
    fn multidisc_editions_are_not_subsets() {
        // Discs share a normalized album but have disjoint tracks → neither a subset.
        let tracks = vec![
            t("/lib/VA/Comp [Disc 1]/01 A.opus", "VA", "Comp [Disc 1]", "A"),
            t("/lib/VA/Comp [Disc 2]/01 B.opus", "VA", "Comp [Disc 2]", "B"),
        ];
        assert!(subset_editions(&tracks).is_empty());
    }
}
