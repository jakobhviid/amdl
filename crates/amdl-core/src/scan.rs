//! Walking a library and listing audio files — one implementation for every
//! command. Each used to fork its own `walk` + `list_*` (subtly different),
//! which is why single-file targeting was inconsistent and any traversal change
//! had to be made ~10 times. Route it all through here: a path may be a **file
//! or a directory** everywhere (so targeting one track is free), and the
//! traversal lives in one place.
use std::path::{Path, PathBuf};

/// Audio containers amdl recognises (superset; callers narrow as needed).
pub const AUDIO_EXTS: &[&str] = &["opus", "m4a", "mp3", "flac", "ogg", "oga", "aac", "wav", "aiff", "aif", "wma", "m4b", "alac"];

/// Recursively collect every file under `dir` (unsorted). A non-directory path
/// yields nothing — use [`with_exts`] for the file-or-dir entry point.
pub fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk_into(dir, &mut out);
    out
}

fn walk_into(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk_into(&p, out);
            } else {
                out.push(p);
            }
        }
    }
}

/// Whether `p`'s extension is one of `exts` (case-insensitive).
pub fn has_ext(p: &Path, exts: &[&str]) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .map(|e| exts.iter().any(|x| x.eq_ignore_ascii_case(e)))
        .unwrap_or(false)
}

/// Files matching `exts` under `path`, which may be a **single file** (returned
/// iff it matches) or a **directory** (walked recursively). Sorted. This is the
/// one entry point commands should use so single-file targeting works uniformly.
pub fn with_exts(path: &Path, exts: &[&str]) -> Vec<PathBuf> {
    let mut out = if path.is_file() {
        if has_ext(path, exts) {
            vec![path.to_path_buf()]
        } else {
            Vec::new()
        }
    } else {
        let mut v = walk(path);
        v.retain(|p| has_ext(p, exts));
        v
    };
    out.sort();
    out
}

/// All recognised audio ([`AUDIO_EXTS`]) under `path` (file or dir), sorted.
pub fn audio(path: &Path) -> Vec<PathBuf> {
    with_exts(path, AUDIO_EXTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn has_ext_is_case_insensitive() {
        assert!(has_ext(Path::new("a/b.OPUS"), &["opus"]));
        assert!(has_ext(Path::new("a/b.mp3"), &["opus", "mp3"]));
        assert!(!has_ext(Path::new("a/b.txt"), &["opus", "mp3"]));
        assert!(!has_ext(Path::new("a/b"), &["opus"]));
    }
}
