use std::collections::BTreeMap;

use scryer_plugin_pdk::*;
use scryer_plugin_sdk::current_sdk_constraint;
use scryer_plugin_sdk::{
    ConfigFieldDef, ConfigFieldType, DownloadClientCapabilities, DownloadClientDescriptor,
    DownloadInputKind, DownloadIsolationMode, DownloadItemState, DownloadTorrentCapabilities,
    PluginCompletedDownload, PluginDescriptor, PluginDownloadClientAddRequest,
    PluginDownloadClientAddResponse, PluginDownloadClientControlRequest,
    PluginDownloadClientMarkImportedRequest, PluginDownloadClientStatus, PluginDownloadItem,
    PluginDownloadListRecentCompletedRequest, PluginDownloadOutputKind, PluginError,
    PluginErrorCode, PluginResult, PluginTorrentItem, ProviderDescriptor, SDK_VERSION,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const STATE_KEY: &str = "alldebrid.jobs.v1";
const USER_AGENT: &str = "scryer-alldebrid-plugin/0.1";

wit_bindgen::generate!({
    world: "scryer:download-client/download-client@1.0.0",
    path: ["wit/host-v1.0.0", "wit/download-client-v1.0.0"],
    generate_all,
});

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Job {
    id: String,
    title: String,
    #[serde(default)]
    info_hash: Option<String>,
    #[serde(default)]
    pyload_package_id: Option<i64>,
    #[serde(default)]
    error: Option<String>,
}

#[derive(Debug, Clone)]
struct Config {
    alldebrid_api_key: String,
    pyload_url: String,
    pyload_api_key: String,
    download_root: String,
}

impl Config {
    fn load() -> Result<Self, Error> {
        let read = |key: &str| -> Result<String, Error> {
            config::get(key)
                .map_err(|e| Error::msg(format!("missing config {key}: {e}")))
                .map(|v| v.unwrap_or_default().trim().to_string())
        };
        let value = Self {
            alldebrid_api_key: read("alldebrid_api_key")?,
            pyload_url: read("pyload_url")?.trim_end_matches('/').to_string(),
            pyload_api_key: read("pyload_api_key")?,
            download_root: read("download_root")?.trim_end_matches('/').to_string(),
        };
        if value.alldebrid_api_key.is_empty() {
            return Err(Error::msg("AllDebrid API key is required"));
        }
        if value.pyload_url.is_empty() {
            return Err(Error::msg("pyLoad-ng base URL is required"));
        }
        if value.pyload_api_key.is_empty() {
            return Err(Error::msg("pyLoad-ng API key is required"));
        }
        if value.download_root.is_empty() {
            return Err(Error::msg(
                "download_root must be the pyLoad output path visible to Scryer",
            ));
        }
        Ok(value)
    }
}

fn config_fields() -> Vec<ConfigFieldDef> {
    [
        (
            "alldebrid_api_key",
            "AllDebrid API key",
            ConfigFieldType::Password,
            true,
            "Bearer token from your AllDebrid account.",
        ),
        (
            "pyload_url",
            "pyLoad-ng URL",
            ConfigFieldType::String,
            true,
            "Base URL reachable from Scryer, for example http://pyload:8000.",
        ),
        (
            "pyload_api_key",
            "pyLoad-ng API key",
            ConfigFieldType::Password,
            true,
            "Sent to pyLoad-ng using the X-API-Key header.",
        ),
        (
            "download_root",
            "Scryer-visible download root",
            ConfigFieldType::Path,
            true,
            "The pyLoad output directory as mounted inside Scryer.",
        ),
    ]
    .into_iter()
    .map(
        |(key, label, field_type, required, help_text)| ConfigFieldDef {
            key: key.to_string(),
            label: label.to_string(),
            field_type,
            required,
            help_text: Some(help_text.to_string()),
            ..Default::default()
        },
    )
    .collect()
}

fn build_descriptor() -> PluginDescriptor {
    PluginDescriptor {
        id: "alldebrid".into(),
        name: "AllDebrid".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        sdk_version: SDK_VERSION.into(),
        sdk_constraint: current_sdk_constraint(),
        socket_permissions: vec![],
        provider: ProviderDescriptor::DownloadClient(DownloadClientDescriptor {
            provider_type: "alldebrid".into(),
            provider_aliases: vec!["alldebrid-pyload".into()],
            config_fields: config_fields(),
            default_base_url: None,
            allowed_hosts: vec!["api.alldebrid.com".into()],
            accepted_inputs: vec![DownloadInputKind::MagnetUri],
            isolation_modes: vec![DownloadIsolationMode::Directory],
            capabilities: DownloadClientCapabilities {
                client_status: true,
                test_connection: true,
                host_fs_required: false,
                torrent: Some(DownloadTorrentCapabilities {
                    supported_sources: vec![DownloadInputKind::MagnetUri],
                    preferred_sources: vec![DownloadInputKind::MagnetUri],
                    isolation_modes: vec![DownloadIsolationMode::Directory],
                    reports_content_paths: true,
                    ..Default::default()
                }),
                ..Default::default()
            },
        }),
    }
}

fn plugin_error<T>(code: PluginErrorCode, message: impl Into<String>) -> PluginResult<T> {
    PluginResult::Err(PluginError {
        code,
        public_message: message.into(),
        debug_message: None,
        retry_after_seconds: None,
        details: None,
    })
}

fn response<T: serde::Serialize>(value: PluginResult<T>) -> FnResult<String> {
    Ok(serde_json::to_string(&value)?)
}

fn json_request(
    url: &str,
    method: &str,
    headers: BTreeMap<String, String>,
    body: Option<Vec<u8>>,
) -> Result<Value, Error> {
    let mut request = HttpRequest::new(url).with_method(method);
    for (key, value) in headers {
        request = request.with_header(key, value);
    }
    let result = http::request::<Vec<u8>>(&request, body)
        .map_err(|e| Error::msg(format!("HTTP request failed: {e}")))?;
    if result.status_code() >= 400 {
        return Err(Error::msg(format!(
            "HTTP {}: {}",
            result.status_code(),
            String::from_utf8_lossy(&result.body())
        )));
    }
    serde_json::from_slice(&result.body())
        .map_err(|e| Error::msg(format!("invalid JSON response: {e}")))
}

fn ad_request(config: &Config, path: &str, fields: &[(&str, &str)]) -> Result<Value, Error> {
    let body = form_encode(fields);
    let mut headers = BTreeMap::new();
    headers.insert(
        "Authorization".into(),
        format!("Bearer {}", config.alldebrid_api_key),
    );
    headers.insert(
        "Content-Type".into(),
        "application/x-www-form-urlencoded".into(),
    );
    headers.insert("User-Agent".into(), USER_AGENT.into());
    let (version, endpoint) = path
        .strip_prefix("v4.1/")
        .map_or(("v4", path), |endpoint| ("v4.1", endpoint));
    let value = json_request(
        &format!("https://api.alldebrid.com/{version}/{endpoint}"),
        "POST",
        headers,
        Some(body.into_bytes()),
    )?;
    if value["status"] != "success" {
        return Err(Error::msg(
            value["error"]["message"]
                .as_str()
                .unwrap_or("AllDebrid API request failed")
                .to_string(),
        ));
    }
    Ok(value)
}

fn form_encode(fields: &[(&str, &str)]) -> String {
    fields
        .iter()
        .map(|(key, value)| format!("{}={}", encode_component(key), encode_component(value)))
        .collect::<Vec<_>>()
        .join("&")
}

fn encode_component(value: &str) -> String {
    let mut output = String::new();
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            output.push(byte as char);
        } else {
            output.push_str(&format!("%{byte:02X}"));
        }
    }
    output
}

fn pyload_request(
    config: &Config,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Result<Value, Error> {
    let mut headers = BTreeMap::new();
    headers.insert("X-API-Key".into(), config.pyload_api_key.clone());
    headers.insert("Accept".into(), "application/json".into());
    if body.is_some() {
        headers.insert("Content-Type".into(), "application/json".into());
    }
    json_request(
        &format!("{}{path}", config.pyload_url),
        method,
        headers,
        body.map(|v| serde_json::to_vec(&v).unwrap_or_default()),
    )
}

fn jobs() -> Result<Vec<Job>, Error> {
    Ok(var::get::<Vec<Job>>(STATE_KEY)?.unwrap_or_default())
}

fn save_jobs(jobs: &[Job]) -> Result<(), Error> {
    var::set(STATE_KEY, jobs)
}

fn source_magnet(request: &PluginDownloadClientAddRequest) -> Option<&str> {
    request
        .source
        .magnet_uri
        .as_deref()
        .or(request.source.download_url.as_deref())
}

fn extract_info_hash(magnet: &str) -> Option<String> {
    let query = magnet.strip_prefix("magnet:?")?;
    query.split('&').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if key != "xt" {
            return None;
        }
        let (_, hash) = value.rsplit_once(':')?;
        (!hash.is_empty()).then(|| hash.to_ascii_lowercase())
    })
}

fn download_add(input: String) -> FnResult<String> {
    let request: PluginDownloadClientAddRequest = serde_json::from_str(&input)?;
    let config = match Config::load() {
        Ok(value) => value,
        Err(error) => {
            return response(plugin_error::<PluginDownloadClientAddResponse>(
                PluginErrorCode::InvalidConfig,
                error.to_string(),
            ));
        }
    };
    let Some(magnet) = source_magnet(&request) else {
        return response(plugin_error::<PluginDownloadClientAddResponse>(
            PluginErrorCode::Permanent,
            "AllDebrid requires a magnet URI",
        ));
    };
    if !magnet.starts_with("magnet:?") {
        return response(plugin_error::<PluginDownloadClientAddResponse>(
            PluginErrorCode::Permanent,
            "source is not a valid magnet URI",
        ));
    }
    let magnet = magnet.to_string();
    let expected_hash = request
        .release
        .info_hash_hint
        .clone()
        .or_else(|| extract_info_hash(&magnet));
    if let Some(existing) = jobs()?.iter().find(|job| {
        job.info_hash
            .as_ref()
            .zip(expected_hash.as_ref())
            .is_some_and(|(left, right)| left.eq_ignore_ascii_case(right))
    }) {
        return response(PluginResult::Ok(PluginDownloadClientAddResponse {
            client_item_id: existing.id.clone(),
            info_hash: existing.info_hash.clone(),
        }));
    }
    let title = request
        .release
        .release_title
        .unwrap_or(request.title.title_name);
    let result = ad_request(&config, "magnet/upload", &[("magnets[]", &magnet)])?;
    let Some(magnet) = result["data"]["magnets"]
        .as_array()
        .and_then(|items| items.first())
    else {
        return response(plugin_error::<PluginDownloadClientAddResponse>(
            PluginErrorCode::Permanent,
            "AllDebrid did not return a magnet job",
        ));
    };
    let Some(id) = magnet["id"].as_i64() else {
        return response(plugin_error::<PluginDownloadClientAddResponse>(
            PluginErrorCode::Permanent,
            "AllDebrid returned a magnet without an id",
        ));
    };
    let id = id.to_string();
    let mut jobs = jobs()?;
    jobs.push(Job {
        id: id.clone(),
        title,
        info_hash: magnet["hash"]
            .as_str()
            .map(str::to_string)
            .or(expected_hash),
        pyload_package_id: None,
        error: None,
    });
    save_jobs(&jobs)?;
    response(PluginResult::Ok(PluginDownloadClientAddResponse {
        client_item_id: id,
        info_hash: magnet["hash"].as_str().map(str::to_string),
    }))
}

fn list_queue(_input: String) -> FnResult<String> {
    let config = Config::load()?;
    let mut jobs = jobs()?;
    let mut items = Vec::new();
    for job in &mut jobs {
        let mut magnet = Value::Null;
        let mut state = DownloadItemState::Queued;
        let mut message = None;
        let mut total_size = None;
        let mut remaining = None;
        let mut progress = None;
        if job.pyload_package_id.is_none() && job.error.is_none() {
            let status = ad_request(&config, "v4.1/magnet/status", &[("id", &job.id)])?;
            magnet = status["data"]["magnets"]
                .as_array()
                .and_then(|items| items.first())
                .cloned()
                .unwrap_or(Value::Null);
            let code = magnet["statusCode"].as_i64().unwrap_or(0);
            total_size = magnet["size"].as_i64();
            let downloaded = magnet["downloaded"].as_i64();
            remaining = total_size
                .zip(downloaded)
                .map(|(total, got)| (total - got).max(0));
            progress = total_size
                .zip(downloaded)
                .filter(|(total, _)| *total > 0)
                .map(|(total, got)| ((got.saturating_mul(100) / total).clamp(0, 100)) as u8);
            state = match code {
                4 => DownloadItemState::Verifying,
                5..=15 => DownloadItemState::Failed,
                _ => DownloadItemState::Downloading,
            };
            message = magnet["status"].as_str().map(str::to_string);
            if code == 4 {
                match handoff_ready(&config, job) {
                    Ok(package_id) => {
                        job.pyload_package_id = Some(package_id);
                        state = DownloadItemState::Queued;
                    }
                    Err(error) => {
                        job.error = Some(error.to_string());
                        state = DownloadItemState::Failed;
                        message = job.error.clone();
                    }
                }
            }
        } else if let Some(error) = &job.error {
            state = DownloadItemState::Failed;
            message = Some(error.clone());
        }
        if let Some(package_id) = job.pyload_package_id {
            match pyload_package(&config, package_id) {
                Ok(package) => {
                    let files = package["links"].as_array().cloned().unwrap_or_default();
                    if !files.is_empty() {
                        let done = files
                            .iter()
                            .filter(|file| file["status"].as_i64() == Some(0))
                            .count();
                        let failed = files
                            .iter()
                            .any(|file| matches!(file["status"].as_i64(), Some(4 | 9)));
                        state = if done == files.len() {
                            DownloadItemState::Completed
                        } else if failed {
                            DownloadItemState::Failed
                        } else {
                            DownloadItemState::Downloading
                        };
                        total_size = package["sizetotal"].as_i64();
                        let downloaded = package["sizedone"].as_i64();
                        remaining = total_size
                            .zip(downloaded)
                            .map(|(total, got)| (total - got).max(0));
                        progress = total_size
                            .zip(downloaded)
                            .filter(|(total, _)| *total > 0)
                            .map(|(total, got)| {
                                ((got.saturating_mul(100) / total).clamp(0, 100)) as u8
                            });
                        message = files.iter().find_map(|file| {
                            file["error"]
                                .as_str()
                                .filter(|value| !value.is_empty())
                                .map(str::to_string)
                        });
                    }
                }
                Err(error) => {
                    state = DownloadItemState::Warning;
                    message = Some(format!("pyLoad-ng status lookup failed: {error}"));
                }
            }
        }
        items.push(PluginDownloadItem {
            client_item_id: job.id.clone(),
            download_id: Some(job.id.clone()),
            info_hash: job
                .info_hash
                .clone()
                .or_else(|| magnet["hash"].as_str().map(str::to_string)),
            title: job.title.clone(),
            state,
            message,
            category: None,
            remote_output_path: Some(output_dir(&config, &job.title)),
            torrent: Some(PluginTorrentItem {
                client_native_id: Some(job.id.clone()),
                raw_status: magnet["status"].as_str().map(str::to_string),
                ..Default::default()
            }),
            total_size_bytes: total_size,
            remaining_size_bytes: remaining,
            eta_seconds: None,
            progress_percent: progress,
            can_move_files: Some(state == DownloadItemState::Completed),
            can_remove: Some(false),
            removed: Some(false),
            raw_state: magnet["status"].as_str().map(str::to_string),
            completed_at: None,
        });
    }
    save_jobs(&jobs)?;
    response(PluginResult::Ok(items))
}

fn handoff_ready(config: &Config, job: &Job) -> Result<i64, Error> {
    let files = ad_request(config, "magnet/files", &[("id[]", &job.id)])?;
    let mut links = Vec::new();
    collect_links(&files["data"], &mut links);
    if links.is_empty() {
        return Err(Error::msg(
            "AllDebrid magnet is ready but returned no file links",
        ));
    }
    let added = pyload_request(
        config,
        "POST",
        "/api/add_package",
        Some(json!({"name": safe_name(&job.title), "links": links, "dest": 1})),
    )?;
    added
        .as_i64()
        .ok_or_else(|| Error::msg("pyLoad-ng did not return a package id"))
}

fn output_dir(config: &Config, title: &str) -> String {
    format!("{}/{}", config.download_root, safe_name(title))
}

fn pyload_package(config: &Config, package_id: i64) -> Result<Value, Error> {
    pyload_request(
        config,
        "GET",
        &format!("/api/get_package_data?package_id={package_id}"),
        None,
    )
}

fn collect_links(value: &Value, links: &mut Vec<String>) {
    match value {
        Value::Array(values) => {
            for value in values {
                collect_links(value, links);
            }
        }
        Value::Object(map) => {
            if let Some(link) = map.get("l").and_then(Value::as_str)
                && !links.iter().any(|existing| existing == link)
            {
                links.push(link.to_string());
            }
            for value in map.values() {
                collect_links(value, links);
            }
        }
        _ => {}
    }
}

fn safe_name(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || " ._-()[]".contains(ch) {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let cleaned = cleaned.trim().trim_matches('.');
    if cleaned.is_empty() {
        "download".into()
    } else {
        cleaned.chars().take(120).collect()
    }
}

fn list_history(input: String) -> FnResult<String> {
    list_completed(input)
}
fn list_completed(input: String) -> FnResult<String> {
    let _ = serde_json::from_str::<Value>(&input)?;
    let config = Config::load()?;
    let downloads = jobs()?
        .iter()
        .filter_map(|job| completed_download(&config, job).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    response(PluginResult::Ok(downloads))
}
fn list_recent(input: String) -> FnResult<String> {
    let request: PluginDownloadListRecentCompletedRequest = serde_json::from_str(&input)?;
    let config = Config::load()?;
    let mut downloads = jobs()?
        .iter()
        .filter_map(|job| completed_download(&config, job).transpose())
        .collect::<Result<Vec<_>, _>>()?;
    downloads.reverse();
    downloads.truncate(request.limit);
    response(PluginResult::Ok(downloads))
}

fn completed_download(
    config: &Config,
    job: &Job,
) -> Result<Option<PluginCompletedDownload>, Error> {
    let Some(package_id) = job.pyload_package_id else {
        return Ok(None);
    };
    let package = pyload_package(config, package_id)?;
    let files = package["links"].as_array().cloned().unwrap_or_default();
    if files.is_empty() || files.iter().any(|file| file["status"].as_i64() != Some(0)) {
        return Ok(None);
    }
    let dir = output_dir(config, &job.title);
    Ok(Some(PluginCompletedDownload {
        client_item_id: job.id.clone(),
        download_id: Some(job.id.clone()),
        info_hash: None,
        name: job.title.clone(),
        release_name: None,
        dest_dir: dir,
        category: None,
        output_kind: Some(PluginDownloadOutputKind::Directory),
        // File paths in a torrent can be nested and are untrusted input. The
        // package directory is stable; Scryer should scan it rather than
        // trusting backend-reported relative path strings.
        content_paths: vec![],
        size_bytes: package["sizetotal"].as_i64(),
        completed_at: None,
        parameters: vec![],
    }))
}
fn control(input: String) -> FnResult<String> {
    let request: PluginDownloadClientControlRequest = serde_json::from_str(&input)?;
    let _ = request.action;
    response(plugin_error::<()>(
        PluginErrorCode::Unsupported,
        "download controls are not supported yet",
    ))
}
fn mark_imported(input: String) -> FnResult<String> {
    let _: PluginDownloadClientMarkImportedRequest = serde_json::from_str(&input)?;
    response(PluginResult::Ok(()))
}
fn status(_input: String) -> FnResult<String> {
    let config = Config::load()?;
    let version: String = pyload_request(&config, "GET", "/api/get_server_version", None)?
        .as_str()
        .unwrap_or("pyLoad-ng")
        .to_string();
    response(PluginResult::Ok(PluginDownloadClientStatus {
        version: Some(version),
        is_localhost: None,
        remote_output_roots: vec![config.download_root],
        removes_completed_downloads: Some(false),
        sorting_mode: None,
        warnings: vec![
            "AllDebrid torrent jobs do not seed; local files are downloaded by pyLoad-ng".into(),
        ],
    }))
}
fn test_connection(_input: String) -> FnResult<String> {
    let config = Config::load()?;
    let _ = pyload_request(&config, "GET", "/api/get_server_version", None)?;
    let mut request = HttpRequest::new("https://api.alldebrid.com/v4/user").with_method("GET");
    request = request
        .with_header(
            "Authorization",
            format!("Bearer {}", config.alldebrid_api_key),
        )
        .with_header("User-Agent", USER_AGENT);
    let http_response = http::request::<Vec<u8>>(&request, None)?;
    if http_response.status_code() >= 400 {
        return Err(Error::msg(format!(
            "AllDebrid test failed with HTTP {}",
            http_response.status_code()
        )));
    }
    response(PluginResult::Ok("AllDebrid and pyLoad-ng are reachable"))
}

fn legacy_functions() -> LegacyDownloadClientFunctions {
    LegacyDownloadClientFunctions {
        describe: |_| Ok(serde_json::to_string(&build_descriptor())?),
        add: download_add,
        list_queue,
        list_history,
        list_completed,
        list_recent_completed: Some(list_recent),
        control,
        mark_imported,
        mark_imported_non_destructive: Some(mark_imported),
        status,
        test_connection,
    }
}

fn handle_download_client_command(
    command: PluginDownloadClientCommand,
) -> PluginDownloadClientCommandResult {
    bridge_download_client_command(&legacy_functions(), command)
}

scryer_plugin_pdk::scryer_download_client_component_main!(
    descriptor = build_descriptor,
    handler = handle_download_client_command,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_debrid_form_values_are_encoded_as_utf8() {
        assert_eq!(
            form_encode(&[("magnets[]", "magnet:?xt=urn:btih:abc&dn=Some Name")]),
            "magnets%5B%5D=magnet%3A%3Fxt%3Durn%3Abtih%3Aabc%26dn%3DSome%20Name"
        );
    }

    #[test]
    fn walks_all_debrid_file_tree_and_deduplicates_links() {
        let value = json!({ "data": { "magnets": [{ "files": [
            { "n": "sample.mkv", "l": "https://alldebrid.com/f/a" },
            { "n": "folder", "e": [
                { "n": "episode.mkv", "l": "https://alldebrid.com/f/b" },
                { "n": "duplicate.mkv", "l": "https://alldebrid.com/f/a" }
            ]}
        ]}]}});
        let mut links = Vec::new();
        collect_links(&value, &mut links);
        assert_eq!(
            links,
            ["https://alldebrid.com/f/a", "https://alldebrid.com/f/b"]
        );
    }

    #[test]
    fn descriptor_exposes_magnet_only_and_pyload_config() {
        let descriptor = build_descriptor();
        let ProviderDescriptor::DownloadClient(provider) = descriptor.provider else {
            panic!("AllDebrid must be a download client");
        };
        assert_eq!(provider.accepted_inputs, [DownloadInputKind::MagnetUri]);
        assert!(
            provider
                .config_fields
                .iter()
                .any(|field| field.key == "pyload_api_key")
        );
        assert!(
            provider
                .config_fields
                .iter()
                .any(|field| field.key == "download_root")
        );
    }
}
