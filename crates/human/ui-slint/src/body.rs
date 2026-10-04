// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! A message body as the view draws it: `chat-protocol`'s block tree,
//! flattened into lines of plain text, and the links it carries.
//!
//! The tree is the one parse of the remote bytes (ADR-0050 rule 6). What
//! leaves this module is plain text for `Text` elements -- never markup
//! for a second parser to read (architect-cto's B6 ruling: Slint's own
//! `StyledText` is not given remote-derived text). So inline emphasis,
//! strong, strikethrough and code spans are drawn as their text alone; a
//! link's label stays where it stood, and its destination becomes a
//! separate control; an image is a placeholder naming its alt text, and
//! nothing is fetched.

use interweave_human_chat_protocol::{Block, Inline, Rendered};

/// What a line is, which decides how it is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// Running text: a paragraph, a list item's text, raw HTML shown
    /// literally.
    Text,
    /// A heading, its level 1 to 6.
    Heading(u8),
    /// A code block, as written.
    Code,
    /// A thematic break.
    Rule,
    /// A table's header row, its cells joined.
    TableHeader,
    /// A table body row, its cells joined.
    TableRow,
}

/// One drawn line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Line {
    pub(crate) kind: Kind,
    /// How many lists and quotes it sits in.
    pub(crate) depth: usize,
    /// Whether any enclosing block is a quote.
    pub(crate) quoted: bool,
    /// A list item's marker on its first line: `•`, or `3.`.
    pub(crate) marker: String,
    pub(crate) text: String,
}

/// A body: its lines, and its links' destinations in the order they
/// appear. Every destination is one the tree kept, so an allowlisted
/// scheme; the view checks again on activation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Body {
    pub(crate) lines: Vec<Line>,
    pub(crate) links: Vec<String>,
}

/// Separates a table row's cells.
const CELL_SEPARATOR: &str = " | ";

/// `rendered`, flattened. `image` names an image placeholder from its alt
/// text, in the view's words.
pub(crate) fn flatten(rendered: &Rendered, image: &dyn Fn(&str) -> String) -> Body {
    let mut body = Body::default();
    match rendered {
        // Past a bound: the source as received, one block of plain text.
        Rendered::PlainText { source, .. } => body.lines.push(Line {
            kind: Kind::Text,
            depth: 0,
            quoted: false,
            marker: String::new(),
            text: source.clone(),
        }),
        Rendered::Markdown(blocks) => {
            let mut walk = Walk {
                body: &mut body,
                image,
            };
            for block in blocks {
                walk.block(block, 0, false, "");
            }
        }
    }
    body
}

struct Walk<'a> {
    body: &'a mut Body,
    image: &'a dyn Fn(&str) -> String,
}

impl Walk<'_> {
    /// Push `block`'s lines. `marker` is given to its first line only:
    /// a list item's second paragraph is not numbered again.
    fn block(&mut self, block: &Block, depth: usize, quoted: bool, marker: &str) {
        let line = |kind, text| Line {
            kind,
            depth,
            quoted,
            marker: marker.to_owned(),
            text,
        };
        match block {
            Block::Paragraph(content) => {
                let text = self.inlines(content);
                self.body.lines.push(line(Kind::Text, text));
            }
            Block::Heading { level, content } => {
                let text = self.inlines(content);
                self.body.lines.push(line(Kind::Heading(*level), text));
            }
            Block::Code { text, .. } => {
                // The final newline a fence leaves is not a line.
                let text = text.strip_suffix('\n').unwrap_or(text).to_owned();
                self.body.lines.push(line(Kind::Code, text));
            }
            Block::Html(html) => self.body.lines.push(line(Kind::Text, html.clone())),
            Block::Rule => self.body.lines.push(line(Kind::Rule, String::new())),
            Block::Table { header, rows, .. } => {
                let header = self.cells(header);
                self.body.lines.push(line(Kind::TableHeader, header));
                for row in rows {
                    let row = self.cells(row);
                    self.body.lines.push(Line {
                        marker: String::new(),
                        ..line(Kind::TableRow, row)
                    });
                }
            }
            Block::Quote(blocks) => {
                for (i, inner) in blocks.iter().enumerate() {
                    let marker = if i == 0 { marker } else { "" };
                    self.block(inner, depth + 1, true, marker);
                }
            }
            Block::List { start, items } => {
                for (n, item) in items.iter().enumerate() {
                    let marker = match start {
                        Some(first) => format!("{}.", first.saturating_add(n as u64)),
                        None => "\u{2022}".to_owned(),
                    };
                    for (i, inner) in item.iter().enumerate() {
                        let marker = if i == 0 { marker.as_str() } else { "" };
                        self.block(inner, depth + 1, quoted, marker);
                    }
                }
            }
        }
    }

    fn cells(&mut self, cells: &[Vec<Inline>]) -> String {
        cells
            .iter()
            .map(|cell| self.inlines(cell))
            .collect::<Vec<_>>()
            .join(CELL_SEPARATOR)
    }

    fn inlines(&mut self, content: &[Inline]) -> String {
        let mut text = String::new();
        self.push_inlines(content, &mut text);
        text
    }

    fn push_inlines(&mut self, content: &[Inline], out: &mut String) {
        for inline in content {
            match inline {
                Inline::Text(text) | Inline::Code(text) | Inline::Html(text) => out.push_str(text),
                Inline::Emphasis(inner) | Inline::Strong(inner) | Inline::Strikethrough(inner) => {
                    self.push_inlines(inner, out);
                }
                Inline::Link {
                    destination,
                    content,
                } => {
                    self.push_inlines(content, out);
                    self.body.links.push(destination.clone());
                }
                Inline::Image { alt, .. } => {
                    let alt = self.inlines(alt);
                    out.push_str(&(self.image)(&alt));
                }
                Inline::SoftBreak => out.push(' '),
                Inline::HardBreak => out.push('\n'),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use interweave_human_chat_protocol::render;

    use super::{Body, Kind, Line, flatten};

    fn body(source: &str) -> Body {
        flatten(&render(source), &|alt| format!("[image: {alt}]"))
    }

    fn line(kind: Kind, depth: usize, quoted: bool, marker: &str, text: &str) -> Line {
        Line {
            kind,
            depth,
            quoted,
            marker: marker.to_owned(),
            text: text.to_owned(),
        }
    }

    #[test]
    fn blocks_become_lines_of_their_text_and_inline_marks_drop_to_text() {
        let got = body(
            "# Title\n\nSome *soft* and **strong** ~~gone~~ `code`.\n\n---\n\n```rust\nfn x() {}\n```\n",
        );
        assert_eq!(
            got.lines,
            [
                line(Kind::Heading(1), 0, false, "", "Title"),
                line(Kind::Text, 0, false, "", "Some soft and strong gone code."),
                line(Kind::Rule, 0, false, "", ""),
                line(Kind::Code, 0, false, "", "fn x() {}"),
            ]
        );
        assert!(got.links.is_empty());
    }

    #[test]
    fn lists_and_quotes_nest_by_depth_and_number_from_their_start() {
        let got = body("3. three\n4. four\n   - inner\n\n> quoted\n> > deeper\n");
        assert_eq!(
            got.lines,
            [
                line(Kind::Text, 1, false, "3.", "three"),
                line(Kind::Text, 1, false, "4.", "four"),
                line(Kind::Text, 2, false, "\u{2022}", "inner"),
                line(Kind::Text, 1, true, "", "quoted"),
                line(Kind::Text, 2, true, "", "deeper"),
            ]
        );
    }

    #[test]
    fn a_link_keeps_its_label_in_place_and_its_destination_becomes_a_link() {
        let got = body("See [the docs](https://example.org/a) and [mail](mailto:a@example.org).");
        assert_eq!(
            got.lines,
            [line(Kind::Text, 0, false, "", "See the docs and mail.")]
        );
        assert_eq!(got.links, ["https://example.org/a", "mailto:a@example.org"]);
    }

    #[test]
    fn a_link_outside_the_allowlist_is_its_text_alone() {
        let got = body("[click](javascript:alert(1)) and [file](file:///etc/passwd)");
        assert_eq!(
            got.lines,
            [line(Kind::Text, 0, false, "", "click and file")]
        );
        assert!(got.links.is_empty(), "no control for an inert link");
    }

    #[test]
    fn an_image_is_a_placeholder_naming_its_alt_and_never_a_link() {
        let got = body("![a cat](https://example.org/cat.png)");
        assert_eq!(
            got.lines,
            [line(Kind::Text, 0, false, "", "[image: a cat]")]
        );
        assert!(got.links.is_empty(), "the image's address is not offered");
    }

    #[test]
    fn raw_html_is_literal_text() {
        let got = body("<b>bold?</b>\n\ninline <i>x</i> html\n");
        assert_eq!(
            got.lines,
            [
                line(Kind::Text, 0, false, "", "<b>bold?</b>"),
                line(Kind::Text, 0, false, "", "inline <i>x</i> html"),
            ]
        );
    }

    #[test]
    fn a_table_is_a_header_and_rows_of_joined_cells() {
        let got = body("| a | b |\n|---|---|\n| 1 | 2 |\n| 3 | 4 |\n");
        assert_eq!(
            got.lines,
            [
                line(Kind::TableHeader, 0, false, "", "a | b"),
                line(Kind::TableRow, 0, false, "", "1 | 2"),
                line(Kind::TableRow, 0, false, "", "3 | 4"),
            ]
        );
    }

    #[test]
    fn a_source_past_a_bound_is_one_line_of_the_source() {
        let deep = format!("{}x", "> ".repeat(17));
        let got = body(&deep);
        assert_eq!(got.lines, [line(Kind::Text, 0, false, "", &deep)]);
        assert!(got.links.is_empty());
    }
}
