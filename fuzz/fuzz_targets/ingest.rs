#![no_main]
//! Fuzzes diagnostic ingestion adapters with arbitrary byte input.

use aifix::adapter::parse_diagnostics;
use aifix::digest::build_digest;
use aifix::model::Invocation;
use aifix::model::Protocol;
use libfuzzer_sys::fuzz_target;

/// Protocols exercised for every fuzz input.
///
/// # Contract
/// Preconditions: each protocol is accepted by `parse_diagnostics`.
/// Postconditions: keeps fuzz coverage aligned with every ingest parser exposed
/// by the CLI. Failure modes: none.
/// Panics: none.
const FUZZ_PROTOCOLS: [Protocol; 6] = [
    Protocol::Auto,
    Protocol::AifixJson,
    Protocol::ClippyJson,
    Protocol::TypescriptText,
    Protocol::LspJson,
    Protocol::NushellText,
];

/// Builds minimal invocation metadata when a parser finds diagnostics.
///
/// # Contract
/// Preconditions: `input` is the exact text passed to the parser.
/// Postconditions: returns successful fuzz invocation metadata that preserves
/// parser input. Failure modes: none.
/// Panics: none.
fn fuzz_invocation(input: &str) -> Invocation
{
    Invocation::new(
        vec!["fuzz".to_owned(), "ingest".to_owned()],
        None::<String>,
        input.to_owned(),
        String::new(),
        Some(0),
    )
}

// Fuzz entry point.
//
// Contract:
// Preconditions: libFuzzer supplies arbitrary bytes; invalid UTF-8 is tolerated
// lossy. Postconditions: every configured parser sees the input and successful
// parses build a digest. Failure modes: parser errors are ignored so fuzzing
// can continue across protocols. Panics: debug assertion failure only if the
// static parser list is empty or omits auto-detection.
fuzz_target!(|data: &[u8]| {
    let input = String::from_utf8_lossy(data);
    debug_assert!(
        !FUZZ_PROTOCOLS.is_empty(),
        "fuzz parser loop should exercise at least one protocol"
    );
    debug_assert!(
        FUZZ_PROTOCOLS.contains(&Protocol::Auto),
        "fuzz parser loop should include auto-detection coverage"
    );

    for protocol in FUZZ_PROTOCOLS {
        if let Ok(diagnostics) = parse_diagnostics(protocol, input.as_ref()) {
            let digest = build_digest(diagnostics, fuzz_invocation(input.as_ref()), Some(32));
            core::hint::black_box(digest);
        }
    }
});
