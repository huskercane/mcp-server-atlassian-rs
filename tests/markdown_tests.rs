use mcp_server_atlassian_bitbucket::format::markdown::{format_heading, format_separator};
use pretty_assertions::assert_eq;

#[test]
fn separator_is_triple_dash() {
    assert_eq!(format_separator(), "---");
}

#[test]
fn heading_levels_1_through_6() {
    assert_eq!(format_heading("A", 1), "# A");
    assert_eq!(format_heading("B", 2), "## B");
    assert_eq!(format_heading("C", 3), "### C");
    assert_eq!(format_heading("D", 4), "#### D");
    assert_eq!(format_heading("E", 5), "##### E");
    assert_eq!(format_heading("F", 6), "###### F");
}

#[test]
fn heading_level_is_clamped() {
    assert_eq!(format_heading("Lo", 0), "# Lo");
    assert_eq!(format_heading("Hi", 42), "###### Hi");
}
