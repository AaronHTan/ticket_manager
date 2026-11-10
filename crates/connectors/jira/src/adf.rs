//! Minimal conversion between plain text and Atlassian Document Format (ADF),
//! which Jira REST v3 uses for descriptions and comments.

use serde_json::{Value, json};

/// Extracts readable text from an ADF document. Plain strings (REST v2
/// payloads) are returned as they are.
#[must_use]
pub fn adf_to_text(node: &Value) -> String {
    if let Value::String(s) = node {
        return s.clone();
    }
    let mut out = String::new();
    walk(node, &mut out);
    out.trim().to_owned()
}

fn walk(node: &Value, out: &mut String) {
    let kind = node.get("type").and_then(Value::as_str);
    match kind {
        Some("text") => {
            if let Some(text) = node.get("text").and_then(Value::as_str) {
                out.push_str(text);
            }
        }
        Some("hardBreak") => out.push('\n'),
        _ => {}
    }
    if let Some(children) = node.get("content").and_then(Value::as_array) {
        for child in children {
            walk(child, out);
        }
    }
    if matches!(
        kind,
        Some("paragraph" | "heading" | "listItem" | "codeBlock" | "blockquote")
    ) {
        out.push('\n');
    }
}

/// Wraps plain text in an ADF document, one paragraph per non-empty line.
#[must_use]
pub fn text_to_adf(text: &str) -> Value {
    let paragraphs: Vec<Value> = text
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| json!({ "type": "paragraph", "content": [{ "type": "text", "text": line }] }))
        .collect();
    json!({ "type": "doc", "version": 1, "content": paragraphs })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_text_from_nested_adf() {
        let doc = json!({
            "type": "doc", "version": 1,
            "content": [
                { "type": "paragraph", "content": [
                    { "type": "text", "text": "Login fails" },
                    { "type": "hardBreak" },
                    { "type": "text", "text": "since 09:00" }
                ]},
                { "type": "bulletList", "content": [
                    { "type": "listItem", "content": [
                        { "type": "paragraph", "content": [{ "type": "text", "text": "EU only" }] }
                    ]}
                ]}
            ]
        });
        assert_eq!(adf_to_text(&doc), "Login fails\nsince 09:00\nEU only");
        assert_eq!(adf_to_text(&json!("plain v2 text")), "plain v2 text");
    }

    #[test]
    fn text_round_trips_through_adf() {
        let adf = text_to_adf("first line\n\nsecond line");
        assert_eq!(adf["content"].as_array().unwrap().len(), 2);
        assert_eq!(adf_to_text(&adf), "first line\nsecond line");
    }
}
