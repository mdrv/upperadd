//! Markdown → block model for the preview pane (spec 00: pulldown-cmark,
//! GFM). Phase 1: headings, paragraphs, quotes, list items, code fences,
//! rules, and inline bold/italic/code/strikethrough/link as highlight
//! spans over the block text (byte ranges — `StyledText` contract).
//! Tables stay unenabled for now (their syntax shows as plain text);
//! images are skipped entirely until `img()` support lands.

use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag, TagEnd};

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
    let mut skip = 0usize; // inside Image alt text etc.

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
            Event::Start(Tag::Image { .. }) => skip += 1,
            Event::End(TagEnd::Image) => skip = skip.saturating_sub(1),
            Event::Code(t) => {
                if skip > 0 || in_code {
                    if in_code {
                        code.push_str(&t);
                    }
                    continue;
                }
                let start = text.len();
                text.push_str(&t);
                spans.push((start..text.len(), Inline::Code));
            }
            Event::Text(t) => {
                if skip > 0 {
                    continue;
                }
                if in_code {
                    code.push_str(&t);
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
                if skip == 0 && !in_code {
                    text.push(' ');
                }
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
    fn code_fences_rules_and_image_skip() {
        let src = "# T\n\n```rust\nlet x = 1;\n```\n\n---\n\n![alt text](x.png)\n";
        let blocks = parse(src);
        // The image contributes nothing (alt text skipped, no block).
        assert_eq!(blocks.len(), 3);
        assert!(matches!(&blocks[1], Block::Code { code } if code == "let x = 1;\n"));
        assert!(matches!(blocks[2], Block::Rule));
    }
}
