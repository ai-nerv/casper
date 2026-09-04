//! Saying what output *means*, from a declaration.
//!
//! ```lua
//! return { said = done.out, shown = casper.paint.diff(done.out) }
//! return { said = done.out, shown = casper.paint.ansi(done.out) }
//! ```
//!
//! Three ways in, and none of them takes a colour. A declaration that could name one would be a
//! second palette — see [`crate::paint`] — and it would be wrong on the first theme anybody set.

use luna::{Callback, CallbackReturn, Table, Value};

/// `casper.paint`, with one entry per way of reading output.
#[must_use]
pub fn table(ctx: luna::Context<'_>) -> Table<'_> {
    let paint = Table::new(&ctx);

    // A unified diff, read the way every diff reader reads one: from the first character.
    let diff = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let text: Value = stack.consume(ctx)?;
        let painted = crate::paint::diff(&text_of(text));
        stack.replace(ctx, lines(ctx, &painted));
        Ok(CallbackReturn::Return)
    });
    paint.set(ctx, "diff", diff).ok();

    // ANSI, with whatever theme is in force. A colour the theme does not name becomes ordinary
    // text: the escapes always go, and what varies is only whether anything was learned.
    let ansi = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let (text, theme): (Value, Value) = stack.consume(ctx)?;
        let painted = crate::paint::ansi(&text_of(text), &read_theme(ctx, theme));
        stack.replace(ctx, lines(ctx, &painted));
        Ok(CallbackReturn::Return)
    });
    paint.set(ctx, "ansi", ansi).ok();

    // Plain, for output that has no structure worth naming. The floor, and never wrong.
    let plain = Callback::from_fn(&ctx, move |ctx, _exec, mut stack| {
        let text: Value = stack.consume(ctx)?;
        let painted = crate::paint::plain(&text_of(text));
        stack.replace(ctx, lines(ctx, &painted));
        Ok(CallbackReturn::Return)
    });
    paint.set(ctx, "plain", plain).ok();

    paint
}

/// One Lua value as the text it stands for.
fn text_of(value: Value<'_>) -> String {
    match value {
        Value::String(s) => String::from_utf8_lossy(s.as_bytes()).into_owned(),
        Value::Nil => String::new(),
        other => format!("{other:?}"),
    }
}

/// A theme table as the map the painter wants.
///
/// Keyed either way, because programs emit both:
///
/// ```lua
/// casper.theme = { [81] = "type",            -- one of the 256
///                  ["#c678dd"] = "keyword" } -- truecolour, which is what bat sends
/// ```
///
/// A role name nobody recognises is dropped rather than guessed at: the vocabulary is closed, and
/// inventing a colour for a name outside it is the one thing roles exist to prevent. A key that
/// is neither a number nor a hex colour is dropped for the same reason.
fn read_theme<'gc>(ctx: luna::Context<'gc>, value: Value<'gc>) -> crate::paint::Theme {
    let mut theme = crate::paint::Theme::new();
    let Value::Table(given) = value else {
        return theme;
    };
    for (key, name) in given.iter(ctx) {
        let Value::String(name) = name else { continue };
        let name = String::from_utf8_lossy(name.as_bytes()).into_owned();
        let Ok(role) = serde_json::from_value(serde_json::Value::String(name)) else {
            continue;
        };
        let colour = match key {
            Value::Integer(n) => u8::try_from(n).ok().map(crate::paint::Colour::Indexed),
            Value::String(text) => hex(&String::from_utf8_lossy(text.as_bytes())),
            _ => None,
        };
        if let Some(colour) = colour {
            theme.insert(colour, role);
        }
    }
    theme
}

/// `#rrggbb`, as a colour.
fn hex(text: &str) -> Option<crate::paint::Colour> {
    let digits = text.strip_prefix('#')?;
    if digits.len() != 6 {
        return None;
    }
    let byte = |at: usize| u8::from_str_radix(digits.get(at..at + 2)?, 16).ok();
    Some(crate::paint::Colour::Rgb(byte(0)?, byte(2)?, byte(4)?))
}

/// Painted lines, as the view a declaration hands back.
///
/// Tagged, because a result carries one of two kinds of view and a reader has to tell them
/// apart: a bare list of lines would be indistinguishable from a question with a very odd shape,
/// and the two are drawn completely differently.
fn lines<'gc>(ctx: luna::Context<'gc>, painted: &[crate::paint::Line]) -> Table<'gc> {
    let view = Table::new(&ctx);
    view.set(ctx, "shown", luna::String::from_slice(&ctx, b"painted"))
        .ok();
    let out = Table::new(&ctx);
    for (nth, line) in painted.iter().enumerate() {
        let row = Table::new(&ctx);
        for (at, span) in line.iter().enumerate() {
            let held = Table::new(&ctx);
            let role = serde_json::to_value(span.role)
                .ok()
                .and_then(|value| value.as_str().map(ToOwned::to_owned))
                .unwrap_or_else(|| "text".to_owned());
            held.set(ctx, "role", luna::String::from_slice(&ctx, role.as_bytes()))
                .ok();
            held.set(
                ctx,
                "text",
                luna::String::from_slice(&ctx, span.text.as_bytes()),
            )
            .ok();
            row.set(ctx, i64::try_from(at + 1).unwrap_or(1), held).ok();
        }
        out.set(ctx, i64::try_from(nth + 1).unwrap_or(1), row).ok();
    }
    view.set(ctx, "lines", out).ok();
    view
}

#[cfg(test)]
mod tests {
    use crate::lua::engine::Engine;
    use crate::tools::Shown;

    /// What a declaration returning `expression` produced.
    fn shown(expression: &str) -> Shown {
        let mut engine = Engine::new();
        engine
            .run(
                &format!("casper.tool(\"t\", {{ run = function() return {expression} end }})"),
                "test.lua",
            )
            .expect("the chunk runs");
        engine
            .call("t", &serde_json::Value::Null)
            .expect("it ran")
            .shown
            .expect("something to show")
    }

    #[test]
    fn a_declaration_paints_a_diff_without_naming_a_colour() {
        let Shown::Painted { lines } =
            shown(r#"{ said = "x", shown = casper.paint.diff("-was\n+now") }"#)
        else {
            panic!("expected painted lines");
        };
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0][0].role, crate::paint::Role::Removed);
        assert_eq!(lines[1][0].role, crate::paint::Role::Added);
        assert_eq!(lines[1][0].text, "+now");
    }

    #[test]
    fn a_theme_a_declaration_wrote_is_what_ansi_is_read_against() {
        // The join between a program that speaks ANSI and a vocabulary that speaks meaning. It
        // belongs in a table somebody can edit when their theme changes.
        let Shown::Painted { lines } = shown(
            r#"{ said = "fn", shown = casper.paint.ansi("\27[38;5;81mfn", { [81] = "keyword" }) }"#,
        ) else {
            panic!("expected painted lines");
        };
        assert_eq!(lines[0][0].role, crate::paint::Role::Keyword);
        assert_eq!(lines[0][0].text, "fn");
    }

    #[test]
    fn a_theme_may_name_a_truecolour_by_its_hex() {
        // Which is the one that matters in practice: `bat` sends `38;2;R;G;B`, so a theme that
        // could only be keyed by index would match nothing it emits.
        let Shown::Painted { lines } = shown(
            r##"{ said = "fn", shown = casper.paint.ansi(
                 "\27[38;2;198;120;221mfn", { ["#c678dd"] = "keyword" }) }"##,
        ) else {
            panic!("expected painted lines");
        };
        assert_eq!(lines[0][0].role, crate::paint::Role::Keyword);
    }

    #[test]
    fn a_key_that_is_neither_a_number_nor_a_colour_is_dropped() {
        // Dropped rather than raising: a theme is a long table somebody edits by hand, and one
        // bad row should cost that row rather than the whole file's highlighting.
        let Shown::Painted { lines } = shown(
            r##"{ said = "x", shown = casper.paint.ansi(
                 "\27[38;5;81mx", { ["mauve"] = "keyword", [81] = "string" }) }"##,
        ) else {
            panic!("expected painted lines");
        };
        assert_eq!(lines[0][0].role, crate::paint::Role::String);
    }

    #[test]
    fn a_role_outside_the_vocabulary_is_dropped_rather_than_invented() {
        // Closed on purpose: a name the harness has no colour for would have one guessed, and
        // guessing is what roles exist to prevent.
        let Shown::Painted { lines } = shown(
            r#"{ said = "x", shown = casper.paint.ansi("\27[38;5;81mx", { [81] = "sparkly" }) }"#,
        ) else {
            panic!("expected painted lines");
        };
        assert_eq!(lines[0][0].role, crate::paint::Role::Text);
    }

    #[test]
    fn the_escapes_go_even_when_the_theme_says_nothing() {
        let Shown::Painted { lines } =
            shown(r#"{ said = "x", shown = casper.paint.ansi("\27[38;5;99mx\27[0m") }"#)
        else {
            panic!("expected painted lines");
        };
        assert_eq!(lines[0][0].text, "x");
    }

    #[test]
    fn plain_output_is_still_a_view() {
        let Shown::Painted { lines } =
            shown(r#"{ said = "a\nb", shown = casper.paint.plain("a\nb") }"#)
        else {
            panic!("expected painted lines");
        };
        assert_eq!(lines.len(), 2);
    }
}
