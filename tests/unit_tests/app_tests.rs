use super::super::app::compact_text;

#[test]
fn compact_text_preserves_short_model_ids_and_truncates_long_ones() {
    assert_eq!(compact_text("openai/gpt-5-mini", 24), "openai/gpt-5-mini");
    assert_eq!(
        compact_text("provider/a-very-long-model-name", 16),
        "provider/a-ve..."
    );
}

#[test]
fn published_version_is_consistent_across_native_web_and_docs() {
    let version = env!("CARGO_PKG_VERSION");
    let html = include_str!("../../wuziqi.html");
    let readme = include_str!("../../README.md");

    assert!(version.contains("-beta."));
    assert!(html.contains(&format!("const APP_VERSION='{version}';")));
    assert!(html.contains(&format!("<title>Wuziqi - Gomoku v{version}</title>")));
    assert!(readme.contains(&format!("当前版本：`{version}`")));
}
