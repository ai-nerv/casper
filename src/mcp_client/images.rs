use super::*;

#[test]
fn malformed_images_are_explicit_failures_and_base64_is_not_prose() {
    let result = attachment_result(json!({
        "isError":false, "content":[{"type":"text","text":"screenshot"},
            {"type":"image","data":"broken!","mimeType":"image/png"}]
    }))
    .expect("result");
    assert!(result.failed);
    assert!(result.images.is_empty());
    assert!(result.said.contains("Image omitted"));
    assert!(result.said.contains("screenshot"));
    assert!(!result.said.contains("broken!"));
}

#[test]
fn embedded_image_resources_become_attachments() {
    use base64::Engine as _;
    let raster = image::DynamicImage::new_rgba8(16, 16);
    let mut bytes = std::io::Cursor::new(Vec::new());
    raster
        .write_to(&mut bytes, image::ImageFormat::Png)
        .expect("png");
    let data = base64::engine::general_purpose::STANDARD.encode(bytes.into_inner());
    let result = attachment_result(json!({"contents":[{
        "uri":"image://fixture", "mimeType":"image/png", "blob":data
    }]}))
    .expect("result");
    assert!(!result.failed);
    assert_eq!(result.images.len(), 1);
    assert!(!result.said.contains(&data));
}
