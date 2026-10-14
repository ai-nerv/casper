#![allow(clippy::expect_used, clippy::indexing_slicing)]

use base64::{Engine as _, engine::general_purpose::STANDARD};
use casper::scratch::Scratch;
use image::ImageFormat;
use serde_json::json;

fn raster(path: &std::path::Path) {
    image::DynamicImage::new_rgba8(2000, 1000)
        .save_with_format(path, ImageFormat::Png)
        .expect("raster");
}

fn read(dir: &Scratch, path: &std::path::Path, jail: bool) -> casper::tools::Ran {
    use std::io::Write as _;
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_casper"));
    command
        .arg("run")
        .current_dir(dir.join("work"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("HOME", &**dir)
        .env_remove("CASPER_CONFIGURE")
        .env_remove("CASPER_JAIL")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped());
    if jail {
        command.env("CASPER_JAIL", "1");
    }
    let mut child = command.spawn().expect("Casper");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(
            json!({"tool":"read","args":{"path":path}})
                .to_string()
                .as_bytes(),
        )
        .expect("call");
    let out = child.wait_with_output().expect("output");
    let reply: serde_json::Value = serde_json::from_slice(&out.stdout).expect("reply");
    serde_json::from_value(reply["result"][0].clone()).expect("Ran")
}

fn fixture(name: &str) -> Scratch {
    let dir = Scratch::new("images", name);
    std::fs::create_dir_all(dir.join("config/casper")).expect("config");
    std::fs::create_dir_all(dir.join("work")).expect("work");
    std::fs::write(
        dir.join("config/casper/tools.lua"),
        include_str!("../config/tools.lua"),
    )
    .expect("tools");
    dir
}

#[test]
fn read_delivers_a_typed_image_and_plain_text_still_works() {
    let dir = fixture("read");
    let image = dir.join("work/image.png");
    raster(&image);
    let ran = read(&dir, &image, false);
    assert!(!ran.failed, "{}", ran.said);
    assert_eq!(ran.images.len(), 1);
    let decoded = STANDARD.decode(&ran.images[0].data).expect("base64");
    assert!(decoded.len() <= casper::images::INLINE_BYTES);
    assert!(image::load_from_memory(&decoded).expect("pixels").width() <= 1536);
    assert!(!ran.said.contains(&ran.images[0].data));
    let text = dir.join("work/plain.txt");
    std::fs::write(&text, "hello images\n").expect("text");
    let ran = read(&dir, &text, false);
    assert!(!ran.failed, "{}", ran.said);
    assert!(ran.images.is_empty());
    assert_eq!(ran.said, "hello images");
}

#[test]
fn image_reads_cannot_cross_the_jail() {
    let dir = fixture("jail");
    let allowed = dir.join("work/allowed.png");
    raster(&allowed);
    let inside = read(&dir, &allowed, true);
    assert!(!inside.failed, "valid jailed read failed: {}", inside.said);
    assert_eq!(inside.images.len(), 1);
    let outside = Scratch::new("images-secret", "outside");
    let image = outside.join("secret.png");
    raster(&image);
    let ran = read(&dir, &image, true);
    assert!(ran.failed, "image escaped the jail");
    assert!(ran.images.is_empty());
    assert!(ran.keep);
}

/// What `casper image` answers for `args`, run from the fixture's working tree.
fn image_verb(dir: &Scratch, args: &[&std::path::Path]) -> serde_json::Value {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_casper"))
        .arg("image")
        .args(args)
        .current_dir(dir.join("work"))
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_DATA_HOME", dir.join("data"))
        .env("HOME", &**dir)
        .env_remove("CASPER_CONFIGURE")
        .env_remove("CASPER_JAIL")
        .output()
        .expect("Casper");
    serde_json::from_slice(&out.stdout).expect("reply")
}

#[test]
fn the_image_verb_answers_a_resized_raster_and_nothing_for_other_files() {
    let dir = fixture("verb");
    let image = dir.join("work/image.png");
    raster(&image);
    let reply = image_verb(&dir, &[&image]);
    assert_eq!(reply["ok"], true, "{reply}");
    let attached: casper::images::Image =
        serde_json::from_value(reply["result"][0].clone()).expect("image");
    let decoded = STANDARD.decode(&attached.data).expect("base64");
    assert!(decoded.len() <= casper::images::INLINE_BYTES);
    assert!(image::load_from_memory(&decoded).expect("pixels").width() <= 1536);

    let text = dir.join("work/plain.txt");
    std::fs::write(&text, "not an image\n").expect("text");
    let reply = image_verb(&dir, &[&text]);
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(reply["result"][0].is_null(), "{reply}");

    let reply = image_verb(&dir, &[]);
    assert_eq!(reply["ok"], false, "{reply}");
    assert!(
        reply["error"]
            .as_str()
            .is_some_and(|why| why.contains("file path")),
        "{reply}"
    );
}
