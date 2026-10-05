use super::*;

#[test]
fn well_formed_lines_parse_in_order() {
    let text = "HIGH|src/lib.rs:42|does the bad thing|breaks prod\n\
                LOW|src/main.rs:0|not reviewed: 900 lines exceeds the chunk limit|split the file or review it by hand";
    assert_eq!(
        parse_findings(text),
        vec![
            Finding {
                severity: Severity::High,
                file: "src/lib.rs".into(),
                line: 42,
                what: "does the bad thing".into(),
                why: "breaks prod".into(),
            },
            Finding {
                severity: Severity::Low,
                file: "src/main.rs".into(),
                line: 0,
                what: "not reviewed: 900 lines exceeds the chunk limit".into(),
                why: "split the file or review it by hand".into(),
            },
        ]
    );
}

#[test]
fn blank_and_malformed_lines_are_skipped() {
    let text = "\nCLEAN\nnot a finding at all\nMEDIUM|only|two|fields|extra\nMEDIUM|a.rs:no-number|what|why";
    assert_eq!(parse_findings(text), vec![]);
}

#[test]
fn severities_order_low_to_high() {
    assert!(Severity::Low < Severity::Medium);
    assert!(Severity::Medium < Severity::High);
}

// Recorded shape of a real round-N.txt, one line per severity plus a
// skipped-file placeholder.
#[test]
fn a_recorded_findings_file_parses() {
    let text = include_str!("../../fixtures/qwen-round.txt");
    let findings = parse_findings(text);
    assert_eq!(findings.len(), 4);
    assert_eq!(findings[0].severity, Severity::High);
    assert_eq!(findings[0].file, "src/pricing.rs");
    assert_eq!(
        findings[3].what,
        "not reviewed: 900 lines exceeds the chunk limit"
    );
}

#[test]
fn a_finding_is_the_same_by_its_file_line_and_what_and_not_its_why_or_severity() {
    let finding = |severity, why: &str| Finding {
        severity,
        file: "src/lib.rs".into(),
        line: 3,
        what: "the flag is read first".into(),
        why: why.into(),
    };
    let one = finding(Severity::High, "a caller sees nothing");
    assert!(one.is_same_as(&finding(Severity::Low, "worded again")));
    assert!(!one.is_same_as(&Finding {
        line: 4,
        ..one.clone()
    }));
    assert!(!one.is_same_as(&Finding {
        file: "src/b.rs".into(),
        ..one.clone()
    }));
    assert!(!one.is_same_as(&Finding {
        what: "another thing".into(),
        ..one.clone()
    }));
}
