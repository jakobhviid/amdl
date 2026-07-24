//! amdl — Apple Music → validated → Opus, into your library. A true CLI around
//! gamdl (download/decrypt) + ffmpeg (validate/convert). No config file: paths
//! and knobs are flags, and the output defaults to the current folder.
mod convert;
mod cookies;
mod download;
mod ui;
mod validate;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use rayon::prelude::*;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "amdl", version, about = "Music-library harness: validate, transcode to Opus, and keep your library consistent (wraps gamdl + ffmpeg).", arg_required_else_help = true)]
struct Cli {
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
    /// Transcode an existing library of .m4a to Opus.
    Convert {
        /// Source library root.
        src: PathBuf,
        /// Destination (default: current directory).
        dest: Option<PathBuf>,
        #[arg(long, default_value = "192k")]
        bitrate: String,
        #[arg(short, long)]
        jobs: Option<usize>,
    },
    /// (planned) Re-acquire broken/missing files from Apple Music by their metadata.
    Recover {
        dir: PathBuf,
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

fn cmd_download(
    urls: Vec<String>,
    out: PathBuf,
    cookies: Option<PathBuf>,
    work_dir: PathBuf,
    keep_work: bool,
    bitrate: &str,
    jobs: usize,
    storefronts: Vec<String>,
    no_convert: bool,
) -> Result<()> {
    let cookies = cookies::resolve(cookies, false)?;
    std::fs::create_dir_all(&work_dir)?;
    std::fs::create_dir_all(&out)?;
    for url in &urls {
        ui::info(&format!("↓ {url}"));
        let opts = download::Opts {
            cookies: cookies.clone(),
            storefronts: storefronts.clone(),
            output: work_dir.clone(),
            artist_auto: url.contains("/artist/"),
        };
        let tracks = download::download(url, &opts)?;

        // Decode-check every downloaded track in parallel, with a progress bar.
        let bad: Vec<PathBuf> = {
            let pb = ui::bar(tracks.len() as u64, "Validating");
            let bad = tracks
                .par_iter()
                .filter_map(|t| {
                    let ok = validate::probe_ok(t);
                    pb.inc(1);
                    (!ok).then(|| t.clone())
                })
                .collect();
            pb.finish_and_clear();
            bad
        };
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
            let n = convert::convert_files(&tracks, &work_dir, &out, bitrate, jobs)?;
            ui::ok(&format!("{n} track(s) → Opus in {}", out.display()));
        }
    }
    if !keep_work {
        std::fs::remove_dir_all(&work_dir).ok();
    } else {
        ui::info(&format!("kept work dir: {}", work_dir.display()));
    }
    Ok(())
}

/// Restore default SIGPIPE so piping output into `head`/`less` exits quietly
/// instead of panicking on a broken pipe (Rust ignores SIGPIPE by default).
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
            let n = convert::convert_dir(&src, &dest.unwrap_or_else(cwd), &bitrate, jobs.unwrap_or_else(num_cpus::get))?;
            ui::ok(&format!("converted {n} track(s)"));
            Ok(())
        }
        Cmd::Recover { dir } => {
            ui::warn(&format!("`recover` is planned (phase 2) — not yet implemented for {}", dir.display()));
            ui::info("It will re-acquire broken/missing files from Apple Music by their metadata");
            ui::info("(needs the catalog-search + track-matching port).");
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

