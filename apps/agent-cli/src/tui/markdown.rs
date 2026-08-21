//! Markdown to ratatui spans — pulldown-cmark parsing + syntect code highlighting.

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use ratatui::prelude::*;
use std::sync::OnceLock;
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;

static SYNTAX_SET: OnceLock<SyntaxSet> = OnceLock::new();
static THEME_SET: OnceLock<ThemeSet> = OnceLock::new();

fn ss() -> &'static SyntaxSet {
    SYNTAX_SET.get_or_init(SyntaxSet::load_defaults_newlines)
}

fn ts() -> &'static ThemeSet {
    THEME_SET.get_or_init(ThemeSet::load_defaults)
}

/// Convert markdown text to ratatui Lines with syntax highlighting for code blocks.
pub fn render_markdown(text: &str, width: u16) -> Vec<Line<'static>> {
    let max_width = width as usize;
    let mut opts = Options::empty();
    opts.insert(Options::ENABLE_STRIKETHROUGH);
    opts.insert(Options::ENABLE_TABLES);

    let parser = Parser::new_ext(text, opts);
    let mut lines: Vec<Line<'static>> = Vec::new();
    let mut current_spans: Vec<Span<'static>> = Vec::new();

    let mut in_code_block = false;
    let mut code_lang = String::new();
    let mut code_buffer = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut heading_level: u8 = 0;

    for event in parser {
        match event {
            Event::Start(Tag::Heading { level, .. }) => {
                flush_line(&mut lines, &mut current_spans);
                heading_level = level as u8;
            }
            Event::End(TagEnd::Heading(_)) => {
                let text: String = current_spans
                    .iter()
                    .map(|s| s.content.to_string())
                    .collect();
                current_spans.clear();
                let prefix = "#".repeat(heading_level as usize);
                current_spans.push(Span::styled(
                    format!("{prefix} {text}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ));
                flush_line(&mut lines, &mut current_spans);
            }

            Event::Start(Tag::CodeBlock(kind)) => {
                flush_line(&mut lines, &mut current_spans);
                in_code_block = true;
                code_buffer.clear();
                code_lang = match kind {
                    CodeBlockKind::Fenced(lang) => lang.to_string(),
                    CodeBlockKind::Indented => String::new(),
                };
                lines.push(Line::from(Span::styled(
                    format!(
                        "  ┌─ {}",
                        if code_lang.is_empty() {
                            "code"
                        } else {
                            &code_lang
                        }
                    ),
                    Style::default().fg(Color::DarkGray),
                )));
            }
            Event::End(TagEnd::CodeBlock) => {
                let highlighted = highlight_code(&code_buffer, &code_lang);
                for hl_line in highlighted {
                    lines.push(hl_line);
                }
                lines.push(Line::from(Span::styled(
                    "  └─",
                    Style::default().fg(Color::DarkGray),
                )));
                in_code_block = false;
                code_buffer.clear();
            }

            Event::Start(Tag::Emphasis) => {
                italic = true;
            }
            Event::End(TagEnd::Emphasis) => {
                italic = false;
            }
            Event::Start(Tag::Strong) => {
                bold = true;
            }
            Event::End(TagEnd::Strong) => {
                bold = false;
            }

            Event::Start(Tag::List(_)) => {
                flush_line(&mut lines, &mut current_spans);
            }
            Event::Start(Tag::Item) => {
                current_spans.push(Span::styled("  • ", Style::default().fg(Color::DarkGray)));
            }
            Event::End(TagEnd::Item) => {
                flush_line(&mut lines, &mut current_spans);
            }

            Event::Start(Tag::BlockQuote(_)) => {
                flush_line(&mut lines, &mut current_spans);
                current_spans.push(Span::styled("  │ ", Style::default().fg(Color::Green)));
            }
            Event::End(TagEnd::BlockQuote(_)) => {
                flush_line(&mut lines, &mut current_spans);
            }

            Event::Start(Tag::Paragraph) => {}
            Event::End(TagEnd::Paragraph) => {
                flush_line(&mut lines, &mut current_spans);
                lines.push(Line::default());
            }

            Event::Text(text) => {
                if in_code_block {
                    code_buffer.push_str(&text);
                } else {
                    let mut style = Style::default();
                    if bold {
                        style = style.add_modifier(Modifier::BOLD);
                    }
                    if italic {
                        style = style.add_modifier(Modifier::ITALIC);
                    }
                    current_spans.push(Span::styled(text.to_string(), style));
                }
            }

            Event::Code(code) => {
                current_spans.push(Span::styled(
                    format!("`{code}`"),
                    Style::default().fg(Color::Cyan),
                ));
            }

            Event::SoftBreak | Event::HardBreak => {
                flush_line(&mut lines, &mut current_spans);
            }

            _ => {}
        }
    }

    flush_line(&mut lines, &mut current_spans);

    // Wrap lines that exceed terminal width
    if max_width > 0 {
        let mut wrapped = Vec::with_capacity(lines.len());
        for line in lines {
            let line_width: usize = line.spans.iter().map(|s| s.content.len()).sum();
            if line_width <= max_width {
                wrapped.push(line);
            } else {
                // Split long line into multiple lines
                let mut current_width = 0usize;
                let mut current_spans: Vec<Span<'static>> = Vec::new();
                for span in line.spans {
                    let span_len = span.content.len();
                    if current_width + span_len <= max_width {
                        current_width += span_len;
                        current_spans.push(span);
                    } else {
                        // Need to split this span. The byte index may land inside
                        // a multi-byte char, so snap it to a char boundary first
                        // (split_at would panic otherwise).
                        let remaining = max_width.saturating_sub(current_width);
                        if remaining > 0 {
                            let text = span.content.to_string();
                            let (first, rest) = text.split_at(text.floor_char_boundary(remaining.min(text.len())));
                            if !first.is_empty() {
                                current_spans.push(Span::styled(first.to_string(), span.style));
                            }
                            wrapped.push(Line::from(std::mem::take(&mut current_spans)));
                            // Continue with rest of span
                            let mut leftover = rest.to_string();
                            while leftover.len() > max_width {
                                let mut boundary = leftover.floor_char_boundary(max_width);
                                if boundary == 0 {
                                    // The first char is wider than the budget:
                                    // emit it alone so the loop makes progress.
                                    boundary = leftover
                                        .chars()
                                        .next()
                                        .map_or(0, |ch| ch.len_utf8());
                                }
                                let (chunk, rem) = leftover.split_at(boundary);
                                wrapped
                                    .push(Line::from(Span::styled(chunk.to_string(), span.style)));
                                leftover = rem.to_string();
                            }
                            if !leftover.is_empty() {
                                current_spans.push(Span::styled(leftover, span.style));
                                current_width = current_spans.iter().map(|s| s.content.len()).sum();
                            } else {
                                current_width = 0;
                            }
                        } else {
                            wrapped.push(Line::from(std::mem::take(&mut current_spans)));
                            current_spans.push(span);
                            current_width = span_len;
                        }
                    }
                }
                if !current_spans.is_empty() {
                    wrapped.push(Line::from(current_spans));
                }
            }
        }
        wrapped
    } else {
        lines
    }
}

fn flush_line(lines: &mut Vec<Line<'static>>, spans: &mut Vec<Span<'static>>) {
    if !spans.is_empty() {
        lines.push(Line::from(std::mem::take(spans)));
    }
}

fn highlight_code(code: &str, lang: &str) -> Vec<Line<'static>> {
    let syntax = ss()
        .find_syntax_by_token(lang)
        .unwrap_or_else(|| ss().find_syntax_plain_text());

    let theme = &ts().themes["base16-ocean.dark"];
    let mut h = HighlightLines::new(syntax, theme);
    let mut result = Vec::new();

    for line in code.lines() {
        let ranges = h.highlight_line(line, ss()).unwrap_or_default();
        let mut spans: Vec<Span<'static>> = Vec::new();
        spans.push(Span::styled("  │ ", Style::default().fg(Color::DarkGray)));

        for (style, text) in ranges {
            let fg = Color::Rgb(style.foreground.r, style.foreground.g, style.foreground.b);
            spans.push(Span::styled(text.to_string(), Style::default().fg(fg)));
        }

        result.push(Line::from(spans));
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The rendered text of a line, its spans joined in order.
    fn line_text(line: &Line<'static>) -> String {
        line.spans.iter().map(|s| s.content.to_string()).collect()
    }

    /// Non-empty rendered lines plus the concatenation of everything, which
    /// must always reconstruct the original text exactly.
    fn render_chunks(text: &str, width: u16) -> (Vec<String>, String) {
        let lines = render_markdown(text, width);
        let all: Vec<String> = lines.iter().map(line_text).collect();
        let non_empty: Vec<String> = all.iter().filter(|s| !s.is_empty()).cloned().collect();
        let joined = all.concat();
        (non_empty, joined)
    }

    #[test]
    fn wraps_without_splitting_multibyte_chars() {
        // The em-dash straddles the byte cutoff; this used to panic in split_at.
        let (chunks, joined) = render_chunks("aaaaaa—bbbbbb", 7);
        assert_eq!(chunks, vec!["aaaaaa", "—bbbb", "bb"]);
        assert_eq!(joined, "aaaaaa—bbbbbb");
        for chunk in &chunks {
            assert!(chunk.len() <= 7);
        }
    }

    #[test]
    fn wraps_when_first_char_is_wider_than_width() {
        // Must terminate and never lose characters, even when the first char
        // alone is wider than the whole row.
        let (chunks, joined) = render_chunks("—a", 1);
        assert_eq!(chunks, vec!["—", "a"]);
        assert_eq!(joined, "—a");
    }

    #[test]
    fn wraps_ascii_normally() {
        let (chunks, joined) = render_chunks("hello world", 5);
        assert_eq!(chunks, vec!["hello", " worl", "d"]);
        assert_eq!(joined, "hello world");
        for chunk in &chunks {
            assert!(chunk.len() <= 5);
        }
    }
}
