//! Logging + progress. Colors respect NO_COLOR / non-TTY (like pwtune); progress
//! bars are used for the slow stages (download, per-track convert/validate).
use indicatif::{ProgressBar, ProgressStyle};
use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::time::Duration;

fn no_color() -> bool { std::env::var_os("NO_COLOR").is_some() }
fn c_out() -> bool { !no_color() && io::stdout().is_terminal() }
fn c_err() -> bool { !no_color() && io::stderr().is_terminal() }

pub fn info(m: &str) { if c_out() { println!("\x1b[1;34m▸ {m}\x1b[0m") } else { println!("▸ {m}") } }
pub fn ok(m: &str) { if c_out() { println!("\x1b[1;32m✓ {m}\x1b[0m") } else { println!("✓ {m}") } }
pub fn warn(m: &str) { if c_out() { println!("\x1b[1;33m⚠ {m}\x1b[0m") } else { println!("⚠ {m}") } }
pub fn err(m: &str) { if c_err() { eprintln!("\x1b[1;31m✗ {m}\x1b[0m") } else { eprintln!("✗ {m}") } }

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

/// Read all of stdin to EOF (for `--cookies -` piping on a server).
pub fn read_all_stdin() -> String {
    let mut buf = String::new();
    let _ = io::stdin().read_to_string(&mut buf);
    buf
}

/// An indeterminate spinner for opaque long steps (gamdl download).
pub fn spinner(msg: &str) -> ProgressBar {
    let pb = ProgressBar::new_spinner();
    pb.set_style(ProgressStyle::with_template("{spinner:.blue} {msg} ({elapsed})").unwrap());
    pb.set_message(msg.to_string());
    pb.enable_steady_tick(Duration::from_millis(120));
    pb
}

/// A determinate bar for N-item work (per-track convert / validate).
pub fn bar(len: u64, msg: &str) -> ProgressBar {
    let pb = ProgressBar::new(len);
    pb.set_style(
        ProgressStyle::with_template("{msg} [{bar:30.cyan/blue}] {pos}/{len} ({elapsed})")
            .unwrap()
            .progress_chars("=>-"),
    );
    pb.set_message(msg.to_string());
    pb
}
