use std::{cell::RefCell, rc::Rc};

use html5ever::{local_name, ns, parse_document, serialize, tendril::TendrilSink, QualName};
use markup5ever_rcdom::{Handle, Node, NodeData, RcDom, SerializableHandle};

fn parse(text: &str) -> RcDom {
    parse_document(RcDom::default(), Default::default()).one(text)
}

fn text_nodes(node: &Handle, output: &mut Vec<(Handle, bool)>, plain: bool) {
    let mut plain = plain;
    if let NodeData::Element { name, .. } = &node.data {
        if name.ns != ns!(html)
            || matches!(
                name.local.as_ref(),
                "script" | "style" | "code" | "pre" | "template"
            )
        {
            return;
        }
        plain |= matches!(name.local.as_ref(), "title" | "textarea" | "option");
    }
    if let NodeData::Text { contents } = &node.data {
        if !contents.borrow().trim().is_empty() {
            output.push((node.clone(), plain));
        }
    }
    for child in node.children.borrow().iter() {
        text_nodes(child, output, plain);
    }
}

pub fn translation_texts(text: &str) -> Vec<String> {
    let document = parse(text);
    let mut nodes = Vec::new();
    text_nodes(&document.document, &mut nodes, false);
    nodes
        .iter()
        .map(|(node, _)| match &node.data {
            NodeData::Text { contents } => contents.borrow().to_string(),
            _ => unreachable!(),
        })
        .collect()
}

fn text_node(value: &str) -> Handle {
    Node::new(NodeData::Text {
        contents: RefCell::new(value.into()),
    })
}

fn line_break() -> Handle {
    Node::new(NodeData::Element {
        name: QualName::new(None, ns!(html), local_name!("br")),
        attrs: RefCell::new(Vec::new()),
        template_contents: RefCell::new(None),
        mathml_annotation_xml_integration_point: false,
    })
}

pub fn render(
    text: &str,
    translations: &[String],
    bilingual: bool,
    separator: Option<&str>,
) -> Result<Vec<u8>, std::io::Error> {
    let document = parse(text);
    let mut nodes = Vec::new();
    text_nodes(&document.document, &mut nodes, false);
    for ((node, plain), translated) in nodes.into_iter().zip(translations) {
        let NodeData::Text { contents } = &node.data else {
            continue;
        };
        let source = contents.borrow().to_string();
        if source.trim() == translated.trim() {
            continue;
        }
        if !bilingual || plain {
            *contents.borrow_mut() = if bilingual {
                format!(
                    "{}{}{}",
                    source.trim_end(),
                    separator.unwrap_or(" | "),
                    translated.trim_start()
                )
            } else {
                translated.clone()
            }
            .into();
            continue;
        }
        if let Some(parent) = node.parent.take().and_then(|parent| parent.upgrade()) {
            let mut children = parent.children.borrow_mut();
            if let Some(index) = children.iter().position(|child| Rc::ptr_eq(child, &node)) {
                let mut replacements = vec![text_node(&source)];
                if let Some(separator) = separator {
                    for (index, line) in separator.replace("\r\n", "\n").split('\n').enumerate() {
                        if index > 0 {
                            replacements.push(line_break());
                        }
                        replacements.push(text_node(line));
                    }
                } else {
                    replacements.extend([line_break(), text_node("─────"), line_break()]);
                }
                replacements.push(text_node(translated));
                for child in &replacements {
                    child.parent.set(Some(Rc::downgrade(&parent)));
                }
                children.splice(index..=index, replacements);
            }
        }
    }
    let mut output = Vec::new();
    serialize(
        &mut output,
        &SerializableHandle::from(document.document.clone()),
        Default::default(),
    )?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn custom_separator_is_escaped_and_line_breaks_are_visible() {
        let output = render(
            "<p>Hello</p>",
            &["你好".into()],
            true,
            Some("\n<split>&\n\n"),
        )
        .unwrap();
        let output = String::from_utf8(output).unwrap();
        assert!(output.contains("<p>Hello<br>&lt;split&gt;&amp;<br><br>你好</p>"));
        let output = render("<p>Hello</p>", &["你好".into()], true, Some("")).unwrap();
        assert!(String::from_utf8(output)
            .unwrap()
            .contains("<p>Hello你好</p>"));
    }

    #[test]
    fn html_preserves_markup_scripts_attributes_and_decodes_entities() {
        let source = "<!doctype html><html><head><style>p { color: red }</style><script>if (a < b) alert('hello')</script></head><body><p title='hello'>Hello &amp; world</p><pre>keep code</pre><table><tr><td>Value</td></tr></table></body></html>";
        assert_eq!(translation_texts(source), ["Hello & world", "Value"]);
        let result = String::from_utf8(
            render(source, &["你好 & 世界".into(), "数值".into()], true, None).unwrap(),
        )
        .unwrap();
        assert!(
            result.contains("<p title=\"hello\">Hello &amp; world<br>─────<br>你好 &amp; 世界</p>")
        );
        assert!(result.contains("<td>Value<br>─────<br>数值</td>"));
        assert!(result.contains("<script>if (a < b) alert('hello')</script>"));
        assert!(result.contains("<pre>keep code</pre>"));
    }

    #[test]
    fn raw_text_elements_use_text_separators_and_ai_markup_is_escaped() {
        let source = "<title>Hello</title><p>Hello</p><textarea>Hello</textarea>";
        let result = String::from_utf8(
            render(
                source,
                &["你好".into(), "<bad>你好".into(), "你好".into()],
                true,
                None,
            )
            .unwrap(),
        )
        .unwrap();
        assert!(result.contains("<title>Hello | 你好</title>"));
        assert!(result.contains("&lt;bad&gt;你好"));
        assert!(result.contains("<textarea>Hello | 你好</textarea>"));
    }
}
