use std::collections::{BTreeMap, HashMap};

use scryer_plugin_pdk::component::{self, LogLevel, StartRateGate};
use scryer_plugin_pdk::*;
use scryer_plugin_sdk::current_sdk_constraint;
use scryer_plugin_sdk::{
    ConfigFieldDef, ConfigFieldRole, ConfigFieldType, IndexerCapabilities as Capabilities,
    IndexerFeedMode, IndexerLimitCapabilities, IndexerProtocol, IndexerResponseFeatures,
    IndexerSearchInput, IndexerSourceKind, PluginDescriptor, PluginSearchRequest as SearchRequest,
    PluginSearchResponse as SearchResponse, PluginSearchResult as SearchResult, ProviderDescriptor,
    SDK_VERSION,
};
use serde::Deserialize;
use serde_json::{Value, json};

const PROVIDER_ID: &str = "mediathekviewweb";
const DEFAULT_BASE_URL: &str = "https://mediathekviewweb.de";
const MAX_RESULTS: usize = 1000;
const MAX_RESPONSE_BYTES: usize = 32 * 1024 * 1024;

fn build_descriptor() -> PluginDescriptor {
    PluginDescriptor {
        id: PROVIDER_ID.to_string(),
        name: "MediathekViewWeb Indexer".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        sdk_version: SDK_VERSION.to_string(),
        sdk_constraint: current_sdk_constraint(),
        socket_permissions: vec![],
        provider: ProviderDescriptor::Indexer(scryer_plugin_sdk::IndexerDescriptor {
            provider_type: PROVIDER_ID.to_string(),
            provider_aliases: vec!["mediathekview".to_string(), "mvw".to_string()],
            provider_profiles: vec![],
            search_semantics_version: Some(1),
            strategy_plan: None,
            // MediathekViewWeb results are HTTP media URLs rather than torrent
            // or Usenet payloads. SDK 3.11 has no DirectMediaUrl source kind.
            source_kind: IndexerSourceKind::Generic,
            capabilities: Capabilities {
                supported_ids: HashMap::new(),
                deduplicates_aliases: false,
                season_param: None,
                episode_param: None,
                query_param: Some("query".to_string()),
                supported_query_facets: vec![],
                search: true,
                imdb_search: false,
                tvdb_search: false,
                anidb_search: false,
                rss: false,
                protocols: vec![IndexerProtocol::Unknown],
                feed_modes: vec![
                    IndexerFeedMode::Recent,
                    IndexerFeedMode::AutomaticSearch,
                    IndexerFeedMode::InteractiveSearch,
                ],
                search_inputs: vec![
                    IndexerSearchInput::TextQuery,
                    IndexerSearchInput::TitleQuery,
                    IndexerSearchInput::Limit,
                ],
                supported_external_ids: vec![],
                category_model: None,
                limits: Some(IndexerLimitCapabilities {
                    page_size: Some(MAX_RESULTS as u32),
                    max_page_size: Some(MAX_RESULTS as u32),
                    max_pages: Some(1),
                    rate_limit_hint_seconds: Some(1),
                    ..Default::default()
                }),
                torrent: None,
                response_features: Some(IndexerResponseFeatures {
                    languages: true,
                    subtitles: true,
                    info_url: true,
                    guid: true,
                    raw_provider_metadata: true,
                    ..Default::default()
                }),
            },
            scoring_policies: vec![],
            config_fields: vec![ConfigFieldDef {
                key: "base_url".to_string(),
                label: "MediathekViewWeb URL".to_string(),
                field_type: ConfigFieldType::String,
                required: true,
                default_value: Some(DEFAULT_BASE_URL.to_string()),
                help_text: Some("MediathekViewWeb instance URL".to_string()),
                role: Some(ConfigFieldRole::ConnectionUrl),
                ..Default::default()
            }],
            allowed_hosts: vec![],
            rate_limit_seconds: Some(1),
        }),
    }
}

#[derive(Debug, Deserialize)]
struct ApiEntry {
    channel: Option<String>,
    topic: Option<String>,
    title: Option<String>,
    timestamp: Option<i64>,
    duration: Option<i64>,
    size: Option<i64>,
    url_subtitle: Option<String>,
    url_video: Option<String>,
    url_video_low: Option<String>,
    url_video_hd: Option<String>,
    url_website: Option<String>,
}

async fn search(request: SearchRequest) -> FnResult<SearchResponse> {
    let configured = component::config_get("base_url").unwrap_or_else(|| DEFAULT_BASE_URL.into());
    let base = configured.trim().trim_end_matches('/');
    if !(base.starts_with("https://") || base.starts_with("http://")) {
        return Err(Error::msg("base_url must be an HTTP(S) URL"));
    }

    let query = request.query.trim();
    let limit = if request.limit == 0 {
        100
    } else {
        request.limit.clamp(1, MAX_RESULTS)
    };
    let body = json!({
        "queries": if query.is_empty() { vec![] } else { vec![json!({"fields": ["title"], "query": query})] },
        "sortBy": "timestamp",
        "sortOrder": "desc",
        "future": false,
        "offset": 0,
        "size": limit,
    });
    let url = format!("{base}/api/query");
    StartRateGate::new(format!("{PROVIDER_ID}.request-start"), 1, 1_000)
        .acquire()
        .await
        .map_err(component::deadline_deferred_error)?;
    component::log(LogLevel::Debug, format!("MediathekViewWeb query {url}"));
    let response = component::http(PluginHttpRequest {
        url,
        method: Some("POST".to_string()),
        headers: BTreeMap::from([("Content-Type".to_string(), "application/json".to_string())]),
        body: serde_json::to_vec(&body).map_err(|error| Error::msg(error.to_string()))?,
    })
    .await
    .map_err(|error| Error::msg(format!("MediathekViewWeb request failed: {error:?}")))?;
    if response.body.len() > MAX_RESPONSE_BYTES {
        return Err(Error::msg("MediathekViewWeb response exceeded 32 MiB"));
    }
    if !(200..300).contains(&response.status) {
        return Err(Error::msg(format!(
            "MediathekViewWeb returned HTTP {}",
            response.status
        )));
    }
    let payload: Value = serde_json::from_slice(&response.body)
        .map_err(|error| Error::msg(format!("invalid MediathekViewWeb response: {error}")))?;
    if let Some(error) = payload.get("err").filter(|value| !value.is_null())
        && !error.as_str().is_some_and(str::is_empty)
    {
        return Err(Error::msg(format!("MediathekViewWeb API error: {error}")));
    }
    let entries = payload
        .pointer("/result/results")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let results = entries
        .into_iter()
        .filter_map(|entry| serde_json::from_value::<ApiEntry>(entry).ok())
        .filter_map(api_entry_to_result)
        .take(limit)
        .collect();
    Ok(SearchResponse {
        results,
        ..Default::default()
    })
}

fn api_entry_to_result(entry: ApiEntry) -> Option<SearchResult> {
    let title = entry.title?.trim().to_string();
    if title.is_empty() {
        return None;
    }

    // SDK 3.11 has one legacy download_url slot. Prefer the HD rendition, then
    // the ordinary and low-bandwidth variants. Preserve every variant in
    // provider_extra until the typed multi-resource SDK contract is published.
    let variants = [
        ("hd", entry.url_video_hd),
        ("standard", entry.url_video),
        ("low", entry.url_video_low),
    ]
    .into_iter()
    .filter_map(|(quality, url)| {
        let url = url.filter(|value| is_http_url(value));
        url.map(|url| (quality, url))
    })
    .collect::<Vec<_>>();
    let download_url = variants
        .iter()
        .find(|(quality, _)| *quality == "hd")
        .or_else(|| variants.first())
        .map(|(_, url)| url.clone());
    let subtitle = entry.url_subtitle.filter(|value| is_http_url(value));
    let channel = entry.channel.unwrap_or_default();
    let topic = entry.topic.unwrap_or_default();
    let mut provider_extra = HashMap::new();
    provider_extra.insert("channel".to_string(), json!(channel));
    provider_extra.insert("topic".to_string(), json!(topic));
    if let Some(timestamp) = entry.timestamp {
        provider_extra.insert("timestamp".to_string(), json!(timestamp));
    }
    if let Some(duration) = entry.duration {
        provider_extra.insert("duration_seconds".to_string(), json!(duration));
    }
    if !variants.is_empty() {
        // Keep the legacy display metadata and add the forward-compatible
        // resource wire shape. SDK 3.11 ignores unknown provider_extra keys,
        // while updated Scryer hosts can expose/select these alternatives.
        provider_extra.insert(
            "download_options".to_string(),
            Value::Array(
                variants
                    .iter()
                    .map(|(quality, url)| json!({"quality": quality, "url": url}))
                    .collect(),
            ),
        );
    }
    if !variants.is_empty() || subtitle.is_some() {
        provider_extra.insert(
            "download_resources".to_string(),
            Value::Array(
                variants
                    .iter()
                    .map(|(_, url)| {
                        json!({
                            "url": url,
                            "kind": "download_url",
                            "role": "alternative",
                            "selection_group": "video",
                        })
                    })
                    .chain(subtitle.iter().map(|url| {
                        json!({
                            "url": url,
                            "kind": "download_url",
                            "role": "subtitle",
                        })
                    }))
                    .collect(),
            ),
        );
    }

    let info_url = entry.url_website.filter(|value| is_http_url(value));
    Some(SearchResult {
        title,
        link: info_url.clone(),
        download_url,
        size_bytes: entry.size.filter(|size| *size >= 0),
        published_at: entry.timestamp.and_then(unix_timestamp_rfc3339),
        languages: vec!["de".to_string()],
        subtitles: subtitle.into_iter().collect(),
        provider_extra,
        guid: info_url.clone(),
        info_url,
        source_kind: Some(IndexerSourceKind::Generic),
        protocol: Some(IndexerProtocol::Unknown),
        categories: (!channel.is_empty())
            .then_some(channel)
            .into_iter()
            .collect(),
        ..Default::default()
    })
}

fn is_http_url(value: &str) -> bool {
    value.starts_with("https://") || value.starts_with("http://")
}

fn unix_timestamp_rfc3339(timestamp: i64) -> Option<String> {
    // Avoid an additional date dependency. The canonical epoch value remains
    // available in provider_extra if it falls outside the supported range.
    let days = timestamp.div_euclid(86_400);
    let seconds = timestamp.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days)?;
    Some(format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    ))
}

fn civil_from_days(days_since_epoch: i64) -> Option<(i64, i64, i64)> {
    let z = days_since_epoch.checked_add(719_468)?;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + i64::from(month <= 2);
    (1..=9999).contains(&year).then_some((year, month, day))
}

scryer_plugin_pdk::scryer_indexer_component_main!(descriptor = build_descriptor, search = search,);
