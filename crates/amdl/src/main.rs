//! amdl — Apple Music → validated → Opus, into your library. A CLI around
//! gamdl (download/decrypt) + ffmpeg (validate/convert), plus library-maintenance
//! commands. The logic lives in `amdl-core`; this is the thin CLI layer.
use amdl_core::{config, convert, cookies, covers, dedup, doctor, download, identify, lyrics, recover, retag, ui, validate};
use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "amdl", version, about = "Music-library harness: validate, transcode to Opus, and keep your library consistent (wraps gamdl + ffmpeg).", arg_required_else_help = true)]
struct Cli {
    /// Emit machine-readable JSON instead of the human summary (composable).
    #[arg(long, global = true)]
    json: bool,
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
        /// Library to backfill (default: config [paths] output).
        output: Option<PathBuf>,
        /// Parallel lookups (LRCLIB tolerates ~10).
        #[arg(short, long, default_value_t = 8)]
        jobs: usize,
    },
    /// Identify tracks by sound (AcoustID fingerprint) to fix untagged/mis-tagged
    /// files. Needs [keys] acoustid (or $ACOUSTID_KEY). --apply writes the match.
    Identify {
        /// File or directory to identify.
        path: PathBuf,
        /// Write the resolved artist/title/album (default: report only).
        #[arg(long)]
        apply: bool,
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
        #[arg(long)]
        init: bool,
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
    /// Check login cookies: report gamdl's file and auto-detect from your browser.
    Cookies,
    /// Print a shell completion script (bash|zsh|fish|…) to stdout.
    #[command(hide = true)]
    Completions { shell: clap_complete::Shell },
    /// Print a man page (roff) to stdout.
    #[command(hide = true)]
    Man,
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
            for t in &tracks {
                let rel = t.strip_prefix(&work_dir).unwrap_or(t);
                let dst = out.join(rel);
                if let Some(p) = dst.parent() {
                    std::fs::create_dir_all(p).ok();
                }
                std::fs::rename(t, &dst).or_else(|_| std::fs::copy(t, &dst).map(|_| ())).ok();
            }
            ui::ok(&format!("kept {} .m4a in {}", tracks.len(), out.display()));
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

fn print_health(h: &doctor::Health) {
    ui::info(&format!("scanned {} opus", h.total));
    let cat = |label: &str, v: &[String]| {
        if !v.is_empty() {
            ui::warn(&format!("{}: {}", label, v.len()));
            for x in v.iter().take(10) {
                println!("    {x}");
            }
            if v.len() > 10 {
                println!("    … and {} more", v.len() - 10);
            }
        }
    };
    cat("unreadable", &h.unreadable);
    cat("missing cover", &h.missing_cover);
    cat("missing tags", &h.missing_tags);
    cat("truncated", &h.truncated);
    cat("source without opus", &h.source_without_opus);
    if h.is_clean() {
        ui::ok("clean — no issues");
    }
}

fn print_covers(r: &covers::Report) {
    let tag = if r.dry_run { " (dry-run)" } else { "" };
    ui::info(&format!("coverless albums: {}{}", r.coverless_albums, tag));
    if r.albums_filled > 0 {
        ui::ok(&format!(
            "filled {} album(s) — {} from source, {} from reference, {} online",
            r.albums_filled, r.filled_from_source, r.filled_from_reference, r.filled_online
        ));
    }
    if !r.stragglers.is_empty() {
        ui::warn(&format!("{} album(s) still need a cover (paste a URL per number, once `covers` gains that pass):", r.stragglers.len()));
        for s in &r.stragglers {
            println!("  {:>3}. {} — {} ({} tracks)", s.n, s.album, s.artist, s.tracks);
        }
    } else if r.coverless_albums > 0 {
        ui::ok("all coverless albums resolved");
    }
}

fn print_recover(r: &recover::Report) {
    ui::info(&format!(
        "broken {} · recovered-from-sibling {} · re-acquired {} · regrouped {}{}",
        r.broken, r.recovered_from_sibling, r.reacquired, r.regrouped,
        if r.dry_run { " [dry-run]" } else { "" }
    ));
    if !r.still_broken.is_empty() {
        ui::warn(&format!("{} still broken (no sibling; run with --online to re-acquire):", r.still_broken.len()));
        for s in r.still_broken.iter().take(20) {
            println!("    {s}");
        }
        if r.still_broken.len() > 20 {
            println!("    … and {} more", r.still_broken.len() - 20);
        }
    } else if r.broken > 0 {
        ui::ok("all recovered");
    }
}

fn print_dedup(r: &dedup::Report) {
    ui::info(&format!(
        "scanned {} · exact-duplicate groups {} · subset editions {}",
        r.total, r.exact_duplicates.len(), r.subset_editions.len()
    ));
    if r.is_clean() {
        ui::ok("no duplicates or orphan editions found");
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
    ui::info("nothing was deleted — review, then remove yourself (or `amdl dedup --print-rm`).");
}

/// Single-quote a path for a POSIX shell (for `--print-rm` output).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

fn print_identify(r: &identify::Report) {
    ui::info(&format!(
        "identified {} of {} · applied {} · no-match {} · failed {}",
        r.matched, r.total, r.applied, r.no_match, r.failed
    ));
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

/// Restore default SIGPIPE so piping output into `head`/`less` exits quietly.
fn reset_sigpipe() {
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

fn main() -> Result<()> {
    reset_sigpipe();
    let _ = ctrlc::set_handler(|| {
        eprintln!("\nCancelled.");
        std::process::exit(0);
    });
    let cli = Cli::parse();
    let json = cli.json;
    match cli.cmd {
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
                ui::ok(&format!(
                    "converted {} · copied {} · skipped {} · failed {} · {} with cover · {} lrc",
                    r.converted, r.copied, r.skipped, r.failed, r.with_cover, r.lrc_copied
                ));
            }
            Ok(())
        }
        Cmd::Doctor { output, source } => {
            let cfg = config::load();
            let output = output
                .or(cfg.paths.output)
                .context("no output dir — pass one or set [paths] output in ~/.config/amdl/config.toml")?;
            let source = source.or(cfg.paths.source);
            let h = doctor::scan(&output, source.as_deref());
            if json {
                println!("{}", serde_json::to_string_pretty(&h)?);
            } else {
                print_health(&h);
            }
            Ok(())
        }
        Cmd::Covers { output, source, reference, online, paste, dry_run, min_dim } => {
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
            if paste && !dry_run {
                run_paste(&output, min_dim)?;
            }
            Ok(())
        }
        Cmd::Lyrics { output, jobs } => {
            let cfg = config::load();
            let output = output
                .or(cfg.paths.output.clone())
                .context("no output dir — pass one or set [paths] output in ~/.config/amdl/config.toml")?;
            let r = lyrics::backfill(&output, jobs);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                ui::ok(&format!(
                    "synced {} · plain {} · not-found {} · instrumental {} · no-meta {} · skipped {}",
                    r.ok_synced, r.ok_plain, r.not_found, r.instrumental, r.no_meta, r.skipped
                ));
            }
            Ok(())
        }
        Cmd::Identify { path, apply } => {
            let cfg = config::load();
            let key = cfg
                .keys
                .acoustid
                .or_else(|| std::env::var("ACOUSTID_KEY").ok())
                .context("no AcoustID application key — set [keys] acoustid in config, or $ACOUSTID_KEY (create one at https://acoustid.org/new-application; it must be the APPLICATION key, not your account key)")?;
            let r = identify::run(&path, &key, apply)?;
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
                    std::fs::write(&p, config::EXAMPLE)?;
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
        Cmd::Cookies => cookies::diagnose(),
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
