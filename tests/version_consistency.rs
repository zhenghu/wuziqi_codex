#[test]
fn published_version_is_consistent_across_native_web_and_docs() {
    let version = env!("CARGO_PKG_VERSION");
    let html = include_str!("../wuziqi.html");
    let readme = include_str!("../README.md");

    assert!(version.contains("-beta."));
    assert!(html.contains(&format!("const APP_VERSION='{version}';")));
    assert!(html.contains(&format!("<title>Wuziqi - Gomoku v{version}</title>")));
    assert!(readme.contains(&format!("当前版本：`{version}`")));
}
