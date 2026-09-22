//! The writing assistant: style prompts, lyrics and score edits for YuE2.
//!
//! YuE2's own language model writes a score and audio codes, not words. MAP's
//! model card leaves the text to a separate model: an agent writes the style
//! and lyrics, finds the words for a cover, and edits the ABC score in
//! response to musical feedback. This module is that agent's contract.
//!
//! Two providers, both optional, because the manual form is the primary way in:
//!
//! * a local OpenAI-compatible server (llama.cpp, LM Studio, Ollama), or a
//!   GGUF the studio runs itself;
//! * OpenRouter, chosen from the live catalogue.

use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The extras every whole-song draft carries.
const EXTRA: &str = "title: a short song title, two to five words, no quotation marks, in the language of the lyrics. cover_prompt: one sentence describing a cover image for this track - a scene, not a poster; no text, no lettering, no logos. duration_seconds: how long this song, as written, runs when sung at its tempo, in seconds, between 30 and 360.";

const VALIDATION: &str = "Before answering, check your own draft: every explicit user constraint kept, an instrumental request still instrumental, vocal gender not contradicted, every section tag alone on its own line, the vocal language named in the style, and no sentence copied from a reference. Fix what fails, then answer.";

/// How YuE2 reads a style prompt, from its checkpoint and the official demo
/// requests: the text reaches the model verbatim under a `[Tags]` header.
const STYLE_CONTRACT: &str = r#"style: the style prompt YuE2 reads verbatim. Write it in English as comma-separated descriptors or one or two dense sentences, 15-70 words, in roughly this order: genre and subgenre; mood and energy arc; the lead vocal (gender, timbre, register, delivery) or "instrumental"; the key instruments; tempo as a BPM or a qualitative pace; the language the vocals are sung in (for example "English", "Mandarin", "Russian vocals"); the production character. Be concrete and musical - name instruments and textures, not adjectives about quality. Keep every explicit user constraint: a required vocal gender, instrument, tempo or exclusion is never reversed. Never put lyric lines, a song title or section instructions in the style."#;

/// The lyric rules. YuE2 sings the words it is given and plans the song's
/// length around them, so structure and density decide the result.
const LYRICS_RULES: &str = r#"lyrics: singable lyrics organised into sections, each introduced by a tag ALONE on its own line: [Intro] [Verse 1] [Pre-Chorus] [Chorus] [Verse 2] [Bridge] [Outro], plus [Instrumental Break] or [Guitar Solo] where an instrumental passage belongs. Put a blank line between sections and never words on the same line as a tag. Size the song to its intended length: about 2 to 3 sung words per second, a verse of 4-8 lines, a chorus repeated where a real song repeats it. Keep neighbouring lines close in length so none is sung rushed. Stage directions, instruments and tempo never belong in the lyrics - they go in the style. For an instrumental, write the same section tags with no words under them. Write the lyrics in the language the user wrote their request in: a Russian idea gets Russian lyrics, a Japanese one Japanese; the style stays English but names that language."#;

const DICTION_RULE: &str = r#"
Diction: the model sings the letters it is given. In Russian write ё as ё rather than е, and mark the stressed vowel with a combining acute - за́мок, замо́к - only where the word would otherwise be read wrong: homographs, rare words and proper names. Never accent every word. In other languages respell only the individual words that come out wrong."#;

const DUET_RULE: &str = r#"
Two voices: name both singers in the style ("male and female duet, warm baritone and airy soprano") and mark the switches in the lyrics with a short tag alone on its own line - [Male Vocals], [Female Vocals], [Both] - switching at section or couplet level, never line by line. A tag of more than two or three words gets sung aloud."#;

const INSTRUMENTAL_RULE: &str = r#"
Instrumental: say "instrumental, no vocals" in the style and name the instrument carrying the lead melody."#;

/// The score YuE2 writes and reads back: ABC notation in the layout of its
/// planning stage and of SheetSage2's transcriptions.
const SCORE_CONTRACT: &str = r#"abc: the complete revised ABC score. Keep the layout the model writes: header lines X:1, T:, M: (metre), L: (unit length), Q: (tempo, 1/4=<bpm>), the voice declarations V: Vocal and V: Ins, and K: (key); then the song as sections, each opened by a comment line such as "% verse" or "% chorus", alternating "V: Vocal" and "V: Ins" blocks. Bars are separated by |; z is a rest and Z a whole-bar rest (Z3 for three bars); chord symbols are quoted before the note they start on ("Am"c4). Chord symbols belong only to a score in full mode: keep them if the score has them, and do not add them to a melody-only score unless asked. Every bar must add up to the metre. Make exactly the change the request asks for - reharmonise, transpose, change the tempo, lengthen or shorten a section, write a solo - and keep the rest of the score note for note. When the melody changes, keep the syllable count of the lyric lines it carries."#;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AssistTarget {
    /// Write the lyrics and the style together.
    All,
    /// Rewrite only the lyrics, coherent with the current style.
    Lyrics,
    /// Rewrite only the style, coherent with the current lyrics.
    Style,
    /// Edit the ABC score as the instruction asks.
    Score,
}

#[derive(Debug, Clone, Deserialize)]
pub struct AssistRequest {
    pub target: AssistTarget,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub instruction: String,
    #[serde(default)]
    pub lyrics: String,
    #[serde(default)]
    pub style: String,
    #[serde(default)]
    pub abc: String,
    #[serde(default = "default_duration")]
    pub duration_seconds: f64,
    #[serde(default)]
    pub instrumental: bool,
}

fn default_duration() -> f64 {
    120.0
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct AssistDraft {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lyrics: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub abc: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cover_prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_seconds: Option<u32>,
}

/// The system prompt and the JSON keys the answer must carry.
pub fn instructions(request: &AssistRequest) -> (String, &'static [&'static str]) {
    let references = references_for(request);
    let notes = craft_notes(request);
    match request.target {
        AssistTarget::Lyrics => (
            format!(
                "You write lyrics for YuE2, a model that turns a style prompt and lyrics into a complete song.\n\
                 Given a lyrics instruction, the current style and a target length, write lyrics coherent with that style.\n\
                 {LYRICS_RULES}{notes}\n\
                 Answer with ONLY a JSON object with key: lyrics."
            ),
            &["lyrics"],
        ),
        AssistTarget::Style => (
            format!(
                "You write the style prompt for YuE2, a model that turns a style prompt and lyrics into a complete song.\n\
                 Given a sound instruction and/or lyrics, write a style that fits them. {STYLE_CONTRACT}{notes}\n\
                 Also write {EXTRA}\n\
                 Answer with ONLY a JSON object with keys: style, title, cover_prompt, duration_seconds.{references}"
            ),
            &["style"],
        ),
        AssistTarget::Score => (
            format!(
                "You are a music editor working on the ABC score YuE2 planned for a song; YuE2 renders whatever score you return.\n\
                 Given the current score, its style and lyrics, and a musical request, revise the score. {SCORE_CONTRACT}\n\
                 If the request also calls for a different style - new instruments, a new genre - write the revised style too, following this: {STYLE_CONTRACT}\n\
                 Answer with ONLY a JSON object with keys: abc, and style only when it should change."
            ),
            &["abc"],
        ),
        AssistTarget::All => (
            format!(
                "You write inputs for YuE2, a model that turns a style prompt and lyrics into a complete song with vocals and accompaniment.\n\
                 Given a song description, produce:\n\
                 1. {LYRICS_RULES}\n\
                 2. {STYLE_CONTRACT}{notes}\n\
                 3-5. {EXTRA}\n\
                 Answer with ONLY a JSON object with keys: lyrics, style, title, cover_prompt, duration_seconds.\n\
                 {VALIDATION}{references}"
            ),
            &["lyrics", "style"],
        ),
    }
}

/// Rules that only apply to some songs arrive only for them: a duet rule told
/// to a solo song would invite a second voice.
fn craft_notes(request: &AssistRequest) -> String {
    let mut notes = String::new();
    if matches!(request.target, AssistTarget::All | AssistTarget::Lyrics) && !request.instrumental {
        notes.push_str(DICTION_RULE);
    }
    if request.instrumental {
        notes.push_str(INSTRUMENTAL_RULE);
    } else if wants_two_voices(request) {
        notes.push_str(DUET_RULE);
    }
    notes
}

fn wants_two_voices(request: &AssistRequest) -> bool {
    const CUES: &[&str] = &[
        "duet", "дуэт", "two voices", "два голоса", "male and female", "female and male",
        "мужской и женский", "женский и мужской", "call and response", "перекличк", "вдвоём", "вдвоем",
    ];
    let brief = format!("{} {} {} {}", request.description, request.instruction, request.style, request.lyrics).to_lowercase();
    CUES.iter().any(|cue| brief.contains(cue))
}

/// The closest official YuE2 requests, as the model was shown them.
fn references_for(request: &AssistRequest) -> String {
    let brief = format!("{} {} {}", request.description, request.instruction, request.style);
    let references = crate::skill::references(&brief);
    if references.is_empty() {
        return String::new();
    }
    let mut block = String::from(
        "\n\nReference requests from the official YuE2 demo, close to this one. Use them for the shape and level of detail of the style and the lyric layout. Do not copy their sentences, instruments or story.\n",
    );
    for (index, reference) in references.iter().enumerate() {
        block.push_str(&format!(
            "\n--- reference {} ---\nstyle: {}\nlyrics:\n{}\n",
            index + 1,
            reference.style.trim(),
            reference.lyrics.trim()
        ));
    }
    block
}

pub fn user_message(request: &AssistRequest) -> String {
    let instruction = request.instruction.trim();
    let description = request.description.trim();
    let brief = if !instruction.is_empty() { instruction } else { description };
    let instrumental = if request.instrumental { "\nThis piece is instrumental: no sung words." } else { "" };

    match request.target {
        AssistTarget::Lyrics => format!(
            "Lyrics instruction: {}\nCurrent style, keep the lyrics coherent with it:\n{}\nTarget length: about {} seconds.{instrumental}",
            if brief.is_empty() { "(none - write lyrics that fit the style)" } else { brief },
            request.style.trim(),
            request.duration_seconds.round() as i64,
        ),
        AssistTarget::Style => format!(
            "Sound instruction: {}\nCurrent lyrics, keep the style coherent with them:\n{}{instrumental}",
            if brief.is_empty() { "(none - describe a sound that fits the lyrics)" } else { brief },
            request.lyrics.trim(),
        ),
        AssistTarget::Score => format!(
            "Request: {}\n\nStyle:\n{}\n\nLyrics:\n{}\n\nCurrent score:\n{}",
            if brief.is_empty() { "(none - tidy the score without changing the music)" } else { brief },
            request.style.trim(),
            request.lyrics.trim(),
            request.abc.trim(),
        ),
        AssistTarget::All => {
            // What the user already wrote is material to build around.
            let mut carried = String::new();
            for (label, value) in [("Lyrics", &request.lyrics), ("Style", &request.style)] {
                let value = value.trim();
                if !value.is_empty() {
                    carried.push_str(&format!("\n{label} (the user wrote this - keep it, build around it):\n{value}"));
                }
            }
            format!(
                "Song description: {}{carried}{instrumental}",
                if brief.is_empty() { "(none - choose something musical and specific)" } else { brief },
            )
        }
    }
}

/// Extracts the answer, tolerating a model that wraps its JSON in prose or a
/// code fence.
pub fn parse_draft(content: &str, required: &[&str]) -> Result<AssistDraft> {
    let start = content.find('{').context("the assistant returned no JSON object")?;
    let end = content.rfind('}').context("the assistant returned no JSON object")?;
    if end <= start {
        bail!("the assistant returned no JSON object");
    }
    let value: Value = serde_json::from_str(&content[start..=end]).with_context(|| {
        let sample: String = content.chars().take(220).collect();
        format!("the assistant returned invalid JSON. It answered: {sample}")
    })?;
    // A string, or very often an array of lines: both are the same text.
    let field = |key: &str| -> Option<String> {
        let text = match value.get(key)? {
            Value::String(text) => text.trim().to_owned(),
            Value::Array(items) => items.iter().filter_map(|item| item.as_str()).collect::<Vec<_>>().join("\n").trim().to_owned(),
            _ => return None,
        };
        (!text.is_empty()).then_some(text)
    };
    for key in required {
        if field(key).is_none() {
            bail!("the assistant answer is missing '{key}'");
        }
    }
    let abc = field("abc");
    if let Some(score) = &abc {
        if !score.contains("K:") || !score.contains('|') {
            bail!("the assistant returned a score that is not ABC notation");
        }
    }
    Ok(AssistDraft {
        lyrics: field("lyrics"),
        style: field("style"),
        abc,
        title: field("title"),
        cover_prompt: field("cover_prompt"),
        duration_seconds: value
            .get("duration_seconds")
            .and_then(|value| value.as_u64().or_else(|| value.as_f64().map(|seconds| seconds.round() as u64)).or_else(|| value.as_str().and_then(|text| text.trim().parse().ok())))
            .map(|seconds| seconds.clamp(10, 360) as u32),
    })
}

/// The answer's shape as a schema the server can enforce: llama-server turns it
/// into a grammar, so a local model cannot answer with prose.
pub fn draft_schema(required: &[&str]) -> Value {
    let long = serde_json::json!({ "type": "string", "minLength": 20 });
    let short = serde_json::json!({ "type": "string", "minLength": 3 });
    serde_json::json!({
        "type": "object",
        "properties": {
            "lyrics": long,
            "style": short,
            "abc": long,
            "title": short,
            "cover_prompt": short,
            "duration_seconds": { "type": "number" },
        },
        "required": required,
        "additionalProperties": false,
    })
}

/// An OpenAI-compatible chat request. Both providers speak this shape, so the
/// only difference between them is the endpoint and the credential.
pub fn chat_body(model: &str, system: &str, user: &str) -> Value {
    chat_body_with_reasoning(model, system, user, None)
}

/// The same body, asking the model to think harder.
///
/// `effort` is OpenRouter's unified reasoning control - "minimal" through
/// "max" - which they translate per provider. A local OpenAI-compatible server
/// has no such parameter, so nothing is sent there and the model decides for
/// itself; its thinking is read back out of `reasoning_content` either way.
pub fn chat_body_with_reasoning(model: &str, system: &str, user: &str, effort: Option<&str>) -> Value {
    chat_body_full(model, system, user, effort, None)
}

/// The request as it goes out, with the model's own sampling when it has any.
///
/// OpenRouter publishes `default_parameters` per model, and 83 of them fill it
/// in. Sending one hardcoded temperature to every model overrides what the
/// model asks for; the studio's own value is only a fallback for models that
/// publish nothing.
pub fn chat_body_full(
    model: &str,
    system: &str,
    user: &str,
    effort: Option<&str>,
    defaults: Option<&Value>,
) -> Value {
    chat_body_constrained(model, system, user, effort, defaults, None)
}

/// The same request, with the answer's shape enforced where the server can do
/// it. Asking politely for JSON in the prompt is a hope; a schema is a rule.
pub fn chat_body_constrained(
    model: &str,
    system: &str,
    user: &str,
    effort: Option<&str>,
    defaults: Option<&Value>,
    schema: Option<Value>,
) -> Value {
    let mut body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "system", "content": system },
            { "role": "user", "content": user },
        ],
        "stream": false,
    });

    let mut published = false;
    if let Some(Value::Object(map)) = defaults {
        for (key, value) in map {
            if value.is_null() {
                continue;
            }
            body[key] = value.clone();
            published = true;
        }
    }
    if !published {
        // Nothing published: a little warmth, because these are lyrics.
        body["temperature"] = Value::from(0.8);
    }
    if let Some(effort) = effort.filter(|value| !value.trim().is_empty() && *value != "off") {
        // The draft is what is wanted, not the thinking: exclude keeps the
        // response small and the parser looking in one place.
        //
        // Only for a model that says it takes this. OpenRouter publishes
        // `supported_parameters` for every model and 182 of the 468 do not
        // list reasoning; sending it to those is asking for something they
        // never offered.
        body["reasoning"] = serde_json::json!({ "effort": effort, "exclude": true });
    }
    if let Some(schema) = schema {
        body["response_format"] = serde_json::json!({ "type": "json_schema", "schema": schema });
    }

    body
}

/// Reads the answer out of a chat completion.
///
/// Reasoning models served by llama.cpp put their visible answer in
/// `content` and their thinking in `reasoning_content` - but with several
/// Gemma builds `content` comes back empty and everything, the JSON draft
/// included, arrives in `reasoning_content`. Reading only `content` there
/// looks exactly like a model that answered nothing.
pub fn content_of(response: &Value) -> Result<String> {
    let message = response
        .pointer("/choices/0/message")
        .context("the assistant response contained no message")?;
    // OpenRouter calls it `reasoning`, llama.cpp `reasoning_content`; both
    // appear when a model answers with its thinking and an empty content.
    for field in ["content", "reasoning_content", "reasoning"] {
        if let Some(text) = message.get(field).and_then(Value::as_str) {
            if !text.trim().is_empty() {
                return Ok(text.to_owned());
            }
        }
    }
    Err(anyhow!("the assistant response contained no message content"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(target: AssistTarget) -> AssistRequest {
        AssistRequest {
            target,
            description: "a night drive synth pop song".into(),
            instruction: String::new(),
            lyrics: "[Verse 1]\nneon".into(),
            style: "synth pop, female vocal, 110 BPM".into(),
            abc: "X:1\nM:4/4\nL:1/16\nK:C\nV: Vocal\nc4d4e4f4|".into(),
            duration_seconds: 90.0,
            instrumental: false,
        }
    }

    #[test]
    fn every_target_declares_the_fields_it_writes() {
        assert_eq!(instructions(&request(AssistTarget::Lyrics)).1, &["lyrics"]);
        assert_eq!(instructions(&request(AssistTarget::Style)).1, &["style"]);
        assert_eq!(instructions(&request(AssistTarget::Score)).1, &["abc"]);
        assert_eq!(instructions(&request(AssistTarget::All)).1, &["lyrics", "style"]);
    }

    #[test]
    fn the_style_contract_reaches_the_request_that_goes_out() {
        let (system, _) = instructions(&request(AssistTarget::All));
        assert!(system.contains("verbatim"));
        assert!(system.contains("[Verse 1]"));
        let body = chat_body("any-model", &system, "idea");
        assert!(body["messages"][0]["content"].as_str().unwrap_or_default().contains("style prompt YuE2 reads"));
    }

    #[test]
    fn a_whole_song_prompt_carries_official_references() {
        let mut metal = request(AssistTarget::All);
        metal.description = "heavy metal song with screamed vocals".into();
        metal.style = String::new();
        let (system, _) = instructions(&metal);
        assert!(system.contains("Reference requests from the official YuE2 demo"));
        assert!(system.len() < 20_000, "the prompt grew to {} characters", system.len());
    }

    #[test]
    fn a_score_edit_sends_the_score_and_asks_for_the_whole_revision() {
        let message = user_message(&request(AssistTarget::Score));
        assert!(message.contains("Current score:"));
        assert!(message.contains("K:C"));
        let (system, _) = instructions(&request(AssistTarget::Score));
        assert!(system.contains("keep the rest of the score note for note"));
    }

    #[test]
    fn a_score_answer_must_be_abc() {
        assert!(parse_draft("{\"abc\": \"just some words about music\"}", &["abc"]).is_err());
        let draft = parse_draft("{\"abc\": \"X:1\\nK:C\\nc4|\"}", &["abc"]).unwrap();
        assert!(draft.abc.unwrap().starts_with("X:1"));
    }

    #[test]
    fn the_other_half_of_the_song_travels_as_context() {
        let lyrics_message = user_message(&request(AssistTarget::Lyrics));
        assert!(lyrics_message.contains("synth pop, female vocal"));
        assert!(lyrics_message.contains("90 seconds"));
        assert!(user_message(&request(AssistTarget::Style)).contains("[Verse 1]"));
    }

    #[test]
    fn the_lyrics_follow_the_language_of_the_request() {
        let mut russian = request(AssistTarget::All);
        russian.description = "панк-рок про ёжика в бункере".into();
        let (system, _) = instructions(&russian);
        assert!(system.contains("language the user wrote their request in"));
        assert!(system.contains("combining acute"));
    }

    #[test]
    fn the_duet_rules_arrive_only_for_two_voices() {
        let mut solo = request(AssistTarget::All);
        solo.style = String::new();
        solo.lyrics = String::new();
        assert!(!instructions(&solo).0.contains("[Male Vocals]"));
        let mut duet = solo.clone();
        duet.description = "дуэт мужского и женского голоса, поп-баллада".into();
        assert!(instructions(&duet).0.contains("[Male Vocals]"));
    }

    #[test]
    fn an_instrumental_is_told_what_carries_the_melody() {
        let mut instrumental = request(AssistTarget::All);
        instrumental.instrumental = true;
        let (system, _) = instructions(&instrumental);
        assert!(system.contains("lead melody"));
        assert!(!system.contains("combining acute"));
        assert!(user_message(&instrumental).contains("instrumental"));
    }

    #[test]
    fn json_survives_a_code_fence_and_lines_as_a_list() {
        let draft = parse_draft("Sure!\n```json\n{\"lyrics\": [\"[Verse 1]\", \"line\"], \"style\": \"pop\"}\n```", &["lyrics", "style"]).unwrap();
        assert_eq!(draft.lyrics.unwrap(), "[Verse 1]\nline");
        assert!(parse_draft("{\"lyrics\": \"x\"}", &["style"]).is_err());
        assert!(parse_draft("no json here", &["lyrics"]).is_err());
    }

    #[test]
    fn an_answer_that_arrives_as_reasoning_is_still_an_answer() {
        let response = serde_json::json!({ "choices": [{ "message": { "content": "", "reasoning_content": "{\"lyrics\": \"[Verse]\"}" } }] });
        assert_eq!(content_of(&response).unwrap(), "{\"lyrics\": \"[Verse]\"}");
        assert!(content_of(&serde_json::json!({ "choices": [{ "message": { "content": "  " } }] })).is_err());
    }

    #[test]
    fn a_model_that_published_nothing_gets_the_studio_s_own_warmth() {
        assert_eq!(chat_body_constrained("m", "s", "u", None, None, None)["temperature"], serde_json::json!(0.8));
        let published = serde_json::json!({ "temperature": 0.6 });
        assert_eq!(chat_body_constrained("m", "s", "u", None, Some(&published), None)["temperature"], serde_json::json!(0.6));
    }

    #[test]
    fn reasoning_is_only_asked_of_models_that_take_it() {
        assert_eq!(chat_body_constrained("m", "s", "u", Some("high"), None, None)["reasoning"], serde_json::json!({ "effort": "high", "exclude": true }));
        assert!(chat_body_constrained("m", "s", "u", Some("off"), None, None).get("reasoning").is_none());
    }

    #[test]
    fn the_schema_asks_for_content_not_just_a_key() {
        let schema = draft_schema(&["abc"]);
        assert_eq!(schema["properties"]["abc"]["minLength"], 20);
        assert_eq!(schema["required"][0], "abc");
    }
}
