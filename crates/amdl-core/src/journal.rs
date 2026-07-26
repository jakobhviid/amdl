//! undo journal — records the reversible inverse of each mutating run so
//! `amdl undo` can revert it. On by default (`--no-undo` skips). **Compact
//! minimal-inverse:** a created file stores just its path + content hash (undo
//! deletes it if unchanged); an in-place tag/cover edit stores the *old* tag
//! snapshot (text fields + the old front-cover bytes, content-addressed in
//! `objects/`) and undo restores it. Undo **never clobbers a file the user
//! changed after amdl left it** — every revert is guarded by an after-hash check.
//!
//! Store lives in the persistent XDG state dir (not temp, so undo survives a
//! reboot): `~/.local/state/amdl/undo` (Linux), `~/Library/Application
//! Support/amdl/undo` (macOS); override with `$AMDL_UNDO_DIR`.
use anyhow::{Context, Result};
use lofty::config::WriteOptions;
use lofty::file::{AudioFile, TaggedFileExt};
use lofty::picture::{MimeType, Picture, PictureType};
use lofty::prelude::{Accessor, ItemKey};
use lofty::probe::Probe;
use lofty::tag::{ItemValue, Tag, TagItem, TagType};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// How many recent runs to retain before pruning the oldest.
const KEEP_RUNS: usize = 25;

static ACTIVE: AtomicBool = AtomicBool::new(false);
static JOURNAL: Mutex<Option<Journal>> = Mutex::new(None);

struct Journal {
    argv: Vec<String>,
    started_unix: u64,
    entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Manifest {
    argv: Vec<String>,
    started_unix: u64,
    entries: Vec<Entry>,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(tag = "op", rename_all = "lowercase")]
enum Entry {
    /// A file amdl created; undo deletes it if it still hashes to `hash`.
    Create { path: PathBuf, hash: String },
    /// A file amdl edited in place; undo restores `before` if it still hashes to `after`.
    Restore { path: PathBuf, before: Snapshot, after: String },
    /// A non-audio file amdl overwrote wholesale (e.g. an `.lrc` upgraded to
    /// synced); undo rewrites the old bytes (stored under `objects/` as `before`)
    /// if the file still hashes to `after`.
    RestoreBytes { path: PathBuf, before: String, after: String },
}

impl Entry {
    fn path(&self) -> &Path {
        match self {
            Entry::Create { path, .. }
            | Entry::Restore { path, .. }
            | Entry::RestoreBytes { path, .. } => path,
        }
    }
}

/// The tag state of a file *before* an in-place edit — enough to put it back.
#[derive(Serialize, Deserialize, Clone, Default)]
struct Snapshot {
    title: Option<String>,
    artist: Option<String>,
    album: Option<String>,
    album_artist: Option<String>,
    compilation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lyrics: Option<String>,
    /// blake3 of the old front-cover bytes (stored under `objects/`), or None if
    /// the file had no cover before the edit.
    picture: Option<String>,
    picture_mime: Option<String>,
}

// ── recording (called by the mutating commands) ──────────────────────────────

/// Start journaling for one mutating run. `argv` is stored for `undo --list`.
pub fn begin(argv: Vec<String>) {
    let started_unix = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    *JOURNAL.lock().unwrap() = Some(Journal { argv, started_unix, entries: Vec::new() });
    ACTIVE.store(true, Ordering::Relaxed);
}

/// Record that `path` was newly created (undo will delete it).
pub fn created(path: &Path) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    if let Ok(hash) = hash_file(path) {
        push(Entry::Create { path: abs(path), hash });
    }
}

/// Record that `path`'s raw bytes were replaced wholesale (e.g. an `.lrc`
/// upgraded to synced). Pass the file's *old* bytes; they're backed into
/// `objects/` and undo rewrites them if the file still hashes to its new state.
/// For tiny sidecars like `.lrc` a full-content backup is cheap. Best-effort:
/// a backup/hash failure is swallowed rather than breaking the command.
pub fn replaced(path: &Path, before: &[u8]) {
    if !ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let before_hash = hash_bytes(before);
    if store_object(&before_hash, before).is_err() {
        return;
    }
    if let Ok(after) = hash_file(path) {
        push(Entry::RestoreBytes { path: abs(path), before: before_hash, after });
    }
}

/// Wrap an in-place edit of `path`: snapshot its tags first, run `f`, then record
/// the inverse. A no-op (just runs `f`) when journaling is off. Snapshot/record
/// failures are swallowed — undo is best-effort and must never break the command.
pub fn edit<T, E>(path: &Path, f: impl FnOnce() -> std::result::Result<T, E>) -> std::result::Result<T, E> {
    if !ACTIVE.load(Ordering::Relaxed) {
        return f();
    }
    let before = snapshot(path).ok();
    let r = f()?;
    if let Ok(after) = hash_file(path) {
        let ap = abs(path);
        if let Some(j) = JOURNAL.lock().unwrap().as_mut() {
            // Collapse onto any entry already recorded for this path this run:
            // a file we *created* stays a creation (keep its hash current → undo
            // deletes it); a file already Restored keeps its *original* snapshot.
            match j.entries.iter_mut().find(|e| e.path() == ap.as_path()) {
                Some(Entry::Create { hash, .. }) => *hash = after,
                Some(Entry::Restore { after: a, .. })
                | Some(Entry::RestoreBytes { after: a, .. }) => *a = after,
                None => {
                    if let Some(before) = before {
                        j.entries.push(Entry::Restore { path: ap, before, after });
                    }
                }
            }
        }
    }
    Ok(r)
}

/// Finish the run: write its manifest atomically (only if anything was recorded),
/// then prune old runs. Safe to call even if `begin` was never called.
pub fn commit() -> Result<()> {
    ACTIVE.store(false, Ordering::Relaxed);
    let Some(j) = JOURNAL.lock().unwrap().take() else { return Ok(()) };
    if j.entries.is_empty() {
        return Ok(());
    }
    let run = runs_dir().join(run_id(j.started_unix, &j.argv));
    std::fs::create_dir_all(&run)?;
    let manifest = Manifest { argv: j.argv, started_unix: j.started_unix, entries: j.entries };
    let tmp = run.join("manifest.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(&manifest)?)?;
    std::fs::rename(&tmp, run.join("manifest.json"))?; // atomic: manifest presence = committed
    let _ = prune();
    Ok(())
}

fn push(e: Entry) {
    if let Some(j) = JOURNAL.lock().unwrap().as_mut() {
        j.entries.push(e);
    }
}

// ── undo (called by `amdl undo`) ─────────────────────────────────────────────

#[derive(Default)]
pub struct UndoReport {
    pub run: Option<String>,
    pub command: Option<String>,
    pub reverted: usize,
    pub skipped: Vec<String>,
    pub dry_run: bool,
}

pub struct RunInfo {
    pub id: String,
    /// The subcommand alone (e.g. `lyrics`) — kept for back-compat.
    pub command: String,
    /// A compact rendering of the run's argv (e.g. `lyrics --embed`) for humans.
    pub summary: String,
    pub started_unix: u64,
    pub changes: usize,
    path: PathBuf,
}

/// Compact one-line rendering of a run's argv for display: the subcommand plus
/// its flags, with long path-like args shortened to their basename so the
/// meaningful part (e.g. `lyrics --embed`) stays visible. Truncates the tail.
fn summarize(argv: &[String]) -> String {
    let parts: Vec<String> = argv
        .iter()
        .skip(1)
        .map(|a| {
            if a.contains('/') {
                // path/URL → just the last non-empty segment
                a.rsplit('/').find(|s| !s.is_empty()).unwrap_or(a).to_string()
            } else {
                a.clone()
            }
        })
        .collect();
    if parts.is_empty() {
        return "run".to_string();
    }
    let joined = parts.join(" ");
    const MAX: usize = 44;
    if joined.chars().count() > MAX {
        let mut s: String = joined.chars().take(MAX - 1).collect();
        s.push('…');
        s
    } else {
        joined
    }
}

/// Reverse the most recent run (or a specific one by id). Best-effort: an entry
/// whose file no longer matches what amdl left is skipped and reported, never
/// forced. On success the run is consumed and orphaned objects are GC'd.
pub fn undo(id: Option<&str>, dry_run: bool) -> Result<UndoReport> {
    let mut runs = list_runs();
    let run = match id {
        Some(id) => runs.into_iter().find(|r| r.id == id),
        None => {
            runs.sort_by_key(|r| std::cmp::Reverse(r.started_unix));
            runs.into_iter().next()
        }
    };
    let Some(run) = run else {
        return Ok(UndoReport { dry_run, ..Default::default() });
    };
    let manifest = read_manifest(&run.path)?;
    let mut report = UndoReport {
        run: Some(run.id.clone()),
        command: manifest.argv.get(1).cloned(),
        dry_run,
        ..Default::default()
    };
    for e in manifest.entries.iter().rev() {
        match revert(e, dry_run) {
            Ok(true) => report.reverted += 1,
            Ok(false) => report.skipped.push(format!("{} (changed since — left as-is)", e.path().display())),
            Err(err) => report.skipped.push(format!("{}: {err}", e.path().display())),
        }
    }
    if !dry_run {
        std::fs::remove_dir_all(&run.path).ok();
        let _ = gc_objects();
    }
    Ok(report)
}

/// The recent runs, newest first.
pub fn list() -> Vec<RunInfo> {
    let mut runs = list_runs();
    runs.sort_by_key(|r| std::cmp::Reverse(r.started_unix));
    runs
}

fn revert(e: &Entry, dry_run: bool) -> Result<bool> {
    match e {
        Entry::Create { path, hash } => {
            if !path.exists() {
                return Ok(false);
            }
            if hash_file(path)? != *hash {
                return Ok(false); // user changed it — don't delete
            }
            if !dry_run {
                std::fs::remove_file(path)?;
                prune_empty_parents(path);
            }
            Ok(true)
        }
        Entry::Restore { path, before, after } => {
            if !path.exists() || hash_file(path)? != *after {
                return Ok(false); // gone or changed since — skip
            }
            if !dry_run {
                restore(path, before)?;
            }
            Ok(true)
        }
        Entry::RestoreBytes { path, before, after } => {
            if !path.exists() || hash_file(path)? != *after {
                return Ok(false); // gone or changed since — skip
            }
            if !dry_run {
                std::fs::write(path, read_object(before)?)?;
            }
            Ok(true)
        }
    }
}

// ── tag snapshot / restore ───────────────────────────────────────────────────

fn snapshot(path: &Path) -> Result<Snapshot> {
    let tagged = Probe::open(path)?.read()?;
    let mut s = Snapshot::default();
    if let Some(tag) = tagged.primary_tag().or_else(|| tagged.first_tag()) {
        s.title = tag.title().map(|c| c.into_owned());
        s.artist = tag.artist().map(|c| c.into_owned());
        s.album = tag.album().map(|c| c.into_owned());
        s.album_artist = tag.get_string(&ItemKey::AlbumArtist).map(str::to_string);
        s.compilation = tag.get_string(&ItemKey::FlagCompilation).map(str::to_string);
        s.lyrics = tag.get_string(&ItemKey::Lyrics).map(str::to_string);
        let pics = tag.pictures();
        if let Some(pic) = pics.iter().find(|p| p.pic_type() == PictureType::CoverFront).or_else(|| pics.first()) {
            let hash = hash_bytes(pic.data());
            store_object(&hash, pic.data())?;
            s.picture = Some(hash);
            s.picture_mime = pic.mime_type().map(|m| m.as_str().to_string());
        }
    }
    Ok(s)
}

fn restore(path: &Path, snap: &Snapshot) -> Result<()> {
    let mut tagged = Probe::open(path)?.read()?;
    if tagged.primary_tag().is_none() {
        tagged.insert_tag(Tag::new(TagType::VorbisComments));
    }
    let tag = tagged.primary_tag_mut().expect("tag present");
    match &snap.title { Some(v) => tag.set_title(v.clone()), None => { tag.remove_title(); } }
    match &snap.artist { Some(v) => tag.set_artist(v.clone()), None => { tag.remove_artist(); } }
    match &snap.album { Some(v) => tag.set_album(v.clone()), None => { tag.remove_album(); } }
    set_or_clear(tag, ItemKey::AlbumArtist, &snap.album_artist);
    set_or_clear(tag, ItemKey::FlagCompilation, &snap.compilation);
    set_or_clear(tag, ItemKey::Lyrics, &snap.lyrics);
    while !tag.pictures().is_empty() {
        tag.remove_picture(0);
    }
    if let Some(hash) = &snap.picture {
        let data = read_object(hash)?;
        let mime = mime_from(snap.picture_mime.as_deref());
        tag.push_picture(Picture::new_unchecked(PictureType::CoverFront, mime, None, data));
    }
    tagged
        .save_to_path(path, WriteOptions::default())
        .with_context(|| format!("restore tags to {}", path.display()))?;
    Ok(())
}

fn set_or_clear(tag: &mut Tag, key: ItemKey, value: &Option<String>) {
    match value {
        Some(v) => { tag.insert(TagItem::new(key, ItemValue::Text(v.clone()))); }
        None => { tag.remove_key(&key); }
    }
}

fn mime_from(s: Option<&str>) -> Option<MimeType> {
    Some(match s? {
        "image/png" => MimeType::Png,
        "image/jpeg" => MimeType::Jpeg,
        other => MimeType::Unknown(other.to_string()),
    })
}

// ── store layout / helpers ───────────────────────────────────────────────────

/// Persistent store root: `$AMDL_UNDO_DIR`, else the OS state dir + `amdl/undo`.
fn store_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("AMDL_UNDO_DIR") {
        return PathBuf::from(d);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    #[cfg(target_os = "macos")]
    let base = PathBuf::from(&home).join("Library/Application Support");
    #[cfg(not(target_os = "macos"))]
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(&home).join(".local/state"));
    base.join("amdl/undo")
}
fn runs_dir() -> PathBuf { store_dir().join("runs") }
fn objects_dir() -> PathBuf { store_dir().join("objects") }

fn run_id(started: u64, argv: &[String]) -> String {
    let cmd = argv.get(1).map(String::as_str).unwrap_or("run");
    let short = &hash_bytes(argv.join(" ").as_bytes())[..8];
    format!("{started}-{cmd}-{short}")
}

fn list_runs() -> Vec<RunInfo> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(runs_dir()) else { return out };
    for e in rd.flatten() {
        let path = e.path();
        if let Ok(m) = read_manifest(&path) {
            out.push(RunInfo {
                id: e.file_name().to_string_lossy().into_owned(),
                command: m.argv.get(1).cloned().unwrap_or_default(),
                summary: summarize(&m.argv),
                started_unix: m.started_unix,
                changes: m.entries.len(),
                path,
            });
        }
    }
    out
}

fn read_manifest(run: &Path) -> Result<Manifest> {
    let bytes = std::fs::read(run.join("manifest.json"))?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn store_object(hash: &str, data: &[u8]) -> Result<()> {
    let dir = objects_dir();
    std::fs::create_dir_all(&dir)?;
    let obj = dir.join(hash);
    if !obj.exists() {
        std::fs::write(obj, data)?;
    }
    Ok(())
}
fn read_object(hash: &str) -> Result<Vec<u8>> {
    std::fs::read(objects_dir().join(hash)).with_context(|| format!("undo backup object {hash} is missing"))
}

/// Keep the newest `KEEP_RUNS`, delete older runs, then GC unreferenced objects.
fn prune() -> Result<()> {
    let mut runs = list_runs();
    runs.sort_by_key(|r| std::cmp::Reverse(r.started_unix));
    for r in runs.into_iter().skip(KEEP_RUNS) {
        std::fs::remove_dir_all(&r.path).ok();
    }
    gc_objects()
}

/// Delete objects not referenced by any surviving run manifest.
fn gc_objects() -> Result<()> {
    let mut referenced = std::collections::HashSet::new();
    for r in list_runs() {
        if let Ok(m) = read_manifest(&r.path) {
            for e in &m.entries {
                match e {
                    Entry::Restore { before, .. } => {
                        if let Some(h) = &before.picture {
                            referenced.insert(h.clone());
                        }
                    }
                    Entry::RestoreBytes { before, .. } => {
                        referenced.insert(before.clone());
                    }
                    Entry::Create { .. } => {}
                }
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(objects_dir()) {
        for e in rd.flatten() {
            if !referenced.contains(&e.file_name().to_string_lossy().into_owned()) {
                std::fs::remove_file(e.path()).ok();
            }
        }
    }
    Ok(())
}

fn prune_empty_parents(path: &Path) {
    let mut cur = path.parent();
    while let Some(dir) = cur {
        if std::fs::remove_dir(dir).is_err() {
            break; // not empty (or permission) — stop
        }
        cur = dir.parent();
    }
}

fn abs(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn hash_file(path: &Path) -> Result<String> {
    Ok(hash_bytes(&std::fs::read(path)?))
}
fn hash_bytes(data: &[u8]) -> String {
    blake3::hash(data).to_hex().to_string()
}
