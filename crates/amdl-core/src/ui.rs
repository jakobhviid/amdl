//! Logging, progress, and result rendering. Colors respect NO_COLOR / non-TTY;
//! human output goes to **stdout** (so `--json` stays pipe-clean) while progress
//! bars and errors go to **stderr**. Result summaries are severity-colored (a
//! failure never hides inside a green line) and can carry next-step hints.
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use std::fmt::Display;
use std::io::{self, BufRead, IsTerminal, Write};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

fn no_color() -> bool { std::env::var_os("NO_COLOR").is_some() }
fn c_out() -> bool { !no_color() && io::stdout().is_terminal() }
fn c_err() -> bool { !no_color() && io::stderr().is_terminal() }

// ── verbosity ───────────────────────────────────────────────────────────────
// 0 = quiet (headline + errors only), 1 = normal, 2 = verbose (per-item detail).
static VERBOSITY: AtomicU8 = AtomicU8::new(1);
pub fn set_verbosity(level: u8) { VERBOSITY.store(level, Ordering::Relaxed); }
fn verbosity() -> u8 { VERBOSITY.load(Ordering::Relaxed) }
pub fn is_verbose() -> bool { verbosity() >= 2 }
pub fn is_quiet() -> bool { verbosity() == 0 }

// ── shared progress surface ───────────────────────────────────────────────────
// One MultiProgress on stderr: bars stack cleanly and `mp().println` prints a log
// line *above* live bars instead of tearing them. stdout stays untouched.
fn mp() -> &'static MultiProgress {
    static M: OnceLock<MultiProgress> = OnceLock::new();
    M.get_or_init(MultiProgress::new)
}

// ── plain logging (stdout for info/ok/warn; stderr for err) ───────────────────
pub fn info(m: &str) { if verbosity() >= 1 { line_out(if c_out() { format!("\x1b[1;34m▸ {m}\x1b[0m") } else { format!("▸ {m}") }) } }
pub fn ok(m: &str) { if verbosity() >= 1 { line_out(if c_out() { format!("\x1b[1;32m✓ {m}\x1b[0m") } else { format!("✓ {m}") }) } }
pub fn warn(m: &str) { if verbosity() >= 1 { line_out(if c_out() { format!("\x1b[1;33m⚠ {m}\x1b[0m") } else { format!("⚠ {m}") }) } }
pub fn err(m: &str) { let s = if c_err() { format!("\x1b[1;31m✗ {m}\x1b[0m") } else { format!("✗ {m}") }; let _ = mp().println(s); }

/// Per-item detail, shown only under `-v/--verbose`. Routed through the progress
/// surface so it prints cleanly above any live bar.
pub fn detail(m: &str) { if verbosity() >= 2 { let _ = mp().println(format!("    {m}")); } }

fn line_out(s: String) { println!("{s}"); }

// ── result summaries ──────────────────────────────────────────────────────────
/// Severity of a metric — drives its color and whether the headline flags a problem.
#[derive(Clone, Copy)]
pub enum Tone { Good, Dim, Warn, Bad }
impl Tone {
    fn code(self) -> &'static str {
        match self { Tone::Good => "32", Tone::Dim => "90", Tone::Warn => "33", Tone::Bad => "31" }
    }
}

pub struct Metric { label: String, value: String, tone: Tone }

/// A metric with an explicit tone.
pub fn metric(label: &str, value: impl Display, tone: Tone) -> Metric {
    Metric { label: label.into(), value: value.to_string(), tone }
}
/// A count metric: `0` renders dim (nothing to see), non-zero takes `active`.
/// Use `Tone::Bad`/`Warn` for problem counts so any non-zero pops.
pub fn tally(label: &str, n: usize, active: Tone) -> Metric {
    Metric { label: label.into(), value: n.to_string(), tone: if n == 0 { Tone::Dim } else { active } }
}

/// Render a command's outcome: a bold headline (✓, or ⚠ if any problem metric is
/// non-zero), then a severity-colored metric breakdown, then next-step hints.
/// Headline always prints (even `--quiet`); breakdown + hints print at normal+.
pub fn result(headline: &str, dry_run: bool, metrics: &[Metric], hints: &[String]) {
    let problem = metrics.iter().any(|m| matches!(m.tone, Tone::Bad) && m.value != "0");
    let (sym, hcode) = if problem { ("⚠", "1;33") } else { ("✓", "1;32") };
    let dry = match (dry_run, c_out()) {
        (false, _) => String::new(),
        (true, true) => " \x1b[90m[dry-run]\x1b[0m".into(),
        (true, false) => " [dry-run]".into(),
    };
    if c_out() {
        println!("\x1b[{hcode}m{sym} {headline}\x1b[0m{dry}");
    } else {
        println!("{sym} {headline}{dry}");
    }
    if verbosity() >= 1 && !metrics.is_empty() {
        let parts: Vec<String> = metrics
            .iter()
            .map(|m| {
                if c_out() {
                    format!("{} \x1b[1;{}m{}\x1b[0m", m.label, m.tone.code(), m.value)
                } else {
                    format!("{} {}", m.label, m.value)
                }
            })
            .collect();
        println!("    {}", parts.join("  ·  "));
    }
    if verbosity() >= 1 {
        for h in hints {
            if c_out() { println!("    \x1b[36m→\x1b[0m {h}") } else { println!("    → {h}") }
        }
    }
}

// ── prompts / stdin ───────────────────────────────────────────────────────────
/// Free-text prompt (to stderr so it doesn't pollute piped stdout).
pub fn ask(prompt: &str) -> String {
    eprint!("{prompt} ");
    let _ = io::stderr().flush();
    let mut s = String::new();
    match io::stdin().lock().read_line(&mut s) {
        Ok(0) | Err(_) => String::new(),
        Ok(_) => s.trim().to_string(),
    }
}

/// True if stdin is an interactive terminal (so we can prompt for a paste).
pub fn stdin_tty() -> bool { io::stdin().is_terminal() }

/// Read a multi-line block pasted on stdin, ending on a blank line or EOF (Ctrl-D).
pub fn read_block() -> String {
    let stdin = io::stdin();
    let mut buf = String::new();
    for line in stdin.lock().lines() {
        match line {
            Ok(l) => {
                if l.trim().is_empty() && !buf.is_empty() { break; }
                buf.push_str(&l);
                buf.push('\n');
            }
            Err(_) => break,
        }
    }
    buf
}

// ── progress ──────────────────────────────────────────────────────────────────
/// An indeterminate spinner for opaque long steps (gamdl download).
pub fn spinner(msg: &str) -> ProgressBar {
    let pb = mp().add(ProgressBar::new_spinner());
    pb.set_style(ProgressStyle::with_template("{spinner:.blue} {msg} ({elapsed})").unwrap());
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(120));
    pb
}

/// A determinate bar for N-item work (per-track convert / validate), added to the
/// shared surface so logs/errors during the run print above it cleanly. Leads with
/// the count ("Converting 3/12"). Suppressed entirely under `--quiet`.
pub fn bar(len: u64, msg: &str) -> ProgressBar {
    if verbosity() == 0 {
        return ProgressBar::hidden();
    }
    let pb = mp().add(ProgressBar::new(len));
    pb.set_style(
        ProgressStyle::with_template("  {spinner:.cyan} {msg} {pos}/{len} [{bar:24.cyan/blue}] {elapsed}")
            .unwrap()
            .progress_chars("=>-")
            .tick_chars("⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏ "),
    );
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(90));
    pb
}
