use std::ops::Range;

use pulldown_cmark::{Event, Options, Parser, Tag};

/// Keep complete top-level blocks together, including nested lists and tables.
/// Byte ranges let us retain all untouched whitespace, definitions and code.
pub fn translation_blocks(text: &str) -> Vec<Range<usize>> {
    let mut blocks = Vec::new();
    let mut depth = 0;
    let mut current = None;
    for (event, range) in Parser::new_ext(
        text,
        Options::ENABLE_TABLES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_YAML_STYLE_METADATA_BLOCKS
            | Options::ENABLE_PLUSES_DELIMITED_METADATA_BLOCKS,
    )
    .into_offset_iter()
    {
        match event {
            Event::Start(tag) => {
                if depth == 0
                    && !matches!(
                        tag,
                        Tag::CodeBlock(_) | Tag::HtmlBlock | Tag::MetadataBlock(_)
                    )
                {
                    current = Some(range.clone());
                }
                depth += 1;
            }
            Event::End(_) => {
                depth -= 1;
                if depth == 0 {
                    if let Some(mut block) = current.take() {
                        block.end = block.end.max(range.end);
                        // Leave terminal line breaks outside the replacement.
                        block.end =
                            block.start + text[block.clone()].trim_end_matches(['\r', '\n']).len();
                        blocks.push(block);
                    }
                }
            }
            _ => {}
        }
    }
    blocks
}

pub fn render(
    text: &str,
    blocks: &[Range<usize>],
    translations: &[String],
    bilingual: bool,
) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut output = String::new();
    let mut cursor = 0;
    for (block, translated) in blocks.iter().zip(translations) {
        output.push_str(&text[cursor..block.start]);
        let source = &text[block.clone()];
        if bilingual && source.trim() != translated.trim() {
            output.push_str(source);
            output.push_str(&format!("{newline}{newline}---{newline}{newline}"));
        }
        output.push_str(translated.trim_end_matches(['\r', '\n']));
        // Ensure a following block cannot become part of a list/table/heading.
        if bilingual && source.trim() != translated.trim() {
            output.push_str(newline);
        }
        cursor = block.end;
    }
    output.push_str(&text[cursor..]);
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_blocks_keep_multiline_paragraphs_lists_quotes_and_tables_whole() {
        let text = "# Title\n\nFirst line\nsecond line.\n\n- First\n  - Nested\n- Second\n\n> Quote\n> continues\n\n| Name | Value |\n| --- | --- |\n| Hello | World |\n\n```rust\nlet code = 1;\n```\n\n[ref]: https://example.com\n";
        let blocks = translation_blocks(text);
        let values: Vec<_> = blocks.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(
            values,
            [
                "# Title",
                "First line\nsecond line.",
                "- First\n  - Nested\n- Second",
                "> Quote\n> continues",
                "| Name | Value |\n| --- | --- |\n| Hello | World |"
            ]
        );
        assert_eq!(
            render(
                text,
                &blocks,
                &values
                    .iter()
                    .map(|value| value.to_string())
                    .collect::<Vec<_>>(),
                false
            ),
            text
        );
    }

    #[test]
    fn bilingual_tables_and_lists_remain_separate_valid_blocks() {
        let text = "- Hello\n- World\n\n| Name |\n| --- |\n| Hello |\n\n~~~\ncode\n~~~\n";
        let blocks = translation_blocks(text);
        let output = render(
            text,
            &blocks,
            &[
                "- 你好\n- 世界".into(),
                "| 名称 |\n| --- |\n| 你好 |".into(),
            ],
            true,
        );
        assert!(output.contains("- World\n\n---\n\n- 你好\n- 世界\n\n"));
        assert!(output.contains("| Hello |\n\n---\n\n| 名称 |"));
        assert!(output.ends_with("~~~\ncode\n~~~\n"));
        let events: Vec<_> = Parser::new_ext(&output, Options::ENABLE_TABLES).collect();
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Start(Tag::Table(_))))
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, Event::Start(Tag::List(_))))
                .count(),
            2
        );
    }

    #[test]
    fn metadata_code_reference_definitions_and_crlf_are_preserved() {
        let text = "---\r\ntitle: Hello\r\n---\r\n\r\nHello\r\n\r\n    code here\r\n\r\n[ref]: https://example.com\r\n";
        let blocks = translation_blocks(text);
        assert_eq!(blocks.len(), 1);
        assert_eq!(&text[blocks[0].clone()], "Hello");
        let output = render(text, &blocks, &["你好".into()], true);
        assert!(output.starts_with("---\r\ntitle: Hello\r\n---\r\n"));
        assert!(output.contains("Hello\r\n\r\n---\r\n\r\n你好"));
        assert!(output.ends_with("    code here\r\n\r\n[ref]: https://example.com\r\n"));
    }
}
