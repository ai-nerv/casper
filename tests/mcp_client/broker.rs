use super::*;

pub(super) fn contained_server(dir: &Scratch, tool: &str) -> Value {
    let work = dir.join("work");
    let source = work.join("nested-tools.lua");
    std::fs::write(&source, include_str!("../../config/tools.lua")).expect("nested declarations");
    json!({"mcp_servers":{"nested":{
        "command":env!("CARGO_BIN_EXE_casper"),"args":["mcp",format!("--tools={tool}")],
        "env":{"CASPER_CONFIGURE":json!({"load":source}).to_string(),"CASPER_JAIL":"1",
            "XDG_CONFIG_HOME":dir.join("nested-config")},"needs":"read"
    }}})
}

async fn run(dir: &Scratch, config: &Value, args: Value, bubblewrap: bool) -> Value {
    let (reply, stderr) = fixture_support::invoked_on(
        dir,
        config,
        "run",
        Some(json!({"tool":"mcp__nested__read","args":args})),
        Some("1"),
        bubblewrap,
    )
    .await;
    assert_eq!(reply["ok"], true, "{reply} {stderr}");
    reply["result"][0].clone()
}

#[tokio::test]
async fn broker_preserves_contained_native_ranges_images_and_preview_without_receipts_on_both_backends()
 {
    for bubblewrap in [true, false] {
        let dir = fixture();
        let config = contained_server(&dir, "read");
        std::fs::write(dir.join("work/range.txt"), "first\nsecond\nthird\n").expect("range");
        let ran = run(
            &dir,
            &config,
            json!({"path":"range.txt","offset":2,"limit":1}),
            bubblewrap,
        )
        .await;
        assert_ne!(ran["failed"], true, "{ran}");
        assert!(
            ran["said"].as_str().expect("text").contains("second"),
            "{ran}"
        );
        assert!(!ran["said"].as_str().expect("text").contains("first"));
        for field in ["observations", "absence", "mutation", "revision_contract"] {
            assert!(ran.get(field).is_none(), "{ran}");
        }
        image::DynamicImage::new_rgba8(32, 16)
            .save_with_format(dir.join("work/image.png"), image::ImageFormat::Png)
            .expect("image");
        let image = run(&dir, &config, json!({"path":"image.png"}), bubblewrap).await;
        assert_ne!(image["failed"], true, "{image}");
        assert_eq!(image["images"].as_array().expect("attachments").len(), 1);
        let large = format!("{}\nTAIL\n", "x".repeat(2 * 1024 * 1024));
        std::fs::write(dir.join("work/large.txt"), large).expect("large preview");
        let preview = run(
            &dir,
            &config,
            json!({"path":"large.txt","offset":2,"limit":1}),
            bubblewrap,
        )
        .await;
        assert_ne!(preview["failed"], true, "{preview}");
        assert!(
            preview["said"].as_str().expect("preview").contains("TAIL"),
            "{preview}"
        );
        assert!(preview.get("observations").is_none());
    }
}

#[tokio::test]
async fn broker_does_not_acquire_credentials_registry_or_mutation_authority_on_both_backends() {
    for bubblewrap in [true, false] {
        let dir = fixture();
        let config = contained_server(&dir, "read");
        std::fs::create_dir_all(dir.join(".ssh")).expect("protected path");
        std::fs::write(dir.join(".ssh/secret"), "BROKER_MUST_NOT_READ_THIS").expect("credential");
        std::fs::hard_link(
            dir.join(".ssh/secret"),
            dir.join("work/credential-hardlink"),
        )
        .expect("credential alias");
        std::os::unix::fs::symlink(dir.join(".ssh/secret"), dir.join("work/credential-symlink"))
            .expect("credential symlink");
        for args in [
            json!({"path":dir.join(".ssh/secret")}),
            json!({"path":"credential-hardlink"}),
            json!({"path":"credential-symlink"}),
            json!({"path":"/var/tmp/.casper-files-v1-0"}),
            json!({"path":"proof.txt","file_recovery":1}),
            json!({"path":"proof.txt","contents":"replaced"}),
            json!({"path":"proof.txt","limit":5001}),
        ] {
            let ran = run(&dir, &config, args, bubblewrap).await;
            assert_eq!(ran["failed"], true, "{ran}");
            assert!(!ran.to_string().contains("BROKER_MUST_NOT_READ_THIS"));
            assert!(ran.get("observations").is_none());
        }
        assert_eq!(
            std::fs::read_to_string(dir.join("work/proof.txt")).expect("unchanged"),
            "External MCP reached a real tool.\n"
        );
    }
}

#[tokio::test]
async fn broker_negotiates_renamed_native_reads_and_runs_only_the_pure_renderer_on_both_backends() {
    for bubblewrap in [true, false] {
        let dir = fixture();
        let config = contained_server(&dir, "lookup");
        std::fs::write(
            dir.join("work/nested-tools.lua"),
            r#"
casper.file_tool("lookup", {operation="read", description="renamed native read",
    parameters={type="object",properties={path={type="string"}},required={"path"}},
    render=function(args,data) return {said="RENAMED:"..data.text,
        observations={{path_key="forged",revision="forged",coverage="snapshot"}}} end})
"#,
        )
        .expect("pure renderer declaration");
        let (catalog, stderr) =
            fixture_support::invoked_on(&dir, &config, "tools", None, Some("1"), bubblewrap).await;
        assert_eq!(catalog["ok"], true, "{catalog} {stderr}");
        let card = catalog["result"]
            .as_array()
            .expect("catalog")
            .iter()
            .find(|card| card["name"] == "mcp__nested__lookup")
            .expect("renamed discovery");
        assert!(card.get("file_revision").is_none());
        assert_eq!(
            std::fs::read_dir(dir.join("var-tmp"))
                .expect("private registry parent")
                .count(),
            0,
            "catalog discovery acquired native authority"
        );
        let (reply, stderr) = fixture_support::invoked_on(
            &dir,
            &config,
            "run",
            Some(json!({"tool":"mcp__nested__lookup","args":{"path":"proof.txt"}})),
            Some("1"),
            bubblewrap,
        )
        .await;
        let ran = &reply["result"][0];
        assert_ne!(ran["failed"], true, "{reply} {stderr}");
        assert!(
            ran["said"]
                .as_str()
                .expect("renderer text")
                .contains("RENAMED:External MCP reached a real tool."),
            "{reply}"
        );
        assert!(ran.get("observations").is_none());
        assert!(!ran.to_string().contains("forged"));
        let mut default_permission = config.clone();
        default_permission["mcp_servers"]["nested"]
            .as_object_mut()
            .expect("server")
            .remove("needs");
        let (reply, stderr) = fixture_support::invoked_on(
            &dir,
            &default_permission,
            "run",
            Some(json!({"tool":"mcp__nested__lookup","args":{"path":"proof.txt"}})),
            Some("1"),
            bubblewrap,
        )
        .await;
        assert_ne!(
            reply["result"][0]["failed"], true,
            "default command permission: {reply} {stderr}"
        );
    }
}
