//! amdl — Apple Music → validated → Opus, into your library. A CLI around
//! gamdl (download/decrypt) + ffmpeg (validate/convert), plus library-maintenance
//! commands. The logic lives in `amdl-core`; this is the thin CLI layer.
use amdl_core::{config, convert, cookies, covers, dedup, doctor, download, identify, journal, lyrics, recover, retag, stats, ui, validate};
use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

const REPO_URL: &str = "https://github.com/jakobhviid/amdl";
const AFTER_HELP: &str = concat!(
    "Repository: https://github.com/jakobhviid/amdl (inspect the source there if needed)\n",
    "LLM guide: pass `--llm` for a full machine-readable reference (every command + the workflows)."
);

#[derive(Parser)]
#[command(name = "amdl", version, about = "Music-library harness: validate, transcode to Opus, and keep your library consistent (wraps gamdl + ffmpeg).", after_help = AFTER_HELP, after_long_help = AFTER_HELP, arg_required_else_help = true)]
struct Cli {
    /// Emit machine-readable JSON instead of the human summary (composable).
    #[arg(long, global = true)]
    json: bool,
    /// Print the full LLM-readable guide (every command + workflows + repo link) and exit.
    #[arg(long, global = true)]
    llm: bool,
    /// Quiet: print only the one-line result headline (and errors); no breakdown or bars.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,
    /// Verbose: also list per-item detail (every affected file) under the summary.
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Don't journal this run for `amdl undo` (skips the undo safety net).
    #[arg(long, global = true)]
    no_undo: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Fetch URL(s) via gamdl, then validate → Opus into your library.
    Download {
        /// Source URL(s), fetched via gamdl.
        urls: Vec<String>,
        /// Output library (default: current directory).
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Cookies file (or set $AMDL_COOKIES_FILE; default: auto-detect from your browser).
        #[arg(long, env = "AMDL_COOKIES_FILE")]
        cookies: Option<PathBuf>,
        /// Scratch dir for gamdl output (default: a temp dir).
        #[arg(long)]
        work_dir: Option<PathBuf>,
        /// Keep the intermediate .m4a work dir instead of deleting it.
        #[arg(long)]
        keep_work: bool,
        /// Opus bitrate/quality (default: config [convert] bitrate, else 192k).
        #[arg(long)]
        bitrate: Option<String>,
        /// Parallel conversion jobs (default: CPU count).
        #[arg(short, long)]
        jobs: Option<usize>,
        /// Primary Apple Music storefront (country code).
        #[arg(long, default_value = "dk")]
        storefront: String,
        /// Comma-separated fallback storefronts.
        #[arg(long, default_value = "us,gb")]
        fallback: String,
        /// Keep the downloaded .m4a as-is; skip Opus conversion.
        #[arg(long)]
        no_convert: bool,
    },
    /// Transcode an existing library of .m4a/.mp3 to Opus (source → output).
    Convert {
        /// Source library root, read-only (default: config [paths] source).
        src: Option<PathBuf>,
        /// Output/derived library (default: config [paths] output, else cwd).
        dest: Option<PathBuf>,
        /// Opus bitrate/quality (default: config [convert] bitrate, else 192k).
        #[arg(long)]
        bitrate: Option<String>,
        /// Parallel conversion jobs (default: CPU count).
        #[arg(short, long)]
        jobs: Option<usize>,
    },
    /// Health/integrity scan of a derived Opus library: missing covers/tags,
    /// unreadable, and (with --source) truncated + unconverted files.
    Doctor {
        /// Output/derived (Opus) library to scan (default: config [paths] output).
        output: Option<PathBuf>,
        /// Compare against this source tree to also find truncated + unconverted files.
        #[arg(short, long)]
        source: Option<PathBuf>,
        /// Full-decode every Opus (ffmpeg) to catch stream corruption a metadata
        /// probe misses — no source library needed. Slower (decodes all audio).
        #[arg(long)]
        deep: bool,
    },
    /// Library overview (read-only): formats, bitrate/sample-rate/channel spread,
    /// cover + tag completeness, and lyrics state at every level (sidecar +
    /// embedded, plain/generated/synced). Scans all audio, not just Opus. `--json`.
    Stats {
        /// Library to scan — any audio tree (default: config [paths] output).
        library: Option<PathBuf>,
    },
    /// Backfill missing Opus cover art: copy from source, then cross-library
    /// (--reference). Prints a numbered straggler list for albums still uncovered.
    Covers {
        /// Output/derived library to fill (default: config [paths] output).
        output: Option<PathBuf>,
        /// Source tree to copy embedded art from (default: config [paths] source).
        #[arg(short, long)]
        source: Option<PathBuf>,
        /// Other output libraries to borrow matching-album covers from.
        #[arg(short, long)]
        reference: Vec<PathBuf>,
        /// Enable the online waterfall (MusicBrainz/CAA → iTunes → Discogs).
        #[arg(long)]
        online: bool,
        /// After the automated passes, interactively paste a URL per remaining
        /// album (direct image or Spotify album link) to embed across that album.
        #[arg(long)]
        paste: bool,
        /// Non-interactive paste: read `<n><TAB>url` lines (the numbers from the
        /// straggler list / `--json`) from a file, or `-` for stdin. Scriptable.
        #[arg(long, value_name = "FILE")]
        paste_file: Option<PathBuf>,
        /// Show what would change without writing.
        #[arg(long)]
        dry_run: bool,
        /// Minimum acceptable cover edge (px).
        #[arg(long, default_value_t = 250)]
        min_dim: u32,
    },
    /// Backfill .lrc lyrics from LRCLIB (synced preferred). Writes into the
    /// library only (state-only); skip-existing.
    Lyrics {
        /// Library dir *or* a single audio file to backfill (default: config
        /// [paths] output). A file acts on just that track — handy for a
        /// targeted `lyrics song.opus --force-embed`.
        output: Option<PathBuf>,
        /// Parallel lookups (LRCLIB tolerates ~10).
        #[arg(short, long, default_value_t = 8)]
        jobs: usize,
        /// Don't re-time existing plain .lrc files: only fetch what's missing and
        /// skip anything already present (the cheap pass — no network for existing
        /// sidecars). By default `lyrics` also upgrades plain .lrc to synced when a
        /// source has a timed version (journaled for `undo`; already-synced files
        /// are left untouched either way).
        #[arg(long)]
        no_upgrade: bool,
        /// Deprecated: plain→synced upgrading is the default now, so this is a
        /// no-op kept for compatibility. Use `--no-upgrade` to opt out.
        #[arg(long, hide = true)]
        upgrade_synced: bool,
        /// Also embed each track's lyrics into its audio file's LYRICS tag (the
        /// .lrc sidecar is kept). Embeds existing sidecars and freshly-fetched
        /// alike. Never downgrades: writes only when nothing is embedded or as a
        /// plain→synced upgrade. Journaled for `undo`.
        #[arg(long)]
        embed: bool,
        /// With --embed, overwrite an embedded lyric even when it isn't an
        /// upgrade — including replacing an already-synced embed. Implies --embed.
        #[arg(long)]
        force_embed: bool,
        /// Skip forced alignment. Alignment (generating *synced* lyrics from plain
        /// ones by listening to the track, marked `[re:amdl-align]`) runs
        /// automatically for the untimed residue whenever `[lyrics] aligner_url`
        /// is configured; this opts out of it (still fetches + upgrades).
        #[arg(long)]
        no_align: bool,
        /// Mark the target (a file or a whole album dir) as **instrumental**:
        /// strip any lyrics it has (the .lrc sidecar and the embedded LYRICS tag)
        /// and stamp AMDL_INSTRUMENTAL so `lyrics` skips it forever. For fixing a
        /// wrong lyric that no automatic check catches. Journaled (`undo` restores
        /// the lyrics). No fetching happens on this run.
        #[arg(long, conflicts_with = "unmark_instrumental")]
        mark_instrumental: bool,
        /// Clear a previously-set AMDL_INSTRUMENTAL mark on the target (does not
        /// restore stripped lyrics — re-run `lyrics` for that, or `undo`).
        #[arg(long)]
        unmark_instrumental: bool,
    },
    /// Identify tracks by sound (AcoustID fingerprint) to fix untagged/mis-tagged
    /// files. Needs [keys] acoustid (or $ACOUSTID_KEY). --apply writes the match.
    Identify {
        /// File or directory to identify.
        path: PathBuf,
        /// Write the resolved artist/title/album (default: report only).
        #[arg(long)]
        apply: bool,
        /// Preview what --apply would write, without touching any file.
        #[arg(long)]
        dry_run: bool,
        /// Only auto-apply a match at/above this AcoustID score (0.0–1.0). A wrong
        /// tag is worse than none, so low-confidence matches are left untouched.
        #[arg(long, default_value_t = amdl_core::identify::DEFAULT_MIN_SCORE)]
        min_score: f64,
        /// Skip files that already have artist+title+album (resumes a big untagged
        /// run). Off by default, since identify also fixes mis-tagged files.
        #[arg(long)]
        skip_tagged: bool,
    },
    /// Set tags across a file/folder. `--compilation` groups a Various-Artists
    /// album (albumartist=Various Artists + compilation=1); also set album/artist.
    Tag {
        /// File or directory to retag (all audio under a dir).
        path: PathBuf,
        /// Group as a Various-Artists compilation.
        #[arg(long)]
        compilation: bool,
        /// Set the album.
        #[arg(long)]
        album: Option<String>,
        /// Set the album artist.
        #[arg(long = "album-artist")]
        album_artist: Option<String>,
        /// Set the (track) artist.
        #[arg(long)]
        artist: Option<String>,
        /// Preview without writing.
        #[arg(long)]
        dry_run: bool,
    },
    /// Show config path + values; `--init` writes a starter ~/.config/amdl/config.toml.
    Config {
        /// Write a starter config file (won't overwrite an existing one).
        #[arg(long)]
        init: bool,
    },
    /// Get/set/delete individual config settings. Every write re-renders the full
    /// annotated config.toml, so its inline help is kept. Keys work as words
    /// (`lyrics hints`) or dotted (`lyrics.hints`); booleans take on/off.
    #[command(after_help = CONFIGURE_HELP, after_long_help = CONFIGURE_HELP)]
    Configure {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Re-acquire broken/missing tracks (source files that never produced an
    /// Opus): cross-library copy from --reference, else re-acquire via gamdl
    /// (--online). Metadata from tags, else folder+filename.
    Recover {
        /// Output/derived library (default: config [paths] output).
        output: Option<PathBuf>,
        /// Source tree to detect missing/broken against (default: config [paths] source).
        #[arg(short, long)]
        source: Option<PathBuf>,
        /// Reference libraries to copy matching tracks from (free, no download).
        #[arg(short, long)]
        reference: Vec<PathBuf>,
        /// Re-acquire tracks no reference has, via gamdl (needs cookies).
        #[arg(long)]
        online: bool,
        /// Cookies for re-acquisition (or $AMDL_COOKIES_FILE).
        #[arg(long, env = "AMDL_COOKIES_FILE")]
        cookies: Option<PathBuf>,
        /// Opus bitrate for re-acquired tracks (default: config, else 192k).
        #[arg(long)]
        bitrate: Option<String>,
        /// Report what would be recovered without writing/downloading.
        #[arg(long)]
        dry_run: bool,
    },
    /// Surface duplicate/orphan tracks (never deletes): exact-duplicate recordings
    /// and subset editions (e.g. Standard ⊂ Deluxe), with the paths to remove and
    /// which copy to keep. `--print-rm` emits `rm` lines to stdout for you to review.
    Dedup {
        /// Output/derived library to scan (default: config [paths] output).
        output: Option<PathBuf>,
        /// Print `rm` lines (the redundant copies) to stdout for review — never runs them.
        #[arg(long)]
        print_rm: bool,
    },
    /// Revert a mutating run — deletes files amdl created and restores tags/covers
    /// it changed. Skips anything you've edited since (never clobbers your
    /// changes). On a terminal, a bare `amdl undo` opens an interactive picker
    /// (dated, newest-first); pass a run id, `--dry-run`, `--json`, or pipe it to
    /// get the direct revert-most-recent behavior instead. `--list` shows runs.
    Undo {
        /// A specific run id (from `--list`); default: the most recent run (or
        /// the interactive picker on a terminal).
        run: Option<String>,
        /// List recent undoable runs instead of reverting.
        #[arg(long)]
        list: bool,
        /// Preview what would be reverted without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Check login cookies: report gamdl's file and auto-detect from your browser.
    Cookies,
    /// Print a shell completion script (bash|zsh|fish|…) to stdout.
    #[command(hide = true)]
    Completions { shell: clap_complete::Shell },
    /// Print a man page (roff) to stdout.
    #[command(hide = true)]
    Man,
}

/// Shown under `amdl configure --help`, so the settable keys are discoverable
/// without a separate command. Kept in sync with `config::KEYS` by a unit test.
const CONFIGURE_HELP: &str = "\
Settable keys (run `amdl configure keys` for a description of each):
  paths.source          paths.output          convert.bitrate
  keys.acoustid         keys.discogs
  lyrics.lrcapi_url     lyrics.lrcapi_key     lyrics.lrcapi_first
  lyrics.aligner_url    lyrics.hints

Keys can be written as words or dotted, and booleans take on/off:
  amdl configure set lyrics hints off
  amdl configure set lyrics.hints off
  amdl configure set paths output /music/lib
  amdl configure get lyrics aligner_url";

/// Sub-actions of `amdl configure`. A key can be given as words (`lyrics hints`)
/// or dotted (`lyrics.hints`); [`join_key`] normalizes both. Booleans take
/// on/off (true/false also accepted). See `configure keys` / `configure --help`.
#[derive(Subcommand)]
enum ConfigAction {
    /// Set (or update) a setting: `configure set lyrics hints off`.
    Set {
        /// Key (words or dotted) followed by the value, e.g. `lyrics hints off`.
        #[arg(required = true, num_args = 2.., value_name = "KEY... VALUE")]
        args: Vec<String>,
    },
    /// Delete a setting, reverting it to its default: `configure unset lyrics aligner_url`.
    Unset {
        /// Key, words or dotted (see `configure keys`).
        #[arg(required = true, num_args = 1.., value_name = "KEY")]
        key: Vec<String>,
    },
    /// Print one setting's current value (nothing if unset) — for scripts.
    Get {
        /// Key, words or dotted (see `configure keys`).
        #[arg(required = true, num_args = 1.., value_name = "KEY")]
        key: Vec<String>,
    },
    /// List every setting with its current value (`--json` for a machine object).
    List,
    /// List every settable key with a one-line description.
    Keys,
}

/// Normalize a key given as words (`["lyrics","hints"]`) or as a single dotted
/// token (`["lyrics.hints"]`) into the canonical dotted key `lyrics.hints`.
fn join_key(parts: &[String]) -> String {
    parts.join(".")
}

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn temp_work() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".cache"));
    base.join("amdl").join(format!("work-{}", std::process::id()))
}

#[allow(clippy::too_many_arguments)]
fn cmd_download(
    urls: Vec<String>,
    out: PathBuf,
    cookies_file: Option<PathBuf>,
    work_dir: PathBuf,
    keep_work: bool,
    bitrate: &str,
    jobs: usize,
    storefronts: Vec<String>,
    no_convert: bool,
) -> Result<()> {
    let cookies_file = cookies::resolve(cookies_file, false)?;
    std::fs::create_dir_all(&work_dir)?;
    std::fs::create_dir_all(&out)?;
    for url in &urls {
        ui::info(&format!("↓ {url}"));
        let opts = download::Opts {
            cookies: cookies_file.clone(),
            storefronts: storefronts.clone(),
            output: work_dir.clone(),
            artist_auto: url.contains("/artist/"),
        };
        let tracks = download::download(url, &opts)?;

        let bad = validate::probe_bad(&tracks);
        if !bad.is_empty() {
            ui::warn(&format!("{} track(s) failed the decode check (likely encrypted-payload bug)", bad.len()));
            for b in &bad {
                ui::warn(&format!("  {}", b.display()));
            }
        }

        if no_convert {
            let mut kept = 0usize;
            let mut failed = 0usize;
            for t in &tracks {
                let rel = t.strip_prefix(&work_dir).unwrap_or(t);
                let dst = out.join(rel);
                let moved = dst
                    .parent()
                    .map_or(Ok(()), std::fs::create_dir_all)
                    .and_then(|_| std::fs::rename(t, &dst).or_else(|_| std::fs::copy(t, &dst).map(|_| ())));
                match moved {
                    Ok(_) => kept += 1,
                    Err(e) => {
                        failed += 1;
                        ui::warn(&format!("could not place {}: {e}", rel.display()));
                    }
                }
            }
            if failed > 0 {
                ui::warn(&format!("kept {kept} .m4a in {} ({failed} failed to move)", out.display()));
            } else {
                ui::ok(&format!("kept {kept} .m4a in {}", out.display()));
            }
        } else {
            let r = convert::convert_files(&tracks, &work_dir, &out, bitrate, jobs)?;
            ui::ok(&format!(
                "{} track(s) → Opus in {} ({} with cover, {} skipped)",
                r.converted, out.display(), r.with_cover, r.skipped
            ));
        }
    }
    if !keep_work {
        std::fs::remove_dir_all(&work_dir).ok();
    } else {
        ui::info(&format!("kept work dir: {}", work_dir.display()));
    }
    Ok(())
}

fn print_stats(s: &stats::Stats) {
    use ui::Tone::{Bad, Good, Warn};
    let dur = human_duration(s.total_duration_secs);
    let size = human_bytes(s.total_bytes);
    let metrics = [
        ui::tally("tracks", s.tracks, Good),
        ui::tally("artists", s.artists, Good),
        ui::tally("albums", s.albums, Good),
        ui::tally("no-cover", s.without_cover, Warn),
        ui::tally("unreadable", s.unreadable, Bad),
    ];
    ui::result(&format!("stats · {} · {size} · {dur}", s.root), false, &metrics, &[]);
    if ui::is_quiet() {
        return;
    }

    let dist = |label: &str, items: &[stats::Count]| {
        if items.is_empty() {
            return;
        }
        println!("  {label}:");
        for c in items {
            println!("    {:<10} {}", c.name, c.count);
        }
    };

    dist("formats", &s.formats);

    println!("  bitrate (kbps):");
    if s.bitrate_kbps.avg > 0 {
        println!("    min {}  ·  avg {}  ·  max {}", s.bitrate_kbps.min, s.bitrate_kbps.avg, s.bitrate_kbps.max);
    }
    for c in &s.bitrate_kbps.distribution {
        println!("    {:<10} {}", c.name, c.count);
    }
    if s.bitrate_kbps.unknown > 0 {
        println!("    {:<10} {}", "unknown", s.bitrate_kbps.unknown);
    }

    dist("sample rate (Hz)", &s.sample_rates);
    dist("channels", &s.channels);

    println!("  cover art:");
    println!("    {:<10} {}", "embedded", s.with_cover);
    println!("    {:<10} {}", "missing", s.without_cover);

    println!("  tags:");
    println!("    {:<14} {}", "fully tagged", s.fully_tagged);
    println!("    {:<14} {}", "missing title", s.missing_title);
    println!("    {:<14} {}", "missing artist", s.missing_artist);
    println!("    {:<14} {}", "missing album", s.missing_album);

    let tier = |c: &stats::TierCounts| format!("none {}  plain {}  generated {}  synced {}", c.none, c.plain, c.generated, c.synced);
    println!("  lyrics:");
    println!("    sidecar   {}", tier(&s.lyrics.sidecar));
    println!("    embedded  {}", tier(&s.lyrics.embedded));
    println!(
        "    coverage  timed {}  ·  plain-only {}  ·  none {}",
        s.lyrics.timed_anywhere, s.lyrics.plain_only, s.lyrics.none_anywhere
    );
}

/// Human-readable byte size (binary units), e.g. `12.3 GiB`.
fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KiB", "MiB", "GiB", "TiB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{n} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Human-readable duration, e.g. `12d 3h 4m` / `3h 4m` / `4m`.
fn human_duration(secs: u64) -> String {
    let (d, h, m) = (secs / 86_400, (secs % 86_400) / 3600, (secs % 3600) / 60);
    if d > 0 {
        format!("{d}d {h}h {m}m")
    } else if h > 0 {
        format!("{h}h {m}m")
    } else {
        format!("{m}m")
    }
}

fn print_health(h: &doctor::Health) {
    use ui::Tone::{Bad, Warn};
    let metrics = [
        ui::tally("missing-cover", h.missing_cover.len(), Warn),
        ui::tally("missing-tags", h.missing_tags.len(), Warn),
        ui::tally("unreadable", h.unreadable.len(), Bad),
        ui::tally("truncated", h.truncated.len(), Bad),
        ui::tally("corrupt", h.corrupt.len(), Bad),
        ui::tally("no-opus", h.source_without_opus.len(), Warn),
    ];
    let mut hints = Vec::new();
    if !h.missing_cover.is_empty() { hints.push("fill covers: `amdl covers --source … --online`".to_string()); }
    if !h.missing_tags.is_empty() { hints.push("fix tags: `amdl identify --apply` (by sound) or `amdl tag …`".to_string()); }
    if !h.truncated.is_empty() { hints.push("truncated: delete the bad .opus, then `amdl convert` regenerates it (W3)".to_string()); }
    if !h.corrupt.is_empty() { hints.push("corrupt: re-convert from source, or `amdl recover`".to_string()); }
    if !h.source_without_opus.is_empty() { hints.push("unconverted sources: `amdl recover --source …`".to_string()); }
    ui::result(&format!("doctor · {} opus scanned", h.total), false, &metrics, &hints);
    if ui::is_quiet() {
        return;
    }
    // Offending paths, capped, so the summary stays scannable (full list via --json).
    let cat = |label: &str, v: &[String]| {
        if v.is_empty() { return; }
        let cap = if ui::is_verbose() { v.len() } else { 8 };
        println!("  {label}:");
        for x in v.iter().take(cap) {
            println!("    {x}");
        }
        if v.len() > cap {
            println!("    … and {} more (--json for all, -v to list)", v.len() - cap);
        }
    };
    cat("unreadable", &h.unreadable);
    cat("missing cover", &h.missing_cover);
    cat("missing tags", &h.missing_tags);
    cat("truncated", &h.truncated);
    cat("corrupt (failed decode)", &h.corrupt);
    cat("source without opus", &h.source_without_opus);
}

fn print_covers(r: &covers::Report) {
    use ui::Tone::{Good, Warn};
    let metrics = [
        ui::tally("filled", r.albums_filled, Good),
        ui::tally("from-source", r.filled_from_source, Good),
        ui::tally("from-reference", r.filled_from_reference, Good),
        ui::tally("online", r.filled_online, Good),
        ui::tally("still-need-cover", r.stragglers.len(), Warn),
    ];
    let mut hints = Vec::new();
    if !r.stragglers.is_empty() {
        hints.push("resolve the rest by number: `--paste` (interactive) or `--paste-file <n\\turl>` (scriptable)".to_string());
    }
    ui::result(&format!("covers · {} coverless album(s)", r.coverless_albums), r.dry_run, &metrics, &hints);
    if ui::is_quiet() {
        return;
    }
    for s in &r.stragglers {
        println!("  {:>3}. {} — {} ({} tracks)", s.n, s.album, s.artist, s.tracks);
    }
}

fn print_recover(r: &recover::Report) {
    use ui::Tone::{Good, Warn};
    let metrics = [
        ui::tally("broken", r.broken, Warn),
        ui::tally("from-sibling", r.recovered_from_sibling, Good),
        ui::tally("re-acquired", r.reacquired, Good),
        ui::tally("regrouped", r.regrouped, Good),
        ui::tally("still-broken", r.still_broken.len(), Warn),
    ];
    let mut hints = Vec::new();
    if !r.still_broken.is_empty() && !r.dry_run {
        hints.push("still broken: try `--online` to re-acquire, or `--reference <lib>` to copy from a sibling library".to_string());
    }
    ui::result("recover", r.dry_run, &metrics, &hints);
    if ui::is_quiet() {
        return;
    }
    let cap = if ui::is_verbose() { r.still_broken.len() } else { 20 };
    for s in r.still_broken.iter().take(cap) {
        println!("    {s}");
    }
    if r.still_broken.len() > cap {
        println!("    … and {} more (-v to list)", r.still_broken.len() - cap);
    }
}

/// A single self-contained, plain-text guide an LLM/agent can read to drive amdl
/// from zero: the full command reference (rendered from clap) followed by the
/// end-to-end workflows, plus the repo link for source inspection.
fn llm_guide() -> String {
    let mut cmd = Cli::command();
    let mut out = String::new();
    out.push_str(&format!("amdl {} — LLM guide\n", env!("CARGO_PKG_VERSION")));
    out.push_str(&format!("Repository: {REPO_URL}  (read the source there if you need behavior details)\n"));
    out.push_str("This is the same reference as `man amdl`, laid out plainly for LLM reading.\n\n");

    out.push_str("================================ COMMAND REFERENCE ================================\n\n");
    out.push_str(&cmd.render_long_help().to_string());
    for sub in cmd.get_subcommands_mut() {
        if sub.is_hide_set() {
            continue;
        }
        out.push_str(&format!("\n\n-------------------------------- amdl {} --------------------------------\n\n", sub.get_name()));
        out.push_str(&sub.render_long_help().to_string());
    }

    out.push_str("\n\n================================ WORKFLOWS ================================\n\n");
    out.push_str(include_str!("../../../WORKFLOWS.md"));
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Dependency-free "N ago" rendering of a unix timestamp for the undo list.
fn rel_time(then: u64) -> String {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let secs = now.saturating_sub(then);
    match secs {
        0..=44 => "just now".to_string(),
        45..=89 => "a min ago".to_string(),
        90..=3599 => format!("{} min ago", (secs + 30) / 60),
        3600..=7199 => "an hour ago".to_string(),
        7200..=86399 => format!("{} h ago", (secs + 1800) / 3600),
        86400..=172_799 => "yesterday".to_string(),
        _ => format!("{} days ago", secs / 86400),
    }
}

/// Interactive `undo` for a terminal: list recent runs (newest first, most-recent
/// preselected) and let the user pick one, preview it with a dry-run, or cancel.
/// Only reached on a TTY for a bare `undo`; scripts keep the direct path.
fn undo_interactive() -> Result<()> {
    let runs = journal::list();
    if runs.is_empty() {
        ui::info("nothing to undo — no runs recorded");
        return Ok(());
    }
    loop {
        ui::info("Pick a run to undo (newest first):");
        for (i, r) in runs.iter().enumerate() {
            let marker = if i == 0 { ">" } else { " " };
            println!(
                "  {} {:>2}  {:<11}  {:<40}  {} file(s)",
                marker, i + 1, rel_time(r.started_unix), r.summary, r.changes
            );
        }
        let ans = ui::ask("\n[Enter]=1  number=pick  d[N]=dry-run  q=cancel:");
        let ans = ans.trim();
        if ans.eq_ignore_ascii_case("q") {
            ui::info("cancelled — nothing reverted");
            return Ok(());
        }
        // `d` / `dN` previews without reverting; Enter or a bare number reverts.
        let (dry, num) = match ans.strip_prefix(['d', 'D']) {
            Some(rest) => (true, rest.trim()),
            None => (false, ans),
        };
        let idx = if num.is_empty() {
            0
        } else {
            match num.parse::<usize>() {
                Ok(n) if (1..=runs.len()).contains(&n) => n - 1,
                _ => {
                    ui::warn("enter a run number, d/dN to preview, or q to cancel");
                    continue;
                }
            }
        };
        let rep = journal::undo(Some(&runs[idx].id), dry)?;
        print_undo(&rep);
        if dry {
            continue; // previewed only — pick again or confirm
        }
        return Ok(());
    }
}

fn print_undo(r: &journal::UndoReport) {
    use ui::Tone::{Bad, Good};
    let Some(run) = &r.run else {
        ui::info("nothing to undo — no runs recorded");
        return;
    };
    let head = format!("undo · {}", r.command.as_deref().unwrap_or(run));
    ui::result(
        &head,
        r.dry_run,
        &[ui::tally("reverted", r.reverted, Good), ui::tally("skipped", r.skipped.len(), Bad)],
        &[],
    );
    if ui::is_quiet() {
        return;
    }
    for s in &r.skipped {
        println!("    skipped: {s}");
    }
}

fn print_dedup(r: &dedup::Report) {
    use ui::Tone::Warn;
    let redundant: usize = r.exact_duplicates.iter().map(|d| d.remove.len()).sum::<usize>()
        + r.subset_editions.iter().map(|s| s.remove.len()).sum::<usize>();
    let metrics = [
        ui::tally("duplicate-groups", r.exact_duplicates.len(), Warn),
        ui::tally("subset-editions", r.subset_editions.len(), Warn),
        ui::tally("redundant-files", redundant, Warn),
    ];
    let hints = if r.is_clean() {
        Vec::new()
    } else {
        vec!["nothing was deleted — review, then remove yourself or with `amdl dedup --print-rm`".to_string()]
    };
    ui::result(&format!("dedup · {} tracks scanned", r.total), false, &metrics, &hints);
    if r.is_clean() || ui::is_quiet() {
        return;
    }
    if !r.exact_duplicates.is_empty() {
        ui::warn("exact-duplicate recordings (keep one, the rest are redundant):");
        for d in r.exact_duplicates.iter().take(30) {
            let note = if d.durations_diverge { "  [durations differ — may be distinct versions]" } else { "" };
            println!("  {} — {} · {}{}", d.artist, d.title, d.album, note);
            println!("    keep:   {}", d.keep);
            for rm in &d.remove {
                println!("    remove: {rm}");
            }
        }
        if r.exact_duplicates.len() > 30 {
            println!("    … and {} more groups", r.exact_duplicates.len() - 30);
        }
    }
    if !r.subset_editions.is_empty() {
        ui::warn("subset editions — one edition wholly contained in another (heuristic):");
        for s in r.subset_editions.iter().take(30) {
            println!("  {} — {} ({} tracks) ⊂ {}", s.artist, s.album, s.tracks, s.covered_by);
            for rm in &s.remove {
                println!("    remove: {rm}");
            }
        }
        if r.subset_editions.len() > 30 {
            println!("    … and {} more", r.subset_editions.len() - 30);
        }
    }
}

/// Single-quote a path for a POSIX shell (for `--print-rm` output).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn print_identify(r: &identify::Report) {
    use ui::Tone::{Bad, Dim, Good, Warn};
    let applied = if r.dry_run { "would-apply" } else { "applied" };
    let metrics = [
        ui::metric("matched", format!("{}/{}", r.matched, r.total), if r.matched > 0 { Good } else { Dim }),
        ui::tally(applied, r.applied, Good),
        ui::tally("low-score", r.skipped_low_score, Warn),
        ui::tally("skipped-tagged", r.skipped_tagged, Dim),
        ui::tally("no-match", r.no_match, Dim),
        ui::tally("failed", r.failed, Bad),
    ];
    let mut hints = Vec::new();
    if r.matched > r.applied && !r.dry_run && r.applied == 0 && r.skipped_low_score == 0 {
        hints.push("matches found — re-run with `--apply` to write them".to_string());
    }
    if r.skipped_low_score > 0 {
        hints.push("some matches were below the score gate — lower `--min-score` only if you trust them".to_string());
    }
    ui::result("identify", r.dry_run, &metrics, &hints);
    if ui::is_quiet() {
        return;
    }
    for fr in &r.results {
        if let Some(m) = &fr.matched {
            println!(
                "  {} → {} — {} · {} ({:.0}%)",
                fr.file,
                m.artist.as_deref().unwrap_or("?"),
                m.title.as_deref().unwrap_or("?"),
                m.album.as_deref().unwrap_or("?"),
                m.score * 100.0
            );
        }
    }
}

/// Interactive human-tail: list the remaining coverless albums (most tracks
/// first) and let the operator paste a URL per album; each is embedded across
/// the whole album.
fn run_paste(output: &std::path::Path, min_dim: u32) -> Result<()> {
    let albums = covers::coverless_albums(output);
    if albums.is_empty() {
        ui::ok("no coverless albums remain — nothing to paste");
        return Ok(());
    }
    println!();
    ui::info(&format!("{} album(s) still need a cover (most tracks first):", albums.len()));
    for (i, a) in albums.iter().enumerate() {
        println!("  {:>3}. {} — {} ({} tracks)", i + 1, a.display, a.artist, a.tracks.len());
    }
    println!();
    ui::info("Paste  <number> <url>  per album — a direct image URL or a Spotify album link. Blank line to finish.");
    loop {
        let line = ui::ask("cover>");
        let line = line.trim().to_string();
        if line.is_empty() {
            break;
        }
        let mut it = line.splitn(2, char::is_whitespace);
        let num = it.next().unwrap_or("");
        let url = it.next().unwrap_or("").trim();
        let Ok(n) = num.parse::<usize>() else {
            ui::warn("format: <number> <url>");
            continue;
        };
        if n == 0 || n > albums.len() {
            ui::warn("number out of range");
            continue;
        }
        if url.is_empty() {
            ui::warn("need a URL after the number");
            continue;
        }
        let album = &albums[n - 1];
        match covers::embed_from_url(&album.tracks, url, min_dim) {
            Ok(c) => ui::ok(&format!("embedded into {c} track(s) of \"{}\"", album.display)),
            Err(e) => ui::err(&e.to_string()),
        }
    }
    Ok(())
}

/// Non-interactive counterpart to `run_paste`: read `<n><TAB>url` lines (numbers
/// from the straggler list / `--json`, tab- or space-separated; `#` comments and
/// blank lines ignored) from a file or `-` for stdin, and embed each across its
/// album. Makes the cover funnel fully scriptable for an agent.
fn run_paste_file(output: &std::path::Path, min_dim: u32, src: &std::path::Path) -> Result<()> {
    let albums = covers::coverless_albums(output);
    if albums.is_empty() {
        ui::ok("no coverless albums remain — nothing to paste");
        return Ok(());
    }
    let content = if src == std::path::Path::new("-") {
        let mut s = String::new();
        std::io::Read::read_to_string(&mut std::io::stdin(), &mut s)?;
        s
    } else {
        std::fs::read_to_string(src).with_context(|| format!("read {}", src.display()))?
    };
    let mut applied = 0usize;
    for (i, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut it = line.splitn(2, |c: char| c == '\t' || c.is_whitespace());
        let num = it.next().unwrap_or("");
        let url = it.next().unwrap_or("").trim();
        let Ok(n) = num.parse::<usize>() else {
            ui::warn(&format!("line {}: expected `<n><TAB>url`", i + 1));
            continue;
        };
        if n == 0 || n > albums.len() {
            ui::warn(&format!("line {}: number {n} out of range (1..={})", i + 1, albums.len()));
            continue;
        }
        if url.is_empty() {
            ui::warn(&format!("line {}: missing url", i + 1));
            continue;
        }
        let album = &albums[n - 1];
        match covers::embed_from_url(&album.tracks, url, min_dim) {
            Ok(c) => {
                applied += c;
                ui::ok(&format!("#{n} → embedded into {c} track(s) of \"{}\"", album.display));
            }
            Err(e) => ui::err(&format!("#{n}: {e}")),
        }
    }
    ui::info(&format!("done — embedded {applied} track(s) across the pasted albums"));
    Ok(())
}

/// Restore default SIGPIPE so piping output into `head`/`less` exits quietly.
fn reset_sigpipe() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

fn main() -> Result<()> {
    reset_sigpipe();
    // `--llm` is a documentation flag like `--help`: it works from anywhere and
    // needs no subcommand, so intercept it before clap enforces one.
    if std::env::args().skip(1).any(|a| a == "--llm") {
        print!("{}", llm_guide());
        return Ok(());
    }
    // If we panic mid-progress-bar, put the terminal's echo back before the
    // default hook prints — otherwise the shell is left with echo off.
    let prev_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        ui::restore_term();
        prev_hook(info);
    }));
    let _ = ctrlc::set_handler(|| {
        eprintln!("\nCancelled.");
        ui::restore_term(); // don't leave the terminal with echo suppressed
        // Commit the journal first so a mid-run mutating command's partial writes
        // stay undoable, then exit 130 (SIGINT) — cancellation is not success, and
        // a script must be able to tell an interrupted run from a completed one.
        let _ = journal::commit();
        std::process::exit(130);
    });
    let cli = Cli::parse();
    ui::set_verbosity(if cli.quiet { 0 } else if cli.verbose { 2 } else { 1 });
    let json = cli.json;
    // Journal mutating runs so `amdl undo` can revert them (unless --no-undo).
    if is_mutating(&cli.cmd) && !cli.no_undo {
        journal::begin(std::env::args().collect());
    }
    let outcome = dispatch(cli.cmd, json);
    let _ = journal::commit();
    ui::restore_term(); // belt-and-suspenders: never hand the shell back with echo off
    outcome
}

/// Whether a command writes to the library (so it should be journaled for undo).
fn is_mutating(cmd: &Cmd) -> bool {
    matches!(
        cmd,
        Cmd::Download { .. } | Cmd::Convert { .. } | Cmd::Covers { .. } | Cmd::Lyrics { .. } | Cmd::Tag { .. } | Cmd::Identify { .. } | Cmd::Recover { .. }
    )
}

fn dispatch(cmd: Cmd, json: bool) -> Result<()> {
    match cmd {
        Cmd::Download { urls, out, cookies, work_dir, keep_work, bitrate, jobs, storefront, fallback, no_convert } => {
            let cfg = config::load();
            let out = out.or(cfg.paths.output).unwrap_or_else(cwd);
            let bitrate = bitrate.or(cfg.convert.bitrate).unwrap_or_else(|| "192k".into());
            let mut storefronts = vec![storefront];
            storefronts.extend(fallback.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from));
            cmd_download(
                urls,
                out,
                cookies,
                work_dir.unwrap_or_else(temp_work),
                keep_work,
                &bitrate,
                jobs.unwrap_or_else(num_cpus::get),
                storefronts,
                no_convert,
            )
        }
        Cmd::Convert { src, dest, bitrate, jobs } => {
            let cfg = config::load();
            let src = src
                .or(cfg.paths.source)
                .context("no source dir — pass one or set [paths] source in ~/.config/amdl/config.toml")?;
            let dest = dest.or(cfg.paths.output).unwrap_or_else(cwd);
            let bitrate = bitrate.or(cfg.convert.bitrate).unwrap_or_else(|| "192k".into());
            let r = convert::convert_dir(&src, &dest, &bitrate, jobs.unwrap_or_else(num_cpus::get))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                use ui::Tone::{Bad, Dim, Good};
                let done = r.converted + r.copied;
                let mut hints = Vec::new();
                if r.failed > 0 {
                    hints.push("some files failed — `amdl doctor --source … --deep` to see which".to_string());
                }
                ui::result(
                    &format!("convert · {} of {} → Opus", done, done + r.skipped + r.failed),
                    false,
                    &[
                        ui::tally("converted", r.converted, Good),
                        ui::tally("copied", r.copied, Good),
                        ui::tally("with-cover", r.with_cover, Good),
                        ui::tally("lrc", r.lrc_copied, Good),
                        ui::tally("skipped", r.skipped, Dim),
                        ui::tally("failed", r.failed, Bad),
                    ],
                    &hints,
                );
            }
            Ok(())
        }
        Cmd::Doctor { output, source, deep } => {
            let cfg = config::load();
            let output = output
                .or(cfg.paths.output)
                .context("no output dir — pass one or set [paths] output in ~/.config/amdl/config.toml")?;
            let source = source.or(cfg.paths.source);
            let h = doctor::scan(&output, source.as_deref(), deep)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&h)?);
            } else {
                print_health(&h);
            }
            Ok(())
        }
        Cmd::Stats { library } => {
            let cfg = config::load();
            let library = library
                .or(cfg.paths.output)
                .context("no library — pass a path or set [paths] output in ~/.config/amdl/config.toml")?;
            let s = stats::collect(&library);
            if json {
                println!("{}", serde_json::to_string_pretty(&s)?);
            } else {
                print_stats(&s);
            }
            Ok(())
        }
        Cmd::Covers { output, source, reference, online, paste, paste_file, dry_run, min_dim } => {
            let cfg = config::load();
            let output = output
                .or(cfg.paths.output.clone())
                .context("no output dir — pass one or set [paths] output in ~/.config/amdl/config.toml")?;
            let source = source.or(cfg.paths.source.clone());
            let opts = covers::Opts {
                source,
                references: reference,
                online,
                discogs: cfg.keys.discogs.clone(),
                dry_run,
                min_dim,
            };
            let r = covers::backfill(&output, &opts);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print_covers(&r);
            }
            if !dry_run {
                if let Some(f) = paste_file {
                    run_paste_file(&output, min_dim, &f)?;
                } else if paste {
                    run_paste(&output, min_dim)?;
                }
            }
            Ok(())
        }
        Cmd::Lyrics { output, jobs, no_upgrade, upgrade_synced: _, embed, force_embed, no_align, mark_instrumental, unmark_instrumental } => {
            let cfg = config::load();
            let output = output
                .or(cfg.paths.output.clone())
                .context("no output dir — pass one or set [paths] output in ~/.config/amdl/config.toml")?;
            // Manual instrumental (un)marking is a self-contained repair: no fetch.
            if mark_instrumental || unmark_instrumental {
                let n = lyrics::mark_instrumental(&output, unmark_instrumental);
                let verb = if unmark_instrumental { "un-marked" } else { "marked instrumental (lyrics stripped)" };
                if json {
                    println!("{}", serde_json::json!({ "action": if unmark_instrumental {"unmark"} else {"mark"}, "files": n }));
                } else {
                    ui::ok(&format!("{verb}: {n} file(s)"));
                }
                return Ok(());
            }
            // Alignment runs automatically once [lyrics] aligner_url is configured;
            // --no-align opts out. With no aligner configured it simply doesn't run.
            let aligner_url = cfg.lyrics.aligner_url.clone();
            let want_align = !no_align && aligner_url.is_some();
            // Nudge when no aligner is set — plain lyrics could be timed by an
            // alignment server. Silenceable via config; hidden in --json.
            if aligner_url.is_none() && cfg.lyrics.hints && !json {
                ui::info("tip: an alignment server can generate synced lyrics for tracks no source has timed — set [lyrics] aligner_url. https://github.com/jakobhviid/amdl-aligner");
                ui::info("     turn off these hints: amdl configure set lyrics hints off");
            }
            let opts = lyrics::Options {
                upgrade_synced: !no_upgrade, // upgrade is the default; --no-upgrade opts out
                embed: embed || force_embed, // --force-embed implies --embed
                force_embed,
                align: want_align,
            };
            // Optional LrcApi fallback from config (both url + key required).
            let fallback = match (&cfg.lyrics.lrcapi_url, &cfg.lyrics.lrcapi_key) {
                (Some(url), Some(key)) => Some(lyrics::Fallback {
                    api: lyrics::LrcApi { url: url.clone(), key: key.clone() },
                    first: cfg.lyrics.lrcapi_first,
                }),
                (Some(_), None) => {
                    ui::warn("config [lyrics] lrcapi_url is set but lrcapi_key is missing — skipping the fallback server");
                    None
                }
                _ => None,
            };
            let r = lyrics::backfill(&output, jobs, opts, fallback.as_ref(), aligner_url.as_deref());
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                use ui::Tone::{Dim, Good};
                ui::result(
                    &format!("lyrics · {} written", r.ok_synced + r.ok_plain + r.upgraded),
                    false,
                    &[
                        ui::tally("synced", r.ok_synced, Good),
                        ui::tally("plain", r.ok_plain, Good),
                        ui::tally("upgraded", r.upgraded, Good),
                        ui::tally("aligned", r.aligned, Good),
                        ui::tally("embedded", r.embedded, Good),
                        ui::tally("not-found", r.not_found, Dim),
                        ui::tally("instrumental", r.instrumental, Dim),
                        ui::tally("no-meta", r.no_meta, Dim),
                        ui::tally("skipped", r.skipped, Dim),
                    ],
                    &[],
                );
            }
            Ok(())
        }
        Cmd::Identify { path, apply, dry_run, min_score, skip_tagged } => {
            let cfg = config::load();
            let key = cfg
                .keys
                .acoustid
                .or_else(|| std::env::var("ACOUSTID_KEY").ok())
                .context("no AcoustID application key — set [keys] acoustid in config, or $ACOUSTID_KEY (create one at https://acoustid.org/new-application; it must be the APPLICATION key, not your account key)")?;
            let iopts = identify::Opts { apply, dry_run, min_score, skip_tagged };
            let r = identify::run(&path, &key, &iopts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print_identify(&r);
            }
            Ok(())
        }
        Cmd::Tag { path, compilation, album, album_artist, artist, dry_run } => {
            let edit = retag::Edit { compilation, album, album_artist, artist };
            if edit.is_noop() {
                ui::warn("nothing to set — pass --compilation and/or --album/--artist/--album-artist");
                Ok(())
            } else {
                let r = retag::run(&path, &edit, dry_run);
                if json {
                    println!("{}", serde_json::to_string_pretty(&r)?);
                } else {
                    ui::ok(&format!(
                        "tagged {} of {} (failed {}){}",
                        r.changed, r.total, r.failed, if r.dry_run { " [dry-run]" } else { "" }
                    ));
                }
                Ok(())
            }
        }
        Cmd::Config { init } => {
            let p = config::path();
            if init {
                if p.exists() {
                    ui::warn(&format!("config already exists: {}", p.display()));
                } else {
                    if let Some(parent) = p.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    std::fs::write(&p, config::template())?;
                    ui::ok(&format!("wrote {}", p.display()));
                }
            } else {
                let cfg = config::load();
                let show = |x: Option<PathBuf>| x.map(|p| p.display().to_string()).unwrap_or_else(|| "(unset)".into());
                if json {
                    println!("{}", serde_json::json!({
                        "path": p.display().to_string(),
                        "exists": p.exists(),
                        "source": cfg.paths.source,
                        "output": cfg.paths.output,
                    }));
                } else {
                    ui::info(&format!("config: {}{}", p.display(), if p.exists() { "" } else { " (not created)" }));
                    println!("  source: {}", show(cfg.paths.source));
                    println!("  output: {}", show(cfg.paths.output));
                }
            }
            Ok(())
        }
        Cmd::Configure { action } => {
            let as_err = |e: String| anyhow::anyhow!(e);
            match action {
                ConfigAction::Set { args } => {
                    // Last token is the value; everything before it is the key
                    // (words or a single dotted token) — so both grammars work.
                    let value = args.last().cloned().unwrap_or_default();
                    let key = join_key(&args[..args.len() - 1]);
                    let mut cfg = config::load_strict().map_err(as_err)?;
                    config::set_value(&mut cfg, &key, &value).map_err(as_err)?;
                    config::save(&cfg)?;
                    ui::ok(&format!("{key} = {value}"));
                    ui::info(&format!("wrote {}", config::path().display()));
                }
                ConfigAction::Unset { key } => {
                    let key = join_key(&key);
                    let mut cfg = config::load_strict().map_err(as_err)?;
                    config::unset_value(&mut cfg, &key).map_err(as_err)?;
                    config::save(&cfg)?;
                    ui::ok(&format!("unset {key}"));
                }
                ConfigAction::Get { key } => {
                    let key = join_key(&key);
                    let cfg = config::load_strict().map_err(as_err)?;
                    let val = config::get_value(&cfg, &key).map_err(as_err)?;
                    if json {
                        println!("{}", serde_json::json!({ "key": key, "value": val }));
                    } else if let Some(v) = val {
                        // Bare value, no decoration, so `$(amdl configure get …)` composes.
                        println!("{v}");
                    }
                }
                ConfigAction::List => {
                    let cfg = config::load_strict().map_err(as_err)?;
                    if json {
                        let mut map = serde_json::Map::new();
                        for (key, _) in config::KEYS {
                            let v = config::get_value(&cfg, key).map_err(as_err)?;
                            map.insert((*key).to_string(), v.map(serde_json::Value::from).unwrap_or(serde_json::Value::Null));
                        }
                        println!("{}", serde_json::to_string_pretty(&serde_json::Value::Object(map))?);
                    } else {
                        ui::info(&format!("config: {}", config::path().display()));
                        for (key, _) in config::KEYS {
                            let v = config::get_value(&cfg, key).map_err(as_err)?;
                            println!("  {key} = {}", v.unwrap_or_else(|| "(unset)".into()));
                        }
                    }
                }
                ConfigAction::Keys => {
                    if json {
                        let arr: Vec<_> = config::KEYS.iter().map(|(k, d)| serde_json::json!({ "key": k, "description": d })).collect();
                        println!("{}", serde_json::to_string_pretty(&arr)?);
                    } else {
                        for (key, desc) in config::KEYS {
                            println!("  {key:<22} {desc}");
                        }
                    }
                }
            }
            Ok(())
        }
        Cmd::Recover { output, source, reference, online, cookies, bitrate, dry_run } => {
            let cfg = config::load();
            let output = output
                .or(cfg.paths.output.clone())
                .context("no output dir — pass one or set [paths] output")?;
            let source = source
                .or(cfg.paths.source.clone())
                .context("no source dir — pass --source or set [paths] source")?;
            let bitrate = bitrate.or(cfg.convert.bitrate).unwrap_or_else(|| "192k".into());
            let opts = recover::Opts {
                source,
                references: reference,
                online,
                cookies,
                storefronts: vec!["dk".into(), "us".into(), "gb".into()],
                bitrate,
                work_dir: temp_work(),
                jobs: num_cpus::get(),
                dry_run,
            };
            let r = recover::run(&output, &opts)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print_recover(&r);
            }
            Ok(())
        }
        Cmd::Dedup { output, print_rm } => {
            let cfg = config::load();
            let output = output
                .or(cfg.paths.output.clone())
                .context("no output dir — pass one or set [paths] output")?;
            let r = dedup::run(&output);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else if print_rm {
                for path in r.removals() {
                    println!("rm -- {}", shell_quote(path));
                }
            } else {
                print_dedup(&r);
            }
            Ok(())
        }
        Cmd::Undo { run, list, dry_run } => {
            if list {
                let runs = journal::list();
                if json {
                    let items: Vec<_> = runs.iter().map(|r| serde_json::json!({
                        "id": r.id, "command": r.command, "summary": r.summary,
                        "changes": r.changes, "started_unix": r.started_unix,
                    })).collect();
                    println!("{}", serde_json::to_string_pretty(&items)?);
                } else if runs.is_empty() {
                    ui::info("no undoable runs recorded");
                } else {
                    ui::info(&format!("{} undoable run(s), newest first:", runs.len()));
                    for r in &runs {
                        println!(
                            "  {:<11}  {:<40}  {} file(s)   [{}]",
                            rel_time(r.started_unix), r.summary, r.changes, r.id
                        );
                    }
                }
                return Ok(());
            }
            // Humans on a terminal get an interactive picker for a bare `undo`;
            // an explicit id, --dry-run, --json, or a pipe keeps the direct path.
            if run.is_none() && !dry_run && !json && ui::stdin_tty() {
                return undo_interactive();
            }
            let rep = journal::undo(run.as_deref(), dry_run)?;
            if json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "run": rep.run, "command": rep.command,
                        "reverted": rep.reverted, "skipped": rep.skipped, "dry_run": rep.dry_run,
                    }))?
                );
            } else {
                print_undo(&rep);
            }
            Ok(())
        }
        Cmd::Cookies => cookies::diagnose(json),
        Cmd::Completions { shell } => {
            clap_complete::generate(shell, &mut Cli::command(), "amdl", &mut std::io::stdout());
            Ok(())
        }
        Cmd::Man => {
            clap_mangen::Man::new(Cli::command()).render(&mut std::io::stdout())?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn configure_help_lists_every_settable_key() {
        for (key, _) in config::KEYS {
            assert!(CONFIGURE_HELP.contains(key), "configure --help is missing `{key}`");
        }
    }

    #[test]
    fn join_key_accepts_words_and_dotted() {
        assert_eq!(join_key(&["lyrics".into(), "hints".into()]), "lyrics.hints");
        assert_eq!(join_key(&["lyrics.hints".into()]), "lyrics.hints");
        assert_eq!(join_key(&["paths".into(), "output".into()]), "paths.output");
    }

    #[test]
    fn cli_parses_both_configure_grammars() {
        use clap::Parser;
        // words: `configure set lyrics hints off`
        let words = Cli::try_parse_from(["amdl", "configure", "set", "lyrics", "hints", "off"]).unwrap();
        // dotted: `configure set lyrics.hints off`
        let dotted = Cli::try_parse_from(["amdl", "configure", "set", "lyrics.hints", "off"]).unwrap();
        for cli in [words, dotted] {
            let Cmd::Configure { action: ConfigAction::Set { args } } = cli.cmd else { panic!("expected configure set") };
            let value = args.last().unwrap().clone();
            let key = join_key(&args[..args.len() - 1]);
            assert_eq!((key.as_str(), value.as_str()), ("lyrics.hints", "off"));
        }
    }
}
