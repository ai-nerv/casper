//! Putting a question to the person, from a declaration.
//!
//! ```lua
//! run = function(args)
//!   if not args.answered then
//!     return casper.ask("run `" .. args.command .. "`?", {
//!       { id = "once",   label = "Allow once" },
//!       { id = "always", label = "Allow any " .. head, about = "for the rest of this session" },
//!       { id = "no",     label = "Deny", about = "the model is told, and carries on" },
//!     })
//!   end
//!   if args.answered == "no" then return { said = "not permitted", failed = true } end
//!   …
//! end
//! ```
//!
//! A question is not a result: what comes back has a view and no `said`. The harness draws the
//! question, hands the chosen id back as `answered`, and the same tool runs again with it.

use luna::{Callback, CallbackReturn, Table, Value};

/// `casper.ask`, as a callable.
#[must_use]
pub fn table(ctx: luna::Context<'_>) -> Callback<'_> {
    Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (question, options, detail): (Value, Value, Value) = stack.consume(ctx)?;
        let Value::String(question) = question else {
            return Err(raise(
                ctx,
                "casper.ask(question, options): a question first",
            ));
        };

        let answers = read(ctx, options);
        if matches!(answers.get_value(ctx, 1), Value::Nil) {
            return Err(raise(
                ctx,
                "casper.ask: a question with no answers is a message, not a question",
            ));
        }

        let ask = Table::new(&ctx);
        ask.set(ctx, "shown", luna::String::from_slice(&ctx, b"ask"))
            .ok();
        ask.set(ctx, "question", question).ok();
        ask.set(ctx, "options", answers).ok();
        if let Value::Table(detail) = detail {
            ask.set(ctx, "detail", detail).ok();
        }

        // The whole result, not just its view: `return casper.ask(…)` means the call is unfinished.
        let out = Table::new(&ctx);
        out.set(ctx, "shown", ask).ok();
        stack.replace(ctx, out);
        Ok(CallbackReturn::Return)
    })
}

/// The answers a declaration offered, keeping only the ones that can be chosen.
///
/// A row with no `id` is dropped: the id is what comes back, so a row without one never resumes.
fn read<'gc>(ctx: luna::Context<'gc>, options: Value<'gc>) -> Table<'gc> {
    let out = Table::new(&ctx);
    let Value::Table(given) = options else {
        return out;
    };
    let mut kept = 0_i64;
    for nth in 1.. {
        let Value::Table(row) = given.get_value(ctx, nth) else {
            break;
        };
        let Value::String(id) = row.get_value(ctx, "id") else {
            continue;
        };
        let held = Table::new(&ctx);
        held.set(ctx, "id", id).ok();
        // Falling back to the id: an unlabelled option is a blank line in a picker.
        let label = match row.get_value(ctx, "label") {
            Value::String(label) => label,
            _ => id,
        };
        held.set(ctx, "label", label).ok();
        if let Value::String(about) = row.get_value(ctx, "about") {
            held.set(ctx, "about", about).ok();
        }
        kept += 1;
        out.set(ctx, kept, held).ok();
    }
    out
}

/// Raise a message into Lua.
fn raise<'gc>(ctx: luna::Context<'gc>, message: &str) -> luna::Error<'gc> {
    luna::Error::from_value(Value::String(luna::String::from_slice(
        &ctx,
        message.as_bytes(),
    )))
}

#[cfg(test)]
mod tests {
    use crate::lua::engine::Engine;
    use crate::tools::{Ran, Shown};

    /// What a declaration returning `body` produced, given `args`.
    fn ran(body: &str, args: serde_json::Value) -> Ran {
        let mut engine = Engine::new();
        engine
            .run(
                &format!("casper.tool(\"t\", {{ run = function(args) {body} end }})"),
                "test.lua",
            )
            .expect("the chunk runs");
        engine.call("t", &args).expect("it ran")
    }

    #[test]
    fn a_question_comes_back_as_a_view_with_no_result() {
        let out = ran(
            r#"return casper.ask("run it?", { { id = "yes", label = "Allow" },
                                              { id = "no",  label = "Deny" } })"#,
            serde_json::Value::Null,
        );
        assert!(out.waiting());
        assert!(out.said.is_empty());
        let Some(Shown::Ask(ask)) = out.shown else {
            panic!("expected a question");
        };
        assert_eq!(ask.question, "run it?");
        assert_eq!(ask.options.len(), 2);
        assert_eq!(ask.options[0].id, "yes");
        assert_eq!(ask.options[0].label, "Allow");
    }

    #[test]
    fn the_answer_comes_back_and_the_same_tool_finishes_the_call() {
        let body = r#"if not args.answered then
                        return casper.ask("go?", { { id = "yes", label = "Yes" } })
                      end
                      return { said = "the person said " .. args.answered }"#;
        let asked = ran(body, serde_json::json!({}));
        assert!(asked.waiting());

        let answered = ran(body, serde_json::json!({"answered": "yes"}));
        assert!(!answered.waiting());
        assert_eq!(answered.said, "the person said yes");
    }

    #[test]
    fn a_row_with_no_id_is_dropped_because_nothing_could_come_back_from_it() {
        let out = ran(
            r#"return casper.ask("?", { { label = "nameless" }, { id = "real", label = "Real" } })"#,
            serde_json::Value::Null,
        );
        let Some(Shown::Ask(ask)) = out.shown else {
            panic!("expected a question");
        };
        assert_eq!(ask.options.len(), 1);
        assert_eq!(ask.options[0].id, "real");
    }

    #[test]
    fn a_row_with_no_label_still_says_something() {
        let out = ran(
            r#"return casper.ask("?", { { id = "carry-on" } })"#,
            serde_json::Value::Null,
        );
        let Some(Shown::Ask(ask)) = out.shown else {
            panic!("expected a question");
        };
        assert_eq!(ask.options[0].label, "carry-on");
    }

    #[test]
    fn a_question_with_no_answers_is_refused_rather_than_drawn() {
        let out = ran(r#"return casper.ask("?", { })"#, serde_json::Value::Null);
        assert!(out.failed, "{out:?}");
        assert!(out.said.contains("not a question"), "{}", out.said);
    }

    #[test]
    fn what_is_being_asked_about_can_carry_painted_rows() {
        let out = ran(
            r#"return casper.ask("run it?", { { id = "no", label = "Deny" } },
                                 casper.paint.diff("-was\n+now").lines)"#,
            serde_json::Value::Null,
        );
        let Some(Shown::Ask(ask)) = out.shown else {
            panic!("expected a question");
        };
        assert_eq!(ask.detail.len(), 2);
        assert_eq!(ask.detail[0][0].role, crate::paint::Role::Removed);
    }
}

/// What a resumed call is handed.
///
/// The answer travels beside the arguments on the wire, and the declaration reads it among them.
#[cfg(test)]
mod resuming {
    use crate::tools::Call;

    /// What `casper run` hands a declaration, for this call.
    fn given(call: &Call) -> serde_json::Value {
        let mut args = call.args.clone();
        let Some(answered) = &call.answered else {
            return args;
        };
        match &mut args {
            serde_json::Value::Object(fields) => {
                fields.insert("answered".to_owned(), serde_json::json!(answered));
            }
            other => *other = serde_json::json!({ "answered": answered }),
        }
        args
    }

    fn call(args: serde_json::Value, answered: Option<&str>) -> Call {
        Call {
            tool: "shell".to_owned(),
            args,
            cwd: String::new(),
            answered: answered.map(ToOwned::to_owned),
        }
    }

    #[test]
    fn the_answer_arrives_among_the_arguments() {
        let given = given(&call(serde_json::json!({"command": "ls"}), Some("once")));
        assert_eq!(given["answered"], "once");
        assert_eq!(given["command"], "ls", "the arguments survive it");
    }

    #[test]
    fn a_first_call_carries_no_answer_to_confuse_a_declaration_with() {
        let given = given(&call(serde_json::json!({"command": "ls"}), None));
        assert!(given.get("answered").is_none(), "{given}");
    }

    #[test]
    fn a_tool_that_takes_no_arguments_can_still_be_resumed() {
        let given = given(&call(serde_json::Value::Null, Some("yes")));
        assert_eq!(given["answered"], "yes");
    }
}
