//! Criterion benchmark for the diagnostic ingestion hot path.

use aifix::adapter::parse_diagnostics;
use aifix::digest::build_digest;
use aifix::model::Invocation;
use aifix::model::Protocol;
use criterion::Criterion;
use criterion::criterion_group;
use criterion::criterion_main;

/// Clippy compiler-message JSONL used by integration tests and benchmarks.
const CLIPPY_FIXTURE: &str = include_str!("../tests/fixtures/clippy.jsonl");

/// Creates stable invocation metadata for the ingest benchmark.
fn benchmark_invocation() -> Invocation
{
    debug_assert!(
        !CLIPPY_FIXTURE.is_empty(),
        "embedded clippy benchmark fixture should not be empty"
    );
    Invocation::new(
        vec!["cargo".to_owned(), "clippy".to_owned()],
        None::<String>,
        CLIPPY_FIXTURE.to_owned(),
        String::new(),
        Some(0),
    )
}

/// Parses diagnostics and builds the same digest shape used by the CLI.
///
/// # Contract
/// - requires: `CLIPPY_FIXTURE` remains parseable as clippy JSONL.
/// - ensures: sends a built digest to `black_box` when parsing succeeds.
/// - fails: parser failures are debug-asserted and skipped in optimized
///   benchmarks.
/// - panics: debug assertion failure only when the embedded fixture stops
///   parsing.
fn parse_and_digest()
{
    let parsed = parse_diagnostics(Protocol::ClippyJson, CLIPPY_FIXTURE);
    debug_assert!(
        parsed.is_ok(),
        "embedded clippy benchmark fixture should remain parseable"
    );
    let Ok(diagnostics) = parsed
    else {
        return;
    };
    let digest = build_digest(diagnostics, benchmark_invocation(), Some(16));
    core::hint::black_box(digest);
}

/// Registers ingestion benchmarks.
fn bench_ingest(c: &mut Criterion)
{
    c.bench_function("clippy fixture parse and digest", |b| {
        b.iter(parse_and_digest);
    });
}

criterion_group!(benches, bench_ingest);
criterion_main!(benches);
