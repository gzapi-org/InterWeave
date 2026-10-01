// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The markdown render model (plan §17 (3), `HUMAN-CHAT.md` §Text is
//! markdown): an envelope's `text` as a renderer-independent block tree,
//! or -- past a bound -- the source as plain text.
//!
//! The subset is pinned here, by the parser's options: CommonMark plus
//! GFM's `table` and `strikethrough` and NOTHING else, so a task list, a
//! footnote or an extended autolink is literal text. On top of the parse:
//!
//! - raw HTML, block or inline, is [`Block::Html`] / [`Inline::Html`]:
//!   LITERAL text, never markup;
//! - a link whose destination [`is_allowed_link_scheme`] refuses is not a
//!   link at all: its content stands in its place, inert;
//! - an image is [`Inline::Image`], a placeholder a client shows on the
//!   user's request -- nothing here fetches anything;
//! - the bounds apply AFTER parsing, where a construct's depth and size
//!   are known: block nesting over [`MAX_BLOCK_NESTING`] (a level is one
//!   blockquote or one list, a list and its items counting as one), a
//!   table over [`MAX_TABLE_ROWS`] body rows or [`MAX_TABLE_COLUMNS`]
//!   columns, and input over [`MAX_DECOMPRESSED_BYTES`] -- which is never
//!   parsed -- each give [`Rendered::PlainText`], the source as written.
//!   Never a rejected envelope.
//!
//! Linear in the input: the parser is a single-pass pull parser, and
//! the tree is built in one pass over its events with a stack no deeper
//! than the nesting bound allows before it falls back
//! (`tests/human-chat`'s scaling test).

use pulldown_cmark::{Alignment as CmarkAlignment, Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::decode::MAX_DECOMPRESSED_BYTES;
use crate::envelope::{
    MAX_BLOCK_NESTING, MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, is_allowed_link_scheme,
};

/// What a client renders for an envelope's `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rendered {
    /// Inside the subset and its bounds: the block tree.
    Markdown(Vec<Block>),
    /// Past a bound: the source, displayed as plain text.
    PlainText {
        /// The `text` as received.
        source: String,
        /// Which bound it crossed.
        reason: OverBound,
    },
}

/// The bound a source crossed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OverBound {
    /// Over [`MAX_DECOMPRESSED_BYTES`]: not parsed at all.
    Input,
    /// Block nesting over [`MAX_BLOCK_NESTING`] levels.
    Nesting,
    /// A table over [`MAX_TABLE_ROWS`] body rows.
    TableRows,
    /// A table over [`MAX_TABLE_COLUMNS`] columns.
    TableColumns,
}

/// One block of rendered markdown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    /// A paragraph.
    Paragraph(Vec<Inline>),
    /// A heading, level 1 to 6.
    Heading {
        /// 1 to 6.
        level: u8,
        /// Its content.
        content: Vec<Inline>,
    },
    /// A blockquote: one nesting level.
    Quote(Vec<Block>),
    /// A list: one nesting level, its items included.
    List {
        /// The first number of an ordered list; `None` for a bullet list.
        start: Option<u64>,
        /// Each item's blocks.
        items: Vec<Vec<Block>>,
    },
    /// A code block, shown as written.
    Code {
        /// The fence's info string; empty when indented or bare.
        info: String,
        /// The code.
        text: String,
    },
    /// Raw HTML from the source: LITERAL text, never markup.
    Html(String),
    /// A thematic break.
    Rule,
    /// A table within the bounds.
    Table {
        /// Each column's alignment; its length is the column count.
        columns: Vec<Alignment>,
        /// The header row's cells.
        header: Vec<Vec<Inline>>,
        /// The body rows' cells.
        rows: Vec<Vec<Vec<Inline>>>,
    },
}

/// A table column's alignment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Alignment {
    /// No alignment given.
    None,
    /// `:---`.
    Left,
    /// `:---:`.
    Center,
    /// `---:`.
    Right,
}

/// Inline content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    /// Text.
    Text(String),
    /// A code span.
    Code(String),
    /// `*emphasis*`.
    Emphasis(Vec<Inline>),
    /// `**strong**`.
    Strong(Vec<Inline>),
    /// `~~strikethrough~~` (GFM).
    Strikethrough(Vec<Inline>),
    /// A link whose destination is an allowlisted scheme. Any other link
    /// is not represented: its content stands in its place.
    Link {
        /// The destination, `https:` or `mailto:`.
        destination: String,
        /// The link text.
        content: Vec<Inline>,
    },
    /// An image REFERENCE: a placeholder a client shows on the user's
    /// request, never fetched on receipt.
    Image {
        /// What the source names, any scheme; inert data.
        destination: String,
        /// The alt text.
        alt: Vec<Inline>,
    },
    /// Raw inline HTML: LITERAL text, never markup.
    Html(String),
    /// A soft line break.
    SoftBreak,
    /// A hard line break.
    HardBreak,
}

/// Render an envelope's `text`.
#[must_use]
pub fn render(source: &str) -> Rendered {
    let fallback = |reason| Rendered::PlainText {
        source: source.to_owned(),
        reason,
    };
    if source.len() > MAX_DECOMPRESSED_BYTES {
        return fallback(OverBound::Input);
    }
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH;
    let mut tree = Tree::default();
    for event in Parser::new_ext(source, options) {
        if let Err(reason) = tree.event(event) {
            return fallback(reason);
        }
    }
    Rendered::Markdown(tree.finish())
}

/// A block container's content: its blocks, and the inlines a tight list
/// item carries without a paragraph around them.
#[derive(Default)]
struct Container {
    blocks: Vec<Block>,
    loose: Vec<Inline>,
}

impl Container {
    fn flush(&mut self) {
        if !self.loose.is_empty() {
            self.blocks
                .push(Block::Paragraph(std::mem::take(&mut self.loose)));
        }
    }

    fn into_blocks(mut self) -> Vec<Block> {
        self.flush();
        self.blocks
    }
}

enum Frame {
    Quote(Container),
    List {
        start: Option<u64>,
        items: Vec<Vec<Block>>,
    },
    Item(Container),
    Paragraph(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    Code {
        info: String,
        text: String,
    },
    HtmlBlock(String),
    Table {
        columns: Vec<Alignment>,
        header: Vec<Vec<Inline>>,
        rows: Vec<Vec<Vec<Inline>>>,
    },
    Row(Vec<Vec<Inline>>),
    Cell(Vec<Inline>),
    Emphasis(Vec<Inline>),
    Strong(Vec<Inline>),
    Strikethrough(Vec<Inline>),
    /// A link: kept only if its destination is allowlisted.
    Link {
        destination: Option<String>,
        content: Vec<Inline>,
    },
    Image {
        destination: String,
        alt: Vec<Inline>,
    },
    /// An out-of-subset tag the parser was not asked for and so never
    /// emits: were one to arrive, its content passes into its parent.
    Transparent(Vec<Inline>),
}

#[derive(Default)]
struct Tree {
    root: Container,
    stack: Vec<Frame>,
    /// Open blockquotes and lists: the nesting level.
    depth: usize,
}

impl Tree {
    fn finish(mut self) -> Vec<Block> {
        // A well-formed event stream closes every frame; anything left
        // open is folded into its parent rather than lost.
        while let Some(frame) = self.stack.pop() {
            self.close(frame);
        }
        self.root.into_blocks()
    }

    fn event(&mut self, event: Event<'_>) -> Result<(), OverBound> {
        match event {
            Event::Start(tag) => return self.open(tag),
            Event::End(end) => self.end(end),
            Event::Text(text) => self.text(&text),
            Event::Code(code) => self.inline(Inline::Code(code.to_string())),
            Event::Html(html) => self.text_or(&html, |h| Inline::Html(h.to_owned())),
            Event::InlineHtml(html) => self.inline(Inline::Html(html.to_string())),
            Event::SoftBreak => self.inline(Inline::SoftBreak),
            Event::HardBreak => self.inline(Inline::HardBreak),
            Event::Rule => self.block(Block::Rule),
            // Outside the subset's options, so never emitted; were one to
            // arrive, it is shown as its text.
            Event::FootnoteReference(text) | Event::InlineMath(text) | Event::DisplayMath(text) => {
                self.inline(Inline::Text(text.to_string()));
            }
            Event::TaskListMarker(done) => {
                self.inline(Inline::Text(if done { "[x] " } else { "[ ] " }.to_owned()));
            }
        }
        Ok(())
    }

    fn open(&mut self, tag: Tag<'_>) -> Result<(), OverBound> {
        let frame = match tag {
            Tag::BlockQuote(_) | Tag::List(_) => {
                self.depth += 1;
                if self.depth > MAX_BLOCK_NESTING {
                    return Err(OverBound::Nesting);
                }
                match tag {
                    Tag::List(start) => Frame::List {
                        start,
                        items: Vec::new(),
                    },
                    _ => Frame::Quote(Container::default()),
                }
            }
            Tag::Item => Frame::Item(Container::default()),
            Tag::Paragraph => Frame::Paragraph(Vec::new()),
            Tag::Heading { level, .. } => Frame::Heading(level as u8, Vec::new()),
            Tag::CodeBlock(kind) => Frame::Code {
                info: match kind {
                    pulldown_cmark::CodeBlockKind::Fenced(info) => info.to_string(),
                    pulldown_cmark::CodeBlockKind::Indented => String::new(),
                },
                text: String::new(),
            },
            Tag::HtmlBlock => Frame::HtmlBlock(String::new()),
            Tag::Table(alignments) => {
                if alignments.len() > MAX_TABLE_COLUMNS {
                    return Err(OverBound::TableColumns);
                }
                Frame::Table {
                    columns: alignments.into_iter().map(Alignment::from).collect(),
                    header: Vec::new(),
                    rows: Vec::new(),
                }
            }
            Tag::TableHead => Frame::Row(Vec::new()),
            Tag::TableRow => {
                // The bound is on BODY rows, the header apart: checked as
                // the next row opens, so the 257th is never built.
                if let Some(Frame::Table { rows, .. }) = self.stack.last()
                    && rows.len() >= MAX_TABLE_ROWS
                {
                    return Err(OverBound::TableRows);
                }
                Frame::Row(Vec::new())
            }
            Tag::TableCell => Frame::Cell(Vec::new()),
            Tag::Emphasis => Frame::Emphasis(Vec::new()),
            Tag::Strong => Frame::Strong(Vec::new()),
            Tag::Strikethrough => Frame::Strikethrough(Vec::new()),
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                // An email autolink's destination is the bare address; it
                // is a mailto link in all but spelling.
                let destination = if link_type == LinkType::Email {
                    format!("mailto:{dest_url}")
                } else {
                    dest_url.to_string()
                };
                Frame::Link {
                    destination: is_allowed_link_scheme(&destination).then_some(destination),
                    content: Vec::new(),
                }
            }
            Tag::Image { dest_url, .. } => Frame::Image {
                destination: dest_url.to_string(),
                alt: Vec::new(),
            },
            _ => Frame::Transparent(Vec::new()),
        };
        if let Frame::Table { .. }
        | Frame::Code { .. }
        | Frame::HtmlBlock(_)
        | Frame::Paragraph(_)
        | Frame::Heading(..)
        | Frame::Quote(_)
        | Frame::List { .. } = frame
        {
            self.flush_parent();
        }
        self.stack.push(frame);
        Ok(())
    }

    fn end(&mut self, end: TagEnd) {
        if matches!(end, TagEnd::BlockQuote(_) | TagEnd::List(_)) {
            self.depth = self.depth.saturating_sub(1);
        }
        if let Some(frame) = self.stack.pop() {
            self.close(frame);
        }
    }

    /// A finished frame, added to what holds it.
    fn close(&mut self, frame: Frame) {
        match frame {
            Frame::Quote(container) => self.block(Block::Quote(container.into_blocks())),
            Frame::List { start, items } => self.block(Block::List { start, items }),
            Frame::Item(container) => {
                let blocks = container.into_blocks();
                if let Some(Frame::List { items, .. }) = self.stack.last_mut() {
                    items.push(blocks);
                } else {
                    self.blocks(blocks);
                }
            }
            Frame::Paragraph(content) => self.block(Block::Paragraph(content)),
            Frame::Heading(level, content) => self.block(Block::Heading { level, content }),
            Frame::Code { info, text } => self.block(Block::Code { info, text }),
            Frame::HtmlBlock(html) => self.block(Block::Html(html)),
            Frame::Table {
                columns,
                header,
                rows,
            } => self.block(Block::Table {
                columns,
                header,
                rows,
            }),
            Frame::Row(cells) => match self.stack.last_mut() {
                Some(Frame::Table { header, rows, .. }) => {
                    if header.is_empty() && rows.is_empty() {
                        *header = cells;
                    } else {
                        rows.push(cells);
                    }
                }
                _ => self.inlines(cells.into_iter().flatten().collect()),
            },
            Frame::Cell(content) => match self.stack.last_mut() {
                Some(Frame::Row(cells)) => cells.push(content),
                _ => self.inlines(content),
            },
            Frame::Emphasis(content) => self.inline(Inline::Emphasis(content)),
            Frame::Strong(content) => self.inline(Inline::Strong(content)),
            Frame::Strikethrough(content) => self.inline(Inline::Strikethrough(content)),
            Frame::Link {
                destination: Some(destination),
                content,
            } => self.inline(Inline::Link {
                destination,
                content,
            }),
            Frame::Image { destination, alt } => self.inline(Inline::Image { destination, alt }),
            // Not an allowlisted scheme: the link text, inert, in its
            // place; and an out-of-subset tag's content, likewise.
            Frame::Link {
                destination: None,
                content,
            }
            | Frame::Transparent(content) => self.inlines(content),
        }
    }

    /// Close the loose inlines of the container about to receive a block.
    fn flush_parent(&mut self) {
        match self.stack.last_mut() {
            Some(Frame::Quote(container) | Frame::Item(container)) => container.flush(),
            None => self.root.flush(),
            _ => {}
        }
    }

    fn block(&mut self, block: Block) {
        self.flush_parent();
        match self.stack.last_mut() {
            Some(Frame::Quote(container) | Frame::Item(container)) => container.blocks.push(block),
            None => self.root.blocks.push(block),
            // A block inside an inline container cannot come from the
            // parser; its text is kept rather than the block dropped.
            Some(_) => self.inlines(block_text(block)),
        }
    }

    fn blocks(&mut self, blocks: Vec<Block>) {
        for block in blocks {
            self.block(block);
        }
    }

    fn inline(&mut self, inline: Inline) {
        self.inlines(vec![inline]);
    }

    fn inlines(&mut self, inlines: Vec<Inline>) {
        let target = match self.stack.last_mut() {
            Some(
                Frame::Paragraph(content)
                | Frame::Heading(_, content)
                | Frame::Cell(content)
                | Frame::Emphasis(content)
                | Frame::Strong(content)
                | Frame::Strikethrough(content)
                | Frame::Link { content, .. }
                | Frame::Image { alt: content, .. }
                | Frame::Transparent(content),
            ) => content,
            Some(Frame::Quote(container) | Frame::Item(container)) => &mut container.loose,
            Some(Frame::Code { text, .. } | Frame::HtmlBlock(text)) => {
                for inline in inlines {
                    text.push_str(&inline_text(&inline));
                }
                return;
            }
            Some(Frame::Row(cells)) => {
                cells.push(inlines);
                return;
            }
            Some(Frame::List { items, .. }) => {
                items.push(vec![Block::Paragraph(inlines)]);
                return;
            }
            Some(Frame::Table { rows, .. }) => {
                rows.push(vec![inlines]);
                return;
            }
            None => &mut self.root.loose,
        };
        // Adjacent text is one run: the parser splits text at characters
        // it might have read as syntax, and a client should not have to
        // rejoin what the source wrote as one.
        for inline in inlines {
            match (target.last_mut(), inline) {
                (Some(Inline::Text(run)), Inline::Text(more)) => run.push_str(&more),
                (_, inline) => target.push(inline),
            }
        }
    }

    fn text(&mut self, text: &str) {
        self.text_or(text, |t| Inline::Text(t.to_owned()));
    }

    /// Text into a code or HTML block as written, or else as an inline.
    fn text_or(&mut self, text: &str, inline: impl FnOnce(&str) -> Inline) {
        match self.stack.last_mut() {
            Some(Frame::Code { text: body, .. } | Frame::HtmlBlock(body)) => body.push_str(text),
            _ => self.inline(inline(text)),
        }
    }
}

impl From<CmarkAlignment> for Alignment {
    fn from(alignment: CmarkAlignment) -> Self {
        match alignment {
            CmarkAlignment::None => Self::None,
            CmarkAlignment::Left => Self::Left,
            CmarkAlignment::Center => Self::Center,
            CmarkAlignment::Right => Self::Right,
        }
    }
}

fn inline_text(inline: &Inline) -> String {
    match inline {
        Inline::Text(t) | Inline::Code(t) | Inline::Html(t) => t.clone(),
        Inline::Emphasis(c) | Inline::Strong(c) | Inline::Strikethrough(c) => {
            c.iter().map(inline_text).collect()
        }
        Inline::Link { content, .. } => content.iter().map(inline_text).collect(),
        Inline::Image { alt, .. } => alt.iter().map(inline_text).collect(),
        Inline::SoftBreak | Inline::HardBreak => "\n".to_owned(),
    }
}

fn block_text(block: Block) -> Vec<Inline> {
    match block {
        Block::Paragraph(c) | Block::Heading { content: c, .. } => c,
        Block::Code { text, .. } | Block::Html(text) => vec![Inline::Text(text)],
        Block::Quote(blocks) => blocks.into_iter().flat_map(block_text).collect(),
        Block::List { items, .. } => items.into_iter().flatten().flat_map(block_text).collect(),
        Block::Table { header, rows, .. } => header
            .into_iter()
            .chain(rows.into_iter().flatten())
            .flatten()
            .collect(),
        Block::Rule => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn blocks(source: &str) -> Vec<Block> {
        match render(source) {
            Rendered::Markdown(blocks) => blocks,
            other @ Rendered::PlainText { .. } => panic!("inside the bounds: {other:?}"),
        }
    }

    fn text(t: &str) -> Inline {
        Inline::Text(t.to_owned())
    }

    /// Raw HTML is literal text, block and inline alike: never markup.
    #[test]
    fn raw_html_is_literal_text() {
        assert_eq!(
            blocks("<script>alert(1)</script>\n"),
            [Block::Html("<script>alert(1)</script>\n".to_owned())]
        );
        assert_eq!(
            blocks("a <b>bold</b> word"),
            [Block::Paragraph(vec![
                text("a "),
                Inline::Html("<b>".to_owned()),
                text("bold"),
                Inline::Html("</b>".to_owned()),
                text(" word"),
            ])]
        );
    }

    /// An allowlisted link is a link; any other is its text, inert --
    /// `javascript:`, `file:`, `data:` and a relative reference alike. An
    /// email autolink is a mailto link.
    #[test]
    fn only_an_allowlisted_scheme_is_a_link() {
        assert_eq!(
            blocks("[ok](https://example.invalid/x)"),
            [Block::Paragraph(vec![Inline::Link {
                destination: "https://example.invalid/x".to_owned(),
                content: vec![text("ok")],
            }])]
        );
        for blocked in [
            "javascript:alert(1)",
            "file:///etc/passwd",
            "data:text/html,x",
            "/relative",
        ] {
            assert_eq!(
                blocks(&format!("[click]({blocked})")),
                [Block::Paragraph(vec![text("click")])],
                "{blocked}"
            );
        }
        assert_eq!(
            blocks("<someone@example.invalid>"),
            [Block::Paragraph(vec![Inline::Link {
                destination: "mailto:someone@example.invalid".to_owned(),
                content: vec![text("someone@example.invalid")],
            }])]
        );
    }

    /// An image is a placeholder carrying what the source names: nothing
    /// here fetches it, whatever its scheme.
    #[test]
    fn an_image_is_a_placeholder() {
        assert_eq!(
            blocks("![a cat](https://example.invalid/cat.png)"),
            [Block::Paragraph(vec![Inline::Image {
                destination: "https://example.invalid/cat.png".to_owned(),
                alt: vec![text("a cat")],
            }])]
        );
    }

    /// A tight list item's text, which the parser gives without a
    /// paragraph, is the item's paragraph; strikethrough is GFM's.
    #[test]
    fn a_tight_list_and_strikethrough() {
        assert_eq!(
            blocks("- one\n- ~~two~~\n"),
            [Block::List {
                start: None,
                items: vec![
                    vec![Block::Paragraph(vec![text("one")])],
                    vec![Block::Paragraph(vec![Inline::Strikethrough(vec![text(
                        "two"
                    )])])],
                ],
            }]
        );
    }

    /// A table's header apart from its body rows, its alignments kept.
    #[test]
    fn a_table_keeps_its_header_rows_and_alignments() {
        assert_eq!(
            blocks("| a | b |\n|:--|--:|\n| 1 | 2 |\n"),
            [Block::Table {
                columns: vec![Alignment::Left, Alignment::Right],
                header: vec![vec![text("a")], vec![text("b")]],
                rows: vec![vec![vec![text("1")], vec![text("2")]]],
            }]
        );
    }

    /// GFM beyond the two extensions is literal text: a task list's box
    /// and a footnote reference are their characters.
    #[test]
    fn the_rest_of_gfm_stays_literal() {
        let list = blocks("- [ ] todo\n");
        let [Block::List { items, .. }] = list.as_slice() else {
            panic!("a list");
        };
        assert_eq!(items[0], [Block::Paragraph(vec![text("[ ] todo")])]);
        assert_eq!(blocks("see[^1]"), [Block::Paragraph(vec![text("see[^1]")])]);
    }
}
