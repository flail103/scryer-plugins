use std::collections::BTreeMap;

use list_provider_common::testing::{RecordedHttp, block_on};
use scryer_plugin_sdk::command::{PluginListCommand, PluginListCommandResult};
use scryer_plugin_sdk::{
    ListMediaKind, ListPluginAccountRequest, ListPluginFetchRequest, ListPluginHealthRequest,
    PluginDescriptor, PluginErrorCode, PluginResult,
};

use super::*;

fn v3(path_and_query: &str) -> String {
    format!("{API_BASE}{path_and_query}")
}

/// A fetch as the host sends it: with the linked member's credential.
fn request(
    source_type: &str,
    params: &[(&str, &str)],
    cursor: Option<&str>,
) -> ListPluginFetchRequest {
    ListPluginFetchRequest {
        source_type: source_type.to_string(),
        params: params
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<BTreeMap<_, _>>(),
        credential: Some(member()),
        page_cursor: cursor.map(str::to_string),
        since_fingerprint: None,
    }
}

fn fetch(
    http: &RecordedHttp,
    request: ListPluginFetchRequest,
) -> PluginResult<scryer_plugin_sdk::ListPluginFetchResponse> {
    match block_on(run(http, PluginListCommand::Fetch(request))) {
        PluginListCommandResult::Fetch(result) => result,
        other => panic!("unexpected {other:?}"),
    }
}

fn ok<T: std::fmt::Debug>(result: PluginResult<T>) -> T {
    match result {
        PluginResult::Ok(value) => value,
        PluginResult::Err(error) => panic!("unexpected error {error:?}"),
    }
}

fn err<T: std::fmt::Debug>(result: PluginResult<T>) -> scryer_plugin_sdk::PluginError {
    match result {
        PluginResult::Err(error) => error,
        PluginResult::Ok(value) => panic!("unexpected success {value:?}"),
    }
}

const LIST_PAGE_1: &str = r#"{
  "id": 8100001, "name": "Fixture Picks", "item_count": 23, "page": 1, "total_pages": 2, "total_results": 23,
  "results": [
    {"id": 990201, "media_type": "movie", "title": "Fixture Feature Alpha", "release_date": "2031-03-04",
     "vote_average": 7.4, "vote_count": 120, "genre_ids": [18, 53], "original_language": "en"},
    {"id": 990202, "media_type": "tv", "name": "Fixture Serial Beta", "first_air_date": "2029-10-01",
     "vote_average": 0, "vote_count": 0, "genre_ids": [10765], "original_language": "ja"},
    {"id": 990203, "media_type": "person", "name": "Fixture Person"},
    {"id": 990201, "media_type": "movie", "title": "Fixture Feature Alpha", "release_date": "2031-03-04"}
  ]
}"#;

const LIST_PAGE_2: &str = r#"{
  "id": 8100001, "name": "Fixture Picks", "page": 2, "total_pages": 2, "total_results": 23,
  "results": [{"id": 990204, "media_type": "movie", "title": "Fixture Feature Gamma", "release_date": ""}]
}"#;

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
    assert_eq!(list.provider_type, "tmdb");
    assert_eq!(
        list.auth,
        ListProviderAuth::MemberAccount {
            flow: ListAccountFlow::TmdbApproval,
            exchange: ListAccountExchange::Direct,
            byo_app: false,
            scopes: Vec::new(),
        }
    );
    assert!(!list.capabilities.health);
    assert!(
        list.config_fields.is_empty(),
        "the server holds no TMDb key"
    );
    // A member's own token starts the approval; it is never the credential.
    let member_key = list.member_key.as_ref().expect("members bring a token");
    assert_eq!(member_key.use_as, ListMemberKeyUse::AccountApp);
    assert_eq!(
        member_key.help_url.as_deref(),
        Some("https://www.themoviedb.org/settings/api")
    );
    assert_eq!(list.allowed_hosts, vec!["api.themoviedb.org".to_string()]);
    // Every source a pattern produces is one the descriptor declares, and
    // every capture targets a declared parameter.
    let items: Vec<_> = list.groups.iter().flat_map(|group| &group.items).collect();
    for pattern in &list.url_patterns {
        let item = items
            .iter()
            .find(|item| item.source_type == pattern.source_type)
            .unwrap();
        for capture in &pattern.captures {
            assert!(item.params.iter().any(|param| param.key == capture.param));
        }
    }
}

#[test]
fn url_patterns_extract_numeric_ids_from_site_addresses() {
    let list = descriptor().list_provider().cloned().unwrap();
    let cases = [
        (
            "https://www.themoviedb.org/list/8100001",
            "list",
            "list_id",
            "8100001",
        ),
        (
            "https://www.themoviedb.org/person/4400-fixture-person?language=en",
            "person",
            "person_id",
            "4400",
        ),
        (
            "https://themoviedb.org/company/77/movie",
            "company",
            "company_id",
            "77",
        ),
        (
            "https://www.themoviedb.org/keyword/9951-fixture-keyword/tv",
            "keyword",
            "keyword_id",
            "9951",
        ),
    ];
    for (address, source, param, id) in cases {
        let matched = list
            .url_patterns
            .iter()
            .find_map(|pattern| {
                let regex = regex::Regex::new(&pattern.pattern).unwrap();
                regex.captures(address).map(|captures| {
                    (
                        pattern,
                        captures[pattern.captures[0].group.as_str()].to_string(),
                    )
                })
            })
            .unwrap_or_else(|| panic!("no pattern for {address}"));
        assert_eq!(matched.0.source_type, source);
        assert_eq!(matched.0.captures[0].param, param);
        assert_eq!(matched.1, id);
    }
    let any = |address: &str| {
        list.url_patterns.iter().any(|pattern| {
            regex::Regex::new(&pattern.pattern)
                .unwrap()
                .is_match(address)
        })
    };
    assert!(!any("https://www.themoviedb.org/movie/990201"));
    assert!(!any("https://themoviedb.org.example.test/list/1"));
}

#[test]
fn list_pages_through_the_cursor_with_global_ranks() {
    let http = RecordedHttp::new()
        .with(&v4("/list/8100001?page=1"), 200, LIST_PAGE_1)
        .with(&v4("/list/8100001?page=2"), 200, LIST_PAGE_2);
    let first = ok(fetch(
        &http,
        request("list", &[("list_id", "8100001")], None),
    ));
    assert_eq!(first.list_name.as_deref(), Some("Fixture Picks"));
    assert_eq!(
        first.list_url.as_deref(),
        Some("https://www.themoviedb.org/list/8100001")
    );
    assert_eq!(first.next_cursor.as_deref(), Some("2"));
    assert_eq!(first.total_hint, Some(23));
    assert!(first.fingerprint.is_none());
    assert_eq!(
        first.items.len(),
        2,
        "the person and the duplicate are dropped"
    );

    let movie = &first.items[0];
    assert_eq!(movie.item_key, "tmdb:movie:990201");
    assert_eq!(movie.rank, Some(1));
    assert_eq!(movie.year, Some(2031));
    assert_eq!(
        movie.genres,
        vec!["Drama".to_string(), "Thriller".to_string()]
    );
    assert_eq!(movie.language.as_deref(), Some("en"));
    assert_eq!(
        movie
            .provider_rating
            .as_ref()
            .map(|rating| (rating.scale.as_str(), rating.value)),
        Some(("tmdb", 7.4))
    );
    assert_eq!(movie.external_ids[0].kind.as_deref(), Some("movie"));

    let show = &first.items[1];
    assert_eq!(show.item_key, "tmdb:series:990202");
    assert_eq!(show.kind_hint, Some(ListMediaKind::Series));
    assert!(show.provider_rating.is_none(), "no votes, no rating");

    let second = ok(fetch(
        &http,
        request(
            "list",
            &[("list_id", "8100001")],
            first.next_cursor.as_deref(),
        ),
    ));
    assert!(second.next_cursor.is_none());
    assert_eq!(second.items[0].rank, Some(21));
    assert_eq!(second.items[0].year, None);
    assert_member_call(&http.requests()[0], &v4("/list/8100001?page=1"));
}

#[test]
fn person_credits_are_filtered_sorted_and_fingerprinted() {
    let body = r#"{
      "id": 4400, "name": "Fixture Person",
      "combined_credits": {
        "cast": [
          {"id": 990301, "media_type": "movie", "title": "Fixture Early", "release_date": "2021-01-01"},
          {"id": 990302, "media_type": "tv", "name": "Fixture Late Show", "first_air_date": "2030-01-01", "genre_ids": [10767]},
          {"id": 990303, "media_type": "tv", "name": "Fixture Serial", "first_air_date": "2028-05-05", "genre_ids": [18]},
          {"id": 990304, "media_type": "movie", "title": "Fixture Recent", "release_date": "2032-02-02"}
        ],
        "crew": [
          {"id": 990301, "media_type": "movie", "title": "Fixture Early", "release_date": "2021-01-01", "job": "Writer"},
          {"id": 990305, "media_type": "movie", "title": "Fixture Directed", "release_date": "2025-07-07", "job": "Director"}
        ]
      }
    }"#;
    let path = "/person/4400?append_to_response=combined_credits";
    let http = RecordedHttp::new().with(&v3(path), 200, body);

    let cast = ok(fetch(
        &http,
        request("person", &[("person_id", "4400-fixture-person")], None),
    ));
    let keys: Vec<_> = cast
        .items
        .iter()
        .map(|item| item.item_key.as_str())
        .collect();
    assert_eq!(
        keys,
        vec![
            "tmdb:movie:990304",
            "tmdb:series:990303",
            "tmdb:movie:990301"
        ]
    );
    assert_eq!(cast.list_name.as_deref(), Some("Fixture Person"));
    assert!(cast.fingerprint.is_some());

    let all_movies = ok(fetch(
        &http,
        request(
            "person",
            &[("person_id", "4400"), ("credit", "all"), ("kind", "movie")],
            None,
        ),
    ));
    let keys: Vec<_> = all_movies
        .items
        .iter()
        .map(|item| item.item_key.as_str())
        .collect();
    assert_eq!(
        keys,
        vec![
            "tmdb:movie:990304",
            "tmdb:movie:990305",
            "tmdb:movie:990301"
        ]
    );

    let mut again = request("person", &[("person_id", "4400")], None);
    again.since_fingerprint = cast.fingerprint.clone();
    let unchanged = ok(fetch(&http, again));
    assert!(unchanged.unchanged);
    assert!(unchanged.items.is_empty());
    // People are read from the v3 API, still with the member's token.
    assert_member_call(&http.requests()[0], &v3(path));
}

#[test]
fn person_crew_narrows_to_one_department() {
    let body = r#"{
      "id": 4401, "name": "Fixture Filmmaker",
      "combined_credits": {
        "cast": [
          {"id": 990311, "media_type": "movie", "title": "Fixture Cameo", "release_date": "2024-01-01"}
        ],
        "crew": [
          {"id": 990312, "media_type": "movie", "title": "Fixture Directed", "release_date": "2026-01-01", "department": "Directing", "job": "Director"},
          {"id": 990313, "media_type": "movie", "title": "Fixture Produced", "release_date": "2025-01-01", "department": "Production", "job": "Producer"},
          {"id": 990314, "media_type": "movie", "title": "Fixture Thanked", "release_date": "2023-01-01", "department": "Crew", "job": "Thanks"},
          {"id": 990315, "media_type": "tv", "name": "Fixture Written Serial", "first_air_date": "2022-01-01", "department": "Writing", "job": "Writer", "genre_ids": [18]}
        ]
      }
    }"#;
    let http = RecordedHttp::new().with(
        &v3("/person/4401?append_to_response=combined_credits"),
        200,
        body,
    );
    let keys = |credit: &str| -> Vec<String> {
        ok(fetch(
            &http,
            request("person", &[("person_id", "4401"), ("credit", credit)], None),
        ))
        .items
        .into_iter()
        .map(|item| item.item_key)
        .collect()
    };
    assert_eq!(keys("directing"), vec!["tmdb:movie:990312"]);
    assert_eq!(keys("production"), vec!["tmdb:movie:990313"]);
    assert_eq!(keys("Writing"), vec!["tmdb:series:990315"]);
    assert!(keys("sound").is_empty());
    assert_eq!(
        keys("crew"),
        vec![
            "tmdb:movie:990312",
            "tmdb:movie:990313",
            "tmdb:movie:990314",
            "tmdb:series:990315"
        ],
        "crew keeps every department"
    );
    let person = descriptor().list_provider().cloned().unwrap().groups[1].items[1].clone();
    let credit = person
        .params
        .iter()
        .find(|param| param.key == PARAM_CREDIT)
        .unwrap();
    assert_eq!(
        credit.options,
        vec![
            "cast",
            "crew",
            "all",
            "directing",
            "production",
            "sound",
            "writing"
        ]
    );
}

#[test]
fn company_and_keyword_use_discover_with_a_page_cap() {
    let body = r#"{"page": 99, "total_pages": 99, "total_results": 1980,
      "results": [{"id": 990401, "name": "Fixture Company Serial", "first_air_date": "2033-01-01"}]}"#;
    let http = RecordedHttp::new()
        .with(&v3("/discover/tv?with_companies=77&sort_by=first_air_date.desc&include_adult=false&page=99"), 200, body)
        .with(&v3("/discover/movie?with_keywords=9951&sort_by=primary_release_date.desc&include_adult=false&page=1"), 200, body);

    let company = ok(fetch(
        &http,
        request(
            "company",
            &[("company_id", "77"), ("kind", "series")],
            Some("99"),
        ),
    ));
    assert!(
        company.next_cursor.is_none(),
        "page {MAX_PAGES} is the last one followed"
    );
    assert_eq!(company.items[0].item_key, "tmdb:series:990401");
    assert_eq!(company.items[0].rank, Some(1961));
    assert_eq!(company.total_hint, Some(MAX_PAGES * 20));

    let keyword = ok(fetch(
        &http,
        request("keyword", &[("keyword_id", "9951")], None),
    ));
    assert_eq!(keyword.next_cursor.as_deref(), Some("2"));
    assert_eq!(
        keyword.items[0].item_key, "tmdb:movie:990401",
        "discover's kind wins over the payload"
    );
    assert_eq!(
        keyword.list_url.as_deref(),
        Some("https://www.themoviedb.org/keyword/9951/movie")
    );
    assert_member_call(
        &http.requests()[0],
        &v3(
            "/discover/tv?with_companies=77&sort_by=first_air_date.desc&include_adult=false&page=99",
        ),
    );
}

#[test]
fn sources_by_id_past_the_page_cap_fail_instead_of_being_cut_short() {
    let oversized = format!(
        r#"{{"page": 1, "total_pages": {}, "total_results": 8403, "items": [], "results": []}}"#,
        MAX_PAGES + 1
    );
    let http = RecordedHttp::new()
        .with(&v4("/list/8100001?page=1"), 200, &oversized)
        .with(
            &v3("/discover/movie?with_keywords=9951&sort_by=primary_release_date.desc&include_adult=false&page=1"),
            200,
            &oversized,
        );
    for request in [
        request("list", &[("list_id", "8100001")], None),
        request("keyword", &[("keyword_id", "9951")], None),
    ] {
        let error = err(fetch(&http, request));
        assert_eq!(error.code, PluginErrorCode::Permanent);
        assert!(error.public_message.contains("1980 titles"));
    }
    assert_eq!(http.requests().len(), 2, "one page read per source");
}

#[test]
fn errors_map_to_host_failure_classes() {
    let person = |status: u16, headers: &[(&str, &str)], body: &str| {
        let http = RecordedHttp::new().with_headers(
            &v3("/person/4400?append_to_response=combined_credits"),
            status,
            headers,
            body,
        );
        err(fetch(
            &http,
            request("person", &[("person_id", "4400")], None),
        ))
    };
    // A token TMDb turns away is the member's account to reconnect.
    for body in [
        r#"{"status_code": 7, "status_message": "Invalid API key"}"#,
        r#"{"status_code": 35, "status_message": "Invalid token."}"#,
        r#"{"status_code": 3, "status_message": "Authentication failed"}"#,
    ] {
        let rejected = person(401, &[], body);
        assert_eq!(rejected.code, PluginErrorCode::AuthFailed);
        assert_no_secrets(&rejected);
    }
    let missing = person(404, &[], r#"{"status_code": 34}"#);
    assert_eq!(
        missing.code,
        PluginErrorCode::from(scryer_plugin_pdk::error::codes::ITEM_NOT_FOUND)
    );
    assert!(missing.public_message.contains("not found"));
    let limited = person(429, &[("Retry-After", "30")], "");
    assert_eq!(
        (limited.code, limited.retry_after_seconds),
        (PluginErrorCode::RateLimited, Some(30))
    );
    assert_eq!(
        person(503, &[], "").code,
        PluginErrorCode::UpstreamUnavailable
    );
    assert_eq!(
        person(200, &[], "not json").code,
        PluginErrorCode::from(scryer_plugin_pdk::error::codes::INVALID_RESPONSE)
    );

    let http = RecordedHttp::new();
    assert_eq!(
        err(fetch(
            &http,
            request("list", &[("list_id", "fixture")], None)
        ))
        .code,
        PluginErrorCode::InvalidConfig
    );
    assert_eq!(
        err(fetch(&http, request("list", &[], None))).code,
        PluginErrorCode::InvalidConfig
    );
    assert_eq!(
        err(fetch(&http, request("collection", &[], None))).code,
        PluginErrorCode::Unsupported
    );
    assert!(http.urls().is_empty());
}

#[test]
fn health_has_no_server_key_to_check() {
    let http = RecordedHttp::new();
    let health = match block_on(run(
        &http,
        PluginListCommand::Health(ListPluginHealthRequest {}),
    )) {
        PluginListCommandResult::Health(result) => result,
        other => panic!("unexpected {other:?}"),
    };
    assert_eq!(err(health).code, PluginErrorCode::Unsupported);
    assert!(http.urls().is_empty());
}

const MEMBER_TOKEN: &str = "eyJhbGciOiJIUzI1NiJ9.eyJtZW1iZXIiOnRydWV9.bWVtYmVyc2lnbg";
const ACCOUNT_ID: &str = "fixture0account0object01";

fn member() -> ListCredential {
    ListCredential {
        access_token: MEMBER_TOKEN.to_string(),
        token_type: Some("Bearer".to_string()),
        external_user_id: Some(ACCOUNT_ID.to_string()),
        username: Some("fixture-member".to_string()),
    }
}

fn v4(path_and_query: &str) -> String {
    format!("{API_V4_BASE}{path_and_query}")
}

fn account_url(path_and_query: &str) -> String {
    v4(&format!("/account/{ACCOUNT_ID}{path_and_query}"))
}

fn account(
    http: &RecordedHttp,
    credential: ListCredential,
) -> PluginResult<ListPluginAccountResponse> {
    match block_on(run(
        http,
        PluginListCommand::Account(ListPluginAccountRequest { credential }),
    )) {
        PluginListCommandResult::Account(result) => result,
        other => panic!("unexpected {other:?}"),
    }
}

/// A member call carries the member's token as the bearer and nowhere else,
/// and no key rides in the address.
fn assert_member_call(sent: &PluginHttpRequest, expected_url: &str) {
    assert_eq!(sent.url, expected_url);
    assert_eq!(
        sent.headers.get("Authorization").map(String::as_str),
        Some(format!("Bearer {MEMBER_TOKEN}").as_str())
    );
    assert!(!sent.url.contains(MEMBER_TOKEN));
    assert!(!sent.url.contains("api_key"));
}

fn assert_no_secrets(error: &scryer_plugin_sdk::PluginError) {
    let rendered = serde_json::to_string(error).unwrap();
    assert!(!rendered.contains(MEMBER_TOKEN));
}

#[test]
fn every_source_is_a_members_own() {
    let list = descriptor().list_provider().cloned().unwrap();
    let [mine, by_id] = &list.groups[..] else {
        panic!("the account's sources, then the sources named by id");
    };
    for group in &list.groups {
        assert_eq!(group.auth_badge, ListAuthBadge::MemberAccount);
        for item in &group.items {
            assert!(item.personal, "{}", item.id);
            assert_eq!(item.default_interval_seconds, 12 * 60 * 60);
            assert_eq!(
                item.kinds,
                vec![ListMediaKind::Movie, ListMediaKind::Series]
            );
        }
    }
    let sources = |group: &ListProviderGroup| -> Vec<String> {
        group
            .items
            .iter()
            .map(|item| item.source_type.clone())
            .collect()
    };
    assert_eq!(
        sources(mine),
        vec![
            "watchlist",
            "favorites",
            "rated",
            "recommendations",
            "account_list"
        ]
    );
    assert_eq!(sources(by_id), vec!["list", "person", "company", "keyword"]);
    for item in &mine.items[..4] {
        let [kind] = &item.params[..] else {
            panic!("{} takes only a media kind", item.id);
        };
        assert_eq!(kind.key, PARAM_KIND);
        assert!(!kind.required);
        assert_eq!(kind.options, vec!["all", "movie", "series"]);
    }
    let [list_id] = &mine.items[4].params[..] else {
        panic!("an account list takes only its id");
    };
    assert_eq!(list_id.key, PARAM_LIST_ID);
    assert!(list_id.required);

    assert!(list.capabilities.account);
    // The host hands this plugin no fetch without a member's credential.
    assert!(list.capabilities.requires_member_credential);
}

#[test]
fn watchlist_pages_movies_then_shows_with_the_member_token() {
    let movies_1 = r#"{"page": 1, "total_pages": 2, "total_results": 21, "results": [
      {"id": 990501, "media_type": "movie", "title": "Fixture Watch Alpha", "release_date": "2032-01-01",
       "vote_average": 6.5, "vote_count": 40},
      {"id": 990502, "media_type": "movie", "title": "Fixture Watch Beta", "release_date": "2031-06-06"}
    ]}"#;
    let movies_2 = r#"{"page": 2, "total_pages": 2, "total_results": 21, "results": [
      {"id": 990503, "media_type": "movie", "title": "Fixture Watch Gamma", "release_date": "2030-02-02"}
    ]}"#;
    let shows_1 = r#"{"page": 1, "total_pages": 1, "total_results": 1, "results": [
      {"id": 990601, "media_type": "tv", "name": "Fixture Watch Serial", "first_air_date": "2029-09-09"}
    ]}"#;
    let movie_page = |page| {
        account_url(&format!(
            "/movie/watchlist?sort_by=created_at.desc&page={page}"
        ))
    };
    let show_page = account_url("/tv/watchlist?sort_by=created_at.desc&page=1");
    let http = RecordedHttp::new()
        .with(&movie_page(1), 200, movies_1)
        .with(&movie_page(2), 200, movies_2)
        .with(&show_page, 200, shows_1);

    let first = ok(fetch(&http, request("watchlist", &[], None)));
    let keys: Vec<_> = first
        .items
        .iter()
        .map(|item| item.item_key.as_str())
        .collect();
    assert_eq!(keys, vec!["tmdb:movie:990501", "tmdb:movie:990502"]);
    assert_eq!(first.items[0].rank, Some(1));
    assert_eq!(first.items[0].kind_hint, Some(ListMediaKind::Movie));
    assert_eq!(first.next_cursor.as_deref(), Some("movie:2:0"));
    assert!(first.fingerprint.is_none() && first.total_hint.is_none());
    assert!(first.list_url.is_none());

    let second = ok(fetch(
        &http,
        request("watchlist", &[], first.next_cursor.as_deref()),
    ));
    assert_eq!(second.items[0].item_key, "tmdb:movie:990503");
    assert_eq!(second.items[0].rank, Some(21));
    assert_eq!(
        second.next_cursor.as_deref(),
        Some("series:1:21"),
        "shows follow the last movie"
    );

    let third = ok(fetch(
        &http,
        request(
            "watchlist",
            &[("kind", "all")],
            second.next_cursor.as_deref(),
        ),
    ));
    assert_eq!(third.items[0].item_key, "tmdb:series:990601");
    assert_eq!(third.items[0].rank, Some(22));
    assert!(third.next_cursor.is_none());

    let sent = http.requests();
    assert_eq!(sent.len(), 3);
    for (request, expected) in sent.iter().zip([movie_page(1), movie_page(2), show_page]) {
        assert_member_call(request, &expected);
    }
}

#[test]
fn favorites_page_one_kind_by_number() {
    let shows = r#"{"page": 1, "total_pages": 99, "total_results": 1980, "results": [
      {"id": 990611, "name": "Fixture Favorite Serial", "first_air_date": "2028-03-03"}
    ]}"#;
    let page = |page| {
        account_url(&format!(
            "/tv/favorites?sort_by=created_at.desc&page={page}"
        ))
    };
    let http = RecordedHttp::new()
        .with(&page(1), 200, shows)
        .with(&page(MAX_PAGES), 200, shows);

    let first = ok(fetch(
        &http,
        request("favorites", &[("kind", "series")], None),
    ));
    assert_eq!(
        first.items[0].item_key, "tmdb:series:990611",
        "favorites carry no media type; the endpoint's kind decides"
    );
    assert_eq!(first.next_cursor.as_deref(), Some("2"));
    assert_eq!(first.total_hint, Some(MAX_PAGES * 20));

    let last = ok(fetch(
        &http,
        request("favorites", &[("kind", "series")], Some("99")),
    ));
    assert!(
        last.next_cursor.is_none(),
        "page {MAX_PAGES} is the last one followed"
    );
    assert_eq!(last.items[0].rank, Some(1961));
    assert_member_call(&http.requests()[0], &page(1));

    // Past the cap the collection fails rather than being cut short, which
    // would read as its oldest titles leaving it. With both kinds each side
    // gets half the pages.
    let oversized = |pages: u32| {
        format!(r#"{{"page": 1, "total_pages": {pages}, "total_results": 9999, "results": []}}"#)
    };
    let too_long = RecordedHttp::new().with(&page(1), 200, &oversized(MAX_PAGES + 1));
    let error = err(fetch(
        &too_long,
        request("favorites", &[("kind", "series")], None),
    ));
    assert_eq!(error.code, PluginErrorCode::Permanent);
    assert!(error.public_message.contains("1980 titles"));
    assert!(!error.public_message.contains("not found"));
    let too_long_for_half = RecordedHttp::new().with(
        &account_url("/movie/favorites?sort_by=created_at.desc&page=1"),
        200,
        &oversized(MAX_PAGES / 2 + 1),
    );
    let error = err(fetch(&too_long_for_half, request("favorites", &[], None)));
    assert_eq!(error.code, PluginErrorCode::Permanent);
    assert!(
        error.public_message.contains("980 movies")
            && error.public_message.contains("movies and shows together"),
        "the per-kind limit is named: {}",
        error.public_message
    );

    // With both kinds, an empty movie side hands straight over to shows.
    let empty = RecordedHttp::new().with(
        &account_url("/movie/favorites?sort_by=created_at.desc&page=1"),
        200,
        r#"{"page": 1, "total_pages": 0, "total_results": 0, "results": []}"#,
    );
    let none = ok(fetch(&empty, request("favorites", &[], None)));
    assert!(none.items.is_empty());
    assert_eq!(none.next_cursor.as_deref(), Some("series:1:0"));
}

#[test]
fn rated_sorts_newest_first_and_recommendations_take_tmdbs_order() {
    let page = r#"{"page": 1, "total_pages": 1, "total_results": 1, "results": [
      {"id": 990701, "title": "Fixture Rated Feature", "release_date": "2027-07-07",
       "account_rating": {"created_at": "2030-01-01T00:00:00.000Z", "value": 4}}
    ]}"#;
    let rated = account_url("/tv/rated?sort_by=created_at.desc&page=1");
    let recommended = account_url("/movie/recommendations?page=1");
    let http = RecordedHttp::new()
        .with(&rated, 200, page)
        .with(&recommended, 200, page);

    let shows = ok(fetch(&http, request("rated", &[("kind", "series")], None)));
    assert_eq!(shows.items[0].item_key, "tmdb:series:990701");
    assert!(shows.next_cursor.is_none());

    let movies = ok(fetch(
        &http,
        request("recommendations", &[("kind", "movie")], None),
    ));
    assert_eq!(movies.items[0].item_key, "tmdb:movie:990701");

    let sent = http.requests();
    assert_member_call(&sent[0], &rated);
    assert_member_call(&sent[1], &recommended);
}

#[test]
fn account_list_reads_the_members_list_with_their_token() {
    let body = r#"{
      "id": 8200001, "name": "Fixture Private Shelf", "public": false, "page": 1,
      "total_pages": 1, "total_results": 2,
      "results": [
        {"id": 990201, "media_type": "movie", "title": "Fixture Feature Alpha", "release_date": "2031-03-04"},
        {"id": 990202, "media_type": "tv", "name": "Fixture Serial Beta", "first_air_date": "2029-10-01"}
      ]
    }"#;
    let http = RecordedHttp::new().with(&v4("/list/8200001?page=1"), 200, body);

    let mine = ok(fetch(
        &http,
        request("account_list", &[("list_id", "8200001")], None),
    ));
    assert_member_call(&http.requests()[0], &v4("/list/8200001?page=1"));
    assert_eq!(mine.list_name.as_deref(), Some("Fixture Private Shelf"));
    assert_eq!(
        mine.list_url.as_deref(),
        Some("https://www.themoviedb.org/list/8200001")
    );
    assert_eq!(mine.total_hint, Some(2));
    assert!(mine.next_cursor.is_none());

    // A list followed by its id is the same read.
    let by_id = ok(fetch(
        &http,
        request("list", &[("list_id", "8200001")], None),
    ));
    assert_member_call(&http.requests()[1], &v4("/list/8200001?page=1"));
    let keys = |response: &scryer_plugin_sdk::ListPluginFetchResponse| {
        response
            .items
            .iter()
            .map(|item| item.item_key.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(keys(&mine), keys(&by_id));
}

#[test]
fn account_names_the_member_and_their_lists() {
    let lists_1 = r#"{"page": 1, "total_pages": 2, "total_results": 3, "results": [
      {"id": 8200001, "name": "Fixture Private Shelf", "public": 0, "number_of_items": 2},
      {"id": 8200002, "name": "Fixture Weekend Queue", "public": 1, "number_of_items": 0}
    ]}"#;
    let lists_2 =
        r#"{"page": 2, "total_pages": 2, "total_results": 3, "results": [{"id": 8200003}]}"#;
    let details = r#"{"id": 7300001, "username": "fixture-member-handle", "name": "Fixture Member",
       "include_adult": false, "iso_639_1": "en", "iso_3166_1": "US",
       "avatar": {"gravatar": {"hash": ""}, "tmdb": {"avatar_path": "/fixtureavatar.png"}}}"#;
    let http = RecordedHttp::new()
        .with(&account_url("/lists?page=1"), 200, lists_1)
        .with(&account_url("/lists?page=2"), 200, lists_2)
        .with(&v3("/account"), 200, details);

    let linked = ok(account(&http, member()));
    assert_eq!(linked.external_user_id, ACCOUNT_ID);
    assert_eq!(linked.username, "fixture-member-handle");
    assert_eq!(linked.display_name.as_deref(), Some("Fixture Member"));
    assert_eq!(
        linked.avatar_url.as_deref(),
        Some("https://image.tmdb.org/t/p/original/fixtureavatar.png")
    );
    let lists: Vec<_> = linked
        .owned_lists
        .iter()
        .map(|list| (list.id.as_str(), list.name.as_str()))
        .collect();
    assert_eq!(
        lists,
        vec![
            ("8200001", "Fixture Private Shelf"),
            ("8200002", "Fixture Weekend Queue"),
            ("8200003", "List 8200003"),
        ]
    );
    assert!(
        linked
            .owned_lists
            .iter()
            .all(|list| list.kinds == vec![ListMediaKind::Movie, ListMediaKind::Series])
    );
    assert!(linked.statuses.is_empty());
    let sent = http.requests();
    for (request, expected) in sent.iter().zip([
        account_url("/lists?page=1"),
        account_url("/lists?page=2"),
        v3("/account"),
    ]) {
        assert_member_call(request, &expected);
    }
    assert_eq!(sent.len(), 3);

    // A member without lists is named by their account details all the same.
    let empty_index = r#"{"page": 1, "total_pages": 0, "total_results": 0, "results": []}"#;
    let listless = RecordedHttp::new()
        .with(&account_url("/lists?page=1"), 200, empty_index)
        .with(&v3("/account"), 200, details);
    let named = ok(account(&listless, member()));
    assert_eq!(named.username, "fixture-member-handle");
    assert_eq!(named.display_name.as_deref(), Some("Fixture Member"));
    assert!(named.owned_lists.is_empty());

    // Where TMDb does not serve the details, the owner of the member's first
    // list names them.
    let first_list = format!(
        r#"{{"id": 8200001, "name": "Fixture Private Shelf", "page": 1, "total_pages": 1, "results": [],
           "created_by": {{"id": "{ACCOUNT_ID}", "username": "fixture-shelf-owner",
                           "name": "Fixture Shelf Owner", "avatar_path": "/fixtureshelf.png", "gravatar_hash": ""}}}}"#
    );
    for (status, body) in [
        (401, r#"{"status_code": 3}"#),
        (404, r#"{"status_code": 34}"#),
        (500, ""),
        (200, "not json"),
        (
            200,
            r#"{"id": 7300001, "username": "", "name": "Fixture Nameless"}"#,
        ),
    ] {
        let http = RecordedHttp::new()
            .with(&account_url("/lists?page=1"), 200, lists_1)
            .with(&account_url("/lists?page=2"), 200, lists_2)
            .with(&v3("/account"), status, body)
            .with(&v4("/list/8200001?page=1"), 200, &first_list);
        let linked = ok(account(&http, member()));
        assert_eq!(linked.username, "fixture-shelf-owner", "{status} {body}");
        assert_eq!(
            linked.display_name.as_deref(),
            Some("Fixture Shelf Owner"),
            "{status} {body}"
        );
        assert_eq!(
            linked.avatar_url.as_deref(),
            Some("https://image.tmdb.org/t/p/original/fixtureshelf.png")
        );
        assert_member_call(&http.requests()[3], &v4("/list/8200001?page=1"));
    }

    // A member named by neither is named by the credential, or by the account
    // id.
    let no_lists = RecordedHttp::new().with(&account_url("/lists?page=1"), 200, empty_index);
    let bare = ok(account(&no_lists, member()));
    assert_eq!(bare.username, "fixture-member");
    assert!(bare.display_name.is_none() && bare.avatar_url.is_none());
    assert!(bare.owned_lists.is_empty());
    assert_eq!(
        no_lists.urls(),
        vec![account_url("/lists?page=1"), v3("/account")]
    );
    let anonymous = ListCredential {
        username: None,
        ..member()
    };
    assert_eq!(ok(account(&no_lists, anonymous)).username, ACCOUNT_ID);

    // A first list owned by someone else lends the member no identity.
    let foreign = RecordedHttp::new()
        .with(
            &account_url("/lists?page=1"),
            200,
            r#"{"total_pages": 1, "results": [{"id": 8200001}]}"#,
        )
        .with(
            &v4("/list/8200001?page=1"),
            200,
            r#"{"created_by": {"id": "fixture0other0account0002", "username": "fixture-other"}}"#,
        );
    let named = ok(account(&foreign, member()));
    assert_eq!(named.username, "fixture-member");
    assert!(named.display_name.is_none());
}

#[test]
fn a_rejected_member_token_is_an_expired_account() {
    let watchlist = |status: u16, headers: &[(&str, &str)], body: &str| {
        let http = RecordedHttp::new().with_headers(
            &account_url("/movie/watchlist?sort_by=created_at.desc&page=1"),
            status,
            headers,
            body,
        );
        err(fetch(&http, request("watchlist", &[], None)))
    };
    let own_list = |status: u16, body: &str| {
        let http = RecordedHttp::new().with(&v4("/list/8200001?page=1"), status, body);
        err(fetch(
            &http,
            request("account_list", &[("list_id", "8200001")], None),
        ))
    };

    // Any 401 on the member's own account means the link no longer works,
    // whatever code TMDb gives it.
    let mut errors = vec![
        watchlist(
            401,
            &[],
            r#"{"status_code": 3, "status_message": "Authentication failed"}"#,
        ),
        watchlist(401, &[], r#"{"status_code": 7}"#),
        watchlist(401, &[], ""),
        own_list(401, r#"{"status_code": 3}"#),
    ];
    let lists_401 =
        RecordedHttp::new().with(&account_url("/lists?page=1"), 401, r#"{"status_code": 3}"#);
    errors.push(err(account(&lists_401, member())));
    for error in &errors {
        assert_eq!(error.code, PluginErrorCode::AuthFailed);
        assert_no_secrets(error);
    }

    // A list TMDb calls private is not this member's to read; the account is
    // fine.
    let private = own_list(
        401,
        r#"{"status_code": 39, "status_message": "This resource is private."}"#,
    );
    assert_eq!(
        private.code,
        PluginErrorCode::from(scryer_plugin_pdk::error::codes::ITEM_NOT_FOUND)
    );
    assert!(private.public_message.contains("not found"));
    let missing = own_list(404, r#"{"status_code": 34}"#);
    assert!(missing.public_message.contains("not found"));

    // A suspended member is refused with a 403 whatever the read names, and
    // nothing they follow is gone.
    let suspended = r#"{"status_code": 45, "status_message": "This user has been suspended."}"#;
    let person = RecordedHttp::new().with(
        &v3("/person/4401?append_to_response=combined_credits"),
        403,
        suspended,
    );
    for error in [
        own_list(403, suspended),
        err(fetch(
            &person,
            request("person", &[("person_id", "4401")], None),
        )),
    ] {
        assert_eq!(error.code, PluginErrorCode::AuthFailed);
        assert!(!error.public_message.contains("not found"));
        assert_no_secrets(&error);
    }

    let limited = watchlist(429, &[("Retry-After", "12")], "");
    assert_eq!(
        (limited.code.clone(), limited.retry_after_seconds),
        (PluginErrorCode::RateLimited, Some(12))
    );
    assert_eq!(
        watchlist(503, &[], "").code,
        PluginErrorCode::UpstreamUnavailable
    );
    for error in [private, missing, limited] {
        assert_no_secrets(&error);
    }
}

#[test]
fn every_source_needs_a_linked_account() {
    let http = RecordedHttp::new();
    let personal_with =
        |source: &str, params: &[(&str, &str)], credential: Option<ListCredential>| {
            let mut request = request(source, params, None);
            request.credential = credential;
            err(fetch(&http, request))
        };
    let cases = [
        ("watchlist", &[][..]),
        ("favorites", &[("kind", "movie")][..]),
        ("account_list", &[("list_id", "8200001")][..]),
        ("list", &[("list_id", "8100001")][..]),
        ("person", &[("person_id", "4400")][..]),
        ("company", &[("company_id", "77")][..]),
        ("keyword", &[("keyword_id", "9951")][..]),
    ];
    for (source, params) in cases {
        for credential in [
            None,
            Some(ListCredential {
                access_token: "  ".to_string(),
                ..member()
            }),
            Some(ListCredential {
                external_user_id: None,
                ..member()
            }),
            Some(ListCredential {
                external_user_id: Some(String::new()),
                ..member()
            }),
        ] {
            let error = personal_with(source, params, credential);
            assert_eq!(error.code, PluginErrorCode::AuthFailed, "{source}");
            assert_no_secrets(&error);
        }
    }
    assert_eq!(
        err(account(
            &http,
            ListCredential {
                external_user_id: None,
                ..member()
            }
        ))
        .code,
        PluginErrorCode::AuthFailed
    );
    assert_eq!(
        personal_with("account_list", &[], Some(member())).code,
        PluginErrorCode::InvalidConfig
    );
    assert_eq!(
        personal_with("watchlist", &[("kind", "person")], Some(member())).code,
        PluginErrorCode::InvalidConfig
    );
    assert!(http.urls().is_empty());
}

#[test]
fn both_kind_cursors_are_validated() {
    let http = RecordedHttp::new();
    for cursor in [
        "movie:0:0",
        "show:1:0",
        "movie:2",
        "movie:x:0",
        "series:1:-1",
        "2",
    ] {
        let error = err(fetch(&http, request("watchlist", &[], Some(cursor))));
        assert_eq!(error.code, PluginErrorCode::Permanent, "{cursor}");
    }
    let error = err(fetch(
        &http,
        request("favorites", &[("kind", "movie")], Some("series:1:0")),
    ));
    assert_eq!(error.code, PluginErrorCode::Permanent);
    assert!(http.urls().is_empty());
}

/// A list page without `total_pages`, holding `count` movies whose ids start
/// at `first_id`, with `item_count` when given.
fn untotalled_list_page(first_id: u32, count: u32, item_count: Option<u32>) -> String {
    let items: Vec<String> = (first_id..first_id + count)
        .map(|id| {
            format!(
                r#"{{"id": {id}, "media_type": "movie", "title": "Fixture Shelf Feature", "release_date": "2030-01-01"}}"#
            )
        })
        .collect();
    let count_field = item_count
        .map(|count| format!(r#""item_count": {count}, "#))
        .unwrap_or_default();
    format!(
        r#"{{"id": 8100002, "name": "Fixture Untotalled", {count_field}"results": [{}]}}"#,
        items.join(",")
    )
}

#[test]
fn a_list_without_total_pages_pages_by_its_item_count() {
    let http = RecordedHttp::new()
        .with(
            &v4("/list/8100002?page=1"),
            200,
            &untotalled_list_page(990800, 20, Some(23)),
        )
        .with(
            &v4("/list/8100002?page=2"),
            200,
            &untotalled_list_page(990820, 3, Some(23)),
        );
    let list =
        |cursor: Option<&str>| fetch(&http, request("list", &[("list_id", "8100002")], cursor));
    let first = ok(list(None));
    assert_eq!(
        first.next_cursor.as_deref(),
        Some("2"),
        "23 items do not fit on one page"
    );
    assert_eq!(first.total_hint, Some(23));
    let second = ok(list(first.next_cursor.as_deref()));
    assert!(second.next_cursor.is_none());
    assert_eq!(second.items[0].rank, Some(21));
    assert_eq!(second.items.len(), 3);

    // A list served whole on its first page ends there.
    let whole = RecordedHttp::new().with(
        &v4("/list/8100002?page=1"),
        200,
        &untotalled_list_page(990800, 23, Some(23)),
    );
    let response = ok(fetch(
        &whole,
        request("list", &[("list_id", "8100002")], None),
    ));
    assert!(response.next_cursor.is_none());
    assert_eq!(response.items.len(), 23);

    // A short list with no count at all is a single page.
    let short = RecordedHttp::new().with(
        &v4("/list/8100002?page=1"),
        200,
        &untotalled_list_page(990800, 4, None),
    );
    let response = ok(fetch(
        &short,
        request("list", &[("list_id", "8100002")], None),
    ));
    assert!(response.next_cursor.is_none());
    assert_eq!(response.items.len(), 4);
}

#[test]
fn a_list_that_cannot_vouch_for_its_end_fails() {
    let fails = |cursor: Option<&str>, page: u32, body: String| {
        let http = RecordedHttp::new().with(&v4(&format!("/list/8100002?page={page}")), 200, &body);
        let error = err(fetch(
            &http,
            request("list", &[("list_id", "8100002")], cursor),
        ));
        assert_eq!(error.code, PluginErrorCode::Permanent);
        assert!(
            !error.public_message.contains("not found"),
            "the list is not gone: {}",
            error.public_message
        );
        error
    };
    // An empty page before every item was read.
    fails(Some("2"), 2, untotalled_list_page(990820, 0, Some(23)));
    // A full page with no count may have more behind it.
    fails(None, 1, untotalled_list_page(990800, 20, None));
    // A count past the cap fails on the first page.
    let error = fails(
        None,
        1,
        untotalled_list_page(990800, 20, Some(MAX_PAGES * 20 + 1)),
    );
    assert!(error.public_message.contains("1980 titles"));
}

#[test]
fn cursors_past_the_page_cap_are_never_sent() {
    let http = RecordedHttp::new();
    let past = (MAX_PAGES + 1).to_string();
    for request in [
        request("list", &[("list_id", "8100001")], Some(&past)),
        request("company", &[("company_id", "77")], Some(&past)),
        request("keyword", &[("keyword_id", "9951")], Some(&past)),
        request("favorites", &[("kind", "movie")], Some(&past)),
        request("account_list", &[("list_id", "8200001")], Some(&past)),
        request(
            "watchlist",
            &[],
            Some(&format!("series:{}:0", MAX_PAGES / 2 + 1)),
        ),
        request("watchlist", &[], Some("movie:4294967295:4294967295")),
    ] {
        let source = request.source_type.clone();
        let error = err(fetch(&http, request));
        assert_eq!(error.code, PluginErrorCode::Permanent, "{source}");
    }
    assert!(http.urls().is_empty());

    // The last allowed both-kinds page still reads, and huge rank bases
    // saturate rather than overflow.
    let last = account_url(&format!(
        "/tv/watchlist?sort_by=created_at.desc&page={}",
        MAX_PAGES / 2
    ));
    let http = RecordedHttp::new().with(
        &last,
        200,
        r#"{"page": 49, "total_pages": 49, "results": [{"id": 990901, "name": "Fixture Late Serial"}]}"#,
    );
    let response = ok(fetch(
        &http,
        request(
            "watchlist",
            &[],
            Some(&format!("series:{}:4294967290", MAX_PAGES / 2)),
        ),
    ));
    assert_eq!(response.items[0].rank, Some(u32::MAX));
    assert!(response.next_cursor.is_none());
}

#[test]
fn recommendations_past_the_page_cap_fail_instead_of_being_cut_short() {
    let page = |kind: &str, pages: u32| {
        (
            account_url(&format!("/{kind}/recommendations?page=1")),
            format!(
                r#"{{"page": 1, "total_pages": {pages}, "total_results": 9999, "results": [
                  {{"id": 990951, "title": "Fixture Suggested"}}]}}"#
            ),
        )
    };
    let (at_cap_url, at_cap) = page("movie", MAX_PAGES);
    let http = RecordedHttp::new().with(&at_cap_url, 200, &at_cap);
    let response = ok(fetch(
        &http,
        request("recommendations", &[("kind", "movie")], None),
    ));
    assert_eq!(response.next_cursor.as_deref(), Some("2"));

    let (past_url, past) = page("movie", MAX_PAGES + 1);
    let http = RecordedHttp::new().with(&past_url, 200, &past);
    let error = err(fetch(
        &http,
        request("recommendations", &[("kind", "movie")], None),
    ));
    assert_eq!(error.code, PluginErrorCode::Permanent);
    assert!(error.public_message.contains("1980 titles"));

    let (half_url, half) = page("movie", MAX_PAGES / 2 + 1);
    let http = RecordedHttp::new().with(&half_url, 200, &half);
    let error = err(fetch(&http, request("recommendations", &[], None)));
    assert_eq!(error.code, PluginErrorCode::Permanent);
    assert!(error.public_message.contains("980 movies"));
}

#[test]
fn a_refused_or_missing_account_collection_never_reads_as_gone() {
    let collections = [
        account_url("/movie/watchlist?sort_by=created_at.desc&page=1"),
        account_url("/movie/favorites?sort_by=created_at.desc&page=1"),
        account_url("/movie/rated?sort_by=created_at.desc&page=1"),
        account_url("/movie/recommendations?page=1"),
    ];
    let sources = ["watchlist", "favorites", "rated", "recommendations"];
    for (address, source) in collections.iter().zip(sources) {
        for (status, code) in [
            (403, PluginErrorCode::AuthFailed),
            (404, PluginErrorCode::Permanent),
            (410, PluginErrorCode::Permanent),
        ] {
            let http = RecordedHttp::new().with(address, status, r#"{"status_code": 34}"#);
            let error = err(fetch(&http, request(source, &[("kind", "movie")], None)));
            assert_eq!(error.code, code, "{source} {status}");
            assert!(
                !error.public_message.contains("not found"),
                "{source} {status}: {}",
                error.public_message
            );
            assert_no_secrets(&error);
        }
    }

    // The list index the account operation reads is the member's own too.
    for (status, code) in [
        (403, PluginErrorCode::AuthFailed),
        (404, PluginErrorCode::Permanent),
    ] {
        let http = RecordedHttp::new().with(&account_url("/lists?page=1"), status, "");
        let error = err(account(&http, member()));
        assert_eq!(error.code, code, "{status}");
        assert!(!error.public_message.contains("not found"), "{status}");
    }
}

#[test]
fn an_unreadable_first_list_leaves_the_account_unnamed_by_it() {
    let lists =
        r#"{"page": 1, "total_pages": 1, "results": [{"id": 8200001, "name": "Fixture Shelf"}]}"#;
    for (status, body) in [
        (500, ""),
        (404, r#"{"status_code": 34}"#),
        (401, r#"{"status_code": 39}"#),
        (200, "not json"),
    ] {
        let http = RecordedHttp::new()
            .with(&account_url("/lists?page=1"), 200, lists)
            .with(&v4("/list/8200001?page=1"), status, body);
        let linked = ok(account(&http, member()));
        assert_eq!(linked.username, "fixture-member", "{status}");
        assert!(linked.display_name.is_none() && linked.avatar_url.is_none());
        assert_eq!(linked.owned_lists.len(), 1);
    }
}

#[test]
fn a_rejected_member_token_is_auth_failed_with_tmdbs_reason_in_debug_only() {
    // `auth_failed`, not `invalid_api_key`: the host expires the linked
    // account and asks for a reconnect only on this well-known code.
    let http = RecordedHttp::new().with(
        &v3("/person/4400?append_to_response=combined_credits"),
        401,
        r#"{"status_code": 7, "status_message": "Invalid API key: fixture"}"#,
    );
    let rejected = err(fetch(
        &http,
        request("person", &[("person_id", "4400")], None),
    ));
    assert_eq!(rejected.code, PluginErrorCode::AuthFailed);
    assert!(!rejected.public_message.contains("401"));
    assert!(!rejected.public_message.contains("Invalid API key"));
    let debug = rejected.debug_message.as_deref().unwrap();
    assert!(debug.ends_with("HTTP 401, TMDb status_code 7"), "{debug}");
    assert_no_secrets(&rejected);
}
