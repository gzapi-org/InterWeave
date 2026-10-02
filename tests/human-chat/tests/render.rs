// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrea Benetton
//! The markdown subset as a rendering contract (`HUMAN-CHAT.md` §Text is
//! markdown, plan §17 (3)), through `chat-protocol`'s render model:
//! - the fixture cases the spec names;
//! - each bound on both sides of its edge;
//! - GFM 0.29's own examples for the two admitted extensions.

#![allow(clippy::expect_used, clippy::panic)]

use interweave_human_chat_protocol::{
    Alignment, Block, HumanChatV2, Inline, MAX_BLOCK_NESTING, MAX_INLINE_NESTING,
    MAX_TABLE_COLUMNS, MAX_TABLE_ROWS, OverBound, Rendered, render,
};

fn blocks(source: &str) -> Vec<Block> {
    match render(source) {
        Rendered::Markdown(blocks) => blocks,
        other @ Rendered::PlainText { .. } => panic!("inside the bounds: {other:?}"),
    }
}

fn plain(source: &str) -> OverBound {
    match render(source) {
        Rendered::PlainText {
            source: kept,
            reason,
        } => {
            assert_eq!(kept, source, "the source is shown as written");
            reason
        }
        Rendered::Markdown(_) => panic!("past a bound, plain text"),
    }
}

fn text(t: &str) -> Inline {
    Inline::Text(t.to_owned())
}

/// `levels` nested containers alternating blockquote and list, so both
/// count: each list adds ONE level, its item adding none.
fn nested(levels: usize) -> String {
    let mut prefix = String::new();
    for level in 0..levels {
        prefix.push_str(if level % 2 == 0 { "> " } else { "- " });
    }
    format!("{prefix}deep\n")
}

fn table(columns: usize, rows: usize) -> String {
    let row = |cell: &str| format!("|{}\n", format!(" {cell} |").repeat(columns));
    let delimiter = format!("|{}\n", " --- |".repeat(columns));
    let body = row("c").repeat(rows);
    [row("h"), delimiter, body].concat()
}

/// The depth of the deepest blockquote/list chain in `blocks`.
fn depth(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .map(|block| match block {
            Block::Quote(inner) => 1 + depth(inner),
            Block::List { items, .. } => 1 + items.iter().map(|i| depth(i)).max().unwrap_or(0),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

// --- HUMAN-CHAT.md §Fixture cases -------------------------------------

#[test]
fn raw_html_is_displayed_literally() {
    assert_eq!(
        blocks("<img src=x onerror=alert(1)>\n"),
        [Block::Html("<img src=x onerror=alert(1)>\n".to_owned())]
    );
}

#[test]
fn a_javascript_link_is_inert() {
    assert_eq!(
        blocks("[press](javascript:alert(1))"),
        [Block::Paragraph(vec![text("press")])]
    );
}

#[test]
fn a_remote_image_is_a_placeholder_and_never_fetched() {
    // Nothing in the model can fetch: it carries the reference, and a
    // client shows a placeholder until the user asks.
    assert_eq!(
        blocks("![chart](https://example.invalid/chart.png)"),
        [Block::Paragraph(vec![Inline::Image {
            destination: "https://example.invalid/chart.png".to_owned(),
            alt: vec![text("chart")],
        }])]
    );
}

/// Past a bound the TEXT falls back; the ENVELOPE is not rejected.
#[test]
fn nesting_17_inline_17_and_a_33_column_table_fall_back_without_rejecting_the_envelope() {
    for source in [nested(17), strong(17), images(17), table(33, 1)] {
        let envelope = serde_json::json!({
            "v": 2, "kind": "text", "app_message_id": "0".repeat(32), "text": source,
        })
        .to_string();
        let parsed = HumanChatV2::parse(&envelope).expect("the envelope is valid");
        assert!(matches!(render(&parsed.text), Rendered::PlainText { .. }));
    }
}

// --- the bounds, each on both sides of its edge -----------------------

#[test]
fn sixteen_levels_render_and_seventeen_fall_back() {
    assert_eq!(MAX_BLOCK_NESTING, 16);
    assert_eq!(depth(&blocks(&nested(16))), 16, "16 levels, all kept");
    assert_eq!(plain(&nested(17)), OverBound::Nesting);
    // A list and its items are ONE level: 16 nested lists with many
    // items each are still 16 levels.
    let lists: String = (0..16)
        .flat_map(|level| {
            let indent = "  ".repeat(level);
            [
                indent.clone(),
                "- item\n".to_owned(),
                indent,
                "- item\n".to_owned(),
            ]
        })
        .collect();
    assert_eq!(depth(&blocks(&lists)), 16);
}

#[test]
fn thirty_two_columns_render_and_thirty_three_fall_back() {
    assert_eq!(MAX_TABLE_COLUMNS, 32);
    let wide = blocks(&table(32, 1));
    let [Block::Table { columns, .. }] = wide.as_slice() else {
        panic!("a table");
    };
    assert_eq!(columns.len(), 32);
    assert_eq!(plain(&table(33, 1)), OverBound::TableColumns);
}

#[test]
fn two_hundred_fifty_six_body_rows_render_and_257_fall_back() {
    assert_eq!(MAX_TABLE_ROWS, 256);
    let long = blocks(&table(2, 256));
    let [Block::Table { rows, .. }] = long.as_slice() else {
        panic!("a table");
    };
    assert_eq!(rows.len(), 256, "the header apart");
    assert_eq!(plain(&table(2, 257)), OverBound::TableRows);
}

#[test]
fn input_past_the_decoded_ceiling_is_never_parsed() {
    let ceiling = interweave_human_chat_protocol::MAX_DECOMPRESSED_BYTES;
    assert!(matches!(
        render(&"a".repeat(ceiling)),
        Rendered::Markdown(_)
    ));
    assert_eq!(plain(&"a".repeat(ceiling + 1)), OverBound::Input);
}

/// Strong nested `levels` deep: `**` twice per level, each closing run
/// matching its opener, so the parser nests one level per four bytes.
fn strong(levels: usize) -> String {
    format!("{}a{}", "**".repeat(levels), "**".repeat(levels))
}

/// Images nested `levels` deep, alt inside alt.
fn images(levels: usize) -> String {
    format!("{}a{}", "![".repeat(levels), "](x)".repeat(levels))
}

/// The depth of the deepest inline chain in `inlines`.
fn inline_depth(inlines: &[Inline]) -> usize {
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::Emphasis(inner) | Inline::Strong(inner) | Inline::Strikethrough(inner) => {
                1 + inline_depth(inner)
            }
            Inline::Link { content, .. } => 1 + inline_depth(content),
            Inline::Image { alt, .. } => 1 + inline_depth(alt),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

#[test]
fn sixteen_inline_levels_render_and_seventeen_fall_back() {
    assert_eq!(MAX_INLINE_NESTING, 16);
    for (shape, build) in [
        ("strong", strong as fn(usize) -> String),
        ("images", images),
    ] {
        let kept = blocks(&build(16));
        let [Block::Paragraph(inlines)] = kept.as_slice() else {
            panic!("{shape}: one paragraph: {kept:?}");
        };
        assert_eq!(inline_depth(inlines), 16, "{shape}: all 16 levels kept");
        assert_eq!(plain(&build(17)), OverBound::InlineNesting, "{shape}");
    }
    // Counted SEPARATELY from block nesting: 16 blockquotes holding 16
    // nested strongs is inside both bounds.
    let both = format!("{}{}", "> ".repeat(16), strong(16));
    assert_eq!(depth(&blocks(&both)), 16, "16 block levels and 16 inline");
}

/// A remote source nesting inline once per few bytes, up to the decoded
/// ceiling, must not abort the client: the result is rendered, cloned,
/// compared and dropped -- each recursive over the tree -- on a thread
/// with a 2 MiB stack, the size of a tokio worker's. Unbounded, the
/// strong shape built a tree 49,000 levels deep and dropping it aborted
/// the process with a stack overflow.
#[test]
fn ceiling_sized_inline_nesting_falls_back_on_a_small_stack() {
    let ceiling = interweave_human_chat_protocol::MAX_DECOMPRESSED_BYTES;
    for source in [strong((ceiling - 1) / 4), images((ceiling - 1) / 6)] {
        assert!(source.len() <= ceiling, "parsed, not refused as input");
        let survived = std::thread::Builder::new()
            .stack_size(2 << 20)
            .spawn(move || {
                let rendered = render(&source);
                let copy = rendered.clone();
                assert_eq!(copy, rendered);
                matches!(
                    rendered,
                    Rendered::PlainText {
                        reason: OverBound::InlineNesting,
                        ..
                    }
                )
            })
            .expect("spawn")
            .join()
            .expect("no panic");
        assert!(survived, "falls back as inline nesting");
    }
}

// --- GFM 0.29: the table and strikethrough examples -------------------

/// A table's columns, header cells and body rows.
type TableParts = (Vec<Alignment>, Vec<Vec<Inline>>, Vec<Vec<Vec<Inline>>>);

fn one_table(source: &str) -> TableParts {
    match blocks(source).as_slice() {
        [
            Block::Table {
                columns,
                header,
                rows,
            },
            ..,
        ] => (columns.clone(), header.clone(), rows.clone()),
        other => panic!("a table first: {other:?}"),
    }
}

fn cells(row: &[Vec<Inline>]) -> Vec<String> {
    row.iter()
        .map(|cell| {
            cell.iter()
                .map(|inline| match inline {
                    Inline::Text(t) | Inline::Code(t) => t.clone(),
                    Inline::Strong(inner) => inner
                        .iter()
                        .map(|i| match i {
                            Inline::Text(t) => t.clone(),
                            other => format!("{other:?}"),
                        })
                        .collect(),
                    other => format!("{other:?}"),
                })
                .collect()
        })
        .collect()
}

#[test]
fn gfm_example_198_a_table() {
    let (columns, header, rows) = one_table("| foo | bar |\n| --- | --- |\n| baz | bim |");
    assert_eq!(columns, [Alignment::None, Alignment::None]);
    assert_eq!(cells(&header), ["foo", "bar"]);
    assert_eq!(rows.len(), 1);
    assert_eq!(cells(&rows[0]), ["baz", "bim"]);
}

#[test]
fn gfm_example_199_alignment_and_unpiped_rows() {
    let (columns, header, rows) = one_table("| abc | defghi |\n:-: | -----------:\nbar | baz");
    assert_eq!(columns, [Alignment::Center, Alignment::Right]);
    assert_eq!(cells(&header), ["abc", "defghi"]);
    assert_eq!(cells(&rows[0]), ["bar", "baz"]);
}

#[test]
fn gfm_example_200_escaped_pipes_stay_in_their_cell() {
    let (_, header, rows) = one_table("| f\\|oo  |\n| ------ |\n| b `\\|` az |\n| b **\\|** im |");
    assert_eq!(cells(&header), ["f|oo"]);
    assert_eq!(cells(&rows[0]), ["b | az"]);
    assert_eq!(cells(&rows[1]), ["b | im"]);
}

#[test]
fn gfm_example_201_a_blockquote_ends_the_table() {
    let rendered = blocks("| abc | def |\n| --- | --- |\n| bar | baz |\n> bar");
    let [Block::Table { rows, .. }, Block::Quote(_)] = rendered.as_slice() else {
        panic!("a table, then a blockquote: {rendered:?}");
    };
    assert_eq!(rows.len(), 1);
}

#[test]
fn gfm_example_202_a_blank_line_ends_the_table() {
    let rendered = blocks("| abc | def |\n| --- | --- |\n| bar | baz |\nbar\n\nbar");
    let [Block::Table { rows, .. }, Block::Paragraph(after)] = rendered.as_slice() else {
        panic!("a table, then a paragraph: {rendered:?}");
    };
    assert_eq!(rows.len(), 2, "`bar` is the table's second row");
    assert_eq!(cells(&rows[1]), ["bar", ""]);
    assert_eq!(after, &[text("bar")]);
}

#[test]
fn gfm_example_203_a_mismatched_delimiter_row_is_no_table() {
    let rendered = blocks("| abc | def |\n| --- |\n| bar |");
    assert!(
        !rendered.iter().any(|b| matches!(b, Block::Table { .. })),
        "{rendered:?}"
    );
}

#[test]
fn gfm_example_204_short_rows_pad_and_long_rows_truncate() {
    let (_, _, rows) = one_table("| abc | def |\n| --- | --- |\n| bar |\n| bar | baz | boo |");
    assert_eq!(cells(&rows[0]), ["bar", ""]);
    assert_eq!(cells(&rows[1]), ["bar", "baz"]);
}

#[test]
fn gfm_example_205_a_header_alone_is_a_table() {
    let (_, header, rows) = one_table("| abc | def |\n| --- | --- |");
    assert_eq!(cells(&header), ["abc", "def"]);
    assert!(rows.is_empty());
}

#[test]
fn gfm_example_491_strikethrough() {
    assert_eq!(
        blocks("~~Hi~~ Hello, world!"),
        [Block::Paragraph(vec![
            Inline::Strikethrough(vec![text("Hi")]),
            text(" Hello, world!"),
        ])]
    );
}

#[test]
fn gfm_example_492_strikethrough_does_not_span_paragraphs() {
    let rendered = blocks("This ~~has a\n\nnew paragraph~~.");
    assert_eq!(rendered.len(), 2);
    assert!(
        !format!("{rendered:?}").contains("Strikethrough"),
        "{rendered:?}"
    );
}
