//! retag — first-class tag edits across a file or folder (spec §7). The headline
//! use is **compilation grouping**: a Various-Artists album whose track artists
//! differ (or are blank) scatters in the player unless `albumartist` is a single
//! value and the compilation flag is set — `--compilation` sets
//! `albumartist=Various Artists` + `compilation=1` across the album so it groups
//! as one. Also sets album/artist/albumartist directly. Idempotent, dry-runnable.
use crate::ui;
use anyhow::Result;
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;
use lofty::tag::{ItemValue, Tag, TagItem};
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Clone, Default)]
pub struct Edit {
    pub compilation: bool,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub artist: Option<String>,
}

impl Edit {
    pub fn is_noop(&self) -> bool {
        !self.compilation && self.album.is_none() && self.album_artist.is_none() && self.artist.is_none()
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Report {
    pub total: usize,
    pub changed: usize,
    pub failed: usize,
    pub dry_run: bool,
}

pub fn run(path: &Path, edit: &Edit, dry_run: bool) -> Report {
    let files = list_audio(path);
    let mut report = Report { total: files.len(), dry_run, ..Default::default() };
    if files.is_empty() || edit.is_noop() {
        return report;
    }
    let pb = ui::bar(files.len() as u64, "Tagging");
    let (changed, failed) = (AtomicUsize::new(0), AtomicUsize::new(0));
    files.par_iter().for_each(|f| {
        if dry_run {
            changed.fetch_add(1, Ordering::Relaxed);
        } else {
            match apply(f, edit) {
                Ok(()) => {
                    changed.fetch_add(1, Ordering::Relaxed);
                }
                Err(e) => {
                    ui::err(&format!("{}: {e}", f.display()));
                    failed.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        pb.inc(1);
    });
    pb.finish_and_clear();
    report.changed = changed.into_inner();
    report.failed = failed.into_inner();
    report
}

fn apply(path: &Path, edit: &Edit) -> Result<()> {
    let mut tagged = Probe::open(path)?.read()?;
    if tagged.primary_tag().is_none() {
        let tt = tagged.file_type().primary_tag_type();
        tagged.insert_tag(Tag::new(tt));
    }
    let tag = tagged.primary_tag_mut().expect("tag present");

    if let Some(a) = &edit.album {
        tag.set_album(a.clone());
    }
    if let Some(a) = &edit.artist {
        tag.set_artist(a.clone());
    }
    // album artist: explicit value, or "Various Artists" implied by --compilation.
    let album_artist = edit
        .album_artist
        .clone()
        .or_else(|| edit.compilation.then(|| "Various Artists".to_string()));
    if let Some(aa) = album_artist {
        tag.insert(TagItem::new(ItemKey::AlbumArtist, ItemValue::Text(aa)));
    }
    if edit.compilation {
        tag.insert(TagItem::new(ItemKey::FlagCompilation, ItemValue::Text("1".to_string())));
    }

    tagged.save_to_path(path, WriteOptions::default())?;
    Ok(())
}

fn list_audio(path: &Path) -> Vec<PathBuf> {
    if path.is_file() {
        return vec![path.to_path_buf()];
    }
    let mut out = Vec::new();
    walk(path, &mut out);
    out.retain(|p| matches!(p.extension().and_then(|e| e.to_str()), Some("opus") | Some("m4a") | Some("mp3")));
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
    use super::Edit;

    #[test]
    fn noop_guard() {
        assert!(Edit::default().is_noop());
        assert!(!Edit { compilation: true, ..Default::default() }.is_noop());
        assert!(!Edit { album: Some("x".into()), ..Default::default() }.is_noop());
        assert!(!Edit { artist: Some("y".into()), ..Default::default() }.is_noop());
    }
}
