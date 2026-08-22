//! Allocation + timing probe for the shared tool-response pipeline
//! (`apply_jq_filter` → `render` → `truncate_for_ai`).
//!
//! Not a pass/fail benchmark — it prints bytes allocated, allocation count, and
//! wall time per stage so regressions in *allocation* (not just speed) are
//! visible. Run with `cargo bench --bench response_pipeline`.
//!
//! Each stage is also measured against the shape it had before the pipeline was
//! de-allocated, so the win stays legible and a regression is obvious.
//!
//! `unsafe` is confined to the `GlobalAlloc` shim below: counting allocations
//! requires a global allocator, and there is no safe way to write one. The
//! crate-wide `unsafe_code = "deny"` still applies to all production code.
#![allow(unsafe_code, clippy::print_stdout, clippy::pedantic)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use mcp_server_atlassian::format::jmespath::apply_jq_filter;
use mcp_server_atlassian::format::truncation::truncate_for_ai;
use mcp_server_atlassian::format::{OutputFormat, render, to_pretty_json};
use serde_json::{Value, json};

static BYTES: AtomicU64 = AtomicU64::new(0);
static COUNT: AtomicU64 = AtomicU64::new(0);

struct Counting;

// SAFETY: every method forwards to `System` unchanged; the only added work is
// two relaxed atomic counters, which cannot affect allocator invariants.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        BYTES.fetch_add(l.size() as u64, Relaxed);
        COUNT.fetch_add(1, Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        if new > l.size() {
            BYTES.fetch_add((new - l.size()) as u64, Relaxed);
        }
        COUNT.fetch_add(1, Relaxed);
        unsafe { System.realloc(p, l, new) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

fn probe<T>(label: &str, iters: u32, mut f: impl FnMut() -> T) -> T {
    let (b0, c0) = (BYTES.load(Relaxed), COUNT.load(Relaxed));
    let t0 = std::time::Instant::now();
    let mut out = f();
    for _ in 1..iters {
        out = f();
    }
    let dt = t0.elapsed();
    let n = u64::from(iters);
    println!(
        "  {label:<40} {:>9} KB {:>10} allocs {:>8.2} ms",
        (BYTES.load(Relaxed) - b0) / 1024 / n,
        (COUNT.load(Relaxed) - c0) / n,
        dt.as_secs_f64() * 1000.0 / f64::from(iters)
    );
    out
}

/// A Jira-search-shaped response: the payload shape that actually flows through
/// the tool pipeline in production.
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

/// TOON exists to save the LLM tokens. That trade only makes sense if the
/// output is actually smaller than JSON for the shapes our vendors return, so
/// measure it rather than assume it: TOON wins big on uniform tabular arrays
/// and can *lose* on deeply nested objects.
fn output_size_comparison() {
    let flat: Vec<Value> = (0..200)
        .map(
            |i| json!({"id": i, "key": format!("PROJ-{i}"), "status": "open", "assignee": "alice"}),
        )
        .collect();
    let cases: Vec<(&str, Value)> = vec![
        (
            "uniform flat rows (TOON's best case)",
            json!({"issues": flat}),
        ),
        ("nested objects (Jira search shape)", payload(200)),
    ];

    println!(
        "  {:<38} {:>10} {:>10} {:>10}",
        "", "compact", "pretty", "TOON"
    );
    for (label, v) in &cases {
        let compact = serde_json::to_string(v).unwrap().len();
        let pretty = to_pretty_json(v).len();
        let toon = render(v, OutputFormat::Toon).len();
        println!(
            "  {label:<38} {:>9}B {:>9}B {:>9}B   TOON is {:.0}% of compact, {:.0}% of pretty",
            compact,
            pretty,
            toon,
            100.0 * toon as f64 / compact as f64,
            100.0 * toon as f64 / pretty as f64,
        );
    }
}

fn main() {
    println!("=== output size: is TOON earning its CPU? ===");
    output_size_comparison();

    for (n, iters) in [(500usize, 20u32), (5000, 4)] {
        let data = payload(n);
        let kb = serde_json::to_string(&data).unwrap().len() / 1024;
        println!("\n=== {n} issues / {kb} KB compact JSON  (mean of {iters}) ===");

        println!(" -- stage 1: jq filter, no filter supplied (the default path)");
        probe("BEFORE: deep clone of the payload", iters, || data.clone());
        let filtered = probe("NOW:    apply_jq_filter(None) borrows", iters, || {
            apply_jq_filter(&data, None)
        });

        println!(" -- stage 2: render");
        probe("BEFORE: eager pretty-JSON, then discard", iters, || {
            let fallback = to_pretty_json(&filtered);
            serde_toon::to_string(&filtered).unwrap_or(fallback)
        });
        let rendered = probe("NOW:    render(Toon), lazy fallback", iters, || {
            render(&filtered, OutputFormat::Toon)
        });
        probe("ref:    render(Json) on same data", iters, || {
            render(&filtered, OutputFormat::Json)
        });
        println!(" -- stage 3: truncate");
        probe("truncate_for_ai (over budget)", iters, || {
            truncate_for_ai(&rendered, None)
        });

        println!("   rendered output: {} KB", rendered.len() / 1024);
    }
}
