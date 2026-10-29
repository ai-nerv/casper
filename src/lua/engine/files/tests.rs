#![allow(clippy::expect_used)]
use super::*;

#[test]
fn declaration_only_vm_preserves_the_shipped_tool_cards_and_pure_settings() {
    let source = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/config/tools.lua"));
    let mut legacy = Engine::new();
    legacy
        .run(source, "shipped-tools.lua")
        .expect("legacy declarations");
    legacy.harvest();
    let mut pure = Engine::declarations(None);
    pure.run(source, "shipped-tools.lua")
        .expect("effect-free declarations");
    pure.harvest();
    assert_eq!(pure.tools(), legacy.tools());
    assert_eq!(pure.setting("theme"), legacy.setting("theme"));
    assert!(pure.declarations_clean());
    for (name, operation) in [
        ("read", FileOperation::Read),
        ("edit", FileOperation::Edit),
        ("write", FileOperation::Write),
    ] {
        assert_eq!(pure.native(name), Some(operation));
        let card = pure
            .tools()
            .into_iter()
            .find(|card| card.name == name)
            .expect("shipped card");
        assert_eq!(card.script, operation == FileOperation::Read);
        assert_eq!(card.group, "files");
        assert!(!card.deferred);
    }
}

#[test]
fn native_identity_is_keyed_and_ordinary_overrides_revoke_capability() {
    let mut engine = Engine::declarations(None);
    engine.run(r#"casper.file_tool("record",{operation="read",description="native",parameters={type="object"},script=true})"#,"native.lua").expect("native registrar");
    assert_eq!(engine.native("record"), Some(FileOperation::Read));
    assert_eq!(
        engine
            .tools()
            .first()
            .expect("native card")
            .file_revision
            .as_ref()
            .expect("capability")
            .version,
        1
    );
    assert!(
        engine.call("record", &serde_json::json!({})).is_none(),
        "native selection cannot invoke an old callback"
    );
    engine.run(r#"casper.tool("record",{description="legacy",parameters={type="object"},file_revision={version=1,operation="read"},run=function()return "ordinary"end})"#,"override.lua").expect("ordinary override");
    assert_eq!(engine.native("record"), None);
    assert!(
        engine
            .tools()
            .first()
            .expect("legacy card")
            .file_revision
            .is_none()
    );
    assert_eq!(
        engine
            .call("record", &serde_json::json!({}))
            .expect("legacy callback")
            .said,
        "ordinary"
    );
    engine.run(r#"casper.file_tool("record",{operation="write",description="new native",parameters={type="object"}})"#,"native-again.lua").expect("native override");
    assert_eq!(engine.native("record"), Some(FileOperation::Write));
    assert_eq!(engine.tools().len(), 1);
}

#[test]
fn file_registrars_refuse_operation_callbacks_permission_lies_and_invalid_names() {
    for spec in [
        r#"operation="write",needs="read""#,
        r#"operation="write",script=true"#,
        r#"operation="shell""#,
        r#"operation="read",run=function()end"#,
        r#"operation="read",screen=function()end"#,
        r#"operation="read",render=true"#,
    ] {
        let mut engine = Engine::declarations(None);
        assert!(
            engine
                .run(
                    &format!(
                        "casper.file_tool('file',{{description='probe',parameters={{}}, {spec}}})"
                    ),
                    "bad-native.lua"
                )
                .is_err(),
            "accepted {spec}"
        );
    }
    for name in ["", "mcp_remote", "name with spaces"] {
        let mut engine = Engine::declarations(None);
        assert!(engine.run(&format!("casper.file_tool({name:?},{{operation='read',description='probe',parameters={{}}}})"),"bad-name.lua").is_err());
    }
}

#[test]
fn every_effectful_api_and_global_stream_alias_taints_admission_even_when_caught() {
    for expression in [
        "casper.exec('sh',{})",
        "casper.source('source')",
        "casper.replace_source('source','changed')",
        "casper.stream.connect('/unused')",
        "__stream.connect('/unused')",
        "casper.fs.ls('.')",
        "casper.dirs.list('.')",
        "casper.dirs.tree('.')",
        "casper.seek('question')",
        "casper.web({})",
        "casper.mcp('remote')",
        "casper.image('source')",
        "casper.ask({})",
        "casper.wonder('session',{})",
        "casper.surface({rows=2,about='probe'})",
    ] {
        let mut engine = Engine::declarations(None);
        engine.run("assert(type(casper.exec)=='function'); assert(type(casper.fs)=='table'); assert(__stream==casper.stream)","presence.lua").expect("callable API presence preserved");
        let error = engine
            .run(
                &format!("pcall(function() {expression} end)"),
                "effect-attempt.lua",
            )
            .expect_err("caught effects must still refuse");
        assert!(
            error.to_string().contains("effectful"),
            "{expression}: {error}"
        );
        assert!(!engine.declarations_clean());
        assert!(
            engine.run("local safe=1", "later.lua").is_err(),
            "taint must not be cleared by a later chunk"
        );
    }
}

#[test]
fn lent_callbacks_are_blocked_and_lua_results_cannot_forge_machine_observations() {
    fn forbidden(ctx: luna::Context<'_>) -> Callback<'_> {
        Callback::from_fn(&ctx, |_ctx, _exec, _stack| {
            panic!("effectful lent callback executed")
        })
    }
    let mut engine = Engine::declarations(None);
    engine.lend("python", forbidden);
    assert!(engine.run("pcall(casper.python)", "lent.lua").is_err());
    let mut engine = Engine::new();
    engine.run(r#"casper.tool("fake",{description="fake",parameters={},run=function()return{said="ordinary",revision_contract=1,observations={{path_key="forged",revision="forged",coverage="snapshot"}},mutation={id="forged",outcome="committed"}}end})"#,"fake.lua").expect("arbitrary callback declaration");
    let result = engine
        .call("fake", &serde_json::json!({}))
        .expect("ordinary result");
    assert_eq!(result.said, "ordinary");
    assert_eq!(result.files, crate::tools::FileMetadata::default());
    engine.run(r#"casper.tool("odd",{description="legacy unrelated fields",parameters={},run=function()return{said="preserved",observations="ordinary unknown field",mutation=42}end})"#,"unrelated.lua").expect("legacy declaration");
    assert_eq!(
        engine
            .call("odd", &serde_json::json!({}))
            .expect("legacy unknown fields ignored")
            .said,
        "preserved"
    );
}

#[test]
fn native_renderers_have_no_effects_and_no_machine_metadata_authority() {
    let mut engine = Engine::declarations(None);
    engine.run(r#"casper.file_tool("view",{operation="read",description="view",parameters={},render=function(args,data)return{said=data.text,revision_contract=1,observations={{path_key="forged",revision="forged",coverage="snapshot"}}}end})"#,"render.lua").expect("native renderer");
    let result = engine
        .native_render(
            "view",
            &serde_json::json!({}),
            &serde_json::json!({"text":"actual presentation"}),
        )
        .expect("renderer result");
    assert_eq!(result.said, "actual presentation");
    assert_eq!(result.files, crate::tools::FileMetadata::default());
    engine.run(r#"casper.file_tool("bad",{operation="read",description="bad",parameters={},render=function()pcall(casper.exec,"sh",{});return{said="hidden effect"}end})"#,"bad-render.lua").expect("declare without executing renderer");
    assert!(
        engine
            .native_render("bad", &serde_json::json!({}), &serde_json::json!({}))
            .expect("blocked renderer")
            .failed
    );
}
