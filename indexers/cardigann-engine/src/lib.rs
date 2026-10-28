//! Cardigann v12 tracker definitions as a Scryer WASIp2 indexer component.
//!
//! The engine itself (definition parsing, the Go-template renderer, the filter
//! chain, and the search/login/grab flow machine) is host-independent: it is a
//! pure state machine that asks for one HTTP attempt at a time. This module is
//! the whole ABI boundary — descriptor, configuration, and the async component
//! `search`/`action` exports.

use std::collections::{BTreeMap, HashSet};

use scryer_plugin_pdk::{
    CONFIG_FIELDS_ACTION, ConfigFieldsRequest, ConfigFieldsResponse, Error, FnResult, component,
    plugin_error, sdk,
};
use scryer_plugin_sdk::command::{PluginActionRequest, PluginActionResponse};
use serde_json::Value;
use url::Url;

mod baked;
mod definition;
mod filters;
#[cfg(test)]
mod gate;
mod info_text;
#[cfg(test)]
mod risk;
mod runtime;
mod template;

use baked::CUSTOM_DEFINITION_ID;
use definition::{Definition, Setting};
use runtime::EngineAction;

const CARDIGANN_DEFINITION_VERSION: u16 = 12;
const HOST_CONFIG_KEYS: [&str; 5] = [
    "base_url",
    "username",
    "password",
    "cookie",
    "cardigannCaptcha",
];

fn build_descriptor() -> sdk::PluginDescriptor {
    sdk::PluginDescriptor {
        id: "cardigann-engine".to_string(),
        name: "Advanced Indexers".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        sdk_version: sdk::SDK_VERSION.to_string(),
        sdk_constraint: sdk::current_sdk_constraint(),
        socket_permissions: vec![],
        settings: vec![],
        provider: sdk::ProviderDescriptor::Indexer(sdk::IndexerDescriptor {
            provider_type: "cardigann".to_string(),
            provider_aliases: vec![],
            // The operator supplies the definition; there is nothing generic to
            // preset, and a profile must never assert provider facts the pasted
            // definition contradicts.
            provider_profiles: vec![],
            // Cardigann definitions describe arbitrary third-party trackers, so
            // this engine cannot attest that an empty or truncated page means
            // the tracker had nothing more. Leaving this unset keeps the host
            // from crediting convergence coverage to those responses.
            search_semantics_version: None,
            // Deliberately no strategy plan: one configured Cardigann indexer
            // shares a single login session and the definition's `requestdelay`,
            // so fanning several strategies out inside one invocation would race
            // concurrent logins against one cookie jar. The host runs the tiers
            // sequentially instead.
            strategy_plan: None,
            source_kind: sdk::IndexerSourceKind::Torrent,
            capabilities: sdk::IndexerCapabilities {
                raw_nab_search: true,
                rss: true,
                search: true,
                imdb_search: true,
                tvdb_search: true,
                // Cardigann has no AniDB concept: no definition binds an AniDB
                // id, so advertising it would route id searches this engine
                // cannot answer.
                anidb_search: false,
                // The engine binds `.Query.IMDBID`, `.Query.TVDBID` and
                // `.Query.TMDBID` from the host's `<source>_id` keys, so the
                // host has to know it may route those ids here at all.
                supported_ids: std::collections::HashMap::from([
                    (
                        "movie".to_string(),
                        vec!["imdb_id".to_string(), "tmdb_id".to_string()],
                    ),
                    (
                        "series".to_string(),
                        vec![
                            "tvdb_id".to_string(),
                            "imdb_id".to_string(),
                            "tmdb_id".to_string(),
                        ],
                    ),
                    (
                        "anime".to_string(),
                        vec![
                            "tvdb_id".to_string(),
                            "imdb_id".to_string(),
                            "tmdb_id".to_string(),
                        ],
                    ),
                ]),
                supported_external_ids: vec![
                    "imdb_id".to_string(),
                    "tvdb_id".to_string(),
                    "tmdb_id".to_string(),
                    "tvmaze_id".to_string(),
                    "tvrage_id".to_string(),
                ],
                // 90 definitions expand `.Query.Season`/`.Query.Ep`, which the
                // engine already binds; these name them for the host.
                season_param: Some("season".to_string()),
                episode_param: Some("ep".to_string()),
                query_param: Some("query".to_string()),
                protocols: vec![sdk::IndexerProtocol::Torrent],
                feed_modes: vec![
                    sdk::IndexerFeedMode::Recent,
                    sdk::IndexerFeedMode::Rss,
                    sdk::IndexerFeedMode::AutomaticSearch,
                    sdk::IndexerFeedMode::InteractiveSearch,
                ],
                search_inputs: vec![
                    sdk::IndexerSearchInput::TextQuery,
                    sdk::IndexerSearchInput::IdQuery,
                    sdk::IndexerSearchInput::AggregateIdQuery,
                    sdk::IndexerSearchInput::Season,
                    sdk::IndexerSearchInput::Episode,
                    sdk::IndexerSearchInput::Category,
                    sdk::IndexerSearchInput::Offset,
                    sdk::IndexerSearchInput::Limit,
                ],
                // Tracker origins come from the configured definition, so the
                // host asks the engine for them instead of a static allowlist.
                resolve_network_endpoints: true,
                // A bundled definition's own settings become form fields once
                // the operator picks it.
                dynamic_config_fields: Some(sdk::DynamicConfigFieldsCapability {
                    trigger_keys: vec!["definition".to_string()],
                }),
                ..sdk::IndexerCapabilities::default()
            },
            scoring_policies: vec![],
            config_fields: config_fields(),
            // The component host scopes transport to the configured connection
            // URL. Definition links are configuration, not static plugin
            // allowlist entries.
            allowed_hosts: vec![],
            rate_limit_seconds: None,
        }),
    }
}

fn config_fields() -> Vec<sdk::ConfigFieldDef> {
    let baked = baked::index().expect("bundled Cardigann definitions must be valid");
    vec![
        baked::selector(&baked),
        field(
            "base_url",
            "Base URL",
            sdk::ConfigFieldType::String,
            true,
            Some(sdk::ConfigFieldRole::ConnectionUrl),
            Some("The tracker URL selected from the definition's links."),
        ),
        // Only meaningful for Custom, and required there. This used to be
        // unconditionally optional with the rule explained in prose and
        // enforced later by `load_definition_source`; the form can state it now.
        sdk::ConfigFieldDef {
            visible_when: Some(custom_definition_selected()),
            required_when: Some(custom_definition_selected()),
            ..field(
                "definition_yaml",
                "Cardigann Definition",
                sdk::ConfigFieldType::Multiline,
                false,
                None,
                Some("A Cardigann v12 YAML definition; v11 definitions work too."),
            )
        },
        sdk::ConfigFieldDef {
            advanced: true,
            ..field(
                "extra_field_data_json",
                "Definition Settings JSON",
                sdk::ConfigFieldType::Multiline,
                false,
                None,
                Some("JSON object for definition-specific Cardigann settings."),
            )
        },
        field(
            "username",
            "Username",
            sdk::ConfigFieldType::String,
            false,
            None,
            None,
        ),
        field(
            "password",
            "Password",
            sdk::ConfigFieldType::Password,
            false,
            None,
            None,
        ),
        sdk::ConfigFieldDef {
            advanced: true,
            ..field(
                "cookie",
                "Cookie",
                sdk::ConfigFieldType::Password,
                false,
                None,
                Some("Optional initial tracker cookie."),
            )
        },
        // Shown only for a definition whose login has a CAPTCHA; Scryer then
        // fetches the image through `checkCaptcha` and shows it beside this
        // field.
        field(
            "cardigannCaptcha",
            "CAPTCHA Answer",
            sdk::ConfigFieldType::String,
            false,
            Some(sdk::ConfigFieldRole::LoginChallengeAnswer),
            Some("Type the characters in the CAPTCHA image, then save."),
        ),
    ]
}

/// The Tracker Definition sitting on Custom, which is when a pasted definition
/// is both the only source and a required one.
fn custom_definition_selected() -> sdk::FieldCondition {
    sdk::FieldCondition {
        key: "definition".to_string(),
        op: sdk::ConditionOp::Eq,
        values: vec![baked::CUSTOM_DEFINITION_ID.to_string()],
    }
}

fn field(
    key: &str,
    label: &str,
    field_type: sdk::ConfigFieldType,
    required: bool,
    role: Option<sdk::ConfigFieldRole>,
    help_text: Option<&str>,
) -> sdk::ConfigFieldDef {
    sdk::ConfigFieldDef {
        key: key.to_string(),
        label: label.to_string(),
        field_type,
        required,
        default_value: None,
        value_source: sdk::ConfigFieldValueSource::User,
        role,
        host_binding: None,
        options: vec![],
        help_text: help_text.map(str::to_string),
        ..Default::default()
    }
}

async fn search(request: sdk::PluginSearchRequest) -> FnResult<sdk::PluginSearchResponse> {
    let (definition, config) = load_definition_and_config()?;
    runtime::search(definition, request, config)
        .await
        .map_err(engine_error)
}

/// The engine's failure as the component reports it: a typed SDK error keeps
/// its code and operator-facing message through the PDK's result adapter, and
/// anything else stays the generic component failure with the text as detail.
fn engine_error(error: runtime::EngineError) -> Error {
    match error {
        runtime::EngineError::Message(message) => Error::msg(message),
        runtime::EngineError::Plugin(error) => component::structured_plugin_error(error),
    }
}

/// The component's named-action surface.
///
/// `checkCaptcha` runs the definition's login flow far enough to fetch the
/// image challenge and hands the operator its bytes; the answer comes back in
/// the `cardigannCaptcha` setting. `grab` runs the definition's authenticated
/// download flow for one release URL, which is how Scryer's download router
/// obtains an artifact a tracker will not serve to an unauthenticated fetch.
/// Anything else is reported as unsupported so the host can keep its own
/// fallbacks.
async fn action(request: PluginActionRequest) -> FnResult<PluginActionResponse> {
    if request.action == CONFIG_FIELDS_ACTION {
        let request: ConfigFieldsRequest =
            serde_json::from_value(request.payload).map_err(|error| {
                plugin_error(
                    sdk::PluginErrorCode::Permanent,
                    "The config fields request was malformed.",
                )
                .debug(error)
                .into_error()
            })?;
        let response = dynamic_config_fields(&request)?;
        return Ok(PluginActionResponse {
            payload: serde_json::to_value(response)?,
        });
    }
    if request.action == "resolve_network_endpoints" {
        let (definition, config) = load_definition_and_config()?;
        let origins =
            runtime::resolve_network_endpoints(&definition, &config).map_err(endpoint_error)?;
        return Ok(PluginActionResponse {
            payload: serde_json::json!({ "origins": origins }),
        });
    }
    let action = match request.action.as_str() {
        "checkCaptcha" | "check_captcha" => EngineAction::CheckCaptcha,
        "grab" => EngineAction::Grab(action_url(&request.payload).map_err(|error| {
            plugin_error(
                sdk::PluginErrorCode::Permanent,
                "The grab action needs a release URL.",
            )
            .debug(error)
            .into_error()
        })?),
        action => {
            return Err(component::structured_plugin_error(
                component::action_unsupported(action),
            ));
        }
    };
    let (definition, config) = load_definition_and_config()?;
    Ok(PluginActionResponse {
        payload: runtime::action(definition, action, config)
            .await
            .map_err(engine_error)?,
    })
}

/// Static setting keys a definition setting can never reuse as a dynamic
/// field: the host rejects a dynamic field that shadows a static one, and the
/// engine already reads these from their static fields.
const STATIC_CONFIG_KEYS: [&str; 8] = [
    "definition",
    "base_url",
    "definition_yaml",
    "extra_field_data_json",
    "username",
    "password",
    "cookie",
    "cardigannCaptcha",
];

/// Static credential fields a definition may or may not use.
const CREDENTIAL_KEYS: [&str; 4] = ["username", "password", "cookie", "cardigannCaptcha"];

/// The form fields for the selected definition.
///
/// A bundled definition contributes its own settings as fields and hides the
/// static credential fields it does not use. With no definition selected the
/// credential fields are hidden until one is. Custom keeps the static form:
/// its settings live in the pasted YAML, which is not a trigger value.
fn dynamic_config_fields(request: &ConfigFieldsRequest) -> FnResult<ConfigFieldsResponse> {
    let selected = request
        .values
        .get("definition")
        .map(|value| value.trim())
        .filter(|value| !value.is_empty());
    let Some(id) = selected else {
        return Ok(ConfigFieldsResponse {
            fields: Vec::new(),
            hidden_field_keys: CREDENTIAL_KEYS.iter().map(|key| key.to_string()).collect(),
        });
    };
    if id == CUSTOM_DEFINITION_ID {
        return Ok(ConfigFieldsResponse::default());
    }
    let yaml = baked::definition_yaml(id)
        .map_err(|error| invalid_setting("definition", error))?
        .ok_or_else(|| invalid_setting("definition", format!("`{id}` is not bundled")))?;
    let definition =
        parse_definition(&yaml).map_err(|error| invalid_setting("definition", error))?;
    Ok(definition_config_fields(&definition))
}

fn definition_config_fields(definition: &Definition) -> ConfigFieldsResponse {
    let fields = definition
        .settings
        .iter()
        .filter(|setting| !STATIC_CONFIG_KEYS.contains(&setting.name.as_str()))
        .filter_map(setting_field)
        .take(256)
        .collect();
    let uses = |key: &str| match key {
        "cardigannCaptcha" => definition
            .login
            .as_ref()
            .is_some_and(|login| login.captcha.is_some()),
        key => definition
            .settings
            .iter()
            .any(|setting| setting.name == key),
    };
    let hidden_field_keys = CREDENTIAL_KEYS
        .into_iter()
        .filter(|key| !uses(key))
        .map(str::to_string)
        .collect();
    ConfigFieldsResponse {
        fields,
        hidden_field_keys,
    }
}

/// One definition setting as a form field. Informational settings become
/// read-only notes that never take a value; setting types the engine does not
/// bind are left out.
fn setting_field(setting: &Setting) -> Option<sdk::ConfigFieldDef> {
    let key = setting.name.trim();
    if key.is_empty() || key.len() > 128 {
        return None;
    }
    if setting.setting_type.starts_with("info") {
        let default_text = setting
            .default_value
            .as_ref()
            .and_then(definition::scalar_to_string);
        let (label, text) = info_text::info_note(
            &setting.setting_type,
            setting.label.as_deref(),
            default_text.as_deref(),
        )?;
        return Some(field(
            key,
            &label,
            sdk::ConfigFieldType::Info,
            false,
            None,
            (!text.is_empty()).then_some(text.as_str()),
        ));
    }
    let default = setting
        .default_value
        .as_ref()
        .and_then(definition::scalar_to_string);
    let (field_type, options, default_value) = match setting.setting_type.as_str() {
        "text" => (sdk::ConfigFieldType::String, Vec::new(), default),
        "password" => (sdk::ConfigFieldType::Password, Vec::new(), None),
        "checkbox" => (
            sdk::ConfigFieldType::Bool,
            Vec::new(),
            default.map(|value| {
                let on = value == "1"
                    || value.eq_ignore_ascii_case("true")
                    || value.eq_ignore_ascii_case("on");
                on.to_string()
            }),
        ),
        "select" => {
            let options = select_options(setting.options.as_ref());
            if options.is_empty() {
                return None;
            }
            let default_value =
                default.filter(|value| options.iter().any(|option| &option.value == value));
            (sdk::ConfigFieldType::Select, options, default_value)
        }
        _ => return None,
    };
    let label = setting
        .label
        .as_deref()
        .map(str::trim)
        .filter(|label| !label.is_empty())
        .unwrap_or(key);
    // The engine falls back to the definition's default, so an operator only
    // has to fill in a required setting that has none.
    let required = setting.required && default_value.is_none();
    Some(sdk::ConfigFieldDef {
        default_value,
        options,
        ..field(
            key,
            label,
            field_type,
            required,
            None,
            setting.help_text.as_deref(),
        )
    })
}

fn select_options(options: Option<&serde_yaml::Value>) -> Vec<sdk::ConfigFieldOption> {
    let Some(serde_yaml::Value::Mapping(options)) = options else {
        return Vec::new();
    };
    options
        .iter()
        .filter_map(|(value, label)| {
            let value = definition::scalar_to_string(value)?;
            let label = definition::scalar_to_string(label).unwrap_or_else(|| value.clone());
            Some(sdk::ConfigFieldOption {
                value,
                label,
                config_overrides: BTreeMap::new(),
            })
        })
        .collect()
}

fn action_url(payload: &Value) -> Result<String, String> {
    let url = match payload {
        Value::String(url) => Some(url.as_str()),
        Value::Object(object) => object.get("url").and_then(Value::as_str),
        _ => None,
    }
    .map(str::trim)
    .filter(|url| !url.is_empty())
    .ok_or_else(|| "grab action requires a non-empty URL payload".to_string())?;
    Ok(url.to_string())
}

/// The YAML for the configured indexer, from whichever of the two sources the
/// operator chose.
///
/// A bundled selection wins over a pasted definition: switching a configured
/// indexer from Custom to a bundled tracker would otherwise keep silently using
/// the stale paste. Custom — the contrib and testing escape hatch — is the only
/// mode that requires `definition_yaml`, and says so when it is missing.
/// The source YAML and the setting key that supplied it, which is the key an
/// invalid definition is reported against.
fn load_definition_source() -> Result<(String, &'static str), Error> {
    let selected = host_config("definition").map_err(Error::msg)?;
    let pasted = host_config("definition_yaml").map_err(Error::msg)?;
    let key = definition_source_key(selected.as_deref());
    resolve_definition_source(selected.as_deref(), pasted.as_deref())
        .map(|source| (source, key))
        .map_err(|error| invalid_setting(key, error))
}

/// `definition_yaml` when the operator pastes the definition (Custom, or no
/// selection), otherwise `definition`.
fn definition_source_key(selected: Option<&str>) -> &'static str {
    match selected.map(str::trim).filter(|value| !value.is_empty()) {
        None | Some(CUSTOM_DEFINITION_ID) => "definition_yaml",
        Some(_) => "definition",
    }
}

/// Endpoints the configured URL or the definition name that the plugin
/// cannot declare to the host: a setting the operator has to fix.
fn endpoint_error(detail: impl std::fmt::Display) -> Error {
    plugin_error(
        sdk::PluginErrorCode::InvalidConfig,
        "The indexer's URL or definition names an endpoint the plugin cannot use.",
    )
    .debug(detail)
    .into_error()
}

/// A missing or invalid setting, named for the operator; the detail stays in
/// the debug message.
fn invalid_setting(key: &str, detail: impl std::fmt::Display) -> Error {
    plugin_error(
        sdk::PluginErrorCode::InvalidConfig,
        format!("The {key} setting is missing or invalid."),
    )
    .debug(detail)
    .into_error()
}

fn resolve_definition_source(
    selected: Option<&str>,
    pasted: Option<&str>,
) -> Result<String, String> {
    let selected = selected.map(str::trim).filter(|value| !value.is_empty());
    match selected {
        None | Some(CUSTOM_DEFINITION_ID) => pasted
            .filter(|value| !value.trim().is_empty())
            .map(str::to_string)
            .ok_or_else(|| {
                "`definition_yaml` must be configured when `definition` is `Custom`; pick a \
                 bundled Cardigann definition instead, or paste a Cardigann v12 YAML definition"
                    .to_string()
            }),
        Some(id) => baked::definition_yaml(id)?.ok_or_else(|| {
            format!(
                "`{id}` is not a bundled Cardigann definition; pick one from the `definition` \
                 list, or select `Custom` and paste the definition into `definition_yaml`"
            )
        }),
    }
}

fn load_definition_and_config() -> Result<(Definition, BTreeMap<String, String>), Error> {
    let (definition_yaml, source_key) = load_definition_source()?;
    let definition =
        parse_definition(&definition_yaml).map_err(|error| invalid_setting(source_key, error))?;
    let mut config = BTreeMap::new();
    for key in HOST_CONFIG_KEYS.into_iter().chain(
        definition
            .settings
            .iter()
            .map(|setting| setting.name.as_str()),
    ) {
        if let Some(value) = host_config(key).map_err(Error::msg)?
            && !value.trim().is_empty()
        {
            config.insert(key.to_string(), value);
        }
    }
    merge_extra_field_data(
        &mut config,
        host_config("extra_field_data_json").map_err(Error::msg)?,
    )
    .map_err(|error| invalid_setting("extra_field_data_json", error))?;
    if config
        .get("base_url")
        .is_none_or(|base_url| base_url.trim().is_empty())
    {
        return Err(invalid_setting("base_url", "base_url must be configured"));
    }
    validate_configured_base_url(&definition, &config)
        .map_err(|error| invalid_setting("base_url", error))?;
    Ok((definition, config))
}

fn validate_configured_base_url(
    definition: &Definition,
    config: &BTreeMap<String, String>,
) -> Result<(), String> {
    let configured = config
        .get("base_url")
        .expect("base_url was checked before validation");
    let configured = Url::parse(configured)
        .map_err(|error| format!("invalid base_url `{configured}`: {error}"))?;
    let configured_origin = configured.origin();
    let matches_definition = definition
        .links
        .iter()
        .chain(&definition.legacy_links)
        .map(|link| {
            Url::parse(link).map_err(|error| format!("invalid definition link `{link}`: {error}"))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .any(|link| link.origin() == configured_origin);
    if matches_definition {
        Ok(())
    } else {
        Err(format!(
            "base_url `{configured}` has an origin not declared by the Cardigann definition"
        ))
    }
}

/// One descriptor-bound configuration value from the component host.
fn host_config(key: &str) -> Result<Option<String>, String> {
    Ok(component::config_get(key))
}

fn merge_extra_field_data(
    config: &mut BTreeMap<String, String>,
    raw: Option<String>,
) -> Result<(), String> {
    let Some(raw) = raw.filter(|value| !value.trim().is_empty()) else {
        return Ok(());
    };
    let Value::Object(values) = serde_json::from_str::<Value>(&raw)
        .map_err(|error| format!("invalid extra_field_data_json: {error}"))?
    else {
        return Err("extra_field_data_json must be a JSON object".to_string());
    };
    for (key, value) in values {
        let value = match value {
            Value::String(value) => value,
            Value::Bool(value) => value.to_string(),
            Value::Number(value) => value.to_string(),
            Value::Null => continue,
            Value::Array(_) | Value::Object(_) => {
                return Err(format!(
                    "extra_field_data_json value `{key}` must be a string, number, boolean, or null"
                ));
            }
        };
        config.insert(key, value);
    }
    Ok(())
}

fn parse_definition(source: &str) -> Result<Definition, String> {
    let definition: Definition = serde_yaml::from_str(source.trim_start_matches('\u{feff}'))
        .map_err(|error| {
            format!("invalid Cardigann v{CARDIGANN_DEFINITION_VERSION} definition: {error}")
        })?;
    validate_metadata(&definition)?;
    definition.validate_supported()?;
    Ok(definition)
}

fn validate_metadata(definition: &Definition) -> Result<(), String> {
    if definition.id.trim().is_empty() {
        return Err("definition id must not be empty".to_string());
    }
    if definition.name.trim().is_empty() {
        return Err("definition name must not be empty".to_string());
    }
    if definition.definition_type.trim().is_empty() {
        return Err("definition type must not be empty".to_string());
    }
    if definition.links.is_empty() {
        return Err("definition links must contain at least one base URL".to_string());
    }
    let mut setting_names = HashSet::new();
    for Setting {
        name, setting_type, ..
    } in &definition.settings
    {
        if name.trim().is_empty() {
            return Err("definition setting name must not be empty".to_string());
        }
        if setting_type.trim().is_empty() {
            return Err(format!("setting `{name}` has no type"));
        }
        if !setting_names.insert(name.as_str()) {
            return Err(format!("duplicate definition setting `{name}`"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod component_abi_tests {
    use super::*;

    #[test]
    fn an_unusable_endpoint_is_an_invalid_setting() {
        let error = scryer_plugin_pdk::error::plugin_error_from(&endpoint_error(
            "Endpoint authorities must be HTTP(S) URLs without credentials",
        ));
        assert_eq!(error.code, sdk::PluginErrorCode::InvalidConfig);
        assert!(!error.public_message.contains("credentials"));
    }

    const DEFINITION: &str = r#"
id: example
name: Example Tracker
description: Test definition
language: en-US
type: private
encoding: UTF-8
links: [https://tracker.example/path]
legacylinks: [https://legacy.tracker.example/]
caps: {}
search:
  paths:
    - path: /search
"#;

    /// A typed engine failure keeps its code and operator message through the
    /// PDK's result adapter; anything else stays the generic component error.
    #[test]
    fn engine_errors_reach_the_host_with_their_sdk_code() {
        let typed = plugin_error(
            sdk::PluginErrorCode::AuthFailed,
            "Tracker login failed: synthetic reason",
        )
        .debug("detail")
        .build();
        let result = component::to_plugin_result::<()>(Err(engine_error(
            runtime::EngineError::Plugin(typed.clone()),
        )));
        let sdk::PluginResult::Err(error) = result else {
            panic!("typed error expected")
        };
        assert_eq!(error.code, typed.code);
        assert_eq!(error.public_message, typed.public_message);
        assert_eq!(error.debug_message, typed.debug_message);

        let result = component::to_plugin_result::<()>(Err(engine_error(
            runtime::EngineError::Message("engine detail".to_string()),
        )));
        let sdk::PluginResult::Err(error) = result else {
            panic!("generic error expected")
        };
        assert_eq!(error.code, sdk::PluginErrorCode::Temporary);
        assert_eq!(error.debug_message.as_deref(), Some("engine detail"));
    }

    fn config_fields_for(definition: Option<&str>) -> ConfigFieldsResponse {
        let mut values = BTreeMap::new();
        if let Some(definition) = definition {
            values.insert("definition".to_string(), definition.to_string());
        }
        match dynamic_config_fields(&ConfigFieldsRequest { values }) {
            Ok(response) => response,
            Err(error) => panic!("config fields failed: {error}"),
        }
    }

    /// With nothing selected the credential fields wait for a definition;
    /// Custom keeps the whole static form.
    #[test]
    fn config_fields_hide_credentials_until_a_definition_is_chosen() {
        for none in [None, Some(""), Some("  ")] {
            let response = config_fields_for(none);
            assert!(response.fields.is_empty());
            assert_eq!(
                response.hidden_field_keys,
                vec!["username", "password", "cookie", "cardigannCaptcha"]
            );
        }
        assert_eq!(
            config_fields_for(Some(CUSTOM_DEFINITION_ID)),
            ConfigFieldsResponse::default()
        );
        let error = component::to_plugin_result::<()>(
            dynamic_config_fields(&ConfigFieldsRequest {
                values: BTreeMap::from([(
                    "definition".to_string(),
                    "synthetic-not-bundled".to_string(),
                )]),
            })
            .map(|_| ()),
        );
        let sdk::PluginResult::Err(error) = error else {
            panic!("unknown definition must be rejected")
        };
        assert_eq!(error.code, sdk::PluginErrorCode::InvalidConfig);
        assert!(error.public_message.contains("definition"));
    }

    /// A definition's settings become fields typed the way the engine binds
    /// them, info settings become read-only notes, credentials it does not use
    /// are hidden, and settings that shadow a static field stay out.
    #[test]
    fn config_fields_turn_definition_settings_into_fields() {
        let definition = parse_definition(&format!(
            "{DEFINITION}settings:\n  - {{name: cookie, type: text, label: Cookie}}\n  - {{name: info_note, type: info, label: Note, default: Read me}}\n  - {{name: apikey, type: text, label: API Key}}\n  - {{name: passkey, type: password, label: Passkey, default: should-not-leak}}\n  - {{name: freeleech, type: checkbox, label: Freeleech only, default: true}}\n  - {{name: sort, type: select, label: Sort, default: seeders, options: {{created: Created, seeders: Seeders, 7: Week}}}}\n  - {{name: order, type: select, label: Order, default: missing, options: {{asc: Up, desc: Down}}}}\n  - {{name: required_key, type: text, label: Required, required: true}}\n"
        ))
        .unwrap();
        let response = definition_config_fields(&definition);
        let keys: Vec<_> = response
            .fields
            .iter()
            .map(|field| field.key.as_str())
            .collect();
        assert_eq!(
            keys,
            [
                "info_note",
                "apikey",
                "passkey",
                "freeleech",
                "sort",
                "order",
                "required_key"
            ]
        );
        let field = |key: &str| {
            response
                .fields
                .iter()
                .find(|field| field.key == key)
                .unwrap()
        };
        // An info setting is a read-only note: its text is help, never a value.
        assert_eq!(field("info_note").field_type, sdk::ConfigFieldType::Info);
        assert_eq!(field("info_note").label, "Note");
        assert_eq!(field("info_note").help_text.as_deref(), Some("Read me"));
        assert_eq!(field("info_note").default_value, None);
        assert_eq!(field("apikey").field_type, sdk::ConfigFieldType::String);
        assert_eq!(field("apikey").label, "API Key");
        assert_eq!(field("passkey").field_type, sdk::ConfigFieldType::Password);
        assert_eq!(field("passkey").default_value, None);
        assert_eq!(field("freeleech").field_type, sdk::ConfigFieldType::Bool);
        assert_eq!(field("freeleech").default_value.as_deref(), Some("true"));
        assert_eq!(field("sort").field_type, sdk::ConfigFieldType::Select);
        assert_eq!(field("sort").default_value.as_deref(), Some("seeders"));
        assert_eq!(
            field("sort")
                .options
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            ["created", "seeders", "7"]
        );
        // A default that names no option is dropped rather than rejected.
        assert_eq!(field("order").default_value, None);
        assert!(field("required_key").required);
        assert!(!field("apikey").required);
        assert!(response.fields.iter().all(|field| {
            field.value_source == sdk::ConfigFieldValueSource::User
                && field.role.is_none()
                && field.host_binding.is_none()
        }));
        // `cookie` is used, so only the unused credentials are hidden.
        assert_eq!(
            response.hidden_field_keys,
            vec!["username", "password", "cardigannCaptcha"]
        );
    }

    /// Every bundled definition answers within the host's rules: unique keys
    /// that never shadow a static field, select fields with options, and
    /// defaults the field type accepts.
    #[test]
    fn config_fields_for_every_bundled_definition_pass_host_rules() {
        let rows = baked::index().unwrap();
        let mut with_fields = 0;
        let mut with_notes = 0;
        for row in &rows {
            let response = config_fields_for(Some(&row.id));
            let mut keys = HashSet::new();
            assert!(response.fields.len() <= 256, "{}", row.id);
            for field in &response.fields {
                assert!(
                    keys.insert(field.key.clone()),
                    "{}: duplicate {}",
                    row.id,
                    field.key
                );
                assert!(
                    !STATIC_CONFIG_KEYS.contains(&field.key.as_str()),
                    "{}: shadows {}",
                    row.id,
                    field.key
                );
                if let Some(default) = field.default_value.as_deref() {
                    match field.field_type {
                        sdk::ConfigFieldType::Bool => {
                            assert!(matches!(default, "true" | "false"), "{}", row.id)
                        }
                        sdk::ConfigFieldType::Select => assert!(
                            field.options.iter().any(|option| option.value == default),
                            "{}: {}",
                            row.id,
                            field.key
                        ),
                        _ => {}
                    }
                }
                if field.field_type == sdk::ConfigFieldType::Select {
                    assert!(!field.options.is_empty(), "{}: {}", row.id, field.key);
                }
                // The host refuses a note that asks for a value.
                if field.field_type == sdk::ConfigFieldType::Info {
                    assert!(
                        !field.required
                            && field.required_when.is_none()
                            && field.options.is_empty()
                            && field.default_value.is_none()
                            && !field.label.trim().is_empty(),
                        "{}: {}",
                        row.id,
                        field.key
                    );
                    with_notes += 1;
                }
            }
            for key in &response.hidden_field_keys {
                assert!(CREDENTIAL_KEYS.contains(&key.as_str()), "{}", row.id);
            }
            with_fields += usize::from(!response.fields.is_empty());
        }
        assert!(with_fields > 0);
        assert!(with_notes > 0, "bundled definitions carry info settings");
    }

    /// Config failures name the setting the operator has to fix.
    #[test]
    fn config_errors_name_the_setting() {
        assert_eq!(definition_source_key(None), "definition_yaml");
        assert_eq!(
            definition_source_key(Some(CUSTOM_DEFINITION_ID)),
            "definition_yaml"
        );
        assert_eq!(definition_source_key(Some("synthetic")), "definition");
        let result = component::to_plugin_result::<()>(Err(invalid_setting(
            "base_url",
            "base_url `https://tracker.example/` has an origin not declared",
        )));
        let sdk::PluginResult::Err(error) = result else {
            panic!("typed error expected")
        };
        assert_eq!(error.code, sdk::PluginErrorCode::InvalidConfig);
        assert_eq!(
            error.public_message,
            "The base_url setting is missing or invalid."
        );
        assert!(
            error
                .debug_message
                .as_deref()
                .is_some_and(|debug| debug.contains("origin not declared"))
        );
    }

    #[test]
    fn descriptor_is_a_connection_scoped_component_indexer() {
        let descriptor = build_descriptor();
        let sdk::ProviderDescriptor::Indexer(indexer) = descriptor.provider else {
            panic!("expected indexer descriptor");
        };
        assert_eq!(indexer.provider_type, "cardigann");
        assert!(indexer.allowed_hosts.is_empty());
        assert!(indexer.capabilities.search);
        let wire: Value = serde_json::from_slice(&descriptor_bytes()).unwrap();
        assert_eq!(wire["sdk_version"], sdk::SDK_VERSION);
        assert_eq!(wire["sdk_constraint"], sdk::current_sdk_constraint());
        assert_eq!(
            wire["provider"]["capabilities"]["resolve_network_endpoints"],
            true
        );
        assert_eq!(
            wire["provider"]["capabilities"]["dynamic_config_fields"]["trigger_keys"],
            serde_json::json!(["definition"])
        );
        assert_eq!(
            indexer
                .config_fields
                .iter()
                .find(|field| field.key == "base_url")
                .and_then(|field| field.role),
            Some(sdk::ConfigFieldRole::ConnectionUrl)
        );
    }

    /// Cardigann definitions read `.Query.IMDBID`, `.Query.TVDBID`,
    /// `.Query.TMDBID`, `.Query.Season` and `.Query.Ep`, so the descriptor has
    /// to tell the host it may route id and episode searches here. AniDB has no
    /// Cardigann counterpart and stays off.
    #[test]
    fn the_descriptor_advertises_the_ids_and_episode_inputs_the_engine_binds() {
        let descriptor = build_descriptor();
        let sdk::ProviderDescriptor::Indexer(indexer) = descriptor.provider else {
            panic!("expected indexer descriptor");
        };
        let capabilities = indexer.capabilities;
        assert!(!capabilities.anidb_search);
        assert_eq!(
            capabilities.supported_ids.get("movie"),
            Some(&vec!["imdb_id".to_string(), "tmdb_id".to_string()])
        );
        for facet in ["series", "anime"] {
            assert_eq!(
                capabilities.supported_ids.get(facet),
                Some(&vec![
                    "tvdb_id".to_string(),
                    "imdb_id".to_string(),
                    "tmdb_id".to_string(),
                ]),
                "{facet}"
            );
        }
        assert!(
            capabilities
                .supported_external_ids
                .contains(&"imdb_id".to_string())
        );
        assert_eq!(capabilities.season_param.as_deref(), Some("season"));
        assert_eq!(capabilities.episode_param.as_deref(), Some("ep"));
        for input in [
            sdk::IndexerSearchInput::IdQuery,
            sdk::IndexerSearchInput::AggregateIdQuery,
            sdk::IndexerSearchInput::Season,
            sdk::IndexerSearchInput::Episode,
        ] {
            assert!(capabilities.search_inputs.contains(&input), "{input:?}");
        }
        // One configured indexer still shares a login session, so the engine
        // deliberately declines a parallel strategy plan.
        assert!(indexer.strategy_plan.is_none());
    }

    /// One option per bundled definition is what makes this descriptor large,
    /// and the host carries descriptors in a custom section capped at 1 MiB.
    /// Options stay to an id, a label, and a base URL for exactly this reason;
    /// this is the guard that notices if something heavier creeps in.
    #[test]
    fn the_bundled_selector_leaves_descriptor_headroom() {
        let encoded = serde_json::to_vec(&build_descriptor()).expect("descriptor encodes");
        eprintln!("Cardigann descriptor: {} bytes", encoded.len());
        assert!(
            encoded.len() < 512 * 1024,
            "descriptor grew to {} bytes",
            encoded.len()
        );
    }

    #[test]
    fn parser_keeps_cardigann_schema_validation() {
        assert_eq!(parse_definition(DEFINITION).unwrap().id, "example");
    }

    #[test]
    fn configuration_leads_with_the_definition_selector_and_demotes_pasted_yaml() {
        let fields = config_fields();
        assert_eq!(fields[0].key, "definition");
        assert_eq!(fields[0].field_type, sdk::ConfigFieldType::FilteredSelect);
        let pasted = fields
            .iter()
            .find(|field| field.key == "definition_yaml")
            .expect("the paste-in escape hatch stays available");
        assert!(
            !pasted.required,
            "a bundled definition supplies the YAML, so the field is not required outright"
        );

        // It is required, and only shown, when the selector sits on Custom.
        let custom = |key: &str| (key == "definition").then_some("custom");
        let bundled = |key: &str| (key == "definition").then_some("0magnet");
        assert!(scryer_plugin_sdk_visible(pasted, custom));
        assert!(scryer_plugin_sdk_required(pasted, custom));
        assert!(!scryer_plugin_sdk_visible(pasted, bundled));
        assert!(!scryer_plugin_sdk_required(pasted, bundled));

        assert!(
            fields
                .iter()
                .find(|field| field.key == "base_url")
                .is_some_and(|field| field.required)
        );
    }

    /// The host evaluates conditions; these mirror its rules so a declaration
    /// change that would read differently there fails here first.
    fn scryer_plugin_sdk_visible<'a>(
        field: &sdk::ConfigFieldDef,
        value_of: impl Fn(&str) -> Option<&'a str>,
    ) -> bool {
        field
            .visible_when
            .as_ref()
            .is_none_or(|condition| condition_holds(condition, value_of(&condition.key)))
    }

    fn scryer_plugin_sdk_required<'a>(
        field: &sdk::ConfigFieldDef,
        value_of: impl Fn(&str) -> Option<&'a str> + Copy,
    ) -> bool {
        scryer_plugin_sdk_visible(field, value_of)
            && (field.required
                || field
                    .required_when
                    .as_ref()
                    .is_some_and(|condition| condition_holds(condition, value_of(&condition.key))))
    }

    fn condition_holds(condition: &sdk::FieldCondition, value: Option<&str>) -> bool {
        let value = value.map(str::trim).unwrap_or("");
        match condition.op {
            sdk::ConditionOp::Eq => condition.values.first().is_some_and(|f| f == value),
            sdk::ConditionOp::Ne => !condition.values.first().is_some_and(|f| f == value),
            sdk::ConditionOp::In => condition.values.iter().any(|c| c == value),
            sdk::ConditionOp::NotIn => !condition.values.iter().any(|c| c == value),
            sdk::ConditionOp::NonEmpty => !value.is_empty(),
        }
    }

    #[test]
    fn secondary_configuration_sits_behind_the_advanced_disclosure() {
        let fields = config_fields();
        let advanced: Vec<&str> = fields
            .iter()
            .filter(|field| field.advanced)
            .map(|field| field.key.as_str())
            .collect();
        assert_eq!(advanced, vec!["extra_field_data_json", "cookie"]);
        // The things an operator needs to connect at all stay up front, and
        // so does the CAPTCHA answer: it is shown only when the login asks
        // for one, with the image beside it.
        for key in [
            "definition",
            "base_url",
            "username",
            "password",
            "cardigannCaptcha",
        ] {
            assert!(
                fields
                    .iter()
                    .find(|field| field.key == key)
                    .is_some_and(|field| !field.advanced),
                "{key} belongs in the standard set"
            );
        }
    }

    #[test]
    fn a_bundled_selection_loads_its_baked_definition() {
        let source = resolve_definition_source(Some("0magnet"), None).unwrap();
        let definition = parse_definition(&source).unwrap();
        assert_eq!(definition.id, "0magnet");
        // A bundled selection is authoritative: a stale paste left behind by a
        // Custom configuration must not keep being used.
        assert_eq!(
            resolve_definition_source(Some("0magnet"), Some(DEFINITION)).unwrap(),
            source
        );
    }

    #[test]
    fn a_bundled_definition_keeps_the_declared_origin_check() {
        let definition =
            parse_definition(&resolve_definition_source(Some("0magnet"), None).unwrap()).unwrap();
        let declared = definition
            .links
            .first()
            .cloned()
            .expect("a bundled definition declares a link");
        validate_configured_base_url(
            &definition,
            &BTreeMap::from([("base_url".to_string(), declared)]),
        )
        .unwrap();
        let error = validate_configured_base_url(
            &definition,
            &BTreeMap::from([(
                "base_url".to_string(),
                "https://attacker.example/".to_string(),
            )]),
        )
        .unwrap_err();
        assert!(error.contains("not declared"));
    }

    #[test]
    fn custom_is_the_only_mode_that_demands_pasted_yaml() {
        for selected in [None, Some(""), Some("  "), Some(CUSTOM_DEFINITION_ID)] {
            assert_eq!(
                resolve_definition_source(selected, Some(DEFINITION)).unwrap(),
                DEFINITION
            );
            for pasted in [None, Some(""), Some("   \n ")] {
                let error = resolve_definition_source(selected, pasted).unwrap_err();
                assert!(
                    error.contains("`definition_yaml` must be configured"),
                    "{error}"
                );
            }
        }
    }

    #[test]
    fn an_unknown_selection_names_both_ways_out() {
        let error = resolve_definition_source(Some("not-a-bundled-tracker"), None).unwrap_err();
        assert!(
            error.contains("not a bundled Cardigann definition"),
            "{error}"
        );
        assert!(error.contains("Custom"), "{error}");
    }

    #[test]
    fn extra_fields_override_explicit_values_and_stay_scalar() {
        let mut config = BTreeMap::from([("username".to_string(), "host-user".to_string())]);
        merge_extra_field_data(
            &mut config,
            Some(r#"{"username":"definition-user","apiurl":"api.example","enabled":true}"#.into()),
        )
        .unwrap();
        assert_eq!(config.get("username").unwrap(), "definition-user");
        assert_eq!(config.get("apiurl").unwrap(), "api.example");
        assert_eq!(config.get("enabled").unwrap(), "true");
    }

    #[test]
    fn base_url_must_match_a_declared_definition_origin() {
        let definition = parse_definition(DEFINITION).unwrap();
        let accepted = BTreeMap::from([(
            "base_url".to_string(),
            "https://legacy.tracker.example/alternate".to_string(),
        )]);
        validate_configured_base_url(&definition, &accepted).unwrap();

        let rejected = BTreeMap::from([(
            "base_url".to_string(),
            "https://attacker.example/".to_string(),
        )]);
        let error = validate_configured_base_url(&definition, &rejected).unwrap_err();
        assert!(error.contains("not declared"));
    }

    #[test]
    fn extra_field_base_url_override_cannot_escape_definition_origins() {
        let definition = parse_definition(DEFINITION).unwrap();
        let mut config = BTreeMap::from([(
            "base_url".to_string(),
            "https://tracker.example/".to_string(),
        )]);
        merge_extra_field_data(
            &mut config,
            Some(r#"{"base_url":"https://attacker.example/"}"#.to_string()),
        )
        .unwrap();
        let error = validate_configured_base_url(&definition, &config).unwrap_err();
        assert!(error.contains("attacker.example"));
    }

    #[test]
    fn grab_accepts_the_action_payload_shapes() {
        assert_eq!(
            action_url(&Value::String("https://tracker.example/download".into())).unwrap(),
            "https://tracker.example/download"
        );
        assert_eq!(
            action_url(&serde_json::json!({"url": "https://tracker.example/download"})).unwrap(),
            "https://tracker.example/download"
        );
    }

    #[test]
    fn executes_requested_corpus_to_an_initial_http_step() {
        // An upstream Prowlarr/Indexers `definitions/` root.
        let Some(definitions_root) = std::env::var_os("CARDIGANN_DEFINITIONS_DIR") else {
            return;
        };

        // The same selection and gate `xtask cardigann sync-definitions`
        // applies, so a definition that ships in the baked asset and a
        // definition that passes here can never diverge.
        let definitions = gate::select_candidates(std::path::Path::new(&definitions_root))
            .expect("read the definitions corpus")
            .into_iter()
            .map(|candidate| candidate.path)
            .collect::<Vec<_>>();
        assert!(!definitions.is_empty(), "Cardigann corpus is empty");

        let total = definitions.len();
        let mut passed = 0usize;
        let mut failures = Vec::new();
        for path in definitions {
            let source = match std::fs::read_to_string(&path) {
                Ok(source) => source,
                Err(error) => {
                    failures.push(format!("{}: could not read: {error}", path.display()));
                    continue;
                }
            };
            match gate::gate_definition(&source) {
                Ok(_) => passed += 1,
                Err(error) => failures.push(format!("{}: {error}", path.display())),
            }
        }

        let failed = failures.len();
        eprintln!("Cardigann corpus: total={total}, passed={passed}, failed={failed}");
        assert!(
            failures.is_empty(),
            "Cardigann corpus: total={total}, passed={passed}, failed={failed}\n{}",
            failures.join("\n")
        );
    }
}

fn descriptor_bytes() -> Vec<u8> {
    serde_json::to_vec(&build_descriptor()).expect("JSON descriptor")
}

fn install_component_runtime() {
    component::install_config_get();
    component::install_log();
    component::install_indexer_runtime();
}

struct CardigannComponent;

impl component::Guest for CardigannComponent {
    fn describe() -> Vec<u8> {
        install_component_runtime();
        descriptor_bytes()
    }

    async fn search(request: Vec<u8>) -> Result<Vec<u8>, component::InvocationError> {
        install_component_runtime();
        component::dispatch_search(request, search).await
    }

    async fn search_plan(request: Vec<u8>) -> Result<Vec<u8>, component::InvocationError> {
        install_component_runtime();
        let parallelism = component::strategy_plan_parallelism(&build_descriptor())
            .ok_or(component::InvocationError::InvalidResponse)?;
        component::dispatch_search_plan(request, parallelism, search).await
    }

    async fn action(request: Vec<u8>) -> Result<Vec<u8>, component::InvocationError> {
        install_component_runtime();
        component::dispatch_action(request, action).await
    }
}

component::export!(CardigannComponent with_types_in component);
