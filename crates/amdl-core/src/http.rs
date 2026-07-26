//! Shared outbound HTTP agent.
//!
//! `ureq` applies **no** timeouts by default, so a hung or half-dead host (a
//! stalled CDN, a lyrics/metadata server that accepts the connection but never
//! answers) would otherwise block forever — and since `covers` fetches serially
//! and `lyrics`/`identify` run pooled network I/O, one bad host can freeze a
//! whole-library run. Route every request through [`agent`] so connect and read
//! stalls are bounded. The agent is process-wide (cheap `Arc` clone) and pools
//! connections. The alignment client keeps its own agent — its responses are
//! legitimately slow (seconds on GPU, minutes on CPU).
use std::sync::OnceLock;
use std::time::Duration;

/// Process-wide HTTP agent with connect/read/write timeouts. Clone is cheap.
pub fn agent() -> ureq::Agent {
    static AGENT: OnceLock<ureq::Agent> = OnceLock::new();
    AGENT
        .get_or_init(|| {
            ureq::AgentBuilder::new()
                .timeout_connect(Duration::from_secs(10))
                .timeout_read(Duration::from_secs(60))
                .timeout_write(Duration::from_secs(30))
                .build()
        })
        .clone()
}
