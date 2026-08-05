use std::collections::HashSet;

use reqwest::Url;
use serde_json::Value;

const MAX_SUPPORTED_PROFILES: usize = 2;

fn required_string<'a>(object: &'a Value, field: &str) -> &'a str {
    object[field]
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| panic!("example profile must contain a non-empty {field}"))
}

fn required_url(profile: &Value) -> Url {
    Url::parse(required_string(profile, "api_url"))
        .expect("example profile api_url must be a valid absolute URL")
}

#[test]
fn example_config_matches_the_public_profile_contract() {
    let document: Value = serde_json::from_str(include_str!("../llm_config.example.json"))
        .expect("example configuration must be valid JSON");

    assert_eq!(document["schema_version"], 3);

    let profiles = document["profiles"]
        .as_array()
        .expect("example configuration must contain a profiles array");
    assert!(!profiles.is_empty(), "at least one profile is required");
    assert!(
        profiles.len() <= MAX_SUPPORTED_PROFILES,
        "the application supports at most {MAX_SUPPORTED_PROFILES} profiles"
    );

    let active_profile = document["active_profile"]
        .as_u64()
        .expect("active_profile must be a non-negative integer") as usize;
    assert!(
        active_profile < profiles.len(),
        "active_profile must identify an existing profile"
    );

    let mut profile_names = HashSet::new();
    for profile in profiles {
        let name = required_string(profile, "name");
        assert!(
            profile_names.insert(name),
            "example profile names must be unique"
        );

        let backend = required_string(profile, "backend");
        assert!(
            matches!(backend, "openrouter" | "local"),
            "example profile backend must be openrouter or local"
        );

        let api_url = required_url(profile);
        assert!(
            api_url.username().is_empty() && api_url.password().is_none(),
            "example API URLs must not contain credentials"
        );
        assert!(
            api_url.query().is_none() && api_url.fragment().is_none(),
            "example API URLs must not contain a query or fragment"
        );
        required_string(profile, "model");
    }

    let cloud = profiles
        .iter()
        .find(|profile| profile["backend"] == "openrouter")
        .expect("example must include a Cloud profile");
    let cloud_url = required_url(cloud);
    assert_eq!(cloud_url.scheme(), "https", "Cloud APIs must use HTTPS");
    assert!(
        cloud_url.host_str().is_some(),
        "Cloud APIs must have a host"
    );
    assert_eq!(
        required_string(cloud, "api_key_origin"),
        cloud_url.origin().ascii_serialization(),
        "Cloud api_key_origin must match the configured API URL origin"
    );
    assert!(
        required_string(cloud, "api_key").starts_with("YOUR_"),
        "the example must use a placeholder API key"
    );
    assert_eq!(
        cloud["no_reasoning"], true,
        "the Cloud example must demonstrate no_reasoning"
    );

    let local = profiles
        .iter()
        .find(|profile| profile["backend"] == "local")
        .expect("example must include a Local profile");
    let local_url = required_url(local);
    assert!(
        matches!(local_url.scheme(), "http" | "https"),
        "Local APIs must use HTTP or HTTPS"
    );
    let local_host = local_url
        .host_str()
        .expect("Local API URL must have a host")
        .trim_matches(['[', ']']);
    assert!(
        matches!(local_host, "127.0.0.1" | "::1"),
        "Local APIs must use 127.0.0.1 or ::1"
    );
    assert!(
        local.get("api_key").is_none(),
        "the Local example must not contain an API key"
    );
    assert!(
        local.get("api_key_origin").is_none(),
        "the Local example must not contain an API key origin"
    );
}
