//! A tight loop over the TOON encoder, for attaching a sampling profiler
//! to. `cargo bench --bench toon_encode_loop` builds it under `profile.bench`,
//! which keeps debug symbols so stacks resolve.
//!
//! The TOON encoder dominates the tool-response pipeline, so it gets its own
//! isolated target rather than being buried under the rest of the pipeline.
//!
//!   # text call tree, no sudo, no extra tooling:
//!   ./target/release/deps/toon_encode_loop-<hash> 14 &
//!   /usr/bin/sample $! 10 1 -f /tmp/toon.sample.txt
//!
//!   # interactive flamegraph:
//!   samply record ./target/release/deps/toon_encode_loop-<hash> 10
//!
//! Takes an optional duration in seconds (default 12).
#![allow(clippy::print_stdout, clippy::pedantic)]

use serde_json::{Value, json};

fn payload(n: usize) -> Value {
    let items: Vec<Value> = (0..n)
        .map(|i| {
            json!({
                "id": i,
                "key": format!("PROJ-{i}"),
                "self": format!("https://example.atlassian.net/rest/api/3/issue/{i}"),
                "fields": {
                    "summary": format!("Issue number {i} with a reasonably long summary line"),
                    "status": {"name": "In Progress", "id": "3", "category": "indeterminate"},
                    "assignee": {"accountId": format!("acct-{i}"), "displayName": "Alice Example"},
                    "labels": ["backend", "perf", "triage"],
                    "description": "Lorem ipsum dolor sit amet, consectetur adipiscing elit, sed do eiusmod tempor incididunt ut labore.",
                }
            })
        })
        .collect();
    json!({"startAt": 0, "maxResults": n, "total": n, "issues": items})
}

fn main() {
    let data = payload(5000);
    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(12);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    let mut encodes = 0u64;
    let mut sink = 0usize;
    while std::time::Instant::now() < deadline {
        // `sink` keeps the encode from being optimised away.
        sink += serde_toon::to_string(&data).unwrap().len();
        encodes += 1;
    }
    println!("{encodes} encodes, sink={sink}");
}
