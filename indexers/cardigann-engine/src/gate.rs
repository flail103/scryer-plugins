//! The admission gate for the baked definition corpus.
//!
//! One definition is admissible when it parses as Cardigann v12, validates
//! against the schema this engine actually supports, and drives the search flow
//! far enough to produce its first HTTP request. That is the same bar the
//! corpus test holds the upstream repository to, and the same bar
//! `xtask cardigann sync-definitions` applies before writing the committed
//! asset — one implementation, used by both, so the shipped corpus can never
//! contain something the corpus test would have rejected.
//!
//! This module is test-only on purpose. Nothing the component does at runtime
//! needs it, and compiling a corpus generator into the shipped Wasm would be
//! dead weight. `xtask cardigann sync-definitions` therefore reaches it by
//! running this crate's test binary with the `CARDIGANN_GATE_*` variables set,
//! which keeps the asset's reader (`crate::baked`) and its writer in one crate.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::baked::{ASSET_SCHEMA_VERSION, BodyRow, CUSTOM_DEFINITION_ID, IndexRow};
use crate::{definition, parse_definition, runtime};

const SOURCE_DIR_ENV: &str = "CARDIGANN_GATE_SOURCE_DIR";
const INDEX_OUT_ENV: &str = "CARDIGANN_GATE_INDEX_OUT";
const BODIES_OUT_ENV: &str = "CARDIGANN_GATE_BODIES_OUT";
const REPORT_OUT_ENV: &str = "CARDIGANN_GATE_REPORT_OUT";

/// What the gate learned about an admissible definition.
#[derive(Debug, Clone)]
pub struct GateEntry {
    pub id: String,
    pub display_name: String,
    pub kind: String,
    pub base_url: String,
}

/// Run one definition through the gate.
pub fn gate_definition(source: &str) -> Result<GateEntry, String> {
    let definition = parse_definition(source)?;
    let base_url = definition
        .links
        .first()
        .cloned()
        .ok_or_else(|| "definition has no base URL".to_string())?;
    let entry = GateEntry {
        id: definition.id.clone(),
        display_name: definition.name.clone(),
        kind: definition.definition_type.clone(),
        base_url: base_url.clone(),
    };
    let compiled_ir = serde_json::to_string(&definition::CompiledIr {
        ir_version: definition::COMPILED_IR_VERSION,
        definition,
    })
    .map_err(|error| format!("could not encode IR: {error}"))?;
    match runtime::begin(
        compiled_ir,
        runtime::Operation::Search(Box::default()),
        BTreeMap::from([("base_url".to_string(), base_url)]),
    ) {
        Ok(runtime::Step::NeedHttp { .. }) => Ok(entry),
        Ok(step) => Err(format!("flow did not begin with HTTP: {step:?}")),
        Err(error) => Err(format!("flow start failed: {error}")),
    }
}

/// The upstream `definitions/<vN>` directories this engine reads, oldest first.
///
/// Prowlarr/Indexers keeps each definition only in the lowest schema version
/// it validates against, so `v12/` holds just the definitions that need v12
/// features. A v12 consumer (Prowlarr reads `DEFINITION_VERSION = 12`) takes
/// each definition from the highest of these directories that has it.
pub const SOURCE_VERSIONS: [&str; 2] = ["v11", "v12"];

/// One definition file chosen from a `definitions/` root.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub path: PathBuf,
    pub file: String,
    /// The `definitions/<vN>` directory the file was taken from.
    pub version: &'static str,
}

/// Every definition a v12 consumer would load from a Prowlarr `definitions/`
/// root, sorted by file name: each file from the highest of
/// [`SOURCE_VERSIONS`] that has it, so a newer copy always wins even when it
/// fails the gate and the older one would not.
pub fn select_candidates(definitions_root: &Path) -> Result<Vec<Candidate>, String> {
    let mut selected: BTreeMap<String, Candidate> = BTreeMap::new();
    for version in SOURCE_VERSIONS {
        let directory = definitions_root.join(version);
        if !directory.is_dir() {
            return Err(format!(
                "{} has no {version} directory; pass the upstream definitions/ root",
                definitions_root.display()
            ));
        }
        for path in candidate_files(&directory)? {
            let file = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            selected.insert(
                file.clone(),
                Candidate {
                    path,
                    file,
                    version,
                },
            );
        }
    }
    Ok(selected.into_values().collect())
}

/// Every `*.yml` / `*.yaml` file in a Prowlarr definitions directory, sorted.
pub fn candidate_files(source_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut paths = std::fs::read_dir(source_dir)
        .map_err(|error| format!("could not read {}: {error}", source_dir.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("could not read a corpus entry: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|path| {
            matches!(
                path.extension().and_then(|extension| extension.to_str()),
                Some("yml" | "yaml")
            )
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

#[derive(Debug, Serialize)]
pub struct Excluded {
    pub file: String,
    pub source_version: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct Baked {
    pub id: String,
    pub file: String,
    pub source_version: String,
}

#[derive(Debug, Serialize)]
pub struct SyncReport {
    pub schema_version: u32,
    pub source_dir: String,
    pub source_versions: Vec<String>,
    pub candidates: usize,
    pub accepted: usize,
    /// How many baked definitions came from each `definitions/<vN>`.
    pub accepted_by_version: BTreeMap<String, usize>,
    pub baked: Vec<Baked>,
    pub excluded: Vec<Excluded>,
}

struct Admitted {
    entry: GateEntry,
    file: String,
    version: &'static str,
    source: String,
}

/// Gate a whole `definitions/` root and render the two asset files plus a report.
fn sync(source_dir: &Path) -> Result<(String, String, SyncReport), String> {
    let candidates = select_candidates(source_dir)?;
    if candidates.is_empty() {
        return Err(format!(
            "{} contains no Cardigann definitions",
            source_dir.display()
        ));
    }

    let mut admitted: Vec<Admitted> = Vec::new();
    let mut excluded = Vec::new();
    let mut seen_ids: BTreeMap<String, String> = BTreeMap::new();
    for candidate in &candidates {
        let file = candidate.file.clone();
        let version = candidate.version;
        let excluded_because = |reason: String| Excluded {
            file: file.clone(),
            source_version: version.to_string(),
            reason,
        };
        let source = match std::fs::read_to_string(&candidate.path) {
            Ok(source) => source,
            Err(error) => {
                excluded.push(excluded_because(format!("could not read: {error}")));
                continue;
            }
        };
        match gate_definition(&source) {
            Ok(entry) => {
                // The id is the selector's stored value and the asset's lookup
                // key, so a collision has to be an exclusion rather than a
                // silent last-writer-wins.
                if entry.id == CUSTOM_DEFINITION_ID {
                    excluded.push(excluded_because(format!(
                        "id `{CUSTOM_DEFINITION_ID}` is reserved"
                    )));
                    continue;
                }
                if let Some(previous) = seen_ids.get(&entry.id) {
                    excluded.push(excluded_because(format!(
                        "id `{}` is already used by {previous}",
                        entry.id
                    )));
                    continue;
                }
                seen_ids.insert(entry.id.clone(), format!("{version}/{file}"));
                admitted.push(Admitted {
                    entry,
                    file: file.clone(),
                    version,
                    source,
                });
            }
            Err(reason) => excluded.push(excluded_because(reason)),
        }
    }
    if admitted.is_empty() {
        return Err("no definition cleared the gate".to_string());
    }

    admitted.sort_by(|left, right| left.entry.id.cmp(&right.entry.id));
    let labels = unique_labels(&admitted);

    let mut index = String::new();
    let mut bodies = String::new();
    for (position, item) in admitted.iter().enumerate() {
        let (display_name, kind) = labels[position].clone();
        let row = IndexRow {
            schema_version: ASSET_SCHEMA_VERSION,
            id: item.entry.id.clone(),
            display_name,
            kind,
            base_url: item.entry.base_url.clone(),
        };
        index.push_str(
            &serde_json::to_string(&row)
                .map_err(|error| format!("could not encode index row: {error}"))?,
        );
        index.push('\n');
        let body = BodyRow {
            id: item.entry.id.clone(),
            definition_yaml: item.source.clone(),
        };
        bodies.push_str(
            &serde_json::to_string(&body)
                .map_err(|error| format!("could not encode body row: {error}"))?,
        );
        bodies.push('\n');
    }

    let mut accepted_by_version: BTreeMap<String, usize> = SOURCE_VERSIONS
        .iter()
        .map(|version| (version.to_string(), 0))
        .collect();
    for item in &admitted {
        *accepted_by_version
            .entry(item.version.to_string())
            .or_default() += 1;
    }
    let report = SyncReport {
        schema_version: ASSET_SCHEMA_VERSION,
        source_dir: source_dir.display().to_string(),
        source_versions: SOURCE_VERSIONS.iter().map(ToString::to_string).collect(),
        candidates: candidates.len(),
        accepted: admitted.len(),
        accepted_by_version,
        baked: admitted
            .iter()
            .map(|item| Baked {
                id: item.entry.id.clone(),
                file: item.file.clone(),
                source_version: item.version.to_string(),
            })
            .collect(),
        excluded,
    };
    Ok((index, bodies, report))
}

/// Give every option a label an operator can tell apart.
///
/// Trackers do reuse display names across definitions, and a select with two
/// identical rows is unusable, so a repeated `name (type)` pair falls back to
/// disambiguating on the definition id.
fn unique_labels(admitted: &[Admitted]) -> Vec<(String, String)> {
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for item in admitted {
        *counts
            .entry((item.entry.display_name.clone(), item.entry.kind.clone()))
            .or_default() += 1;
    }
    admitted
        .iter()
        .map(|item| {
            let key = (item.entry.display_name.clone(), item.entry.kind.clone());
            let display_name = if counts.get(&key).copied().unwrap_or_default() > 1 {
                format!("{} [{}]", item.entry.display_name, item.entry.id)
            } else {
                item.entry.display_name.clone()
            };
            (display_name, item.entry.kind.clone())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The generator half of `xtask cardigann sync-definitions`.
    ///
    /// It is inert unless the xtask points it at a Prowlarr checkout, and it
    /// only ever writes to the paths the xtask hands it — never into the source
    /// tree. The xtask moves the rendered files into place itself.
    #[test]
    fn writes_definition_gate_report() {
        let Some(source_dir) = std::env::var_os(SOURCE_DIR_ENV) else {
            return;
        };
        let source_dir = PathBuf::from(source_dir);
        let (index, bodies, report) = sync(&source_dir).expect("gate the definition corpus");
        for (variable, contents) in [
            (INDEX_OUT_ENV, index),
            (BODIES_OUT_ENV, bodies),
            (
                REPORT_OUT_ENV,
                serde_json::to_string_pretty(&report).expect("encode the sync report"),
            ),
        ] {
            let path = std::env::var_os(variable)
                .unwrap_or_else(|| panic!("{variable} must be set alongside {SOURCE_DIR_ENV}"));
            std::fs::write(&path, contents)
                .unwrap_or_else(|error| panic!("write {variable}: {error}"));
        }
        eprintln!(
            "Cardigann definition gate: candidates={}, accepted={}, excluded={}",
            report.candidates,
            report.accepted,
            report.excluded.len()
        );
    }

    #[test]
    fn selects_each_definition_from_the_highest_version_that_has_it() {
        let root = std::env::temp_dir().join(format!(
            "cardigann-gate-select-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        for (version, file) in [
            ("v10", "older.yml"),
            ("v11", "fixture.yml"),
            ("v11", "only-v11.yml"),
            ("v11", "schema.json"),
            ("v12", "fixture.yml"),
            ("v12", "schema.json"),
            ("v13", "newer.yml"),
        ] {
            let directory = root.join(version);
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(directory.join(file), version).unwrap();
        }

        let selected = select_candidates(&root).unwrap();
        let summary = selected
            .iter()
            .map(|candidate| (candidate.file.as_str(), candidate.version))
            .collect::<Vec<_>>();
        assert_eq!(summary, [("fixture.yml", "v12"), ("only-v11.yml", "v11")]);
        assert_eq!(
            std::fs::read_to_string(&selected[0].path).unwrap(),
            "v12",
            "the v12 copy wins"
        );

        std::fs::remove_dir_all(root.join("v12")).unwrap();
        assert!(
            select_candidates(&root)
                .unwrap_err()
                .contains("has no v12 directory")
        );
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn gate_rejects_definitions_that_cannot_reach_a_request() {
        assert!(gate_definition("not: a definition").is_err());
        let entry = gate_definition(
            r#"
id: fixture
name: Fixture
type: public
links: [https://tracker.example/]
caps: {}
search:
  paths: [{ path: search }]
"#,
        )
        .expect("a minimal definition clears the gate");
        assert_eq!(entry.id, "fixture");
        assert_eq!(entry.display_name, "Fixture");
        assert_eq!(entry.kind, "public");
        assert_eq!(entry.base_url, "https://tracker.example/");
    }

    /// The refresh workflow checks out and guards the directories the upstream
    /// record lists, while the sync reads [`SOURCE_VERSIONS`]; the two must
    /// name the same directories.
    #[test]
    fn source_versions_match_the_upstream_record() {
        let record: serde_json::Value =
            serde_json::from_str(include_str!("../known_cardigann_definitions.upstream.json"))
                .expect("parse the upstream record");
        let dirs: Vec<&str> = record["source_versions"]
            .as_array()
            .expect("source_versions is an array")
            .iter()
            .map(|source| source["dir"].as_str().expect("dir is a string"))
            .collect();
        assert_eq!(dirs, SOURCE_VERSIONS);
        assert_eq!(record["tracked_version"], *SOURCE_VERSIONS.last().unwrap());
    }
}
