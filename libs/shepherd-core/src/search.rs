use std::sync::Arc;

use agent_harness::EntryKind;
use serde::Serialize;

use crate::store::{Store, StoreError};

const FIELDS: [&str; 3] = ["entry_text", "task_title", "project_name"];
const MAX_QUERY_BYTES: usize = 512;
const MAX_TERMS: usize = 12;
const TOKEN_BYTES_EXCLUSIVE: usize = 40;
pub(crate) const RESULT_LIMIT: i64 = 50;

#[derive(Clone)]
pub struct TaskSearch(pub(crate) Arc<dyn Store>);

impl TaskSearch {
    pub async fn search(&self, input: &str) -> Result<Vec<SearchHit>, StoreError> {
        let Some(query) = SearchQuery::parse(input) else {
            return Ok(Vec::new());
        };
        self.0.search_entries(&query).await
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchHit {
    pub project_id: String,
    pub task_id: String,
    pub entry_id: String,
    pub project_name: String,
    pub task_title: String,
    pub snippet: SearchSnippet,
    pub score: f64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchSnippet {
    pub before: String,
    pub matched: String,
    pub after: String,
    pub field: SearchField,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum SearchField {
    Entry,
    Task,
    Project,
}

#[derive(Debug)]
pub struct SearchQuery {
    terms: Vec<String>,
    prefix: bool,
}

impl SearchQuery {
    fn parse(input: &str) -> Option<Self> {
        let input = input.get(..input.floor_char_boundary(MAX_QUERY_BYTES))?;
        let mut words = words_with_offsets(input);
        let terms: Vec<_> = words
            .by_ref()
            .take(MAX_TERMS)
            .map(|(_, word)| word.to_owned())
            .collect();
        let prefix =
            words.next().is_none() && input.chars().next_back().is_some_and(char::is_alphanumeric);
        if terms.is_empty() || terms.iter().any(|term| term.len() >= TOKEN_BYTES_EXCLUSIVE) {
            return None;
        }
        Some(Self { terms, prefix })
    }

    pub(crate) fn fts(&self) -> String {
        let mut clauses = Vec::with_capacity(self.terms.len());
        for (index, word) in self.terms.iter().enumerate() {
            if self.prefix && index + 1 == self.terms.len() {
                // Tantivy 0.26 requires two terms for a phrase-prefix query.
                // A lexical range covers the same completions for a single term.
                let end = prefix_end(word).unwrap_or_else(|| "*".into());
                let fields = FIELDS.map(|field| format!("{field}:[{word} TO {end}}}"));
                clauses.push(format!("({} OR \"{word}\"^2)", fields.join(" OR ")));
            } else {
                clauses.push(format!("\"{word}\""));
            }
        }
        clauses.join(" AND ")
    }

    pub(crate) fn snippet(&self, entry: &str, task: &str, project: &str) -> SearchSnippet {
        let terms: Vec<_> = self.terms.iter().map(|term| lowercase(term)).collect();
        for (field, text) in [
            (SearchField::Entry, entry),
            (SearchField::Task, task),
            (SearchField::Project, project),
        ] {
            if let Some((start, word)) = words_with_offsets(text).find(|(_, word)| {
                terms.iter().enumerate().any(|(index, term)| {
                    matches_term(word, term, self.prefix && index + 1 == terms.len())
                })
            }) {
                return SearchSnippet {
                    before: leading_context(&text[..start]),
                    matched: trailing_context(word),
                    after: trailing_context(&text[start + word.len()..]),
                    field,
                };
            }
        }
        SearchSnippet {
            before: String::new(),
            matched: String::new(),
            after: trailing_context(entry),
            field: SearchField::Entry,
        }
    }
}

fn words_with_offsets(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split_inclusive(|character: char| !character.is_alphanumeric())
        .scan(0, |offset, fragment| {
            let start = *offset;
            *offset += fragment.len();
            Some((
                start,
                fragment.trim_end_matches(|character: char| !character.is_alphanumeric()),
            ))
        })
        .filter(|(_, word)| !word.is_empty())
}

fn lowercase(word: &str) -> String {
    word.chars().flat_map(char::to_lowercase).collect()
}

fn matches_term(word: &str, term: &str, prefix: bool) -> bool {
    let mut folded = word.chars().flat_map(char::to_lowercase);
    if !term
        .chars()
        .all(|character| folded.next() == Some(character))
    {
        return false;
    }
    prefix || folded.next().is_none()
}

fn prefix_end(word: &str) -> Option<String> {
    for (index, character) in word.char_indices().rev() {
        let mut folded = character.to_lowercase();
        let last = folded.next_back()?;
        let next = (u32::from(last) + 1..=0x10ffff)
            .filter_map(char::from_u32)
            .find(|candidate| {
                candidate.is_alphanumeric()
                    && candidate.to_lowercase().eq(std::iter::once(*candidate))
            });
        if let Some(next) = next {
            let upper: String = word[..index]
                .chars()
                .chain(folded)
                .chain(std::iter::once(next))
                .collect();
            if upper.len() < TOKEN_BYTES_EXCLUSIVE {
                return Some(upper);
            }
        }
    }
    None
}

fn leading_context(text: &str) -> String {
    if let Some((start, character)) = text.char_indices().rev().nth(45) {
        format!("…{}", &text[start + character.len_utf8()..])
    } else {
        text.to_owned()
    }
}

fn trailing_context(text: &str) -> String {
    if let Some((end, _)) = text.char_indices().nth(65) {
        format!("{}…", &text[..end])
    } else {
        text.to_owned()
    }
}

pub(crate) fn entry_text(kind: &EntryKind) -> String {
    match kind {
        EntryKind::UserMessage { text }
        | EntryKind::AgentText { text }
        | EntryKind::Thought { text }
        | EntryKind::Notice { text, .. }
        | EntryKind::Failure { text, .. } => text.clone(),
        EntryKind::ToolCall { title, output, .. } => match output {
            Some(output) => format!("{title} {output}"),
            None => title.clone(),
        },
        EntryKind::Permission { title, .. } => title.clone(),
        EntryKind::Plan { rows } => rows
            .iter()
            .map(|row| row.text.as_str())
            .collect::<Vec<_>>()
            .join(" "),
        EntryKind::Usage { .. } => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_snippet_bounds_long_matches_and_preserves_unicode() -> Result<(), &'static str> {
        let query = SearchQuery::parse("écl").ok_or("missing query")?;
        let entry = format!("before ÉCL{} after", "界".repeat(10_000));
        let snippet = query.snippet(&entry, "task", "project");
        assert_eq!(snippet.before, "before ");
        assert_eq!(snippet.matched, format!("ÉCL{}…", "界".repeat(62)));
        assert_eq!(snippet.after, " after");
        Ok(())
    }

    #[test]
    fn case_folded_matching_distinguishes_prefixes_and_full_words() {
        assert!(matches_term("İSTANBUL", "i\u{307}st", true));
        assert!(!matches_term("İSTANBUL", "i\u{307}st", false));
        assert!(matches_term("İSTANBUL", "i\u{307}stanbul", false));
        assert!(!matches_term("İSTANBUL", "istanbul", true));
    }
}
