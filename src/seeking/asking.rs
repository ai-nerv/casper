//! The question a search puts to a model, and what it accepts back.
//!
//! One question per search, not one per batch: the shortlist ([`super::scoring`]) is already the
//! expensive decision, and is sized to fit one request.

use super::fragment::Fragment;
use serde_json::{Value, json};

/// What the model is asked to do. It scores; it does not choose, summarise or explain — a passage
/// that scores well is shown as it stands, so there is nothing for it to write.
const INSTRUCTION: &str = "\
You are ranking passages of source code against a question about what a repository does.

Score each passage from 0 to 10 for how well it answers the question:

  10  this passage is where the thing asked about is implemented or decided
   7  it is directly involved — a caller, a definition it depends on, its tests
   4  it is about the same subject but does not answer the question
   0  it shares words with the question and nothing else

Judge what the code does, not whether it repeats the question's words. A passage using different
names for the same thing still scores well; a passage full of the question's words that does
something unrelated scores 0.

Answer with JSON only: {\"scores\": [{\"at\": \"src/thing.rs:120-158\", \"score\": 8}, ...]}, one
entry per passage, `at` copied exactly as it is written above the passage, and nothing else.";

/// How long the model may take, what its answer may cost, and how much of one passage it is shown.
const PATIENCE: u64 = 45_000;
const MOST_TOKENS: u64 = 2_000;
const OF_EACH: usize = 2_400;

/// The question, as the arguments of a `helper` wonder.
#[must_use]
pub fn question(query: &str, shortlist: &[&Fragment]) -> Value {
    let mut input = format!("Question: {query}\n\nPassages:\n");
    for fragment in shortlist {
        let text: String = fragment.text.chars().take(OF_EACH).collect();
        input.push_str(&format!("\n{}\n{text}\n", fragment.at()));
    }
    json!({
        "role": "search",
        // A search is worth the session's own model where nobody configured a smaller one for it:
        // the alternative is a tool that silently stops being semantic.
        "fallback": "main",
        "instruction": INSTRUCTION,
        "input": input,
        "schema": schema(),
        // Scoring is reading, not reasoning: measured against a repository, a budget here bought
        // nothing a plain read of the passages did not already give.
        "thinking": "off",
        "max_tokens": MOST_TOKENS,
        "timeout_ms": PATIENCE,
    })
}

fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "scores": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "at": { "type": "string" },
                        "score": { "type": "integer", "minimum": 0, "maximum": 10 },
                    },
                    "required": ["at", "score"],
                },
            },
        },
        "required": ["scores"],
    })
}

/// What a harness answered: the model's text, or why there is none.
pub enum Answer {
    Said(String),
    Refused(String),
}

/// Read the JSON magi resumes the call with. Anything unrecognisable is a refusal rather than a
/// panic: the tool carries on with what it worked out itself.
#[must_use]
pub fn answered(text: &str) -> Answer {
    let Ok(said) = serde_json::from_str::<Value>(text) else {
        return Answer::Refused("the harness answered something unreadable".to_owned());
    };
    if let Some(because) = said.get("refused").and_then(Value::as_str) {
        return Answer::Refused(because.to_owned());
    }
    match said.pointer("/told/text").and_then(Value::as_str) {
        Some(text) => Answer::Said(text.to_owned()),
        None => Answer::Refused("no model answered".to_owned()),
    }
}

/// One passage a model scored, named the way it was shown.
#[derive(Debug, Clone, PartialEq)]
pub struct Scored {
    pub path: String,
    pub from: usize,
    pub to: usize,
    pub score: f64,
}

/// The scores a model wrote, however it wrapped them. A label it mangled is dropped rather than
/// guessed at: the label is a path and a line range, and a wrong one reads back a wrong passage.
#[must_use]
pub fn scores(said: &str) -> Vec<Scored> {
    let Some(found) = json_in(said) else {
        return Vec::new();
    };
    let Some(rows) = found.get("scores").and_then(Value::as_array) else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|row| {
            let (path, from, to) = at(row.get("at").and_then(Value::as_str)?)?;
            let score = row
                .get("score")
                .and_then(|score| score.as_f64().or_else(|| score.as_str()?.parse().ok()))?;
            Some(Scored {
                path,
                from,
                to,
                score: score.clamp(0.0, 10.0),
            })
        })
        .collect()
}

/// `src/thing.rs:120-158`, read back. Split from the right, because a path may hold a colon.
#[must_use]
pub fn at(label: &str) -> Option<(String, usize, usize)> {
    let (path, span) = label.trim().rsplit_once(':')?;
    let (from, to) = span.split_once('-')?;
    let (from, to) = (from.trim().parse().ok()?, to.trim().parse().ok()?);
    (!path.is_empty() && from >= 1 && to >= from).then(|| (path.to_owned(), from, to))
}

/// The JSON object in what a model wrote, fences and chatter around it or not.
fn json_in(text: &str) -> Option<Value> {
    let (start, end) = (text.find('{')?, text.rfind('}')?);
    serde_json::from_str(text.get(start..=end)?).ok()
}
