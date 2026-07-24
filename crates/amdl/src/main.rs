//! amdl — Apple Music → validated → Opus, into your library. A CLI around
//! gamdl (download/decrypt) + ffmpeg (validate/convert), plus library-maintenance
//! commands. The logic lives in `amdl-core`; this is the thin CLI layer.
use amdl_core::{config, convert, cookies, covers, doctor, download, lyrics, retag, ui, validate};
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
        /// Opus bitrate.
        #[arg(long, default_value = "192k")]
        bitrate: String,
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
        #[arg(long, default_value = "192k")]
        bitrate: String,
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
    /// (planned) Re-acquire broken/missing files from Apple Music by their metadata.
    Recover { dir: PathBuf },
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
    if r.filled_from_source + r.filled_from_reference > 0 {
        ui::ok(&format!(
            "filled {} album(s) — {} tracks from source, {} from reference",
            r.albums_filled, r.filled_from_source, r.filled_from_reference
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
            let mut storefronts = vec![storefront];
            storefronts.extend(fallback.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from));
            cmd_download(
                urls,
                out.unwrap_or_else(cwd),
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
            let r = convert::convert_dir(&src, &dest, &bitrate, jobs.unwrap_or_else(num_cpus::get))?;
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                ui::ok(&format!(
                    "converted {} · skipped {} · failed {} · {} with cover · {} lrc",
                    r.converted, r.skipped, r.failed, r.with_cover, r.lrc_copied
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
        Cmd::Covers { output, source, reference, dry_run, min_dim } => {
            let cfg = config::load();
            let output = output
                .or(cfg.paths.output.clone())
                .context("no output dir — pass one or set [paths] output in ~/.config/amdl/config.toml")?;
            let source = source.or(cfg.paths.source.clone());
            let opts = covers::Opts { source, references: reference, dry_run, min_dim };
            let r = covers::backfill(&output, &opts);
            if json {
                println!("{}", serde_json::to_string_pretty(&r)?);
            } else {
                print_covers(&r);
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
        Cmd::Recover { dir } => {
            ui::warn(&format!("`recover` is planned — not yet implemented for {}", dir.display()));
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
