//! MDBList list provider.
//!
//! Follows any public MDBList list by its address
//! (`mdblist.com/lists/<user>/<slug>`), read from the list's public JSON
//! export in one request. That needs no key, and the server holds none.
//! MDBList's directory of top lists is not a list itself, so it is not a
//! source here: any top list is followed by its address like every other list.
//!
//! Everything that needs an MDBList API key is a member's own. A member links
//! their MDBList account with their own API key, which the host hands back as
//! that member's credential and never as configuration. It reads two personal
//! sources:
//!
//! - `my_list`: a list by its numeric id, the member's own, public or private.
//! - `watchlist`: the member's MDBList watchlist, movies and shows.
//!
//! The public source never reads a member's key.

use list_provider_common::error::{
    Access, auth_failed, check_status, invalid_api_key, invalid_config, invalid_response,
    missing_param, not_found, permanent, plugin_error, quota_exceeded, retry_after_seconds,
    unsupported_source, with_debug,
};
use list_provider_common::http::{HostHttp, ListHttp, encode_component, get, header, json_body};
use list_provider_common::ids::{Ids, build_item, dedupe_and_rank, json_id, json_text, json_year};
use list_provider_common::page::{numeric_cursor, single_page};
use scryer_plugin_sdk::command::{PluginListCommand, PluginListCommandResult};
use scryer_plugin_sdk::{
    ListAccountList, ListAuthBadge, ListCredential, ListMediaKind, ListMemberKey, ListMemberKeyUse,
    ListNoteTone, ListPluginAccountResponse, ListPluginFetchRequest, ListPluginFetchResponse,
    ListPluginItem, ListProviderAuth, ListProviderCapabilities, ListProviderDescriptor,
    ListProviderGroup, ListProviderItem, ListProviderNote, ListProviderTile, ListSourceParam,
    ListSourceParamType, ListUrlPattern, ListUrlPatternCapture, PluginDescriptor, PluginError,
    PluginErrorCode, PluginResult, ProviderDescriptor,
};
use serde_json::Value;

wit_bindgen::generate!({
    world: "scryer:lists/list-provider@1.0.0",
    path: ["../../pdk/scryer-plugin-pdk/wit/host-v1.0.0", "../../pdk/scryer-plugin-pdk/wit/runtime-v1.0.0", "../../pdk/scryer-plugin-pdk/wit/list-v1.0.0"],
    generate_all,
});

list_provider_common::list_component_main!(descriptor = descriptor, handler = handle_command,);

pub const PLUGIN_ID: &str = "mdblist-list";
pub const PROVIDER_TYPE: &str = "mdblist";
pub const SOURCE_LIST: &str = "list";
pub const PARAM_LIST: &str = "list";
/// One of the linked member's own lists, by the id their account names it by.
pub const SOURCE_MY_LIST: &str = "my_list";
pub const PARAM_LIST_ID: &str = "list_id";
/// The linked member's watchlist.
pub const SOURCE_WATCHLIST: &str = "watchlist";

pub const API_BASE: &str = "https://api.mdblist.com";
pub const SITE_BASE: &str = "https://mdblist.com";
const USER_AGENT: &str = concat!(env!("CARGO_PKG_NAME"), "/", env!("CARGO_PKG_VERSION"));
const ACCEPT: &str = "application/json";
/// Items per API page; MDBList's documented default and a single request for
/// almost every list.
pub const PAGE_LIMIT: u32 = 1000;
/// Deepest API page followed, far inside the host's per-sync page ceiling.
const MAX_PAGES: u32 = 20;
/// Twelve hours, the interval the other arrs use for MDBList.
const DEFAULT_INTERVAL_SECONDS: u64 = 12 * 60 * 60;

pub fn descriptor() -> PluginDescriptor {
    let both = vec![ListMediaKind::Movie, ListMediaKind::Series];
    PluginDescriptor {
        id: PLUGIN_ID.to_string(),
        name: "MDBList".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        sdk_version: scryer_plugin_sdk::SDK_VERSION.to_string(),
        sdk_constraint: scryer_plugin_sdk::current_sdk_constraint(),
        socket_permissions: Vec::new(),
        settings: Vec::new(),
        provider: ProviderDescriptor::ListProvider(ListProviderDescriptor {
            provider_type: PROVIDER_TYPE.to_string(),
            provider_aliases: Vec::new(),
            summary: Some("Any public MDBList list, static or dynamic".to_string()),
            blurb: Some(
                "Paste an MDBList list address to follow any public list; no key is \
                 needed. Members who connect their own MDBList API key can follow their \
                 own lists, private ones included, and their watchlist."
                    .to_string(),
            ),
            tile: Some(ListProviderTile {
                bg: "#1b2433".to_string(),
                ink: "#f5c518".to_string(),
                abbr: "MD".to_string(),
            }),
            brand_url_template: Some(format!("{SITE_BASE}/lists/{{{PARAM_LIST}}}")),
            coverage: both.clone(),
            // Public lists need no key, and the server holds none.
            auth: ListProviderAuth::None,
            // How a member links an account: MDBList has no sign-in to send
            // them through, so they paste the key MDBList issues them.
            member_key: Some(ListMemberKey {
                label: "Your MDBList API key".to_string(),
                help_text: Some(
                    "Copy it from the API access section of your MDBList preferences.".to_string(),
                ),
                help_url: Some(format!("{SITE_BASE}/preferences/#api")),
                use_as: ListMemberKeyUse::Credential,
            }),
            groups: vec![
                ListProviderGroup {
                    label: "Lists".to_string(),
                    auth_badge: ListAuthBadge::NoAccountNeedsValue,
                    items: vec![ListProviderItem {
                        id: "list".to_string(),
                        name: "List".to_string(),
                        description: Some("Any public list, by its address".to_string()),
                        kinds: both.clone(),
                        source_type: SOURCE_LIST.to_string(),
                        params: vec![ListSourceParam {
                            key: PARAM_LIST.to_string(),
                            label: "List (address or user/slug)".to_string(),
                            param_type: ListSourceParamType::Text,
                            options: Vec::new(),
                            required: true,
                        }],
                        personal: false,
                        default_interval_seconds: DEFAULT_INTERVAL_SECONDS,
                    }],
                },
                ListProviderGroup {
                    label: "Your MDBList account".to_string(),
                    auth_badge: ListAuthBadge::MemberAccount,
                    items: vec![
                        ListProviderItem {
                            id: "watchlist".to_string(),
                            name: "Watchlist".to_string(),
                            description: Some(
                                "Movies and shows on your MDBList watchlist".to_string(),
                            ),
                            kinds: both.clone(),
                            source_type: SOURCE_WATCHLIST.to_string(),
                            params: Vec::new(),
                            personal: true,
                            default_interval_seconds: DEFAULT_INTERVAL_SECONDS,
                        },
                        ListProviderItem {
                            id: "my-list".to_string(),
                            name: "Your list".to_string(),
                            description: Some(
                                "One of your own MDBList lists, public or private".to_string(),
                            ),
                            kinds: both,
                            source_type: SOURCE_MY_LIST.to_string(),
                            params: vec![ListSourceParam {
                                key: PARAM_LIST_ID.to_string(),
                                label: "List id".to_string(),
                                param_type: ListSourceParamType::Text,
                                options: Vec::new(),
                                required: true,
                            }],
                            personal: true,
                            default_interval_seconds: DEFAULT_INTERVAL_SECONDS,
                        },
                    ],
                },
            ],
            notes: vec![ListProviderNote {
                tone: ListNoteTone::Info,
                text_key: "lists.note.mdblist_dynamic".to_string(),
            }],
            url_patterns: vec![ListUrlPattern {
                pattern: r"^https?://(?:www\.)?mdblist\.com/lists/(?<list>[^/?#]+/[^/?#]+)(?:/json)?/?(?:[?#].*)?$"
                    .to_string(),
                source_type: SOURCE_LIST.to_string(),
                captures: vec![ListUrlPatternCapture {
                    group: "list".to_string(),
                    param: PARAM_LIST.to_string(),
                }],
            }],
            capabilities: ListProviderCapabilities {
                account: true,
                // There is no server key to check.
                health: false,
                requires_member_credential: false,
            },
            config_fields: Vec::new(),
            default_base_url: None,
            allowed_hosts: vec!["api.mdblist.com".to_string(), "mdblist.com".to_string()],
            rate_limit_seconds: Some(1),
        }),
    }
}

async fn handle_command(command: PluginListCommand) -> PluginListCommandResult {
    run(&HostHttp, command).await
}

fn into_result<T>(result: Result<T, PluginError>) -> PluginResult<T> {
    match result {
        Ok(value) => PluginResult::Ok(value),
        Err(error) => PluginResult::Err(error),
    }
}

pub async fn run<H: ListHttp>(http: &H, command: PluginListCommand) -> PluginListCommandResult {
    match command {
        PluginListCommand::Fetch(request) => {
            PluginListCommandResult::Fetch(into_result(fetch(http, &request).await))
        }
        PluginListCommand::Health(_) => {
            PluginListCommandResult::Health(PluginResult::Err(plugin_error(
                PluginErrorCode::Unsupported,
                "MDBList's public lists need no key and members use their own; \
                 there is no server key to check",
            )))
        }
        PluginListCommand::Account(request) => {
            PluginListCommandResult::Account(into_result(account(http, &request.credential).await))
        }
    }
}

/// The API key a member linked their account with, for one call.
fn member_key(credential: Option<&ListCredential>) -> Result<&str, PluginError> {
    credential
        .map(|credential| credential.access_token.trim())
        .filter(|key| !key.is_empty())
        .ok_or_else(|| auth_failed("this list needs a linked MDBList account"))
}

/// MDBList answers a key it does not accept with 401 or 403. On a member's
/// own read that is the key, never the thing being read.
///
/// The refusal is the open `invalid_api_key` code: the host reads it as "the
/// provider refused the key" when linking, and as "reconnect" on a fetch.
fn check_member_key(
    response: &scryer_plugin_sdk::host::PluginHttpResponse,
) -> Result<(), PluginError> {
    match response.status {
        401 | 403 => Err(with_debug(
            invalid_api_key("MDBList did not accept the API key"),
            format_args!("MDBList answered HTTP {}", response.status),
        )),
        _ => Ok(()),
    }
}

/// A member's own account, watchlist or list index. Neither a refused key nor
/// a 404 says one of those is gone.
fn check_member_status(
    response: &scryer_plugin_sdk::host::PluginHttpResponse,
    what: &str,
) -> Result<(), PluginError> {
    check_member_key(response)?;
    match response.status {
        404 | 410 => Err(with_debug(
            permanent(format!("MDBList has no {what} for this account")),
            format_args!("{what}: HTTP {}", response.status),
        )),
        _ => check_mdblist_status(response, Access::ServerKey, what),
    }
}

/// The shared status mapping, except that a 429 MDBList explains as its daily
/// API allowance being spent is the open `quota_exceeded` code with the
/// `Retry-After` MDBList gave. Any other 429 stays a rate limit. This is the
/// one place MDBList's error text is read.
fn check_mdblist_status(
    response: &scryer_plugin_sdk::host::PluginHttpResponse,
    access: Access,
    what: &str,
) -> Result<(), PluginError> {
    if response.status == 429 && daily_limit_spent(response) {
        return Err(with_debug(
            quota_exceeded(
                "the daily MDBList API allowance for this key is used up",
                retry_after_seconds(response),
            ),
            format_args!("{what}: HTTP 429, daily API limit"),
        ));
    }
    check_status(response, access, what)
}

fn daily_limit_spent(response: &scryer_plugin_sdk::host::PluginHttpResponse) -> bool {
    serde_json::from_slice::<Value>(&response.body)
        .ok()
        .and_then(|body| {
            body.get("error")
                .and_then(Value::as_str)
                .map(str::to_ascii_lowercase)
        })
        .is_some_and(|error| error.contains("daily") && error.contains("limit"))
}

/// Whose key this is and the lists they own. A key MDBList refuses is
/// `invalid_api_key`, which is how the host learns the member typed a wrong one.
pub async fn account<H: ListHttp>(
    http: &H,
    credential: &ListCredential,
) -> Result<ListPluginAccountResponse, PluginError> {
    let key = encode_component(member_key(Some(credential))?);
    let response = http
        .send(get(
            format!("{API_BASE}/user?apikey={key}"),
            USER_AGENT,
            ACCEPT,
        ))
        .await?;
    check_member_status(&response, "account")?;
    let user = json_body(&response)?;
    let external_user_id = json_id(user.get("user_id"))
        .ok_or_else(|| invalid_response("MDBList did not say whose API key this is"))?;
    let username = json_text(user.get("username")).unwrap_or_else(|| external_user_id.clone());

    let response = http
        .send(get(
            format!("{API_BASE}/lists/user?apikey={key}"),
            USER_AGENT,
            ACCEPT,
        ))
        .await?;
    check_member_status(&response, "lists")?;
    let owned_lists = json_body(&response)?
        .as_array()
        .map(|lists| lists.iter().filter_map(account_list).collect())
        .unwrap_or_default();
    Ok(ListPluginAccountResponse {
        external_user_id,
        username,
        display_name: json_text(user.get("name")),
        avatar_url: json_text(user.get("avatar_url")).filter(|url| url.starts_with("https://")),
        owned_lists,
        statuses: Vec::new(),
    })
}

/// One of the member's lists as an account source. An MDBList list holds
/// movies or shows; one that says neither is offered as both.
fn account_list(entry: &Value) -> Option<ListAccountList> {
    Some(ListAccountList {
        id: json_id(entry.get("id"))?,
        name: json_text(entry.get("name"))?,
        kinds: match entry.get("mediatype").and_then(Value::as_str) {
            Some("movie") => vec![ListMediaKind::Movie],
            Some("show") => vec![ListMediaKind::Series],
            _ => vec![ListMediaKind::Movie, ListMediaKind::Series],
        },
    })
}

/// Where a list lives: its owner and slug, or its numeric id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ListRef {
    Slug { user: String, slug: String },
    Id(String),
}

fn is_segment(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_alphanumeric() || matches!(ch, '-' | '_' | '.'))
}

/// Parse `user/slug`, a full list address, or a numeric id.
pub fn parse_list_ref(value: &str) -> Result<ListRef, PluginError> {
    let mut value = value.trim();
    for prefix in ["https://", "http://"] {
        value = value.strip_prefix(prefix).unwrap_or(value);
    }
    value = value.strip_prefix("www.").unwrap_or(value);
    value = value.strip_prefix("mdblist.com/").unwrap_or(value);
    value = value.strip_prefix("lists/").unwrap_or(value);
    let value = value.split(['?', '#']).next().unwrap_or_default();
    let value = value.trim_end_matches('/');
    let value = value.strip_suffix("/json").unwrap_or(value);
    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(ListRef::Id(value.to_string()));
    }
    match value.split_once('/') {
        Some((user, slug)) if is_segment(user) && is_segment(slug) => Ok(ListRef::Slug {
            user: user.to_string(),
            slug: slug.to_string(),
        }),
        _ => Err(invalid_config(
            "an MDBList list is its address, user/slug, or numeric id",
        )),
    }
}

fn what(list: &ListRef) -> String {
    match list {
        ListRef::Slug { user, slug } => format!("MDBList list {user}/{slug}"),
        ListRef::Id(id) => format!("MDBList list {id}"),
    }
}

fn site_url(list: &ListRef) -> Option<String> {
    match list {
        ListRef::Slug { user, slug } => Some(format!("{SITE_BASE}/lists/{user}/{slug}")),
        ListRef::Id(_) => None,
    }
}

pub async fn fetch<H: ListHttp>(
    http: &H,
    request: &ListPluginFetchRequest,
) -> Result<ListPluginFetchResponse, PluginError> {
    match request.source_type.as_str() {
        SOURCE_LIST => {
            let list = parse_list_ref(
                request
                    .params
                    .get(PARAM_LIST)
                    .ok_or_else(|| missing_param(PARAM_LIST))?,
            )?;
            fetch_public(http, &list, request).await
        }
        SOURCE_MY_LIST => {
            let key = member_key(request.credential.as_ref())?;
            let id = request
                .params
                .get(PARAM_LIST_ID)
                .map(|id| id.trim())
                .ok_or_else(|| missing_param(PARAM_LIST_ID))?;
            if id.is_empty() || !id.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(invalid_config("an MDBList list id is a number"));
            }
            fetch_api(http, key, &ListRef::Id(id.to_string()), request).await
        }
        SOURCE_WATCHLIST => {
            fetch_watchlist(http, member_key(request.credential.as_ref())?, request).await
        }
        other => Err(unsupported_source(other)),
    }
}

/// The member's watchlist, movies then shows, a page of each per request.
/// MDBList reports whether more follows in the body's `pagination` block.
async fn fetch_watchlist<H: ListHttp>(
    http: &H,
    key: &str,
    request: &ListPluginFetchRequest,
) -> Result<ListPluginFetchResponse, PluginError> {
    let offset = numeric_cursor(request.page_cursor.as_deref(), 0)?;
    let url = format!(
        "{API_BASE}/watchlist/items?limit={PAGE_LIMIT}&offset={offset}&apikey={}",
        encode_component(key)
    );
    let response = http.send(get(url, USER_AGENT, ACCEPT)).await?;
    check_member_status(&response, "watchlist")?;
    let body = json_body(&response)?;
    let pagination = body.get("pagination");
    let has_more = pagination
        .and_then(|pagination| pagination.get("has_more"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
        && count_entries(&body) > 0;
    let mut items = entries(&body);
    if offset == 0 && !has_more {
        return Ok(single_page(
            items,
            None,
            None,
            request.since_fingerprint.as_deref(),
        ));
    }
    let next_offset = offset.saturating_add(PAGE_LIMIT);
    if has_more && next_offset >= PAGE_LIMIT * MAX_PAGES {
        // Stopping here would read every later title as having left.
        return Err(permanent(format!(
            "this MDBList watchlist is longer than the {} titles Scryer follows",
            PAGE_LIMIT * MAX_PAGES
        )));
    }
    for item in &mut items {
        if let Some(rank) = item.rank.as_mut() {
            *rank += offset;
        }
    }
    let total = |key: &str| {
        pagination
            .and_then(|pagination| pagination.get(key))
            .and_then(Value::as_u64)
    };
    Ok(ListPluginFetchResponse {
        items,
        next_cursor: has_more.then(|| next_offset.to_string()),
        list_name: None,
        list_url: None,
        total_hint: match (total("total_movies"), total("total_shows")) {
            (None, None) => None,
            (movies, shows) => u32::try_from(movies.unwrap_or(0) + shows.unwrap_or(0)).ok(),
        },
        fingerprint: None,
        unchanged: false,
    })
}

async fn fetch_public<H: ListHttp>(
    http: &H,
    list: &ListRef,
    request: &ListPluginFetchRequest,
) -> Result<ListPluginFetchResponse, PluginError> {
    let ListRef::Slug { user, slug } = list else {
        return Err(invalid_config(
            "a numeric MDBList id is read with a member's own MDBList account; \
             use the list address, or follow it under your account",
        ));
    };
    let url = format!(
        "{SITE_BASE}/lists/{}/{}/json",
        encode_component(user),
        encode_component(slug)
    );
    let response = http.send(get(url, USER_AGENT, ACCEPT)).await?;
    check_mdblist_status(&response, Access::Public, &what(list))?;
    let body = json_body(&response)?;
    if let Some(error) = body.get("error").and_then(Value::as_str) {
        return Err(with_debug(
            not_found(what(list)),
            format_args!("{}: MDBList answered {error}", what(list)),
        ));
    }
    Ok(single_page(
        entries(&body),
        None,
        site_url(list),
        request.since_fingerprint.as_deref(),
    ))
}

/// A list read through the API's items endpoint with the member's key, which
/// pages through lists longer than one response.
async fn fetch_api<H: ListHttp>(
    http: &H,
    key: &str,
    list: &ListRef,
    request: &ListPluginFetchRequest,
) -> Result<ListPluginFetchResponse, PluginError> {
    let offset = numeric_cursor(request.page_cursor.as_deref(), 0)?;
    let path = match list {
        ListRef::Slug { user, slug } => {
            format!(
                "/lists/{}/{}/items",
                encode_component(user),
                encode_component(slug)
            )
        }
        ListRef::Id(id) => format!("/lists/{id}/items"),
    };
    let url = format!(
        "{API_BASE}{path}?limit={PAGE_LIMIT}&offset={offset}&apikey={}",
        encode_component(key)
    );
    let response = http.send(get(url, USER_AGENT, ACCEPT)).await?;
    check_member_key(&response)?;
    check_mdblist_status(&response, Access::ServerKey, &what(list))?;
    let body = json_body(&response)?;
    let raw_count = count_entries(&body);
    let mut items = entries(&body);
    let has_more = header(&response, "x-has-more")
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
        && raw_count > 0;
    let next_offset = offset.saturating_add(raw_count as u32);

    if offset == 0 && !has_more {
        return Ok(single_page(
            items,
            None,
            site_url(list),
            request.since_fingerprint.as_deref(),
        ));
    }
    if has_more && next_offset >= PAGE_LIMIT * MAX_PAGES {
        // Stopping here would read every later title as having left.
        return Err(permanent(format!(
            "{} is longer than the {} titles Scryer follows",
            what(list),
            PAGE_LIMIT * MAX_PAGES
        )));
    }
    // Ranks continue across pages when MDBList gives none of its own.
    for item in &mut items {
        if let Some(rank) = item.rank.as_mut() {
            *rank += offset;
        }
    }
    Ok(ListPluginFetchResponse {
        items,
        next_cursor: has_more.then(|| next_offset.to_string()),
        list_name: None,
        list_url: site_url(list),
        total_hint: header(&response, "x-total-items").and_then(|value| value.trim().parse().ok()),
        fingerprint: None,
        unchanged: false,
    })
}

fn raw_entries(body: &Value) -> Vec<&Value> {
    match body {
        Value::Array(entries) => entries.iter().collect(),
        Value::Object(object) => ["movies", "shows", "items"]
            .iter()
            .filter_map(|key| object.get(*key).and_then(Value::as_array))
            .flatten()
            .collect(),
        _ => Vec::new(),
    }
}

fn count_entries(body: &Value) -> usize {
    raw_entries(body).len()
}

/// Items in MDBList's rank order (falling back to response order), movies
/// before shows on equal rank.
fn entries(body: &Value) -> Vec<ListPluginItem> {
    let mut ranked: Vec<(u64, usize, ListPluginItem)> = raw_entries(body)
        .into_iter()
        .enumerate()
        .filter_map(|(position, entry)| {
            let rank = entry
                .get("rank")
                .and_then(Value::as_u64)
                .filter(|rank| *rank > 0)
                .unwrap_or(position as u64 + 1);
            to_item(entry).map(|item| (rank, position, item))
        })
        .collect();
    ranked.sort_by_key(|(rank, position, _)| (*rank, *position));
    let items = ranked.into_iter().map(|(_, _, item)| item).collect();
    dedupe_and_rank(items, 1)
}

fn to_item(entry: &Value) -> Option<ListPluginItem> {
    let kind = match entry.get("mediatype").and_then(Value::as_str) {
        Some("movie") => Some(ListMediaKind::Movie),
        Some("show" | "tv" | "series") => Some(ListMediaKind::Series),
        _ => None,
    };
    let nested = entry.get("ids");
    let ids = Ids::default()
        .with_tmdb(json_id(nested.and_then(|ids| ids.get("tmdb"))))
        .with_imdb(json_text(nested.and_then(|ids| ids.get("imdb"))))
        .with_tvdb(json_id(nested.and_then(|ids| ids.get("tvdb"))))
        .with_imdb(json_text(entry.get("imdb_id")))
        .with_tvdb(json_id(entry.get("tvdb_id")))
        .with_tmdb(json_id(entry.get("tmdb_id")))
        // MDBList's top-level `id` is the TMDb id of the entry.
        .with_tmdb(json_id(entry.get("id")));
    let mut item = build_item(
        &ids,
        kind,
        json_text(entry.get("title")),
        json_year(entry.get("release_year").or_else(|| entry.get("year"))),
    )?;
    item.language = json_text(entry.get("language"));
    Some(item)
}

#[cfg(test)]
mod tests;
