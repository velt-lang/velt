//! Tests of the help texts.

use super::*;

#[test]
fn every_command_has_usage_and_examples() {
    for c in COMMANDS {
        assert!(!c.usage.is_empty(), "{}", c.name);
        assert!(!c.examples.is_empty(), "{} has no examples", c.name);
        assert!(c.usage.iter().all(|u| u.starts_with(c.name)), "{}", c.name);
        let text = command(c);
        assert!(
            text.contains("usage:") && text.contains("examples:"),
            "{text}"
        );
        assert!(text.contains("-h, --help"), "{text}");
    }
}

#[test]
fn overview_lists_every_command_and_template() {
    let text = overview();
    for c in COMMANDS {
        assert!(text.contains(c.summary), "{}", c.name);
    }
    for t in Template::ALL {
        assert!(text.contains(t.description()));
    }
    assert!(text.contains("NO_COLOR"));
}

#[test]
fn flags_come_from_option_labels() {
    let build = find("build").unwrap();
    let flags = build.flags();
    for f in [
        "-o",
        "--output",
        "--release",
        "-g",
        "--host",
        "--emit",
        "-v",
        "--verbose",
        "--help",
    ] {
        assert!(flags.contains(&f), "{f} missing from {flags:?}");
    }
    assert!(!flags.contains(&"<path>"));
}

#[test]
fn wraps_long_text() {
    assert_eq!(wrap("aa bb cc", 5), "aa bb\ncc");
}
