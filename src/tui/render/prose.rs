//! Keys and typed commands inside TUI prose:
//! copy marks them with [`key`] / [`cmd`], every render site
//! turns the marked string into spans through [`spans`] or [`wrap`].
//!
//! The markers are private-use code points, so no literal copy can hold one:
//! a bracket marker would collide with copy that prints `[server.admin]` or a
//! `[ 3 live ]` chip.

use std::ops::Range;

use ratatui::style::{Modifier, Style};
use ratatui::text::Span;

use super::super::theme;
use super::format::{grapheme_cells, prefix_in_cells};
use super::panes;

const KEY_OPEN: char = '\u{E000}';
const CMD_OPEN: char = '\u{E001}';
const CLOSE: char = '\u{E002}';

/// Whether `ch` is one of the marker code points ([`KEY_OPEN`], [`CMD_OPEN`],
/// [`CLOSE`]): a copy source must escape it before the string reaches any
/// tokenizer entry in this module, so an untrusted string cannot style the
/// cells after it or drop a character.
pub(crate) fn is_marker(ch: char) -> bool {
    matches!(ch, KEY_OPEN | CMD_OPEN | CLOSE)
}

/// [`key`] for `&'static` copy built with `concat!`; spells the markers out
/// because `concat!` takes literals only.
macro_rules! key_lit {
    ($name:literal) => {
        concat!("\u{E000}", $name, "\u{E002}")
    };
}

/// [`cmd`] for `&'static` copy built with `concat!`.
macro_rules! cmd_lit {
    ($command:literal) => {
        concat!("\u{E001}", $command, "\u{E002}")
    };
}

pub(crate) use {cmd_lit, key_lit};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Prose,
    Key,
    Cmd,
}

/// A key named in running copy (`press {key("d")} to resolve`).
pub(crate) fn key(name: &str) -> String {
    format!("{KEY_OPEN}{name}{CLOSE}")
}

/// A command the end user types (`run {cmd("clauth login")}`).
pub(crate) fn cmd(command: &str) -> String {
    format!("{CMD_OPEN}{command}{CLOSE}")
}

/// A binary path is never longer than Linux's `PATH_MAX`; capping the
/// candidates there keeps a huge backticked span (a tool's whole stderr) from
/// costing a rescan per space.
const BINARY_PATH_MAX: usize = 4096;

/// The CLIs whose commands clauth's copy tells the end user to run.
const COMMAND_BINARIES: &[&str] = &["clauth", "claude", "codex", "herdr", "shunt", "cargo"];

/// Copy shared with the CLI (an error, a `format::Message`) names a command in
/// backticks. A toast built from it marks each backticked span that is a
/// command (one of [`COMMAND_BINARIES`], bare or as a path's file stem, a path
/// holding spaces included, then at least one more word, on one line) and
/// leaves every other backticked span (an error code, a config key, a bare
/// tool name) as written.
pub(crate) fn mark_commands(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(open) = rest.find('`') {
        let after = &rest[open + 1..];
        let Some(close) = after.find('`') else {
            break;
        };
        let span = &after[..close];
        out.push_str(&rest[..=open]);
        if is_command(span) {
            out.pop();
            out.push_str(&cmd(span));
        } else {
            out.push_str(span);
            out.push('`');
        }
        rest = &after[close + 1..];
    }
    out.push_str(rest);
    out
}

/// Every prefix ending at a space is a candidate binary, so a path holding a
/// space (`C:\Program Files\…\shunt.exe`) still counts; a candidate with a
/// separator must start like a path, one without must be a single word, so
/// `foo /usr/bin/clauth x` is no command. Both separators are split by hand so
/// a Windows path reads the same on every host.
fn is_command(span: &str) -> bool {
    !span.contains('\n')
        && span
            .match_indices(' ')
            .take_while(|(at, _)| *at <= BINARY_PATH_MAX)
            .any(|(at, _)| {
                let candidate = &span[..at];
                let shaped = if candidate.contains(['/', '\\']) {
                    candidate.starts_with(['/', '\\', '~', '.'])
                        || candidate.as_bytes().get(1) == Some(&b':')
                } else {
                    !candidate.contains(' ')
                };
                let leaf = candidate.rsplit(['/', '\\']).next().unwrap_or_default();
                let stem = leaf.rsplit_once('.').map_or(leaf, |(stem, _)| stem);
                shaped && COMMAND_BINARIES.contains(&stem) && !span[at + 1..].trim().is_empty()
            })
}

/// The text as it reads on screen: every marker dropped.
pub(crate) fn plain(text: &str) -> String {
    classify(text).into_iter().map(|(ch, _)| ch).collect()
}

/// One line of marked copy as spans: prose in `base`, a key `ACCENT + bold`,
/// a command `ACCENT` without bold, each keeping `base`'s background.
pub(crate) fn spans(text: &str, base: Style) -> Vec<Span<'static>> {
    runs(&classify(text), base)
}

/// [`spans`] plus the grapheme ranges that wrap as one unit, for a wrapper
/// that breaks a line after it was styled ([`super::modals::chunk_line`]).
pub(crate) struct Marked {
    spans: Vec<Span<'static>>,
    units: Vec<Range<usize>>,
}

impl Marked {
    pub(crate) fn into_parts(self) -> (Vec<Span<'static>>, Vec<Range<usize>>) {
        (self.spans, self.units)
    }
}

/// [`spans`] keeping which graphemes are a marked unit.
pub(crate) fn marked(text: &str, base: Style) -> Marked {
    let chars = classify(text);
    let mut units = Vec::new();
    let mut open = None;
    let mut grapheme = 0usize;
    // Count graphemes span by span, the way the modal body's wrapper walks the
    // styled line: it maps a tab to one space up front, and a grapheme whose
    // text holds another control character is dropped by both walks, so a
    // cluster straddling a span edge counts once per side.
    for (kind, run) in kind_runs(&chars) {
        let run = run.replace('\t', " ");
        for _ in Span::raw(run.as_str()).styled_graphemes(Style::default()) {
            if kind == Kind::Prose {
                if let Some(start) = open.take() {
                    units.push(start..grapheme);
                }
            } else {
                open.get_or_insert(grapheme);
            }
            grapheme += 1;
        }
    }
    if let Some(start) = open {
        units.push(start..grapheme);
    }
    Marked {
        spans: runs(&chars, base),
        units,
    }
}

/// [`spans`] cut to `width` display cells, grapheme-safe: a trailing `…` in
/// `base` when anything was dropped. The budget counts only visible characters
/// (markers never reach the screen), and a wide glyph (`⚠️`) counts its two
/// cells so the `…` stays inside the pane.
pub(crate) fn fit(text: &str, width: usize, base: Style) -> Vec<Span<'static>> {
    // Control graphemes never reach the screen, so they drop out of both the
    // measurement and the kept list — otherwise `chars[..consumed]` indexes a
    // list that still holds them and the cut lands inside a grapheme.
    let chars: Vec<(char, Kind)> = classify(text)
        .into_iter()
        .filter(|(c, _)| !c.is_control())
        .collect();
    let visible: String = chars.iter().map(|(ch, _)| *ch).collect();
    if grapheme_cells(&visible) <= width {
        return runs(&chars, base);
    }
    if width == 0 {
        return Vec::new();
    }
    // One cell is reserved for `…`; the rest fills with whole graphemes.
    let (kept_text, _) = prefix_in_cells(&visible, width - 1);
    let consumed = kept_text.chars().count();
    let mut kept: Vec<(char, Kind)> = chars[..consumed].to_vec();
    kept.push(('…', Kind::Prose));
    runs(&kept, base)
}

/// [`wrap_words`] over marked copy: the wrap budget counts only visible
/// characters, each wrapped line keeps the styling its characters had, and a
/// marked key or command wraps as ONE unit — it joins the row when it fits
/// beside what is there, else starts the next row when it fits one alone, else
/// breaks at its own spaces like prose (only a single word wider than the row
/// hard-splits, by grapheme). Prose-only copy wraps exactly as [`wrap_words`].
pub(crate) fn wrap(text: &str, width: usize, base: Style) -> Vec<Vec<Span<'static>>> {
    fill(&classify(text), width)
        .into_iter()
        .map(|line| runs(&line, base))
        .collect()
}

/// [`wrap`]'s greedy fill as plain strings: the no-unit entry point
/// [`super::panes::wrap_words`] takes.
pub(crate) fn plain_wrap(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<(char, Kind)> = text.chars().map(|ch| (ch, Kind::Prose)).collect();
    fill(&chars, width)
        .into_iter()
        .map(|line| line.into_iter().map(|(ch, _)| ch).collect())
        .collect()
}

/// One wrap token: visible characters with their kinds, and the whitespace run
/// before it. A marked run is one token however many spaces it holds, which is
/// what keeps a key or command from breaking; a prose word is one token.
struct Token {
    chars: Vec<(char, Kind)>,
    width: usize,
    /// The kind of the whitespace run before the token; `None` when it follows
    /// the previous token with nothing between (`run{cmd("x")}`).
    sep: Option<Kind>,
}

fn tokens(chars: &[(char, Kind)]) -> Vec<Token> {
    let mut out: Vec<Token> = Vec::new();
    let mut sep: Option<Kind> = None;
    let mut i = 0;
    while i < chars.len() {
        let (ch, kind) = chars[i];
        if kind == Kind::Prose && ch.is_whitespace() {
            sep = Some(kind);
            i += 1;
            continue;
        }
        let marked = kind != Kind::Prose;
        let start = i;
        while i < chars.len() {
            let (ch, kind) = chars[i];
            let same = if marked {
                kind != Kind::Prose
            } else {
                kind == Kind::Prose && !ch.is_whitespace()
            };
            if !same {
                break;
            }
            i += 1;
        }
        if sep.is_none()
            && let Some(last) = out.last_mut()
        {
            // Pressed against the token before it: keep the visible text.
            last.chars.extend_from_slice(&chars[start..i]);
            normalize_unit_whitespace(&mut last.chars);
            last.width = width_of(&last.chars);
            continue;
        }
        let mut token = chars[start..i].to_vec();
        normalize_unit_whitespace(&mut token);
        out.push(Token {
            width: width_of(&token),
            chars: token,
            sep,
        });
        sep = None;
    }
    out
}

/// A marked run copies its own whitespace, and a tab renders as nothing:
/// collapse a run of ASCII spaces or tabs inside it to one ordinary space and
/// drop a trailing run, so the rendered cells still paste as the command. A
/// non-breaking space is data, never a break or a collapse, so the predicate
/// is the ASCII space and tab alone.
fn normalize_unit_whitespace(chars: &mut Vec<(char, Kind)>) {
    if !chars.iter().any(|(_, kind)| *kind != Kind::Prose) {
        return;
    }
    let mut out = Vec::with_capacity(chars.len());
    let mut i = 0;
    while i < chars.len() {
        let (ch, kind) = chars[i];
        if matches!(ch, ' ' | '\t') {
            let mut end = i;
            while end < chars.len() && matches!(chars[end].0, ' ' | '\t') {
                end += 1;
            }
            if end < chars.len() {
                out.push((' ', kind));
            }
            i = end;
            continue;
        }
        out.push((ch, kind));
        i += 1;
    }
    *chars = out;
}

/// Greedy fill to `width` cells, measured the way the buffer renders: by
/// grapheme, a wide one taking two.
fn fill(chars: &[(char, Kind)], width: usize) -> Vec<Vec<(char, Kind)>> {
    if width == 0 {
        return vec![chars.to_vec()];
    }
    let mut lines: Vec<Vec<(char, Kind)>> = Vec::new();
    let mut cur: Vec<(char, Kind)> = Vec::new();
    let mut cur_w = 0usize;
    for token in tokens(chars) {
        let sep = if cur.is_empty() { None } else { token.sep };
        let sep_w = usize::from(sep.is_some());
        if fits_a_row(&token, width) && cur_w + sep_w + token.width <= width {
            if let Some(kind) = sep {
                cur.push((' ', kind));
                cur_w += 1;
            }
            cur.extend_from_slice(&token.chars);
            cur_w += token.width;
            continue;
        }
        if fits_a_row(&token, width) {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            cur.extend_from_slice(&token.chars);
            cur_w = token.width;
            continue;
        }
        // Too wide for any row: glued punctuation is placed beside the marked
        // run, which keeps a row it fits; otherwise the token is no unit any
        // more and its words fill from where the row already stands.
        place_overflow(&token, sep, width, &mut cur, &mut cur_w, &mut lines);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.is_empty() {
        lines.push(Vec::new());
    }
    lines
}

/// A token holds a break when it carries a newline: no row can hold it whole,
/// however short it looks, so it takes the splitting path.
fn fits_a_row(token: &Token, width: usize) -> bool {
    token.width <= width && !token.chars.iter().any(|(ch, _)| *ch == '\n')
}

/// The first and last marked character in a token: punctuation pressed
/// against a marked run sits outside these bounds.
fn unit_bounds(chars: &[(char, Kind)]) -> Option<(usize, usize)> {
    let start = chars.iter().position(|(_, kind)| *kind != Kind::Prose)?;
    let end = chars.iter().rposition(|(_, kind)| *kind != Kind::Prose)? + 1;
    Some((start, end))
}

/// Place a token no row holds whole. When it carries a marked run that fits a
/// row, the punctuation glued to the run is never measured with it: the
/// leading glue stays on the row it fits (else opens the next), the run takes
/// a row it fits, and the trailing glue rides the run's row when it fits there
/// (else opens the next). Otherwise the token breaks at its spaces like prose.
fn place_overflow(
    token: &Token,
    sep: Option<Kind>,
    width: usize,
    cur: &mut Vec<(char, Kind)>,
    cur_w: &mut usize,
    lines: &mut Vec<Vec<(char, Kind)>>,
) {
    let Some((start, end)) = unit_bounds(&token.chars) else {
        break_at_spaces(&token.chars, sep, width, cur, cur_w, lines);
        return;
    };
    let unit = &token.chars[start..end];
    if width_of(unit) > width || unit.iter().any(|(ch, _)| *ch == '\n') {
        break_at_spaces(&token.chars, sep, width, cur, cur_w, lines);
        return;
    }
    if start > 0 {
        place_word(&token.chars[..start], sep, width, cur, cur_w, lines);
        place_unit(unit, None, width, cur, cur_w, lines);
    } else {
        place_unit(unit, sep, width, cur, cur_w, lines);
    }
    if end < token.chars.len() {
        place_word(&token.chars[end..], None, width, cur, cur_w, lines);
    }
}

/// Append a marked run that fits a row as one piece: beside what the row holds
/// when the run and the whitespace before it fit there, else on a row of its
/// own.
fn place_unit(
    unit: &[(char, Kind)],
    sep: Option<Kind>,
    width: usize,
    cur: &mut Vec<(char, Kind)>,
    cur_w: &mut usize,
    lines: &mut Vec<Vec<(char, Kind)>>,
) {
    let unit_w = width_of(unit);
    let sep = if cur.is_empty() { None } else { sep };
    if !cur.is_empty() && *cur_w + usize::from(sep.is_some()) + unit_w > width {
        lines.push(std::mem::take(cur));
        *cur_w = 0;
    } else if let Some(kind) = sep {
        cur.push((' ', kind));
        *cur_w += 1;
    }
    cur.extend_from_slice(unit);
    *cur_w += unit_w;
}

/// Place a token's words one row at a time: a marked run wider than a row
/// breaks at its own spaces, and a lone word wider than the row by grapheme.
/// `first` is the whitespace run before the token's first word.
fn break_at_spaces(
    chars: &[(char, Kind)],
    first: Option<Kind>,
    width: usize,
    cur: &mut Vec<(char, Kind)>,
    cur_w: &mut usize,
    lines: &mut Vec<Vec<(char, Kind)>>,
) {
    let mut word: Vec<(char, Kind)> = Vec::new();
    let mut sep: Option<Kind> = first;
    for &(ch, kind) in chars {
        if ch.is_whitespace() {
            if !word.is_empty() {
                place_word(&word, sep, width, cur, cur_w, lines);
                word.clear();
            }
            sep = Some(kind);
            continue;
        }
        word.push((ch, kind));
    }
    if !word.is_empty() {
        place_word(&word, sep, width, cur, cur_w, lines);
    }
}

fn place_word(
    word: &[(char, Kind)],
    sep: Option<Kind>,
    width: usize,
    cur: &mut Vec<(char, Kind)>,
    cur_w: &mut usize,
    lines: &mut Vec<Vec<(char, Kind)>>,
) {
    let word_w = width_of(word);
    if word_w > width {
        if !cur.is_empty() {
            lines.push(std::mem::take(cur));
            *cur_w = 0;
        }
        let mut chunks = chunk_word(word, width);
        let last = chunks.pop().unwrap_or_default();
        lines.extend(chunks);
        *cur_w = width_of(&last);
        *cur = last;
        return;
    }
    let sep = if cur.is_empty() { None } else { sep };
    if cur.is_empty() {
        cur.extend_from_slice(word);
        *cur_w = word_w;
    } else if *cur_w + usize::from(sep.is_some()) + word_w > width {
        lines.push(std::mem::take(cur));
        *cur_w = word_w;
        cur.extend_from_slice(word);
    } else {
        if let Some(kind) = sep {
            cur.push((' ', kind));
            *cur_w += 1;
        }
        cur.extend_from_slice(word);
        *cur_w += word_w;
    }
}

/// One word split into `width`-cell chunks by grapheme, a wide one taking two
/// cells. The chunking is the shared grapheme splitter, so a character the
/// buffer never draws (a control character) is dropped exactly as the buffer
/// drops it and no later chunk shifts; each kept character keeps its kind.
fn chunk_word(word: &[(char, Kind)], width: usize) -> Vec<Vec<(char, Kind)>> {
    let text: String = word.iter().map(|(ch, _)| *ch).collect();
    let mut chunks = Vec::new();
    let mut consumed = 0usize;
    for chunk in panes::chunk_graphemes(&text, width) {
        let mut kept = Vec::with_capacity(chunk.len());
        for _ in chunk.chars() {
            while word.get(consumed).is_some_and(|(ch, _)| ch.is_control()) {
                consumed += 1;
            }
            if let Some(&(ch, kind)) = word.get(consumed) {
                kept.push((ch, kind));
                consumed += 1;
            }
        }
        chunks.push(kept);
    }
    if chunks.is_empty() {
        chunks.push(Vec::new());
    }
    chunks
}

/// A run's width in display cells, measured as the string it renders.
fn width_of(chars: &[(char, Kind)]) -> usize {
    let text: String = chars.iter().map(|(ch, _)| *ch).collect();
    Span::raw(text.as_str()).width()
}

/// Each visible character with its kind. An opener with no closer after it
/// marks nothing and is dropped, so a stray opener at a line's tail styles
/// nothing; marker code points never reach the screen.
fn classify(text: &str) -> Vec<(char, Kind)> {
    let mut out = Vec::with_capacity(text.len());
    let mut kind = Kind::Prose;
    for (i, ch) in text.char_indices() {
        match ch {
            KEY_OPEN | CMD_OPEN => {
                let closed = text[i + ch.len_utf8()..].contains(CLOSE);
                if closed {
                    kind = if ch == KEY_OPEN { Kind::Key } else { Kind::Cmd };
                }
            }
            CLOSE => kind = Kind::Prose,
            _ => out.push((ch, kind)),
        }
    }
    out
}

fn runs(chars: &[(char, Kind)], base: Style) -> Vec<Span<'static>> {
    kind_runs(chars)
        .into_iter()
        .map(|(kind, text)| Span::styled(text, style_for(kind, base)))
        .collect()
}

/// The visible characters grouped into maximal runs of one kind: the grouping
/// [`runs`] styles and [`marked`] counts graphemes over.
fn kind_runs(chars: &[(char, Kind)]) -> Vec<(Kind, String)> {
    let mut out: Vec<(Kind, String)> = Vec::new();
    for &(ch, kind) in chars {
        match out.last_mut() {
            Some((run_kind, text)) if *run_kind == kind => text.push(ch),
            _ => out.push((kind, ch.to_string())),
        }
    }
    out
}

fn style_for(kind: Kind, base: Style) -> Style {
    match kind {
        Kind::Prose => base,
        Kind::Key => base.fg(theme::accent_color()).add_modifier(Modifier::BOLD),
        Kind::Cmd => base
            .fg(theme::accent_color())
            .remove_modifier(Modifier::BOLD),
    }
}

#[cfg(test)]
#[path = "../../../tests/inline/tui_render_prose.rs"]
mod tests;
