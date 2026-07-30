#[test]
fn published_version_is_consistent_across_native_web_and_docs() {
    let version = env!("CARGO_PKG_VERSION");
    let html = include_str!("../wuziqi.html");
    let readme = include_str!("../README.md");

    let components = version.split('.').collect::<Vec<_>>();
    assert_eq!(components.len(), 3);
    assert!(
        components
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
    );
    assert!(html.contains(&format!("const APP_VERSION='{version}';")));
    assert!(html.contains(&format!("<title>Wuziqi - Gomoku v{version}</title>")));
    assert!(readme.contains(&format!("当前版本：`{version}`")));
}
