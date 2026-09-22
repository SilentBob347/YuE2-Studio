//! Reference requests for the writing assistant.
//!
//! The official YuE2 demo cases - the style prompts and lyrics MAP published
//! with the model, including the covers - are carried inside the binary, the
//! same files the request form offers as examples. For a brief, the closest
//! ones by shared words are put in front of the text model, so it writes in
//! the shape YuE2 was shown rather than guessing from an abstract description.

use include_dir::{include_dir, Dir};
use serde_json::Value;

static EXAMPLES: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/../../app/examples");

/// Two references keep the prompt within what a small local model reads
/// carefully; each is a style and the opening of its lyrics.
const MAX_REFERENCES: usize = 2;
const LYRICS_EXCERPT_CHARS: usize = 700;

#[derive(Debug, Clone)]
pub struct Reference {
    pub title: String,
    pub style: String,
    pub lyrics: String,
}

fn examples() -> Vec<Reference> {
    EXAMPLES
        .files()
        .filter(|file| file.path().extension().is_some_and(|extension| extension == "json"))
        .filter_map(|file| serde_json::from_slice::<Value>(file.contents()).ok())
        .filter_map(|value| {
            let style = value.get("style")?.as_str()?.trim().to_owned();
            let lyrics = value.get("lyrics").and_then(Value::as_str).unwrap_or_default().trim().to_owned();
            // Covers carry a transcribed melody instead of a written song;
            // their lyrics are real, their style prompts are useful references.
            (!style.is_empty()).then(|| Reference {
                title: value.get("title").and_then(Value::as_str).unwrap_or_default().to_owned(),
                style,
                lyrics,
            })
        })
        .collect()
}

fn words(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric() && c != '&' && c != '-')
        .filter(|word| word.chars().count() > 2)
        .map(str::to_owned)
        .collect()
}

fn score(reference: &Reference, brief_words: &[String]) -> usize {
    let haystack = words(&format!("{} {}", reference.title, reference.style));
    brief_words.iter().filter(|word| haystack.contains(word)).map(|word| word.chars().count()).sum()
}

/// The closest official requests to a brief, most similar first. An empty
/// result is normal: the contract alone still describes the shape.
pub fn references(brief: &str) -> Vec<Reference> {
    let brief_words = words(brief);
    if brief_words.is_empty() {
        return Vec::new();
    }
    let mut scored: Vec<(usize, Reference)> = examples()
        .into_iter()
        .map(|reference| (score(&reference, &brief_words), reference))
        .filter(|(score, _)| *score > 0)
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.title.cmp(&b.1.title)));
    scored
        .into_iter()
        .take(MAX_REFERENCES)
        .map(|(_, mut reference)| {
            if reference.lyrics.chars().count() > LYRICS_EXCERPT_CHARS {
                reference.lyrics = reference.lyrics.chars().take(LYRICS_EXCERPT_CHARS).collect::<String>() + "\n...";
            }
            reference
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_official_examples_are_carried_whole() {
        let all = examples();
        assert!(all.len() >= 100, "only {} examples are embedded", all.len());
        assert!(all.iter().all(|reference| !reference.style.is_empty()));
    }

    #[test]
    fn a_brief_finds_requests_of_its_own_genre() {
        let found = references("heavy metal cover with screamed vocals");
        assert!(!found.is_empty());
        assert!(found.len() <= MAX_REFERENCES);
        assert!(found[0].style.to_lowercase().contains("metal"), "{}", found[0].style);
    }

    #[test]
    fn a_brief_with_no_shared_words_finds_nothing_rather_than_noise() {
        assert!(references("zzqx").is_empty());
        assert!(references("").is_empty());
    }
}
