//! Markdown → blocks for the preview pane and stickies (spec 00:
//! pulldown-cmark, GFM). Rendering lives here too, so the overlay preview
//! and sticky bodies share one pipeline.
//!
//! Phase 1: headings, paragraphs, quotes, list items, code fences, rules,
//! and inline bold/italic/code/strikethrough/link as highlight spans over
//! the block text (byte ranges — the `StyledText` contract). Tables stay
//! unenabled for now (their syntax shows as plain text). Local images
//! render via `img()` resolved against the note's directory (AGENTS.md
//! fork rule 5); remote images show a placeholder until an opt-in network
//! round.

use std::ops::{Range, RangeInclusive};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui::{
    div, hsla, hsla_to_rgba, img, prelude::*, px, relative, AnyElement, Div, FontStyle, FontWeight,
    HighlightStyle, Hsla, StrikethroughStyle, StyledText, UnderlineStyle,
};
use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

use crate::config::Fonts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Inline {
    Bold,
    Italic,
    Code,
    Strike,
    Link,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Heading(u8),
    Paragraph,
    Quote,
    Item,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Styled {
        kind: BlockKind,
        text: String,
        spans: Vec<(Range<usize>, Inline)>,
    },
    Code {
        code: String,
    },
    Image {
        url: String,
        alt: String,
    },
    Rule,
}

/// Parse markdown into flat renderable blocks.
pub fn parse(src: &str) -> Vec<Block> {
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);

    let mut blocks = Vec::new();
    let mut kind: Option<BlockKind> = None;
    let mut text = String::new();
    let mut spans: Vec<(Range<usize>, Inline)> = Vec::new();
    let mut bold = false;
    let mut italic = false;
    let mut strike = false;
    let mut link = false;
    let mut in_code = false;
    let mut code = String::new();
    // Active `![alt](url)` — alt-text events land in `img_alt`.
    let mut img_url: Option<String> = None;
    let mut img_alt = String::new();

    fn current_kind(bold: bool, italic: bool, strike: bool, link: bool) -> Option<Inline> {
        if link {
            Some(Inline::Link)
        } else if bold {
            Some(Inline::Bold)
        } else if italic {
            Some(Inline::Italic)
        } else if strike {
            Some(Inline::Strike)
        } else {
            None
        }
    }

    for ev in Parser::new_ext(src, opts) {
        match ev {
            Event::Start(Tag::Heading { level, .. }) => {
                flush(&mut kind, &mut text, &mut spans, &mut blocks);
                let l = match level {
                    pulldown_cmark::HeadingLevel::H1 => 1,
                    pulldown_cmark::HeadingLevel::H2 => 2,
                    pulldown_cmark::HeadingLevel::H3 => 3,
                    pulldown_cmark::HeadingLevel::H4 => 4,
                    pulldown_cmark::HeadingLevel::H5 => 5,
                    pulldown_cmark::HeadingLevel::H6 => 6,
                };
                kind = Some(BlockKind::Heading(l));
            }
            Event::End(TagEnd::Heading(_)) => flush(&mut kind, &mut text, &mut spans, &mut blocks),
            Event::Start(Tag::Paragraph) => {
                flush(&mut kind, &mut text, &mut spans, &mut blocks);
                kind = Some(BlockKind::Paragraph);
            }
            Event::End(TagEnd::Paragraph) => flush(&mut kind, &mut text, &mut spans, &mut blocks),
            Event::Start(Tag::BlockQuote(_)) => {
                flush(&mut kind, &mut text, &mut spans, &mut blocks);
                kind = Some(BlockKind::Quote);
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush(&mut kind, &mut text, &mut spans, &mut blocks)
            }
            Event::Start(Tag::Item) => {
                flush(&mut kind, &mut text, &mut spans, &mut blocks);
                kind = Some(BlockKind::Item);
            }
            Event::End(TagEnd::Item) => flush(&mut kind, &mut text, &mut spans, &mut blocks),
            Event::Start(Tag::CodeBlock(_)) => {
                flush(&mut kind, &mut text, &mut spans, &mut blocks);
                in_code = true;
                code.clear();
            }
            Event::End(TagEnd::CodeBlock) => {
                if in_code {
                    blocks.push(Block::Code {
                        code: std::mem::take(&mut code),
                    });
                    in_code = false;
                }
            }
            Event::Start(Tag::Strong) => bold = true,
            Event::End(TagEnd::Strong) => bold = false,
            Event::Start(Tag::Emphasis) => italic = true,
            Event::End(TagEnd::Emphasis) => italic = false,
            Event::Start(Tag::Strikethrough) => strike = true,
            Event::End(TagEnd::Strikethrough) => strike = false,
            Event::Start(Tag::Link { .. }) => link = true,
            Event::End(TagEnd::Link) => link = false,
            Event::Start(Tag::Image { dest_url, .. }) => {
                // Inline image: split the current run but KEEP the block
                // kind — text after the image belongs to the same block
                // (flush() takes the kind, which dropped the tail here).
                if let Some(k) = &kind {
                    if !text.trim().is_empty() {
                        trim_run(&mut text, &mut spans);
                        blocks.push(Block::Styled {
                            kind: *k,
                            text: std::mem::take(&mut text),
                            spans: std::mem::take(&mut spans),
                        });
                    } else {
                        text.clear();
                        spans.clear();
                    }
                }
                img_url = Some(dest_url.to_string());
                img_alt.clear();
            }
            Event::End(TagEnd::Image) => {
                if let Some(url) = img_url.take() {
                    blocks.push(Block::Image {
                        url,
                        alt: std::mem::take(&mut img_alt),
                    });
                }
            }
            Event::Code(t) => {
                if in_code {
                    code.push_str(&t);
                    continue;
                }
                let start = text.len();
                text.push_str(&t);
                spans.push((start..text.len(), Inline::Code));
            }
            Event::Text(t) => {
                if in_code {
                    code.push_str(&t);
                    continue;
                }
                if img_url.is_some() {
                    img_alt.push_str(&t);
                    continue;
                }
                match current_kind(bold, italic, strike, link) {
                    Some(k) => {
                        let start = text.len();
                        text.push_str(&t);
                        spans.push((start..text.len(), k));
                    }
                    None => text.push_str(&t),
                }
            }
            Event::SoftBreak | Event::HardBreak => {
                if in_code {
                    continue;
                }
                if img_url.is_some() {
                    img_alt.push(' ');
                    continue;
                }
                text.push(' ');
            }
            Event::Rule => {
                flush(&mut kind, &mut text, &mut spans, &mut blocks);
                blocks.push(Block::Rule);
            }
            _ => {}
        }
    }
    flush(&mut kind, &mut text, &mut spans, &mut blocks);
    blocks
}

fn flush(
    kind: &mut Option<BlockKind>,
    text: &mut String,
    spans: &mut Vec<(Range<usize>, Inline)>,
    blocks: &mut Vec<Block>,
) {
    if let Some(k) = kind.take() {
        if !text.trim().is_empty() {
            trim_run(text, spans);
            blocks.push(Block::Styled {
                kind: k,
                text: std::mem::take(text),
                spans: std::mem::take(spans),
            });
        }
    }
    text.clear();
    spans.clear();
}

/// Trim leading/trailing whitespace from a run; span ranges shift to stay
/// aligned with the surviving text.
fn trim_run(text: &mut String, spans: &mut Vec<(Range<usize>, Inline)>) {
    let lead = text.len() - text.trim_start().len();
    if lead > 0 {
        text.drain(..lead);
        for (r, _) in spans.iter_mut() {
            r.start = r.start.saturating_sub(lead);
            r.end = r.end.saturating_sub(lead);
        }
    }
    trim_tail(text);
    spans.retain(|(r, _)| r.start < r.end && r.end <= text.len());
}

/// Drop trailing whitespace; spans stay valid (they only shrink from above).
fn trim_tail(text: &mut String) {
    while text.ends_with(char::is_whitespace) {
        text.pop();
    }
}

// ----- rendering ---------------------------------------------------------

/// Render parsed blocks as a gap-separated column. Callers own the outer
/// container (padding, scrolling); `base` is the directory relative image
/// URLs resolve against (the note's directory).
/// One markdown block as an unwired div. `selected` tints it — the
/// block-level text selection paints through this flag, and the pane owners
/// attach their own mouse listeners (see selection.rs).
pub fn render_block_div(block: &Block, base: &Path, fonts: &Fonts, selected: bool) -> Div {
    let d = render_block(block, base, fonts);
    if selected {
        d.bg(hsla(220.0, 0.45, 0.55, 0.16)).rounded(px(4.0))
    } else {
        d
    }
}

/// Plain-text form of a block range for the clipboard: styled blocks give
/// their text, code the code, images their markdown; rules contribute
/// nothing.
pub fn copy_range(blocks: &[Block], range: RangeInclusive<usize>) -> String {
    let mut parts: Vec<String> = Vec::new();
    for b in blocks.iter().take(*range.end() + 1).skip(*range.start()) {
        match b {
            Block::Styled { text, .. } => parts.push(text.clone()),
            Block::Code { code } => parts.push(code.clone()),
            Block::Image { url, alt } => parts.push(format!("![{alt}]({url})")),
            Block::Rule => {}
        }
    }
    parts.join("\n\n")
}

/// Pick black or white for text on `bg` by the WCAG contrast ratio computed
/// on real sRGB luminance — what CSS `contrast-color()`'s naive rule gets
/// wrong on mid-tones. Call with an OPAQUE background: translucent colors
/// composite over unknown pixels, so no static choice is reliable there.
pub fn contrast_text(bg: Hsla) -> Hsla {
    let rgba = hsla_to_rgba(bg);
    let lin = |c: f32| {
        if c <= 0.04045 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    let lum = 0.2126 * lin(rgba.red) + 0.7152 * lin(rgba.green) + 0.0722 * lin(rgba.blue);
    // ratio(white) = 1.05/(L+.05) vs ratio(black) = (L+.05)/.05
    if 1.05 / (lum + 0.05) >= (lum + 0.05) / 0.05 {
        hsla(0.0, 0.0, 1.0, 1.0)
    } else {
        hsla(0.0, 0.0, 0.0, 1.0)
    }
}

fn render_block(block: &Block, base: &Path, fonts: &Fonts) -> Div {
    match block {
        Block::Rule => div().h(px(1.0)).w_full().bg(hsla(0.0, 0.0, 1.0, 0.12)),
        Block::Code { code } => div()
            .font_family(fonts.monospace.clone())
            .text_size(px(11.5))
            .text_color(hsla(0.0, 0.0, 0.85, 0.9))
            .bg(hsla(0.0, 0.0, 1.0, 0.05))
            .rounded(px(6.0))
            .px(px(10.0))
            .py(px(8.0))
            .child(code.clone()),
        Block::Image { url, alt } => div().child(render_image(url, alt, base)),
        Block::Styled { kind, text, spans } => {
            let family = match kind {
                BlockKind::Heading(_) => fonts.heading.clone(),
                _ => fonts.body.clone(),
            };
            let line = div()
                .font_family(family)
                .text_color(TEXT)
                .child(styled_line(text, spans, fonts));
            let block = match kind {
                BlockKind::Heading(level) => {
                    let size = match level {
                        1 => 20.0,
                        2 => 17.0,
                        3 => 15.0,
                        _ => 13.5,
                    };
                    line.text_size(px(size)).font_weight(FontWeight::BOLD)
                }
                BlockKind::Paragraph => line.text_size(px(13.0)).line_height(relative(1.5)),
                BlockKind::Quote => line
                    .text_size(px(13.0))
                    .line_height(relative(1.5))
                    .border_l_2()
                    .border_color(hsla(220.0, 0.5, 0.55, 0.8))
                    .pl(px(10.0))
                    .text_color(hsla(0.0, 0.0, 0.85, 0.7)),
                BlockKind::Item => line.text_size(px(13.0)),
            };
            if *kind == BlockKind::Item {
                div()
                    .flex()
                    .gap_2()
                    .child(
                        div()
                            .text_size(px(13.0))
                            .text_color(hsla(220.0, 0.5, 0.65, 0.9))
                            .child("•"),
                    )
                    .child(block)
            } else {
                block
            }
        }
    }
}

/// Images resolve against the note's directory; absolute paths pass
/// through. Remote images don't fetch (v0.1 stays offline-clean) and a
/// missing file renders a placeholder instead of a broken pane.
fn render_image(url: &str, alt: &str, base: &Path) -> AnyElement {
    if url.starts_with("http://") || url.starts_with("https://") {
        return note(&format!("external image: {url}"));
    }
    let p = Path::new(url);
    let abs: PathBuf = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    if !abs.is_file() {
        return note(&format!("image missing: {url}"));
    }
    let mut col = div().flex().flex_col().gap_1().child(
        img(Arc::<Path>::from(abs.as_path()))
            .max_w_full()
            .max_h(px(320.0))
            .rounded(px(6.0)),
    );
    if !alt.trim().is_empty() {
        col = col.child(note(alt));
    }
    col.into_any_element()
}

fn note(text: &str) -> AnyElement {
    div()
        .text_size(px(11.0))
        .text_color(hsla(0.0, 0.0, 0.75, 0.45))
        .child(text.to_string())
        .into_any_element()
}

/// One block's text with inline highlight spans applied; inline code spans
/// get the configured monospace family via font-family overrides (the
/// surrounding div keeps the body/heading family).
/// Base note text color. Markdown output is rendered onto dark translucent
/// panels in both panes, so it carries its own readable color instead of
/// inheriting (gpui's default is near-black — the black sticky-body bug).
pub const TEXT: Hsla = hsla(220.0, 0.15, 0.90, 0.95);

fn styled_line(text: &str, spans: &[(Range<usize>, Inline)], fonts: &Fonts) -> StyledText {
    let line = StyledText::new(text.to_string());
    if spans.is_empty() {
        return line;
    }
    line.with_highlights(
        spans
            .iter()
            .map(|(range, inline)| (range.clone(), highlight(*inline))),
    )
    .with_font_family_overrides(
        spans
            .iter()
            .filter(|(_, inline)| *inline == Inline::Code)
            .map(|(range, _)| (range.clone(), fonts.monospace.clone().into())),
    )
}

fn highlight(inline: Inline) -> HighlightStyle {
    let none = HighlightStyle {
        color: None,
        font_weight: None,
        font_style: None,
        background_color: None,
        underline: None,
        strikethrough: None,
        fade_out: None,
    };
    match inline {
        Inline::Bold => HighlightStyle {
            font_weight: Some(FontWeight::BOLD),
            ..none
        },
        Inline::Italic => HighlightStyle {
            font_style: Some(FontStyle::Italic),
            ..none
        },
        Inline::Code => HighlightStyle {
            background_color: Some(hsla(0.0, 0.0, 1.0, 0.10)),
            ..none
        },
        Inline::Strike => HighlightStyle {
            strikethrough: Some(StrikethroughStyle {
                thickness: px(1.0),
                color: None,
            }),
            ..none
        },
        Inline::Link => HighlightStyle {
            color: Some(hsla(215.0, 0.6, 0.7, 1.0)),
            underline: Some(UnderlineStyle {
                thickness: px(1.0),
                color: None,
                wavy: false,
            }),
            ..none
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn styled(b: &Block) -> (&BlockKind, &str, &[(Range<usize>, Inline)]) {
        let Block::Styled { kind, text, spans } = b else {
            panic!("expected styled block, got {b:?}");
        };
        (kind, text, spans)
    }

    #[test]
    fn headings_and_paragraphs_split() {
        let blocks = parse("# Title\n\nbody text\n");
        assert_eq!(blocks.len(), 2);
        let (kind, text, spans) = styled(&blocks[0]);
        assert_eq!(kind, &BlockKind::Heading(1));
        assert_eq!(text, "Title");
        assert!(spans.is_empty());
        let (kind, text, _) = styled(&blocks[1]);
        assert_eq!(kind, &BlockKind::Paragraph);
        assert_eq!(text, "body text");
    }

    #[test]
    fn inline_marks_use_byte_ranges() {
        let blocks = parse("see **héllo** and `code`\n");
        let (_, text, spans) = styled(&blocks[0]);
        assert_eq!(text, "see héllo and code");
        assert_eq!(spans.len(), 2);
        assert_eq!(&text[spans[0].0.clone()], "héllo");
        assert_eq!(spans[0].1, Inline::Bold);
        assert_eq!(&text[spans[1].0.clone()], "code");
        assert_eq!(spans[1].1, Inline::Code);
    }

    #[test]
    fn code_fences_rules_and_images() {
        let src = "# T\n\n```rust\nlet x = 1;\n```\n\n---\n\n![alt text](x.png)\n";
        let blocks = parse(src);
        assert_eq!(blocks.len(), 4);
        assert!(matches!(&blocks[1], Block::Code { code } if code == "let x = 1;\n"));
        assert!(matches!(blocks[2], Block::Rule));
        assert_eq!(
            &blocks[3],
            &Block::Image {
                url: "x.png".into(),
                alt: "alt text".into(),
            }
        );
    }

    #[test]
    fn paragraph_text_and_image_split_into_separate_blocks() {
        let blocks = parse("before ![a](a.png) after\n");
        assert_eq!(blocks.len(), 3);
        assert_eq!(styled(&blocks[0]).1, "before");
        assert!(matches!(&blocks[1], Block::Image { .. }));
        assert_eq!(styled(&blocks[2]).1, "after");
    }
}
