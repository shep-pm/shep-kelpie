//! A pull request body's soft-wrapped lines, joined through [`join`]

use super::join;

#[test]
fn a_wrapped_paragraph_becomes_one_line() {
    let body = "GitHub shows every newline in a body as a line break, so a body a \
                worker hard-wraps at eighty columns shows as a ragged narrow \
                column.\n\nA second paragraph that\nalso wraps.";

    assert_eq!(
        join(body),
        "GitHub shows every newline in a body as a line break, so a body a worker \
         hard-wraps at eighty columns shows as a ragged narrow column.\n\nA second \
         paragraph that also wraps."
    );
}

#[test]
fn a_wrapped_list_item_and_its_nested_items_each_stay_one_line() {
    let body = "- A list item that the worker wrapped over two\n  lines\n\
                - An item with a nested item:\n  - The nested item that wraps\n    onto a second line\n\
                1. An ordered item that wraps\n    too";

    assert_eq!(
        join(body),
        "- A list item that the worker wrapped over two lines\n\
         - An item with a nested item:\n  - The nested item that wraps onto a second line\n\
         1. An ordered item that wraps too"
    );
}

#[test]
fn a_fenced_or_indented_code_block_is_kept_as_written() {
    let fenced =
        "Run it:\n```sh\nshep kelpie watch   --follow\n```\nText after the fence\njoins back.";
    assert_eq!(
        join(fenced),
        "Run it:\n```sh\nshep kelpie watch   --follow\n```\nText after the fence joins back."
    );

    let indented = "Or indented:\n\n    shep kelpie watch   --follow\n    with   extra spaces\n\nText after the block\njoins back.";
    assert_eq!(
        join(indented),
        "Or indented:\n\n    shep kelpie watch   --follow\n    with   extra spaces\n\nText after the block joins back."
    );
}

#[test]
fn headings_tables_quotes_and_hard_breaks_are_not_joined() {
    let body = "Text before a heading\n## A heading that wraps\nA line after a heading\n\n\
                | Column | What |\nA line after a table row\n\n\
                A line before a quote\n> A quote after a table row\n\n\
                A line ending in two spaces  \nA line after it\n\n\
                A line ending in a backslash\\\nA line after it";

    assert_eq!(join(body), body);
}

#[test]
fn a_table_without_leading_pipes_stays_a_table() {
    let body = "Text before the table\nColumn | What\n--- | ---\na | b\n\nText after the table.";

    assert_eq!(join(body), body);
}

#[test]
fn a_setext_underline_is_not_joined_to_its_title() {
    let body = "Summary\n=======\nText after it\n\nAnother\n-----\nAnd more";

    assert_eq!(join(body), body);
}

#[test]
fn a_paragraph_after_an_html_line_starts_a_line_of_its_own() {
    let body =
        "<!-- note -->\nThe **change** here\n\n<details>\n<summary>More</summary>\n</details>";

    assert_eq!(join(body), body);
}

#[test]
fn a_continuation_that_starts_like_a_flag_or_issue_is_still_joined() {
    let body = "The verb takes the flag\n--no-deps\nand nothing else.\n\nIt follows\n#1498\n\
                and closes it.\n\nThe change costs\n+478\nlines.";

    assert_eq!(
        join(body),
        "The verb takes the flag --no-deps and nothing else.\n\nIt follows #1498 and closes it.\n\nThe change costs +478 lines."
    );
}

#[test]
fn a_body_with_windows_line_endings_is_joined_like_any_other() {
    let body = "A paragraph that wraps\r\nonto a second line.\r\n\r\n    code   line\r\n";

    assert_eq!(
        join(body),
        "A paragraph that wraps onto a second line.\n\n    code   line"
    );
}

#[test]
fn a_body_already_on_one_line_per_paragraph_is_unchanged() {
    let body = "Lead with the point.\n\n## What this changes\n\n- One item\n- Another item\n\n\
                `join` keeps a body readable.\n\nResolves #7";

    assert_eq!(join(body), body);
}
