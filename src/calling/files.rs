use super::observing::Trace;
use crate::events::{Delivery, Durability, MutationState, Observation};
use crate::files::worker::{self, Guard, Operation, Request, Snapshot};
use crate::lua::engine::Engine;
use crate::tools::{
    Call, FileAbsence, FileMetadata, FileMutation, FileObservation, FileOperation, Ran, ReadArgs,
    ReadData, ReadText,
};
use serde::Deserialize;
use serde_json::json;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Edit {
    path: String,
    old: String,
    new: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Write {
    path: String,
    contents: String,
}

pub(super) fn run(
    call: &Call,
    inside: Option<&str>,
    kind: FileOperation,
    engine: &mut Engine,
    trace: &mut Trace,
) -> Result<Ran, String> {
    if kind == FileOperation::Read {
        if call.revision.mutation_requested() {
            return Err("file revision mutation guards cannot be used for a read".into());
        }
        trace.start();
        let mut ran = keep_failure(read(call, engine));
        if inside.is_some() || ran.failed {
            ran.files.observations.clear();
            ran.files.absence = None;
        }
        return Ok(ran);
    }
    let guard = call.revision.guard()?;
    trace.start();
    let id = guard.mutation_id.clone();
    let result = mutate(
        call,
        kind,
        guard.expected_revision,
        guard.mutation_id,
        engine,
        trace,
    );
    Ok(match result {
        Ok(ran) => keep_failure(ran),
        Err((outcome, error)) => {
            trace.mutation(if outcome == "unknown" {
                MutationState::Unknown {}
            } else {
                MutationState::Refused {}
            });
            let mut ran = keep_failure(Ran::failed(error));
            ran.files = FileMetadata {
                absence: None,
                revision_contract: Some(1),
                observations: Vec::new(),
                mutation: Some(FileMutation {
                    id,
                    outcome: outcome.into(),
                    durability: None,
                }),
            };
            ran
        }
    })
}

fn keep_failure(mut ran: Ran) -> Ran {
    if ran.failed {
        ran.keep = true;
        ran.brief.get_or_insert_with(|| {
            let line = ran
                .said
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or_default()
                .trim();
            if line.chars().count() > 120 {
                format!("{}...", line.chars().take(117).collect::<String>())
            } else {
                line.to_owned()
            }
        });
    }
    ran
}

fn read(call: &Call, engine: &mut Engine) -> Ran {
    let args = match ReadArgs::parse(&call.args) {
        Ok(args) => args,
        Err(e) => return Ran::failed(e.to_string()),
    };
    if !crate::jail::Jail::from_env().on() {
        return Ran::failed("native file reads require containment");
    }
    if call.revision.recovering() {
        match worker::presence(&args.path) {
            Ok(presence) if presence.state == worker::PresenceState::Absent => {
                return Ran {
                    said: format!(
                        "{} is absent; no contents were read. A write may only create without replacing another file.",
                        args.path
                    ),
                    files: FileMetadata {
                        absence: Some(FileAbsence {
                            version: 1,
                            path_key: presence.path_key,
                        }),
                        ..metadata(None, None)
                    },
                    ..Ran::default()
                };
            }
            Ok(_) => {}
            Err(error) => return Ran::failed(error.to_string()),
        }
    }
    match crate::images::jailed_file(&args.path) {
        Err(error) => return Ran::failed(error),
        Ok(Some(image)) => {
            return Ran {
                said: format!(
                    "Read image {} [{}; resized for inline vision]",
                    args.path, image.media_type
                ),
                images: vec![image],
                files: metadata(None, None),
                brief: Some(format!("image {}", args.path)),
                back: Some(format!("read {}", args.path)),
                ..Ran::default()
            };
        }
        Ok(None) => {}
    }
    let reading = match worker::read(
        &args.path,
        args.offset.unwrap_or(1),
        args.limit.unwrap_or(2000),
    ) {
        Ok(reading) => reading,
        Err(error) => return Ran::failed(error.to_string()),
    };
    let mut ran = present(call, &args.path, reading.presentation(), engine);
    ran.files = metadata(reading.snapshot.as_ref(), None);
    ran
}

pub(super) fn brokered(
    call: &Call,
    data: ReadData,
    engine: &mut Engine,
    trace: &mut Trace,
) -> Result<Ran, String> {
    let args = ReadArgs::parse(&call.args)?;
    let mut ran = match data {
        ReadData::Text(text) => {
            text.validate(&args)?;
            trace.start();
            present(call, &args.path, text, engine)
        }
        ReadData::Image(image) => {
            let encoded = crate::images::encoded(&image.data, &image.media_type)?;
            trace.start();
            Ran {
                said: format!(
                    "Read image {} [{}; resized for inline vision]",
                    args.path, image.media_type
                ),
                images: vec![encoded],
                brief: Some(format!("image {}", args.path)),
                back: Some(format!("read {}", args.path)),
                ..Ran::default()
            }
        }
    };
    ran.files = FileMetadata::default();
    Ok(keep_failure(ran))
}

fn present(call: &Call, path: &str, reading: ReadText, engine: &mut Engine) -> Ran {
    let ranged = reading.first_line > 1 || reading.last_line < reading.total_lines;
    let span = if ranged {
        format!(
            "lines {}-{} of {}",
            reading.first_line, reading.last_line, reading.total_lines
        )
    } else {
        format!("{} lines", reading.total_lines)
    };
    let mut said = if ranged {
        format!("{}\n({span})", reading.text)
    } else {
        reading.text.clone()
    };
    if reading.omitted_bytes > 0 {
        said.push_str(&format!(
            "\n… {} more bytes, not shown",
            reading.omitted_bytes
        ));
    }
    if reading.source_text.is_none() {
        said.push_str("\nRead-only preview; no editable revision was acquired.");
    }
    let data = json!({"source_text":reading.source_text,
        "source_bytes":reading.source_bytes,"text":reading.text,"said":said,"span":span});
    engine
        .native_render(&call.tool, &call.args, &data)
        .unwrap_or_else(|| Ran {
            said: said.clone(),
            shown: Some(crate::tools::Shown::Painted {
                lines: crate::highlight::code(&reading.text, path),
            }),
            brief: Some(format!("read {path} ({span})")),
            back: Some(format!("read {path}")),
            ..Ran::default()
        })
}

fn mutate(
    call: &Call,
    kind: FileOperation,
    expected: String,
    id: String,
    engine: &mut Engine,
    trace: &Trace,
) -> Result<Ran, (&'static str, String)> {
    let fail = |error: String| ("refused", error);
    let (path, operation, before, default_text) = match kind {
        FileOperation::Write => {
            let args: Write =
                serde_json::from_value(call.args.clone()).map_err(|e| fail(e.to_string()))?;
            let existed = expected != "absent";
            let text = format!(
                "wrote {}: {} lines, {} bytes, {}",
                args.path,
                args.contents.lines().count(),
                args.contents.len(),
                if existed { "replaced" } else { "new file" }
            );
            let operation = if existed {
                Operation::Replace {
                    content: args.contents,
                }
            } else {
                Operation::Create {
                    content: args.contents,
                }
            };
            (args.path, operation, None, text)
        }
        FileOperation::Edit => {
            if expected == "absent" {
                return Err(fail(
                    "native edit requires an existing-file observation; reread first".into(),
                ));
            }
            let args: Edit =
                serde_json::from_value(call.args.clone()).map_err(|e| fail(e.to_string()))?;
            let draft = Request {
                path: args.path,
                operation: Operation::Edit {
                    old_text: args.old,
                    new_text: args.new,
                },
                guard: Some(Guard {
                    revision_contract: 1,
                    expected_revision: expected.clone(),
                    mutation_id: id.clone(),
                }),
            };
            draft.validate().map_err(|e| fail(e.to_string()))?;
            let Operation::Edit { old_text, new_text } = &draft.operation else {
                return Err(fail("invalid edit operation".into()));
            };
            let before = worker::observe(&draft.path).map_err(|e| fail(e.to_string()))?;
            if before.revision != expected {
                return Err(fail(
                    "native source revision is stale; reread before mutation".into(),
                ));
            }
            let line = before.source_text.find(old_text).map_or(1, |offset| {
                before
                    .source_text
                    .get(..offset)
                    .unwrap_or_default()
                    .bytes()
                    .filter(|b| *b == b'\n')
                    .count()
                    + 1
            });
            let old = old_text
                .lines()
                .map(|line| format!("-{line}"))
                .collect::<Vec<_>>()
                .join("\n");
            let new = new_text
                .lines()
                .map(|line| format!("+{line}"))
                .collect::<Vec<_>>()
                .join("\n");
            let text = format!(
                "--- {}\n+++ {}\n@@ -{},{} +{},{} @@\n{}\n{}",
                draft.path,
                draft.path,
                line,
                old_text.lines().count(),
                line,
                new_text.lines().count(),
                old,
                new
            );
            (draft.path, draft.operation, Some(before.source_text), text)
        }
        FileOperation::Read => return Err(fail("read is not a mutation".into())),
    };
    let request = Request {
        path: path.clone(),
        operation,
        guard: Some(Guard {
            revision_contract: 1,
            expected_revision: expected.clone(),
            mutation_id: id,
        }),
    };
    let commit = worker::mutation(&request).map_err(|error| {
        (
            if error.outcome_unknown() {
                "unknown"
            } else {
                "refused"
            },
            error.to_string(),
        )
    })?;
    trace.mutation(MutationState::Committed {
        durability: if commit.mutation.durability == "synced" {
            Durability::Confirmed
        } else {
            Durability::Unconfirmed
        },
        observation: if commit.observation.is_some() {
            Observation::Confirmed
        } else {
            Observation::Unconfirmed
        },
        delivery: Delivery::Unconfirmed,
    });
    let data = json!({"before":before,"source_text":commit.observation.as_ref().map(|value|value.source_text.as_str()),
        "existed":expected!="absent","said":default_text,"mutation":commit.mutation,"warning":commit.warning});
    let mut ran = engine
        .native_render(&call.tool, &call.args, &data)
        .unwrap_or_else(|| Ran::said(default_text));
    if ran.failed {
        ran.said = format!(
            "Native file mutation committed; presentation failed: {}",
            ran.said
        );
    }
    if let Some(warning) = &commit.warning {
        ran.said.push_str(&format!("\n{warning}"));
        ran.keep = true;
    }
    ran.brief.get_or_insert_with(|| {
        format!(
            "{} {path}",
            if kind == FileOperation::Edit {
                "edited"
            } else {
                "wrote"
            }
        )
    });
    ran.back.get_or_insert_with(|| format!("read {path}"));
    ran.files = metadata(
        commit.observation.as_ref(),
        Some(FileMutation {
            id: commit.mutation.id,
            outcome: commit.mutation.outcome,
            durability: Some(commit.mutation.durability),
        }),
    );
    Ok(ran)
}

fn metadata(snapshot: Option<&Snapshot>, mutation: Option<FileMutation>) -> FileMetadata {
    FileMetadata {
        absence: None,
        revision_contract: Some(1),
        observations: snapshot
            .map(|value| FileObservation {
                path_key: value.path_key.clone(),
                revision: value.revision.clone(),
                coverage: value.coverage.clone(),
            })
            .into_iter()
            .collect(),
        mutation,
    }
}
