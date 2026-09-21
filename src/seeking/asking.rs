//! The question a search puts to a model, and what it accepts back.
//!
//! One question per search, not one per batch: the shortlist ([`super::scoring`]) is already the
//! expensive decision, and is sized to fit one request.

use super::fragment::Fragment;
use serde_json::{Value, json};

/// The rungs a passage is scored against, lowest first; the index is the score. Four named rungs
/// rather than a nought-to-ten line, because a model that decides is asked which rung a thing is on.
const RUNGS: [&str; 4] = [
    "shares words with the question and nothing else",
    "about the same subject, but does not answer the question",
    "directly involved: a caller, a definition it depends on, its tests",
    "this is where the thing asked about is implemented or decided",
];

/// The top rung, and what a rung is scaled onto for showing.
const TOP: f64 = (RUNGS.len() - 1) as f64;
const OUT_OF: f64 = 10.0;

/// What a model that writes is told. One that decides never reads it: it is asked the same thing
/// as a question per passage.
const INSTRUCTION: &str = "\
You are ranking passages of source code against a question about what a repository does.

Judge each passage on its own, and say which rung it is on:

  3  this passage is where the thing asked about is implemented or decided
  2  it is directly involved — a caller, a definition it depends on, its tests
  1  it is about the same subject but does not answer the question
  0  it shares words with the question and nothing else

Judge what the code does, not whether it repeats the question's words. A passage using different
names for the same thing still belongs on a high rung; a passage full of the question's words that
does something unrelated is on rung 0.

Answer with JSON only: an object whose keys are the passage labels exactly as they are written
above each passage, and whose values are the rung — {\"src/thing.rs:120-158\": 3, ...}.";

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
        input.push_str(&format!("\nPASSAGE {}\n{text}\n", fragment.at()));
    }
    json!({
        // A judgement, so it asks for the model kept for judgements rather than the session's own.
        "role": "decision",
        "fallback": "skip",
        "instruction": INSTRUCTION,
        "input": input,
        "schema": schema(query, shortlist),
        // The schema is the question itself; without this it never reaches the model.
        "structured": true,
        // Scoring is reading, not reasoning: a budget here bought nothing, measured.
        "thinking": "off",
        "max_tokens": MOST_TOKENS,
        "timeout_ms": PATIENCE,
    })
}

/// A property per passage, named by the label it is shown under. Not one property holding a list:
/// a model that decides answers a question per property and has no way to say a list.
fn schema(query: &str, shortlist: &[&Fragment]) -> Value {
    let mut properties = serde_json::Map::new();
    for fragment in shortlist {
        let at = fragment.at();
        properties.insert(
            at.clone(),
            json!({
                "type": "integer",
                "minimum": 0,
                "maximum": RUNGS.len() - 1,
                "description": format!(
                    "Question: {query}\nJudge ONLY the passage labelled {at}. Which rung is it on?"
                ),
                "x-criteria": RUNGS,
            }),
        );
    }
    let required: Vec<String> = properties.keys().cloned().collect();
    json!({ "type": "object", "properties": properties, "required": required })
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

/// The rung each passage was put on, scaled onto the score sese shows. Flat —
/// `{"src/thing.rs:120-158": 3, …}` — and a key that is not a label is skipped.
#[must_use]
pub fn scores(said: &str) -> Vec<Scored> {
    let Some(found) = json_in(said) else {
        return Vec::new();
    };
    let Some(rows) = found.as_object() else {
        return Vec::new();
    };
    rows.iter()
        .filter_map(|(label, value)| {
            let (path, from, to) = at(label)?;
            let rung = value
                .as_f64()
                .or_else(|| value.as_str()?.parse().ok())?
                .clamp(0.0, TOP);
            Some(Scored {
                path,
                from,
                to,
                score: rung / TOP * OUT_OF,
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
