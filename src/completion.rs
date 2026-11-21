//! Intellisense completion: word-based (always available) + LSP results
//! (rust-analyzer, when the client is up). Items are merged and ranked by
//! prefix match quality.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum CompletionSource {
    /// Identifier scanned from the current buffer.
    Word,
    /// Language server result.
    Lsp,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionItem {
    pub label: String,
    pub detail: Option<String>,
    pub kind: Option<String>,
    pub source: CompletionSource,
}

/// Rank: exact prefix first (shorter label first), then substring, then rest.
fn score(prefix: &str, label: &str) -> i32 {
    if prefix.is_empty() {
        return 0;
    }
    if label.starts_with(prefix) {
        100 - label.len() as i32
    } else if label.to_lowercase().contains(&prefix.to_lowercase()) {
        50
    } else {
        0
    }
}

/// Word-based completions from the buffer text: identifiers of
/// `[A-Za-z0-9_]` matching the prefix, excluding the prefix itself.
pub fn word_completions(text: &str, prefix: &str, max: usize) -> Vec<CompletionItem> {
    if prefix.len() < 2 {
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    let mut items = Vec::new();
    for word in text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_')) {
        if word.len() >= 2
            && word != prefix
            && seen.insert(word.to_string())
            && score(prefix, word) > 0
        {
            items.push(CompletionItem {
                label: word.to_string(),
                detail: None,
                kind: Some("word".into()),
                source: CompletionSource::Word,
            });
        }
        if items.len() >= max * 4 {
            break;
        }
    }
    items.sort_by(|a, b| score(prefix, &b.label).cmp(&score(prefix, &a.label)));
    items.truncate(max);
    items
}

/// Parse an LSP `textDocument/completion` result.
pub fn lsp_completions(result: &serde_json::Value, max: usize) -> Vec<CompletionItem> {
    let items = if result.is_array() {
        result.clone()
    } else {
        result.get("items").cloned().unwrap_or_default()
    };
    let Ok(parsed) = serde_json::from_value::<Vec<lsp_types::CompletionItem>>(items) else {
        return Vec::new();
    };
    parsed
        .into_iter()
        .map(|item| CompletionItem {
            label: item.label,
            detail: item.detail,
            kind: item.kind.map(kind_name),
            source: CompletionSource::Lsp,
        })
        .take(max)
        .collect()
}

/// LSP CompletionItemKind number -> readable name.
fn kind_name(kind: lsp_types::CompletionItemKind) -> String {
    use lsp_types::CompletionItemKind as K;
    let name = match kind {
        K::TEXT => "text",
        K::METHOD => "method",
        K::FUNCTION => "function",
        K::CONSTRUCTOR => "constructor",
        K::FIELD => "field",
        K::VARIABLE => "variable",
        K::CLASS => "class",
        K::INTERFACE => "interface",
        K::MODULE => "module",
        K::PROPERTY => "property",
        K::UNIT => "unit",
        K::VALUE => "value",
        K::ENUM => "enum",
        K::KEYWORD => "keyword",
        K::SNIPPET => "snippet",
        K::COLOR => "color",
        K::FILE => "file",
        K::REFERENCE => "reference",
        K::FOLDER => "folder",
        K::ENUM_MEMBER => "enum member",
        K::CONSTANT => "constant",
        K::STRUCT => "struct",
        K::EVENT => "event",
        K::OPERATOR => "operator",
        K::TYPE_PARAMETER => "type parameter",
        _ => "item",
    };
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_completions_rank_prefix_first() {
        let text = "some_value some_other my_var";
        let items = word_completions(text, "some", 10);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].label, "some_value"); // shorter prefix match first
        assert_eq!(items[0].source, CompletionSource::Word);
    }

    #[test]
    fn lsp_array_and_list_both_parse() {
        let arr = serde_json::json!([
            {"label": "push", "kind": 2, "detail": "fn push(&mut self, v: T)"},
            {"label": "pop", "kind": 2}
        ]);
        let items = lsp_completions(&arr, 10);
        assert_eq!(items[0].label, "push");
        assert_eq!(items[0].kind.as_deref(), Some("method"));

        let list = serde_json::json!({"isIncomplete": false, "items": [{"label": "map"}]});
        let items = lsp_completions(&list, 10);
        assert_eq!(items.len(), 1);
    }
}
