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
//! **A question is not a result.** What comes back has a view and no `said`, because the call has
//! not finished: the harness draws the question, hands the chosen id back as `answered`, and the
//! same tool runs again with it. Sending the model an empty result instead would end a turn that
//! is still waiting on a person.
//!
//! That is the whole mechanism behind "anything can ask" — a permission, a file picker, a
//! confirmation, a form are one shape, and the list of things that can stop and ask stops being
//! a list somebody has to extend.

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
            // A question nobody can answer is a message, and a message is `said`. Left as a
            // raise rather than a silent empty list: a picker with no rows is a session that
            // waits forever on a choice it cannot offer.
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

        // The whole result, not just its view: a declaration writing `return casper.ask(…)` is
        // saying "this call is not finished", and wrapping it here is what makes that one line.
        let out = Table::new(&ctx);
        out.set(ctx, "shown", ask).ok();
        stack.replace(ctx, out);
        Ok(CallbackReturn::Return)
    })
}

/// The answers a declaration offered, keeping only the ones that can be chosen.
///
/// A row with no `id` is dropped: the id is what comes back, so a row without one is a button
/// that does nothing, and a person who picks it would be left with a call that never resumes.
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
        // Falling back to the id, because a row has to say *something*: an unlabelled option is
        // a blank line in a picker, and the id is at least the word the author chose.
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
        // The distinction the two faces exist for. A caller that took this for a finished call
        // would hand the model an empty string and end a turn still waiting on a person.
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
        // The other half of the mechanism, and the reason a question is not a separate kind of
        // tool: it is the same declaration, run again, with one more thing known.
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
        // The id is what returns. A row without one is a button that does nothing, and a person
        // who picked it would be left with a call that never resumes.
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
        // A blank row in a picker is worse than a terse one, and the id is at least the word
        // its author chose.
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
        // A picker with no rows is a session waiting forever on a choice it cannot offer, and
        // the failure would show up as a hung turn rather than as a broken declaration.
        let out = ran(r#"return casper.ask("?", { })"#, serde_json::Value::Null);
        assert!(out.failed, "{out:?}");
        assert!(out.said.contains("not a question"), "{}", out.said);
    }

    #[test]
    fn what_is_being_asked_about_can_carry_painted_rows() {
        // A permission is not answerable from one line: the person needs to see the command.
        // Painted, so the diff or the command is drawn in the palette everything else uses.
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
