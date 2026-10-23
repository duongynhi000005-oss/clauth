use super::super::panes::wrap_words;
use std::time::Instant;

#[test]
fn short_line_unchanged() {
    assert_eq!(wrap_words("hello world", 36), vec!["hello world"]);
}

#[test]
fn wraps_at_word_boundary() {
    let out = wrap_words("terminal too small · enlarge for full layout", 36);
    assert_eq!(out.len(), 2, "expected 2 wrapped lines, got {out:?}");
    for l in &out {
        assert!(
            l.chars().count() <= 36,
            "line exceeds cap: {l:?} ({} chars)",
            l.chars().count()
        );
    }
}

#[test]
fn empty_input_yields_one_empty_line() {
    assert_eq!(wrap_words("", 36), vec![""]);
}

#[test]
fn single_word_exceeding_cap_hard_breaks() {
    let long_word = "a".repeat(80);
    let out = wrap_words(&long_word, 36);
    assert_eq!(out.len(), 3);
    for l in &out {
        assert!(l.chars().count() <= 36);
    }
}

#[test]
fn wide_glyphs_wrap_by_cells() {
    // `漢` takes two cells: three fill a 6-cell line, and a 5-cell line holds
    // two before the word is split.
    assert_eq!(wrap_words("漢漢漢 漢漢", 6), vec!["漢漢漢", "漢漢"]);
    assert_eq!(wrap_words("漢漢漢 漢漢", 5), vec!["漢漢", "漢", "漢漢"]);
}

#[test]
fn an_emoji_presentation_sequence_splits_as_one_two_cell_glyph() {
    // `⚠️` is U+26A0 + U+FE0F: two chars, one grapheme, two cells.
    let w = "\u{26A0}\u{FE0F}";
    assert_eq!(
        wrap_words(&w.repeat(10), 6),
        vec![w.repeat(3), w.repeat(3), w.repeat(3), w.to_string()]
    );
}

#[test]
fn col_width_equals_content_width_plus_chrome() {
    let msg = "· enlarge for full layout";
    let content_cap: u16 = 36;
    let max_content_width = [msg]
        .iter()
        .map(|l| l.chars().count() as u16)
        .max()
        .unwrap()
        .min(content_cap);
    let col_width = max_content_width + 3;
    assert_eq!(max_content_width, 25);
    assert_eq!(col_width, 28);
}

/// Every span a toast lays out carries no background of its own, so the
/// cell-level glass blend stays visible under the text (a span bg would paint a
/// flat fill under it). The bar span (`┃ `) is built inside `draw`, not here,
/// and stays a named residual: the glass overwrites every cell's bg after the
/// spans, so a span bg is not cell-observable — this pins at span level.
#[test]
fn toast_rows_set_no_background_of_their_own() {
    use crate::tui::app::{Toast, ToastKind};

    // A head that wraps onto a second row, a detail row, then a body cut to the
    // three-row cap with its `…`: title (bold), continuation, detail and the
    // ellipsis spans must all carry `bg == None`.
    let toast = Toast {
        kind: ToastKind::Danger,
        body: format!("{}\ndetail line\n{}", "y".repeat(60), "z".repeat(60)),
        born: Instant::now(),
    };
    let rows = super::toast_rows(&toast, 57);
    assert!(!rows.is_empty(), "a toast lays out at least one row");
    for (r, row) in rows.iter().enumerate() {
        for (c, span) in row.iter().enumerate() {
            assert_eq!(
                span.style.bg, None,
                "span ({r},{c}) {:?} sets no bg",
                span.content
            );
        }
    }
}

/// A toast built from CLI copy names its command in backticks; the command
/// stays on one row at a budget that used to split it.
#[test]
fn a_backticked_command_in_a_toast_stays_on_one_row() {
    use crate::tui::app::{Toast, ToastKind};
    let toast = Toast {
        kind: ToastKind::Info,
        body: super::super::prose::mark_commands(
            "update available\nreinstall with `cargo install clauth`",
        ),
        born: Instant::now(),
    };
    let rows: Vec<String> = super::toast_rows(&toast, 20)
        .iter()
        .map(|row| row.iter().map(|span| span.content.as_ref()).collect())
        .collect();
    assert_eq!(
        rows,
        vec![
            "update available".to_string(),
            "reinstall with".to_string(),
            "cargo install clauth".to_string(),
        ]
    );
}
