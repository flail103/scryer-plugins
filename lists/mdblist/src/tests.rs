use std::collections::BTreeMap;

use list_provider_common::testing::{RecordedHttp, block_on};
use scryer_plugin_sdk::command::{PluginListCommand, PluginListCommandResult};
use scryer_plugin_sdk::{
    LIST_CREDENTIAL_TYPE_API_KEY, ListCredential, ListMediaKind, ListPluginAccountRequest,
    ListPluginFetchRequest, ListPluginFetchResponse, ListPluginHealthRequest, PluginDescriptor,
    PluginErrorCode,
};

use super::*;

/// The key a member links their own account with.
const MEMBER_KEY: &str = "fixturememberkey";
const PUBLIC_URL: &str = "https://mdblist.com/lists/fixture-curator/fixture-picks/json";

const PUBLIC_JSON: &str = r#"[
  {"id": 990501, "rank": 2, "adult": 0, "title": "Fixture Film Two", "imdb_id": "tt9900502", "tvdb_id": null,
   "language": "en", "mediatype": "movie", "release_year": 2032},
  {"id": 990500, "rank": 1, "adult": 0, "title": "Fixture Film One", "imdb_id": "tt9900501", "tvdb_id": null,
   "language": "en", "mediatype": "movie", "release_year": 2031},
  {"id": 880503, "rank": 3, "adult": 0, "title": "Fixture Show Three", "imdb_id": "tt9900503", "tvdb_id": 770503,
   "language": "ko", "mediatype": "show", "release_year": 2030},
  {"rank": 4, "title": "", "mediatype": "movie"}
]"#;

const API_JSON: &str = r#"{
  "movies": [
    {"id": 990600, "rank": 1, "title": "Fixture Api Film", "imdb_id": "tt9900600",
     "ids": {"mdblist": "m990600", "imdb": "tt9900600", "tmdb": 990600, "tvdb": null},
     "language": "en", "mediatype": "movie", "release_year": 2034}
  ],
  "shows": [
    {"id": 880601, "rank": 2, "title": "Fixture Api Show", "imdb_id": "tt9900601", "tvdb_id": 770601,
     "language": "en", "mediatype": "show", "release_year": 2033}
  ]
}"#;

fn request(list: &str, cursor: Option<&str>, since: Option<&str>) -> ListPluginFetchRequest {
    ListPluginFetchRequest {
        source_type: SOURCE_LIST.to_string(),
        params: BTreeMap::from([(PARAM_LIST.to_string(), list.to_string())]),
        credential: None,
        page_cursor: cursor.map(str::to_string),
        since_fingerprint: since.map(str::to_string),
    }
}

fn member_credential(key: &str) -> ListCredential {
    ListCredential {
        access_token: key.to_string(),
        token_type: Some(LIST_CREDENTIAL_TYPE_API_KEY.to_string()),
        external_user_id: Some("7301".to_string()),
        username: Some("fixture-member".to_string()),
    }
}

fn member_request(
    source_type: &str,
    params: &[(&str, &str)],
    cursor: Option<&str>,
) -> ListPluginFetchRequest {
    ListPluginFetchRequest {
        source_type: source_type.to_string(),
        params: params
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect(),
        credential: Some(member_credential(MEMBER_KEY)),
        page_cursor: cursor.map(str::to_string),
        since_fingerprint: None,
    }
}

fn member_url(path: &str, offset: u32) -> String {
    format!("{API_BASE}{path}?limit={PAGE_LIMIT}&offset={offset}&apikey={MEMBER_KEY}")
}

fn fetch_ok(http: &RecordedHttp, request: ListPluginFetchRequest) -> ListPluginFetchResponse {
    block_on(fetch(http, &request)).unwrap()
}

#[test]
fn descriptor_round_trips_and_passes_host_checks() {
    let original = descriptor();
    let decoded: PluginDescriptor =
        serde_json::from_slice(&serde_json::to_vec(&original).unwrap()).unwrap();
    assert_eq!(
        serde_json::to_value(&decoded).unwrap(),
        serde_json::to_value(&original).unwrap()
    );
    scryer_plugin_sdk::validate_plugin_descriptor_sdk_contract(
        &decoded,
        scryer_plugin_sdk::SDK_VERSION,
    )
    .unwrap();
    scryer_plugin_sdk::validate_plugin_descriptor_host_permissions(&decoded).unwrap();
    let list = decoded.list_provider().unwrap();
    assert_eq!(list.provider_type, "mdblist");
    assert_eq!(list.auth, ListProviderAuth::None);
    assert!(
        list.config_fields.is_empty(),
        "the server holds no MDBList key"
    );
    assert!(!list.capabilities.health);
    assert_eq!(
        list.allowed_hosts,
        vec!["api.mdblist.com".to_string(), "mdblist.com".to_string()]
    );
}

#[test]
fn descriptor_asks_each_member_for_their_own_key() {
    let list = descriptor().list_provider().cloned().unwrap();
    let key = list.member_key.expect("members link with a key");
    assert_eq!(key.use_as, ListMemberKeyUse::Credential);
    assert_eq!(
        key.help_url.as_deref(),
        Some("https://mdblist.com/preferences/#api")
    );
    assert!(
        list.capabilities.account,
        "the key is verified by the account read"
    );
    assert!(
        !list.capabilities.requires_member_credential,
        "public lists still need no account"
    );
    let personal: Vec<_> = list
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .filter(|item| item.personal)
        .map(|item| item.source_type.as_str())
        .collect();
    assert_eq!(personal, vec![SOURCE_WATCHLIST, SOURCE_MY_LIST]);
    let public: Vec<_> = list
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .filter(|item| !item.personal)
        .map(|item| item.source_type.as_str())
        .collect();
    assert_eq!(public, vec![SOURCE_LIST]);
}

#[test]
fn url_pattern_and_list_refs_agree() {
    let list = descriptor().list_provider().cloned().unwrap();
    let pattern = regex::Regex::new(&list.url_patterns[0].pattern).unwrap();
    for address in [
        "https://mdblist.com/lists/fixture-curator/fixture-picks",
        "https://www.mdblist.com/lists/fixture-curator/fixture-picks/json",
        "https://mdblist.com/lists/fixture-curator/fixture-picks/?sort=rank",
    ] {
        let captured = pattern.captures(address).unwrap()["list"].to_string();
        assert_eq!(captured, "fixture-curator/fixture-picks", "{address}");
        assert_eq!(
            parse_list_ref(address).unwrap(),
            parse_list_ref(&captured).unwrap()
        );
    }
    assert!(!pattern.is_match("https://mdblist.com/lists/fixture-curator"));
    assert!(!pattern.is_match("https://mdblist.com.example.test/lists/a/b"));
    assert_eq!(
        parse_list_ref(" 4242 ").unwrap(),
        ListRef::Id("4242".to_string())
    );
    assert!(parse_list_ref("fixture-curator").is_err());
    assert!(parse_list_ref("a/b/c").is_err());
    assert!(parse_list_ref("../etc/x").is_err());
}

#[test]
fn public_json_is_read_without_a_key_in_rank_order() {
    let http = RecordedHttp::new().with(PUBLIC_URL, 200, PUBLIC_JSON);
    let response = fetch_ok(&http, request("fixture-curator/fixture-picks", None, None));
    assert_eq!(http.urls(), vec![PUBLIC_URL.to_string()]);
    assert_eq!(
        response.list_url.as_deref(),
        Some("https://mdblist.com/lists/fixture-curator/fixture-picks")
    );
    assert!(response.next_cursor.is_none());
    let keys: Vec<_> = response
        .items
        .iter()
        .map(|item| (item.item_key.as_str(), item.rank))
        .collect();
    assert_eq!(
        keys,
        vec![
            ("tmdb:movie:990500", Some(1)),
            ("tmdb:movie:990501", Some(2)),
            ("tmdb:series:880503", Some(3))
        ]
    );
    let show = &response.items[2];
    assert_eq!(show.kind_hint, Some(ListMediaKind::Series));
    assert_eq!(show.year, Some(2030));
    assert_eq!(show.language.as_deref(), Some("ko"));
    let sources: Vec<_> = show
        .external_ids
        .iter()
        .map(|id| id.source.as_str())
        .collect();
    assert_eq!(sources, vec!["tmdb", "imdb", "tvdb"]);

    let again = fetch_ok(
        &http,
        request(
            "fixture-curator/fixture-picks",
            None,
            response.fingerprint.as_deref(),
        ),
    );
    assert!(again.unchanged);
}

#[test]
fn a_members_list_reads_the_items_endpoint_and_pages_on_has_more() {
    let path = "/lists/5101/items";
    let http = RecordedHttp::new()
        .with_headers(&member_url(path, 0), 200, &[("X-Has-More", "true"), ("X-Total-Items", "3")], API_JSON)
        .with_headers(&member_url(path, 2), 200, &[("X-Has-More", "false")], r#"{"movies": [{"id": 990602, "rank": 3, "title": "Fixture Api Tail", "mediatype": "movie"}], "shows": []}"#);
    let first = fetch_ok(
        &http,
        member_request(SOURCE_MY_LIST, &[(PARAM_LIST_ID, "5101")], None),
    );
    assert_eq!(first.next_cursor.as_deref(), Some("2"));
    assert_eq!(first.total_hint, Some(3));
    assert!(first.fingerprint.is_none());
    assert_eq!(first.items[0].item_key, "tmdb:movie:990600");
    assert_eq!(first.items[1].item_key, "tmdb:series:880601");

    let second = fetch_ok(
        &http,
        member_request(SOURCE_MY_LIST, &[(PARAM_LIST_ID, "5101")], Some("2")),
    );
    assert!(second.next_cursor.is_none());
    assert_eq!(second.items[0].rank, Some(3));
}

#[test]
fn a_members_list_past_the_page_cap_fails_instead_of_being_cut_short() {
    let last = PAGE_LIMIT * MAX_PAGES - 2;
    let cursor = last.to_string();
    let http = RecordedHttp::new().with_headers(
        &member_url("/lists/5101/items", last),
        200,
        &[("X-Has-More", "true")],
        API_JSON,
    );
    let error = block_on(fetch(
        &http,
        &member_request(SOURCE_MY_LIST, &[(PARAM_LIST_ID, "5101")], Some(&cursor)),
    ))
    .unwrap_err();
    assert_eq!(error.code, PluginErrorCode::Permanent);
    assert!(!error.public_message.contains("not found"));
}

#[test]
fn a_numeric_id_on_the_public_source_is_refused() {
    // A list by its id is read with a member's key, so it is theirs to follow.
    let http = RecordedHttp::new();
    let error = block_on(fetch(&http, &request("4242", None, None))).unwrap_err();
    assert_eq!(error.code, PluginErrorCode::InvalidConfig);
    assert!(http.urls().is_empty(), "nothing is asked of MDBList");
}

#[test]
fn errors_map_to_host_failure_classes() {
    let with_key = |status: u16, headers: &[(&str, &str)], body: &str| {
        let http = RecordedHttp::new().with_headers(
            &member_url("/lists/5101/items", 0),
            status,
            headers,
            body,
        );
        block_on(fetch(
            &http,
            &member_request(SOURCE_MY_LIST, &[(PARAM_LIST_ID, "5101")], None),
        ))
        .unwrap_err()
    };
    assert_eq!(
        with_key(401, &[], r#"{"error":"Invalid API key"}"#).code,
        PluginErrorCode::AuthFailed
    );
    // A refusal on a member's own list is their key, and the list is not gone.
    let denied = with_key(403, &[], r#"{"error":"Permission denied"}"#);
    assert_eq!(denied.code, PluginErrorCode::AuthFailed);
    assert!(!denied.public_message.contains("not found"));
    let missing = with_key(404, &[], r#"{"error":"Not found"}"#);
    assert_eq!(missing.code, PluginErrorCode::Permanent);
    assert!(missing.public_message.contains("not found"));
    let limited = with_key(
        429,
        &[("Retry-After", "3600")],
        r#"{"error":"Daily API limit exceeded!"}"#,
    );
    assert_eq!(
        (limited.code, limited.retry_after_seconds),
        (PluginErrorCode::RateLimited, Some(3600))
    );
    assert_eq!(
        with_key(500, &[], "").code,
        PluginErrorCode::UpstreamUnavailable
    );

    let public = |status: u16, body: &str| {
        let http = RecordedHttp::new().with(PUBLIC_URL, status, body);
        block_on(fetch(
            &http,
            &request("fixture-curator/fixture-picks", None, None),
        ))
        .unwrap_err()
    };
    assert_eq!(public(401, "").code, PluginErrorCode::Permanent);
    assert!(
        public(200, r#"{"error":"List not found"}"#)
            .public_message
            .contains("not found")
    );
    assert_eq!(
        public(200, "<html></html>").code,
        PluginErrorCode::from(scryer_plugin_pdk::error::codes::INVALID_RESPONSE)
    );

    let unknown = ListPluginFetchRequest {
        source_type: "toplists".to_string(),
        ..request("a/b", None, None)
    };
    assert_eq!(
        block_on(fetch(&RecordedHttp::new(), &unknown))
            .unwrap_err()
            .code,
        PluginErrorCode::Unsupported
    );
}

#[test]
fn health_has_no_server_key_to_check() {
    let http = RecordedHttp::new();
    match block_on(run(
        &http,
        PluginListCommand::Health(ListPluginHealthRequest {}),
    )) {
        PluginListCommandResult::Health(scryer_plugin_sdk::PluginResult::Err(error)) => {
            assert_eq!(error.code, PluginErrorCode::Unsupported);
        }
        other => panic!("unexpected {other:?}"),
    }
    assert!(http.urls().is_empty());
}

const USER_JSON: &str = r#"{"api_requests": 1000, "api_requests_count": 12, "user_id": 7301,
  "username": "fixture-member", "name": "Fixture Member",
  "avatar_url": "https://images.example.test/fixture-member.png"}"#;

const MY_LISTS_JSON: &str = r#"[
  {"id": 5101, "user_id": 7301, "name": "Fixture Films", "slug": "fixture-films",
   "mediatype": "movie", "items": 12, "private": true},
  {"id": 5102, "user_id": 7301, "name": "Fixture Shows", "slug": "fixture-shows",
   "mediatype": "show", "items": 3, "private": false},
  {"id": 5103, "user_id": 7301, "name": "Fixture Mixed", "slug": "fixture-mixed", "items": 0},
  {"name": "No id"}
]"#;

fn account_of(
    http: &RecordedHttp,
    member_key: &str,
) -> Result<scryer_plugin_sdk::ListPluginAccountResponse, scryer_plugin_sdk::PluginError> {
    match block_on(run(
        http,
        PluginListCommand::Account(ListPluginAccountRequest {
            credential: member_credential(member_key),
        }),
    )) {
        PluginListCommandResult::Account(scryer_plugin_sdk::PluginResult::Ok(account)) => {
            Ok(account)
        }
        PluginListCommandResult::Account(scryer_plugin_sdk::PluginResult::Err(error)) => Err(error),
        other => panic!("unexpected {other:?}"),
    }
}

#[test]
fn a_members_key_names_their_account_and_their_lists() {
    let user_url = format!("{API_BASE}/user?apikey={MEMBER_KEY}");
    let lists_url = format!("{API_BASE}/lists/user?apikey={MEMBER_KEY}");
    let http =
        RecordedHttp::new()
            .with(&user_url, 200, USER_JSON)
            .with(&lists_url, 200, MY_LISTS_JSON);
    let account = account_of(&http, MEMBER_KEY).unwrap();
    assert_eq!(http.urls(), vec![user_url, lists_url]);
    assert_eq!(account.external_user_id, "7301");
    assert_eq!(account.username, "fixture-member");
    assert_eq!(account.display_name.as_deref(), Some("Fixture Member"));
    assert_eq!(
        account.avatar_url.as_deref(),
        Some("https://images.example.test/fixture-member.png")
    );
    let lists: Vec<_> = account
        .owned_lists
        .iter()
        .map(|list| (list.id.as_str(), list.name.as_str(), list.kinds.clone()))
        .collect();
    assert_eq!(
        lists,
        vec![
            ("5101", "Fixture Films", vec![ListMediaKind::Movie]),
            ("5102", "Fixture Shows", vec![ListMediaKind::Series]),
            (
                "5103",
                "Fixture Mixed",
                vec![ListMediaKind::Movie, ListMediaKind::Series]
            ),
        ]
    );
    assert!(account.statuses.is_empty());
}

#[test]
fn a_key_mdblist_refuses_is_an_auth_failure_and_other_trouble_is_not() {
    let user_url = format!("{API_BASE}/user?apikey={MEMBER_KEY}");
    let refused = |status: u16| {
        let http = RecordedHttp::new().with(&user_url, status, r#"{"error":"Invalid API key"}"#);
        account_of(&http, MEMBER_KEY).unwrap_err()
    };
    // `auth_failed`, not `invalid_api_key`: for a member key that is the
    // credential, the host reads only this code as "the provider refused the
    // key".
    for status in [401, 403] {
        let error = refused(status);
        assert_eq!(error.code, PluginErrorCode::AuthFailed, "{status}");
        assert!(!error.public_message.contains(MEMBER_KEY), "{status}");
        assert!(
            !error.public_message.contains("Invalid API key"),
            "{status}"
        );
        assert_eq!(
            error.debug_message.as_deref(),
            Some(format!("MDBList answered HTTP {status}").as_str())
        );
    }
    assert_eq!(refused(503).code, PluginErrorCode::UpstreamUnavailable);

    let limited = RecordedHttp::new().with_headers(
        &user_url,
        429,
        &[("Retry-After", "120")],
        r#"{"error":"Daily API limit exceeded!"}"#,
    );
    let limited = account_of(&limited, MEMBER_KEY).unwrap_err();
    assert_eq!(
        (limited.code, limited.retry_after_seconds),
        (PluginErrorCode::RateLimited, Some(120))
    );

    // An answer that names nobody links nobody.
    let nameless = RecordedHttp::new().with(&user_url, 200, r#"{"username": "fixture-member"}"#);
    assert_eq!(
        account_of(&nameless, MEMBER_KEY).unwrap_err().code,
        PluginErrorCode::from(scryer_plugin_pdk::error::codes::INVALID_RESPONSE)
    );

    // A blank key is refused without asking MDBList.
    let silent = RecordedHttp::new();
    assert_eq!(
        account_of(&silent, "  ").unwrap_err().code,
        PluginErrorCode::AuthFailed
    );
    assert!(silent.urls().is_empty());

    // The refusal carries no part of the key.
    let error = refused(401);
    assert!(!format!("{error:?}").contains(MEMBER_KEY));
}

#[test]
fn a_members_own_list_is_read_with_their_key() {
    let http = RecordedHttp::new().with_headers(
        &member_url("/lists/5101/items", 0),
        200,
        &[("X-Has-More", "false")],
        API_JSON,
    );
    let response = fetch_ok(
        &http,
        member_request(SOURCE_MY_LIST, &[(PARAM_LIST_ID, "5101")], None),
    );
    assert_eq!(http.urls(), vec![member_url("/lists/5101/items", 0)]);
    assert_eq!(response.items.len(), 2);
    assert!(response.fingerprint.is_some());

    let fails = |request: ListPluginFetchRequest| {
        let http = RecordedHttp::new();
        let error = block_on(fetch(&http, &request)).unwrap_err();
        assert!(http.urls().is_empty(), "nothing is asked of MDBList");
        error.code
    };
    assert_eq!(
        fails(member_request(
            SOURCE_MY_LIST,
            &[(PARAM_LIST_ID, "fixture-curator/fixture-picks")],
            None
        )),
        PluginErrorCode::InvalidConfig
    );
    assert_eq!(
        fails(member_request(SOURCE_MY_LIST, &[], None)),
        PluginErrorCode::InvalidConfig
    );
    for source in [SOURCE_MY_LIST, SOURCE_WATCHLIST] {
        let unlinked = ListPluginFetchRequest {
            credential: None,
            ..member_request(source, &[(PARAM_LIST_ID, "5101")], None)
        };
        assert_eq!(fails(unlinked), PluginErrorCode::AuthFailed, "{source}");
    }
}

#[test]
fn the_public_source_never_reads_a_members_key() {
    let http = RecordedHttp::new().with(PUBLIC_URL, 200, PUBLIC_JSON);
    let with_credential = ListPluginFetchRequest {
        credential: Some(member_credential(MEMBER_KEY)),
        ..request("fixture-curator/fixture-picks", None, None)
    };
    fetch_ok(&http, with_credential);
    assert_eq!(http.urls(), vec![PUBLIC_URL.to_string()]);
}

const WATCHLIST_JSON: &str = r#"{
  "movies": [
    {"id": 990700, "title": "Fixture Watch Film", "imdb_id": "tt9900700", "tvdb_id": null,
     "language": "en", "mediatype": "movie", "release_year": 2035, "rank": 0}
  ],
  "shows": [
    {"id": 880701, "title": "Fixture Watch Show", "imdb_id": "tt9900701", "tvdb_id": 770701,
     "language": "en", "mediatype": "show", "release_year": 2036, "rank": 1}
  ],
  "pagination": {"offset": 0, "limit": 1000, "total_movies": 1, "total_shows": 1, "has_more": false}
}"#;

#[test]
fn a_members_watchlist_is_read_whole_and_paged_when_mdblist_says_more_follows() {
    let http = RecordedHttp::new().with(&member_url("/watchlist/items", 0), 200, WATCHLIST_JSON);
    let whole = fetch_ok(&http, member_request(SOURCE_WATCHLIST, &[], None));
    assert_eq!(http.urls(), vec![member_url("/watchlist/items", 0)]);
    let keys: Vec<_> = whole
        .items
        .iter()
        .map(|item| item.item_key.as_str())
        .collect();
    assert_eq!(keys, vec!["tmdb:movie:990700", "tmdb:series:880701"]);
    assert!(whole.next_cursor.is_none());
    assert!(whole.fingerprint.is_some());

    let more = WATCHLIST_JSON.replace(r#""has_more": false"#, r#""has_more": true"#);
    let paged = RecordedHttp::new()
        .with(&member_url("/watchlist/items", 0), 200, &more)
        .with(
            &member_url("/watchlist/items", PAGE_LIMIT),
            200,
            WATCHLIST_JSON,
        );
    let first = fetch_ok(&paged, member_request(SOURCE_WATCHLIST, &[], None));
    assert_eq!(first.next_cursor, Some(PAGE_LIMIT.to_string()));
    assert_eq!(first.total_hint, Some(2));
    assert!(first.fingerprint.is_none());
    let cursor = PAGE_LIMIT.to_string();
    let second = fetch_ok(&paged, member_request(SOURCE_WATCHLIST, &[], Some(&cursor)));
    assert!(second.next_cursor.is_none());
    assert_eq!(second.items[0].rank, Some(PAGE_LIMIT + 1));
}

#[test]
fn a_watchlist_is_never_cut_short_or_read_as_gone() {
    let last = PAGE_LIMIT * (MAX_PAGES - 1);
    let cursor = last.to_string();
    let more = WATCHLIST_JSON.replace(r#""has_more": false"#, r#""has_more": true"#);
    let endless = RecordedHttp::new().with(&member_url("/watchlist/items", last), 200, &more);
    let error = block_on(fetch(
        &endless,
        &member_request(SOURCE_WATCHLIST, &[], Some(&cursor)),
    ))
    .unwrap_err();
    assert_eq!(error.code, PluginErrorCode::Permanent);
    assert!(!error.public_message.contains("not found"));

    let answered = |status: u16| {
        let http = RecordedHttp::new().with(&member_url("/watchlist/items", 0), status, "{}");
        block_on(fetch(&http, &member_request(SOURCE_WATCHLIST, &[], None))).unwrap_err()
    };
    assert_eq!(answered(401).code, PluginErrorCode::AuthFailed);
    assert_eq!(answered(403).code, PluginErrorCode::AuthFailed);
    let missing = answered(404);
    assert_eq!(missing.code, PluginErrorCode::Permanent);
    assert!(
        !missing.public_message.contains("not found"),
        "a 404 is not the member's watchlist being gone"
    );
}
