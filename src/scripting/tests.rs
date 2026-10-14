use super::runner::{self, Bridge, Failed, PendingCall, Printed, Resolved};
use super::*;
use serde_json::json;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

/// The memory counters are shared by every script in this process, so these take turns.
static TURN: Mutex<()> = Mutex::new(());

fn turn() -> std::sync::MutexGuard<'static, ()> {
    TURN.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

const SMALL: usize = 2 * 1024 * 1024;
const OVER_LIMIT: &str = "x = [0] * 10_000_000";
const FILE: &str = "notes.txt";
const CONTENT: &str = "old";

type Resolve<'a> = dyn Fn(Vec<PendingCall>) -> Result<Resolved, Failed> + 'a;

fn answering(value: Value) -> Box<Resolve<'static>> {
    Box::new(move |batch| {
        Ok(batch
            .into_iter()
            .map(|c| (c.call_id, Ok(value.clone())))
            .collect())
    })
}

fn read_notes(path: String) -> Result<String, String> {
    if path == FILE {
        Ok(CONTENT.to_owned())
    } else {
        Err("no such file".to_owned())
    }
}

fn interpreted(
    code: &str,
    preamble: &str,
    tools: &[&str],
    resolve: &Resolve<'_>,
    files: bool,
    memory: usize,
) -> Result<runner::Printed, (Printed, Failed)> {
    let callable: HashSet<String> = tools.iter().map(|t| (*t).to_owned()).collect();
    let refused = HashMap::from([("write".to_owned(), "write: refused here".to_owned())]);
    let bridge = Bridge {
        callable: &callable,
        refused: &refused,
        resolve,
        read: files.then_some(&read_notes as &dyn Fn(String) -> Result<String, String>),
    };
    let mut printed = Printed::holding(MAX_BYTES);
    let limits = runner::limits(Duration::from_secs(30), memory);
    match runner::run(code, preamble, &bridge, limits, &mut printed) {
        Ok(value) => {
            if let Some(value) = value {
                printed.text.push_str(&format!("={value}"));
            }
            Ok(printed)
        }
        Err(why) => Err((printed, why)),
    }
}

fn value(code: &str) -> String {
    let _turn = turn();
    interpreted(code, "", &[], &answering(json!(null)), true, MEMORY)
        .map(|printed| printed.text)
        .unwrap_or_else(|(_, why)| format!("ERR {why}"))
}

fn failure(code: &str, tools: &[&str], resolve: &Resolve<'_>) -> String {
    let _turn = turn();
    match interpreted(code, "", tools, resolve, true, MEMORY) {
        Ok(printed) => panic!("expected a failure, got {}", printed.text),
        Err((_, why)) => why.to_string(),
    }
}

#[test]
fn an_expression_returns_its_value_and_print_prints() {
    assert_eq!(value("2 + 3"), "=5");
    assert_eq!(value("print('hello world')"), "hello world\n");
}

#[test]
fn a_parse_error_and_an_unknown_name_are_errors() {
    let _turn = turn();
    let resolve = answering(json!(null));
    let parse = interpreted("def", "", &[], &resolve, true, MEMORY).err();
    assert!(matches!(parse, Some((_, Failed::Parse(_)))), "{parse:?}");
    let unknown = interpreted("nonexistent()", "", &[], &resolve, true, MEMORY).err();
    assert!(
        matches!(&unknown, Some((_, Failed::Runtime(why))) if why.contains("NameError")),
        "{unknown:?}"
    );
}

#[test]
fn the_memory_limit_raises_and_a_small_workload_is_left_alone() {
    let _turn = turn();
    let resolve = answering(json!(null));
    let over = interpreted(OVER_LIMIT, "", &[], &resolve, true, SMALL).err();
    let why = over
        .map(|(_, why)| why.to_string().to_lowercase())
        .unwrap_or_default();
    assert!(why.contains("memory"), "{why}");
    let small = interpreted(
        "sum([i for i in range(1000)])",
        "",
        &[],
        &resolve,
        true,
        SMALL,
    );
    assert_eq!(small.map(|p| p.text).ok().as_deref(), Some("=499500"));
}

#[test]
fn concurrent_memory_limited_runs_both_enforce() {
    let _turn = turn();
    let spawn = || {
        std::thread::spawn(|| {
            let resolve = answering(json!(null));
            interpreted(OVER_LIMIT, "", &[], &resolve, true, SMALL).is_err()
        })
    };
    for handle in [spawn(), spawn()] {
        assert!(handle.join().unwrap_or(false));
    }
}

#[test]
fn text_reads_are_served_by_the_host_and_writes_are_refused() {
    assert_eq!(value(&format!("open('{FILE}').read()")), "=\"old\"");
    assert_eq!(
        value(&format!(
            "from pathlib import Path\nPath('{FILE}').read_text()"
        )),
        "=\"old\""
    );
    for write in [
        format!("open('{FILE}', 'w')"),
        format!("open('{FILE}', 'a').write('x')"),
        format!("from pathlib import Path\nPath('{FILE}').write_text('x')"),
    ] {
        let code = format!(
            "try:\n    {}\nexcept PermissionError as e:\n    err = str(e)\nerr",
            write.replace('\n', "\n    ")
        );
        let said = value(&code);
        assert!(said.contains("cannot write files"), "{write}: {said}");
    }
}

#[test]
fn file_errors_raise_a_catchable_os_error() {
    for (expr, message) in [
        ("open('missing.txt').read()", "no such file"),
        ("open('notes.txt', 'rb')", "binary files"),
        ("import os\nos.getenv('HOME')", "not supported"),
    ] {
        let code = format!(
            "try:\n    {}\nexcept Exception as e:\n    err = str(e)\nerr",
            expr.replace('\n', "\n    ")
        );
        let said = value(&code);
        assert!(said.contains(message), "{expr}: {said}");
    }
}

#[test]
fn os_calls_are_fatal_without_a_file_host() {
    let _turn = turn();
    let resolve = answering(json!(null));
    let why = interpreted("open('notes.txt')", "", &[], &resolve, false, MEMORY).err();
    assert!(matches!(why, Some((_, Failed::Sandboxed(_)))), "{why:?}");
}

#[test]
fn a_refused_tool_is_named_and_raises_its_reason() {
    let code = "try:\n    write(path='x')\nexcept PermissionError as e:\n    err = str(e)\nerr";
    assert_eq!(value(code), "=\"write: refused here\"");
}

#[test]
fn awaited_calls_made_together_resolve_in_one_batch() {
    let _turn = turn();
    let code = "async def main():\n    out = []\n    for call in [tool_a(), tool_fail(), tool_b()]:\n        try:\n            out.append(await call)\n        except Exception as e:\n            out.append(str(e))\n    return out\nawait main()";
    let batches = AtomicUsize::new(0);
    let resolve = |batch: Vec<PendingCall>| -> Result<Resolved, Failed> {
        batches.fetch_add(1, Ordering::SeqCst);
        Ok(batch
            .into_iter()
            .map(|c| match c.name.as_str() {
                "tool_fail" => (c.call_id, Err("boom".to_owned())),
                name => (c.call_id, Ok(json!(name))),
            })
            .collect())
    };
    let tools = ["tool_a", "tool_b", "tool_fail"];
    let out = interpreted(code, "", &tools, &resolve, true, MEMORY).map(|p| p.text);
    assert_eq!(out.ok().as_deref(), Some(r#"=["tool_a","boom","tool_b"]"#));
    assert_eq!(batches.load(Ordering::SeqCst), 1, "calls must be batched");
}

#[test]
fn sequential_awaits_are_separate_batches_and_asyncio_gather_raises() {
    let _turn = turn();
    let batches = AtomicUsize::new(0);
    let resolve = |batch: Vec<PendingCall>| -> Result<Resolved, Failed> {
        batches.fetch_add(1, Ordering::SeqCst);
        Ok(batch
            .into_iter()
            .map(|c| match c.name.as_str() {
                "tool_fail" => (c.call_id, Err("boom".to_owned())),
                _ => (c.call_id, Ok(json!("ok"))),
            })
            .collect())
    };
    let tools = ["tool_a", "tool_b", "tool_fail"];
    let code = "a = await tool_a()\nb = await tool_b()\na + b";
    let out = interpreted(code, "", &tools, &resolve, true, MEMORY).map(|p| p.text);
    assert_eq!(out.ok().as_deref(), Some("=\"okok\""));
    assert!(batches.load(Ordering::SeqCst) >= 2);
    drop(_turn);
    let code = "import asyncio\na, b = await asyncio.gather(tool_a(), tool_fail())";
    assert!(failure(code, &tools, &resolve).contains("boom"));
}

#[test]
fn waiting_on_a_tool_does_not_count_against_the_time_limit() {
    let _turn = turn();
    let resolve = |batch: Vec<PendingCall>| -> Result<Resolved, Failed> {
        std::thread::sleep(Duration::from_millis(1100));
        Ok(batch
            .into_iter()
            .map(|c| (c.call_id, Ok(json!("done"))))
            .collect())
    };
    let callable = HashSet::from(["slow".to_owned()]);
    let refused = HashMap::new();
    let bridge = Bridge {
        callable: &callable,
        refused: &refused,
        resolve: &resolve,
        read: None,
    };
    let mut printed = Printed::holding(MAX_BYTES);
    let limits = runner::limits(Duration::from_millis(1000), MEMORY);
    let code = "a = await slow()\nb = await slow()\na + b";
    let out = runner::run(code, "", &bridge, limits, &mut printed);
    assert_eq!(out.ok().flatten(), Some(json!("donedone")));
}

fn reported_lines(msg: &str) -> Vec<usize> {
    let prefix = format!("File \"{}\", line ", runner::SCRIPT_NAME);
    msg.lines()
        .filter_map(|l| l.trim_start().strip_prefix(&prefix))
        .filter_map(runner::split_line_number)
        .map(|(n, _)| n)
        .collect()
}

#[test]
fn traceback_lines_count_from_the_scripts_first_line() {
    let _turn = turn();
    let resolve = answering(json!(null));
    for (code, preamble) in [
        (
            "x = 1\nprint(boom_undefined)\n",
            "import re\nimport asyncio\n",
        ),
        ("x = 1\nprint(boom_undefined)\n", "import re"),
        ("x = 1\ndef\n", "import re\n"),
    ] {
        let why = interpreted(code, preamble, &[], &resolve, true, MEMORY).err();
        let why = why.map(|(_, why)| why.to_string()).unwrap_or_default();
        assert_eq!(reported_lines(&why), [2], "{why}");
    }
    let raising = "def helper():\n    raise ValueError('inside preamble')\n";
    let why = interpreted("helper()\n", raising, &[], &resolve, true, MEMORY).err();
    let why = why.map(|(_, why)| why.to_string()).unwrap_or_default();
    assert_eq!(reported_lines(&why), [1], "preamble frame leaked: {why}");
    assert!(
        !why.contains("raise ValueError") && why.contains("inside preamble"),
        "{why}"
    );
}

fn card(name: &str, needs: &str, script: bool) -> Card {
    Card {
        name: name.to_owned(),
        description: String::new(),
        parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
        needs: Some(needs.to_owned()),
        deferred: false,
        group: String::new(),
        script,
    }
}

fn cards() -> Vec<Card> {
    vec![
        card("read", "read", true),
        card("ls", "read", true),
        card("write", "write", true),
        card("shell", "run", true),
        card("web", "reach", true),
        card("sese", "read", false),
        card(TOOL, "read", true),
    ]
}

#[test]
fn only_a_declared_reading_tool_is_callable_whatever_the_config_says() {
    let named: Vec<String> = cards()
        .into_iter()
        .filter(callable)
        .map(|c| c.name)
        .collect();
    assert_eq!(named, ["read", "ls"]);
    assert!(!callable(&card("bad-name", "read", true)));
    let (_, refused) = names(&cards());
    assert!(
        refused["write"].contains("needs \"write\""),
        "{}",
        refused["write"]
    );
    assert!(refused["shell"].contains("needs \"run\""));
    assert!(refused["web"].contains("needs \"reach\""));
    assert!(refused[TOOL].contains("inside a script"));
}

#[test]
fn the_description_lists_what_a_script_may_call_and_nothing_else() {
    let mut offered = cards();
    describe(&mut offered);
    let said = &offered
        .iter()
        .find(|c| c.name == TOOL)
        .map(|c| c.description.clone())
        .unwrap_or_default();
    assert!(
        said.contains("- read(path: str) -> str\n- ls(path: str) -> str\n"),
        "{said}"
    );
    assert!(
        !said.contains("write(") && !said.contains("shell(") && !said.contains("sese("),
        "{said}"
    );
    let mut alone = vec![card(TOOL, "read", false)];
    describe(&mut alone);
    assert!(alone[0].description.contains("No tool is callable"));
}

fn script(
    code: &str,
    timeout: Option<i64>,
    run: &(dyn Fn(&Call) -> Result<Ran, String> + Sync),
) -> Ran {
    let _turn = turn();
    let cards = cards();
    super::run(
        code,
        timeout,
        &Host {
            cards: &cards,
            run,
            abandoned: &|| false,
        },
    )
}

fn echo(call: &Call) -> Result<Ran, String> {
    let path = call.args["path"].as_str().unwrap_or_default();
    if path == "missing" {
        return Ok(Ran::failed("no such file"));
    }
    Ok(Ran::said(format!("{} of {path}", call.tool)))
}

#[test]
fn gather_keeps_every_result_and_marks_the_failure() {
    let code = "print(await gather(read(path='a'), read(path='missing'), ls(path='b')))";
    let ran = script(code, None, &echo);
    assert!(!ran.failed, "{}", ran.said);
    assert_eq!(
        ran.said,
        "['read of a', '[ERROR] read: no such file', 'ls of b']"
    );
}

#[test]
fn arguments_are_held_to_the_schema_before_the_tool_runs() {
    let ran = script(
        "print(await gather(read(colour=1), read('a')))",
        None,
        &echo,
    );
    assert!(
        ran.said
            .contains("[ERROR] read() got an unexpected keyword argument 'colour'"),
        "{}",
        ran.said
    );
    assert!(
        ran.said
            .contains("[ERROR] read: pass arguments as keyword arguments"),
        "{}",
        ran.said
    );
}

#[test]
fn a_return_value_is_said_and_silence_says_so() {
    assert_eq!(
        script("print('x')\n2 + 3", None, &echo).said,
        "x\nreturn: 5"
    );
    assert_eq!(script("x = 1", None, &echo).said, NO_OUTPUT);
}

#[test]
fn output_is_cut_to_its_lines_and_says_how_much_went() {
    let ran = script("for i in range(3000):\n    print(i)", None, &echo);
    assert_eq!(
        ran.said.lines().take_while(|l| !l.is_empty()).count(),
        MAX_LINES
    );
    assert!(
        ran.said.contains("[truncated "),
        "{}",
        &ran.said[ran.said.len() - 40..]
    );
    let ran = script("print('x' * 200000)", None, &echo);
    assert!(ran.said.len() < MAX_BYTES + 64);
    assert!(
        ran.said.ends_with("[truncated 148801 bytes]"),
        "{}",
        &ran.said[ran.said.len() - 40..]
    );
}

#[test]
fn a_script_past_its_time_is_cut_and_says_so() {
    let ran = script("print('started')\nwhile True:\n    pass", Some(1), &echo);
    assert!(ran.failed);
    assert_eq!(
        ran.said,
        "started\n[timed out after 1s; output above is partial]"
    );
}

#[test]
fn an_abandoned_call_is_cancelled() {
    let _turn = turn();
    let cards = cards();
    let ran = super::run(
        "while True:\n    pass",
        Some(5),
        &Host {
            cards: &cards,
            run: &echo,
            abandoned: &|| true,
        },
    );
    assert_eq!(ran.said, "[cancelled; no output before the cut]");
}

#[test]
fn what_a_script_cannot_be_given_comes_back_as_an_error() {
    let image = |_: &Call| -> Result<Ran, String> {
        let png = crate::images::Image {
            data: String::new(),
            media_type: "image/png".into(),
        };
        Ok(Ran {
            images: vec![png],
            ..Ran::said("Read image a.png")
        })
    };
    let ran = script("print(await read(path='a.png'))", None, &image);
    assert!(ran.said.contains("pixels are not visible"), "{}", ran.said);
    let asking = |_: &Call| -> Result<Ran, String> {
        let wonder = crate::tools::Wondering {
            wonder: "helper".into(),
            args: Value::Null,
            about: String::new(),
        };
        Ok(Ran::wondering(wonder))
    };
    let ran = script("print(await gather(read(path='a')))", None, &asking);
    assert!(
        ran.said.contains("only the harness can give"),
        "{}",
        ran.said
    );
}

#[test]
fn a_script_that_fails_says_what_it_printed_and_why_and_hints_at_gather() {
    let ran = script(
        "print('before')\nimport asyncio\nawait asyncio.gather(read(path='missing'))",
        None,
        &echo,
    );
    assert!(ran.failed && ran.keep);
    assert!(ran.said.starts_with("before\n"), "{}", ran.said);
    assert!(
        ran.said.contains("no such file") && ran.said.contains("Hint: `gather(...)`"),
        "{}",
        ran.said
    );
}

#[test]
fn keyword_and_dict_arguments_make_the_same_input() {
    let kw = [("path".to_owned(), json!("/foo"))];
    assert_eq!(arguments(&[], &kw), Ok(json!({"path": "/foo"})));
    assert_eq!(
        arguments(&[json!({"path": "/foo"})], &[]),
        Ok(json!({"path": "/foo"}))
    );
    assert_eq!(arguments(&[], &[]), Ok(json!({})));
    assert_eq!(
        arguments(&[json!({"a": 1})], &[("b".to_owned(), json!(2))]),
        Ok(json!({"a": 1}))
    );
    assert!(arguments(&[json!("hello")], &[]).is_err());
}
