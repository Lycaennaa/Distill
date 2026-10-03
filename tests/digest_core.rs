use distill::{
    Diagnostic, Digest, FALLBACK_LINE_LIMIT, OutputLine, Severity, Stream, compact_text,
};

#[test]
fn parses_and_groups_repeated_locations() {
    let mut digest = Digest::default();
    digest.ingest(&OutputLine::new(
        Stream::Stdout,
        0,
        "Sources/Foo.swift:9:3: warning: use let",
    ));
    digest.ingest(&OutputLine::new(
        Stream::Stderr,
        0,
        "Sources/Foo.swift:4:1: warning: use let",
    ));
    digest.ingest(&OutputLine::new(
        Stream::Stderr,
        1,
        "Sources/Foo.swift:4:1: warning: use let",
    ));

    let groups = digest.diagnostics();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0].locations().len(), 2);
    assert_eq!(
        groups[0].render(),
        "Sources/Foo.swift: warning (lines 4, 9): use let"
    );
    assert_eq!(groups[0].locations()[0].count(), 2);
}

#[test]
fn sorts_severity_then_source_and_line() {
    let mut digest = Digest::default();
    digest.add_diagnostic(&Diagnostic::new(
        Severity::Warning,
        Some("b.swift".to_owned()),
        Some(1),
        None,
        "warning",
    ));
    digest.add_diagnostic(&Diagnostic::new(
        Severity::Error,
        Some("z.swift".to_owned()),
        Some(10),
        None,
        "error",
    ));
    digest.add_diagnostic(&Diagnostic::new(
        Severity::Error,
        Some("a.swift".to_owned()),
        Some(20),
        None,
        "error",
    ));

    let groups = digest.diagnostics();
    assert_eq!(groups[0].source(), Some("a.swift"));
    assert_eq!(groups[1].source(), Some("z.swift"));
    assert_eq!(groups[2].severity(), Severity::Warning);
}

#[test]
fn fallback_uses_bounded_tails_and_deterministic_stream_order() {
    let mut digest = Digest::default();
    for ordinal in 0..25 {
        digest.ingest(&OutputLine::new(
            Stream::Stdout,
            ordinal,
            format!("out {ordinal}"),
        ));
        digest.ingest(&OutputLine::new(
            Stream::Stderr,
            ordinal,
            format!("err {ordinal}"),
        ));
    }

    let lines = digest.fallback_lines();
    assert_eq!(lines.len(), FALLBACK_LINE_LIMIT);
    assert_eq!(lines.first().map(OutputLine::stream), Some(Stream::Stderr));
    assert_eq!(lines.first().map(OutputLine::ordinal), Some(15));
    assert_eq!(lines.last().map(OutputLine::stream), Some(Stream::Stdout));
    assert_eq!(lines.last().map(OutputLine::ordinal), Some(24));
}

#[test]
fn strips_ansi_and_compacts_control_separated_text() {
    assert_eq!(
        compact_text("\u{1b}[31merror\u{1b}[0m\tmessage"),
        "error message"
    );
}

#[test]
fn parses_xcode_list_sections_and_test_summary() {
    let mut digest = Digest::default();
    for (ordinal, text) in [
        (0, "Information about project \"Demo\":"),
        (1, "    Targets:"),
        (2, "        Demo"),
        (3, "    Build Configurations:"),
        (4, "        Debug"),
        (5, "    Schemes:"),
        (6, "        Demo"),
    ] {
        digest.ingest(&OutputLine::new(Stream::Stdout, ordinal, text));
    }
    let sections = digest.xcode().list_sections();
    assert_eq!(sections.len(), 4);
    assert_eq!(sections[0].kind(), distill::ListSectionKind::Project);
    assert_eq!(sections[0].values(), &["Demo"]);
    assert_eq!(sections[1].kind(), distill::ListSectionKind::Targets);
    assert_eq!(sections[1].values(), &["Demo"]);
    assert_eq!(sections[2].values(), &["Debug"]);
    assert_eq!(sections[3].kind(), distill::ListSectionKind::Schemes);

    digest.ingest(&OutputLine::new(
        Stream::Stdout,
        7,
        "Test Case '-[DemoTests testOne]' passed (0.001 seconds).",
    ));
    digest.ingest(&OutputLine::new(
        Stream::Stdout,
        8,
        "Test Case '-[DemoTests testTwo]' failed: assertion failed",
    ));
    digest.ingest(&OutputLine::new(
        Stream::Stdout,
        9,
        "Executed 2 tests, with 1 failure (0 unexpected) in 0.2 seconds",
    ));
    assert_eq!(digest.xcode().test_summary().passed(), 1);
    assert_eq!(digest.xcode().test_summary().failed(), 1);
    assert_eq!(digest.xcode().test_failures().len(), 1);
    assert_eq!(
        digest.xcode().test_failures()[0].reason(),
        Some("assertion failed")
    );
}

#[test]
fn retains_aggregate_xcode_counts_when_case_lines_are_absent() {
    let mut digest = Digest::default();
    digest.ingest(&OutputLine::new(
        Stream::Stdout,
        0,
        "Executed 0 tests, with 0 failures (0 unexpected) in 0.0 seconds",
    ));
    assert!(digest.xcode().test_summary().has_tests());
    assert_eq!(digest.xcode().test_summary().passed(), 0);
    assert_eq!(digest.xcode().test_summary().failed(), 0);

    digest.ingest(&OutputLine::new(
        Stream::Stdout,
        1,
        "Executed 4 tests, with 1 failure (0 unexpected) in 0.2 seconds (1 skipped)",
    ));
    assert_eq!(digest.xcode().test_summary().executed(), 4);
    assert_eq!(digest.xcode().test_summary().passed(), 3);
    assert_eq!(digest.xcode().test_summary().failed(), 1);
    assert_eq!(digest.xcode().test_summary().skipped(), 1);
}
