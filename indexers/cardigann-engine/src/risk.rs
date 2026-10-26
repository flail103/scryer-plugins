//! What a corpus refresh changes about where definitions send data.
//!
//! A definition cannot run code: templates and filters are pure. What a
//! changed definition can do is carry an operator's secret somewhere it did
//! not go before, either by rendering it into a new place or by sending
//! requests to a new origin while it holds one. The refresh workflow merges a
//! refresh on its own only when no definition that was already baked does
//! either; anything this module flags waits for a maintainer. A definition
//! that references no secret, such as a public tracker adding mirrors, may
//! gain origins without review: it has nothing of the operator's to send.
//!
//! Like [`crate::gate`], this is test-only and reached through
//! `xtask cardigann sync-definitions`.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use url::Url;

use crate::baked::BodyRow;
use crate::definition::Definition;
use crate::{parse_definition, runtime};

/// Settings whose values the host treats as credentials whatever the
/// definition declares them as.
const CREDENTIAL_SETTINGS: [&str; 3] = ["password", "cookie", "cardigannCaptcha"];

/// Name fragments that mark a `text` setting as a secret: API keys, passkeys,
/// RSS keys, session tokens, PINs and the like.
const SECRET_NAME_FRAGMENTS: [&str; 10] = [
    "pass", "key", "cookie", "token", "secret", "pin", "otp", "2fa", "auth", "pid",
];

/// Where one definition can send requests and where its secrets go.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RiskProfile {
    /// Every origin the engine would allow this definition to reach, or why
    /// they could not be resolved.
    pub origins: Result<BTreeSet<String>, String>,
    /// For each secret setting the definition references, the places in the
    /// definition its value is rendered into, such as `login.inputs.password`
    /// or `search.paths[].path`.
    pub secret_placements: BTreeMap<String, BTreeSet<String>>,
    /// Secret settings declared with the `password` type.
    pub password_settings: BTreeSet<String>,
}

/// Build the profile of one definition's YAML.
pub fn profile(source: &str) -> Result<RiskProfile, String> {
    let definition = parse_definition(source)?;
    let tree: serde_yaml::Value = serde_yaml::from_str(source.trim_start_matches('\u{feff}'))
        .map_err(|error| format!("invalid YAML: {error}"))?;
    let secrets = secret_settings(&definition);
    let mut secret_placements: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    collect_placements(&tree, &mut String::new(), &secrets, &mut secret_placements);
    Ok(RiskProfile {
        origins: origins(&definition),
        secret_placements,
        password_settings: definition
            .settings
            .iter()
            .filter(|setting| setting.setting_type == "password")
            .map(|setting| setting.name.clone())
            .collect(),
    })
}

fn origins(definition: &Definition) -> Result<BTreeSet<String>, String> {
    let mut origins: BTreeSet<String> =
        runtime::resolve_network_endpoints(definition, &BTreeMap::new())?
            .into_iter()
            .collect();
    // A select setting can switch a templated authority to one of its
    // options, which the default configuration above never renders.
    for setting in &definition.settings {
        if setting.setting_type != "select" {
            continue;
        }
        let Some(serde_yaml::Value::Mapping(options)) = &setting.options else {
            continue;
        };
        for (key, value) in options {
            for candidate in [key, value] {
                if let Some(url) = candidate
                    .as_str()
                    .and_then(|text| Url::parse(text.trim()).ok())
                    .filter(|url| matches!(url.scheme(), "http" | "https"))
                {
                    origins.insert(url.origin().ascii_serialization());
                }
            }
        }
    }
    Ok(origins)
}

fn secret_settings(definition: &Definition) -> BTreeSet<String> {
    let mut secrets: BTreeSet<String> = CREDENTIAL_SETTINGS
        .iter()
        .map(ToString::to_string)
        .collect();
    for setting in &definition.settings {
        let name = setting.name.to_ascii_lowercase();
        if setting.setting_type == "password"
            || SECRET_NAME_FRAGMENTS
                .iter()
                .any(|fragment| name.contains(fragment))
        {
            secrets.insert(setting.name.clone());
        }
    }
    secrets
}

/// Record the path of every string that renders a secret setting. Sequence
/// positions are left out, so reordering a list does not read as a move.
fn collect_placements(
    value: &serde_yaml::Value,
    path: &mut String,
    secrets: &BTreeSet<String>,
    placements: &mut BTreeMap<String, BTreeSet<String>>,
) {
    match value {
        serde_yaml::Value::String(text) => {
            for name in config_references(text) {
                if secrets.contains(name) {
                    placements
                        .entry(name.to_string())
                        .or_default()
                        .insert(path.clone());
                }
            }
        }
        serde_yaml::Value::Sequence(items) => {
            let length = path.len();
            path.push_str("[]");
            for item in items {
                collect_placements(item, path, secrets, placements);
            }
            path.truncate(length);
        }
        serde_yaml::Value::Mapping(mapping) => {
            for (key, item) in mapping {
                let key = match key {
                    serde_yaml::Value::String(key) => key.clone(),
                    other => serde_yaml::to_string(other)
                        .unwrap_or_default()
                        .trim()
                        .to_string(),
                };
                let length = path.len();
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(&key);
                // A mapping key can render a secret too, as in a header name.
                collect_placements(&serde_yaml::Value::String(key), path, secrets, placements);
                collect_placements(item, path, secrets, placements);
                path.truncate(length);
            }
        }
        serde_yaml::Value::Tagged(tagged) => {
            collect_placements(&tagged.value, path, secrets, placements);
        }
        _ => {}
    }
}

/// The setting names a template string reads as `.Config.<name>`.
fn config_references(text: &str) -> impl Iterator<Item = &str> {
    text.match_indices(".Config.")
        .filter_map(move |(start, marker)| {
            let rest = &text[start + marker.len()..];
            let end = rest
                .find(|character: char| {
                    !(character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
                })
                .unwrap_or(rest.len());
            (end > 0).then(|| &rest[..end])
        })
}

/// One definition a refresh changes in a way that needs a maintainer.
#[derive(Debug, Serialize)]
pub struct Flagged {
    pub id: String,
    /// Origins the definition can now reach that it could not before, listed
    /// only when it references a secret setting.
    pub new_origins: Vec<String>,
    /// `setting at location` for each secret rendered somewhere new.
    pub new_secret_placements: Vec<String>,
    /// `password` settings that are no longer declared as passwords.
    pub weakened_settings: Vec<String>,
    /// Why a profile could not be built, which is flagged rather than trusted.
    pub unresolved: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RiskReport {
    /// Definitions in the corpus the crate was built with.
    pub previous: usize,
    /// Ids baked now that were not before. Nobody has configured these yet.
    pub added: Vec<String>,
    /// Ids that were baked before and are not now.
    pub removed: Vec<String>,
    /// Ids whose YAML changed.
    pub changed: Vec<String>,
    /// Changed definitions that reference no secret and gained origins. They
    /// are not flagged.
    pub secretless_new_origins: Vec<String>,
    /// Changed definitions that gained an origin while holding a secret,
    /// moved a secret, or weakened a password setting. Empty means the refresh
    /// may merge itself.
    pub flagged: Vec<Flagged>,
}

/// Compare a refreshed corpus with the one the crate was built with.
///
/// `previous_bodies` is a bodies asset; `current` maps each id baked now to
/// its YAML.
pub fn compare(
    previous_bodies: &str,
    current: &BTreeMap<String, String>,
) -> Result<RiskReport, String> {
    let mut previous: BTreeMap<String, String> = BTreeMap::new();
    for (number, line) in previous_bodies.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let row: BodyRow = serde_json::from_str(line)
            .map_err(|error| format!("previous bodies line {}: {error}", number + 1))?;
        previous.insert(row.id, row.definition_yaml);
    }

    let mut report = RiskReport {
        previous: previous.len(),
        added: current
            .keys()
            .filter(|id| !previous.contains_key(*id))
            .cloned()
            .collect(),
        removed: previous
            .keys()
            .filter(|id| !current.contains_key(*id))
            .cloned()
            .collect(),
        changed: Vec::new(),
        secretless_new_origins: Vec::new(),
        flagged: Vec::new(),
    };
    for (id, source) in current {
        let Some(before) = previous.get(id) else {
            continue;
        };
        if before == source {
            continue;
        }
        report.changed.push(id.clone());
        match compare_one(id, before, source) {
            Outcome::Clean => {}
            Outcome::SecretlessNewOrigins => report.secretless_new_origins.push(id.clone()),
            Outcome::Flagged(flagged) => report.flagged.push(flagged),
        }
    }
    Ok(report)
}

enum Outcome {
    Clean,
    SecretlessNewOrigins,
    Flagged(Flagged),
}

fn compare_one(id: &str, before: &str, after: &str) -> Outcome {
    let mut flagged = Flagged {
        id: id.to_string(),
        new_origins: Vec::new(),
        new_secret_placements: Vec::new(),
        weakened_settings: Vec::new(),
        unresolved: None,
    };
    let (before, after) = match (profile(before), profile(after)) {
        (Ok(before), Ok(after)) => (before, after),
        (Err(error), _) => {
            flagged.unresolved = Some(format!("previous definition: {error}"));
            return Outcome::Flagged(flagged);
        }
        (_, Err(error)) => {
            flagged.unresolved = Some(error);
            return Outcome::Flagged(flagged);
        }
    };
    match (&before.origins, &after.origins) {
        (Ok(old), Ok(new)) => flagged.new_origins = new.difference(old).cloned().collect(),
        (_, Err(error)) | (Err(error), _) => {
            flagged.unresolved = Some(format!("origins: {error}"));
        }
    }
    for (setting, locations) in &after.secret_placements {
        let old = before.secret_placements.get(setting);
        for location in locations {
            if !old.is_some_and(|old| old.contains(location)) {
                flagged
                    .new_secret_placements
                    .push(format!("{setting} at {location}"));
            }
        }
    }
    flagged.weakened_settings = before
        .password_settings
        .difference(&after.password_settings)
        .cloned()
        .collect();
    // Either version referencing a secret counts: a definition that drops
    // its last secret in the same change is still reviewed.
    let holds_secret = !before.secret_placements.is_empty() || !after.secret_placements.is_empty();
    let secretless_new_origins = !holds_secret && !flagged.new_origins.is_empty();
    if secretless_new_origins {
        flagged.new_origins.clear();
    }
    let clean = flagged.new_origins.is_empty()
        && flagged.new_secret_placements.is_empty()
        && flagged.weakened_settings.is_empty()
        && flagged.unresolved.is_none();
    match (clean, secretless_new_origins) {
        (false, _) => Outcome::Flagged(flagged),
        (true, true) => Outcome::SecretlessNewOrigins,
        (true, false) => Outcome::Clean,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = r#"---
id: fixture
name: Fixture
description: Fixture
language: en-US
type: private
encoding: UTF-8
links:
  - https://tracker.example/
caps:
  categorymappings:
    - {id: 1, cat: Movies, desc: Movies}
  modes:
    search: [q]
settings:
  - name: username
    type: text
    label: Username
  - name: password
    type: password
    label: Password
  - name: apikey
    type: text
    label: API key
login:
  path: login.php
  method: post
  inputs:
    username: "{{ .Config.username }}"
    password: "{{ .Config.password }}"
  test:
    path: index.php
search:
  paths:
    - path: browse.php
  headers:
    X-Api-Key: ["{{ .Config.apikey }}"]
  rows:
    selector: tr
  fields:
    title:
      selector: a
    download:
      selector: a
      attribute: href
"#;

    fn corpus(source: &str) -> BTreeMap<String, String> {
        BTreeMap::from([("fixture".to_string(), source.to_string())])
    }

    fn previous(source: &str) -> String {
        serde_json::to_string(&BodyRow {
            id: "fixture".to_string(),
            definition_yaml: source.to_string(),
        })
        .unwrap()
            + "\n"
    }

    fn compare_fixture(after: &str) -> RiskReport {
        compare(&previous(BASE), &corpus(after)).expect("compare")
    }

    #[test]
    fn the_fixture_profiles_its_secrets_and_origin() {
        let profile = profile(BASE).expect("profile");
        assert_eq!(
            profile.origins.unwrap(),
            BTreeSet::from(["https://tracker.example".to_string()])
        );
        assert_eq!(
            profile.secret_placements,
            BTreeMap::from([
                (
                    "apikey".to_string(),
                    BTreeSet::from(["search.headers.X-Api-Key[]".to_string()])
                ),
                (
                    "password".to_string(),
                    BTreeSet::from(["login.inputs.password".to_string()])
                ),
            ])
        );
    }

    #[test]
    fn selector_and_text_changes_merge_on_their_own() {
        let after = BASE
            .replace("selector: tr", "selector: table > tr")
            .replace("description: Fixture", "description: Changed");
        let report = compare_fixture(&after);
        assert_eq!(report.changed, ["fixture"]);
        assert!(report.flagged.is_empty(), "{:?}", report.flagged);
    }

    #[test]
    fn an_absolute_login_url_on_another_host_is_flagged() {
        let after = BASE.replace(
            "path: login.php",
            "path: https://collector.example/login.php",
        );
        let report = compare_fixture(&after);
        assert_eq!(report.flagged.len(), 1);
        assert_eq!(report.flagged[0].new_origins, ["https://collector.example"]);
    }

    #[test]
    fn a_new_link_is_flagged() {
        let after = BASE.replace(
            "  - https://tracker.example/\n",
            "  - https://tracker.example/\n  - https://mirror.example/\n",
        );
        assert_eq!(
            compare_fixture(&after).flagged[0].new_origins,
            ["https://mirror.example"]
        );
    }

    #[test]
    fn a_select_option_naming_another_host_is_flagged() {
        let after = BASE.replace(
            "settings:\n",
            "settings:\n  - name: mirror\n    type: select\n    default: https://tracker.example/\n    options:\n      https://tracker.example/: Main\n      https://elsewhere.example/: Other\n",
        );
        assert_eq!(
            compare_fixture(&after).flagged[0].new_origins,
            ["https://elsewhere.example"]
        );
    }

    #[test]
    fn a_secret_moving_into_a_query_string_is_flagged() {
        let after = BASE.replace(
            "    - path: browse.php\n",
            "    - path: \"browse.php?key={{ .Config.apikey }}\"\n",
        );
        assert_eq!(
            compare_fixture(&after).flagged[0].new_secret_placements,
            ["apikey at search.paths[].path"]
        );
    }

    #[test]
    fn a_secret_dropped_from_a_request_is_not_flagged() {
        let after = BASE.replace(
            "    X-Api-Key: [\"{{ .Config.apikey }}\"]\n",
            "    X-Api-Key: [static]\n",
        );
        assert!(compare_fixture(&after).flagged.is_empty());
    }

    #[test]
    fn a_password_setting_turned_into_text_is_flagged() {
        let after = BASE.replace("    type: password\n", "    type: text\n");
        assert_eq!(
            compare_fixture(&after).flagged[0].weakened_settings,
            ["password"]
        );
    }

    #[test]
    fn a_definition_that_no_longer_parses_is_flagged() {
        let report = compare_fixture("id: fixture\nlinks: [");
        assert!(report.flagged[0].unresolved.is_some());
    }

    #[test]
    fn config_references_keep_hyphenated_setting_names() {
        let names: Vec<&str> = config_references(
            "{{ .Config.api-key }}/x?u={{.Config.user_id}}&s={{ .Config.sitelink }}",
        )
        .collect();
        assert_eq!(names, ["api-key", "user_id", "sitelink"]);
    }

    #[test]
    fn a_public_definition_adding_a_mirror_is_listed_but_not_flagged() {
        let public = |links: &str| {
            format!(
                "---\nid: fixture\nname: Fixture\ndescription: Fixture\nlanguage: en-US\ntype: public\nencoding: UTF-8\nlinks:\n{links}caps:\n  categorymappings:\n    - {{id: 1, cat: Movies, desc: Movies}}\n  modes:\n    search: [q]\nsearch:\n  paths:\n    - path: browse.php\n  rows:\n    selector: tr\n  fields:\n    title:\n      selector: a\n    download:\n      selector: a\n      attribute: href\n"
            )
        };
        let before = public("  - https://tracker.example/\n");
        let after = public("  - https://tracker.example/\n  - https://mirror.example/\n");
        let report = compare(&previous(&before), &corpus(&after)).expect("compare");
        assert!(report.flagged.is_empty(), "{:?}", report.flagged);
        assert_eq!(report.secretless_new_origins, ["fixture"]);
    }

    #[test]
    fn added_and_removed_definitions_are_listed_without_flags() {
        let current = BTreeMap::from([(
            "other".to_string(),
            BASE.replace("id: fixture", "id: other"),
        )]);
        let report = compare(&previous(BASE), &current).expect("compare");
        assert_eq!(report.added, ["other"]);
        assert_eq!(report.removed, ["fixture"]);
        assert!(report.flagged.is_empty());
    }

    #[test]
    fn the_baked_corpus_profiles_cleanly() {
        let current: BTreeMap<String, String> = crate::baked::BODIES_ASSET
            .lines()
            .map(|line| {
                let row: BodyRow = serde_json::from_str(line).expect("body row");
                (row.id, row.definition_yaml)
            })
            .collect();
        for (id, source) in &current {
            let profile = profile(source).unwrap_or_else(|error| panic!("{id}: {error}"));
            if let Err(error) = profile.origins {
                panic!("{id}: origins: {error}");
            }
        }
        let report = compare(crate::baked::BODIES_ASSET, &current).expect("compare");
        assert!(report.changed.is_empty() && report.flagged.is_empty());
    }
}
