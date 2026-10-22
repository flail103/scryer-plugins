use std::collections::HashMap;

use newznab_common::{
    Capabilities, IndexerCategoryModel, IndexerCategoryValueKind, IndexerDescriptor,
    IndexerFeedMode, IndexerLimitCapabilities, IndexerProtocol, IndexerSearchInput,
    IndexerSourceKind, NewznabConfig, PluginActionRequest, PluginActionResponse, PluginDescriptor,
    ProviderDescriptor, SDK_VERSION, SearchRequest, SearchResponse, current_sdk_constraint,
    execute_paged_search, extract_profile_metadata, standard_config_fields,
};
use scryer_plugin_pdk::*;

mod provider_profiles;

fn build_descriptor() -> PluginDescriptor {
    let (provider_profiles, response_features) = provider_profiles::load();
    let scoring_policies = provider_profiles::scoring_policies(&provider_profiles);
    let mut config_fields = standard_config_fields(None);
    if let Some(base_url) = config_fields
        .iter_mut()
        .find(|field| field.key == "base_url")
    {
        base_url.required = false;
        base_url.help_text = Some(
            "Required for Custom; known providers use their bundled API endpoint unless overridden."
                .to_string(),
        );
    }
    config_fields.insert(0, provider_profiles::selector(&provider_profiles));
    PluginDescriptor {
        id: "newznab".to_string(),
        name: "Newznab Indexer".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        sdk_version: SDK_VERSION.to_string(),
        sdk_constraint: current_sdk_constraint(),
        socket_permissions: vec![],
        settings: Vec::new(),
        provider: ProviderDescriptor::Indexer(IndexerDescriptor {
            provider_type: "newznab".to_string(),
            provider_aliases: vec!["nzbgeek".to_string(), "dognzb".to_string()],
            provider_profiles,
            search_semantics_version: Some(3),
            strategy_plan: Some(scryer_plugin_sdk::IndexerStrategyPlanCapability {
                version: 1,
                max_parallel_strategies: 4,
            }),
            source_kind: IndexerSourceKind::Usenet,
            capabilities: Capabilities {
                resolve_network_endpoints: false,
                http: None,
                dynamic_config_fields: None,
                supported_ids: HashMap::from([
                    ("movie".into(), vec!["imdb_id".into()]),
                    ("series".into(), vec!["tvdb_id".into()]),
                    ("anime".into(), vec!["tvdb_id".into()]),
                ]),
                deduplicates_aliases: false,
                season_param: Some("season".into()),
                episode_param: Some("ep".into()),
                query_param: Some("q".into()),
                supported_query_facets: vec![],
                search: true,
                imdb_search: true,
                tvdb_search: true,
                anidb_search: false,
                rss: true,
                protocols: vec![IndexerProtocol::Usenet],
                feed_modes: vec![
                    IndexerFeedMode::Recent,
                    IndexerFeedMode::Rss,
                    IndexerFeedMode::AutomaticSearch,
                    IndexerFeedMode::InteractiveSearch,
                ],
                search_inputs: vec![
                    IndexerSearchInput::TitleQuery,
                    IndexerSearchInput::IdQuery,
                    IndexerSearchInput::Season,
                    IndexerSearchInput::Episode,
                    IndexerSearchInput::Category,
                    IndexerSearchInput::Limit,
                ],
                supported_external_ids: vec![
                    "imdb_id".into(),
                    "tvdb_id".into(),
                    "tmdb_id".into(),
                    "tvmaze_id".into(),
                    "tvrage_id".into(),
                ],
                category_model: Some(IndexerCategoryModel {
                    value_kinds: vec![IndexerCategoryValueKind::Numeric],
                    separate_anime_categories: true,
                    provider_category_metadata: true,
                    ..IndexerCategoryModel::default()
                }),
                limits: Some(IndexerLimitCapabilities {
                    page_size: Some(100),
                    max_page_size: Some(100),
                    max_pages: Some(30),
                    api_quota_supported: true,
                    grab_quota_supported: true,
                    // Paged responses carry at most one provider page, so a host
                    // must follow `next_cursor` to see the rest. Hosts that cannot
                    // are kept off this release by the SDK floor above (3.14).
                    paged_search: true,
                    ..IndexerLimitCapabilities::default()
                }),
                torrent: None,
                response_features: Some(response_features),
            },
            scoring_policies,
            config_fields,
            allowed_hosts: vec![],
            rate_limit_seconds: None,
        }),
    }
}

async fn search(req: SearchRequest) -> Result<SearchResponse, Error> {
    let config = NewznabConfig::from_host()?;
    let mut response = execute_paged_search(&config, &req, extract_profile_metadata).await?;
    newznab_common::retain_movie_identity_matches(&mut response, &req);
    Ok(response)
}

async fn action(request: PluginActionRequest) -> Result<PluginActionResponse, Error> {
    if request.action.trim() == "newznabConnectionTest" {
        let config = NewznabConfig::from_host()?;
        return Ok(PluginActionResponse {
            payload: newznab_common::newznab_connection_test(&config).await?,
        });
    }
    newznab_common::execute_provider_action(request).await
}

scryer_plugin_pdk::scryer_indexer_component_main!(
    descriptor = build_descriptor,
    search = search,
    action = action,
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_owns_profiles_selector_and_guarded_policies() {
        let descriptor = build_descriptor();
        let ProviderDescriptor::Indexer(indexer) = descriptor.provider else {
            panic!("expected indexer descriptor");
        };
        let known_profiles = include_str!("../known_newznab_profiles.v1.jsonl")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .count();
        assert!(known_profiles > 0);
        assert_eq!(indexer.provider_profiles.len(), known_profiles);
        assert_eq!(indexer.scoring_policies.len(), 3);
        assert!(
            indexer
                .scoring_policies
                .iter()
                .filter(|policy| policy.name != "indexer_language_confirmed")
                .all(|policy| policy.rego_source.contains("newznab_profile_id"))
        );
        let selector = indexer
            .config_fields
            .iter()
            .find(|field| field.key == "profile_id")
            .expect("provider selector");
        assert_eq!(selector.options.len(), known_profiles + 1);
        let base_url = indexer
            .config_fields
            .iter()
            .find(|field| field.key == "base_url")
            .expect("base URL field");
        assert!(!base_url.required);
    }

    #[test]
    fn descriptor_declares_paged_search_within_the_newznab_page_size() {
        let ProviderDescriptor::Indexer(indexer) = build_descriptor().provider else {
            panic!("expected indexer descriptor");
        };
        let limits = indexer.capabilities.limits.expect("limit capabilities");
        assert!(limits.paged_search);
        assert_eq!(
            limits.paged_response_bound(1000),
            Some(newznab_common::NEWZNAB_MAX_PAGE_SIZE)
        );
    }

    /// A host that answers every HTTP request on this thread with one canned
    /// response, so a handler runs end to end without a network.
    mod canned_host {
        use std::cell::RefCell;
        use std::collections::HashMap;
        use std::future::Future;
        use std::pin::Pin;
        use std::task::{Context, Poll, Waker};

        use scryer_plugin_pdk::runtime::{
            HostError, HostRuntime, PluginHttpFieldsResponse, PluginHttpRequest,
            install_host_runtime,
        };

        #[derive(Default)]
        struct Canned {
            active: bool,
            config: HashMap<String, String>,
            status: u16,
            body: String,
            /// Monotonic time; a sleep advances it by exactly its duration.
            now_ms: u64,
        }

        thread_local! {
            static CANNED: RefCell<Canned> = RefCell::new(Canned::default());
        }

        struct CannedHost;

        static HOST: CannedHost = CannedHost;

        impl HostRuntime for CannedHost {
            fn http_fields(
                &self,
                _request: PluginHttpRequest,
            ) -> Pin<Box<dyn Future<Output = Result<PluginHttpFieldsResponse, HostError>> + '_>>
            {
                Box::pin(async move {
                    CANNED.with(|canned| {
                        let canned = canned.borrow();
                        if !canned.active {
                            return Err(HostError::Transport);
                        }
                        Ok(PluginHttpFieldsResponse {
                            status: canned.status,
                            headers: Vec::new(),
                            body: canned.body.clone().into_bytes(),
                        })
                    })
                })
            }

            fn sleep(&self, duration_ms: u64) -> Pin<Box<dyn Future<Output = ()> + '_>> {
                CANNED.with(|canned| {
                    let mut canned = canned.borrow_mut();
                    canned.now_ms = canned.now_ms.saturating_add(duration_ms);
                });
                Box::pin(async {})
            }

            fn monotonic_now_ms(&self) -> u64 {
                CANNED.with(|canned| canned.borrow().now_ms)
            }

            fn operation_deadline_monotonic_ms(&self) -> u64 {
                u64::MAX
            }

            fn wall_now_ms(&self) -> u64 {
                0
            }

            fn config_get(&self, key: &str) -> Option<String> {
                CANNED.with(|canned| canned.borrow().config.get(key).cloned())
            }

            fn provider_profile_bytes(&self) -> Option<Vec<u8>> {
                None
            }

            fn state_get(&self, _key: &str) -> Option<Vec<u8>> {
                None
            }

            fn state_cas(
                &self,
                _key: &str,
                _expected: Option<&[u8]>,
                _replacement: Option<&[u8]>,
            ) -> bool {
                true
            }

            fn log(&self, _level: scryer_plugin_pdk::log::LogLevel, _message: &str) {}
        }

        /// Answer every request on this thread with `status` and `body`, and
        /// serve `config` as the instance settings.
        pub fn answer(config: &[(&str, &str)], status: u16, body: &str) {
            install_host_runtime(&HOST);
            CANNED.with(|canned| {
                *canned.borrow_mut() = Canned {
                    active: true,
                    config: config
                        .iter()
                        .map(|(key, value)| (key.to_string(), value.to_string()))
                        .collect(),
                    status,
                    body: body.to_string(),
                    now_ms: 1_000_000,
                }
            });
        }

        /// Drive `future` to completion; the canned host never suspends.
        pub fn run<T>(future: impl Future<Output = T>) -> T {
            let mut future = std::pin::pin!(future);
            match future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
            {
                Poll::Ready(output) => output,
                Poll::Pending => panic!("the canned host never suspends"),
            }
        }
    }

    fn plugin_error_of(error: &Error) -> scryer_plugin_pdk::PluginError {
        scryer_plugin_pdk::error::plugin_error_from(error)
    }

    #[test]
    fn a_rejected_api_key_reaches_the_host_as_invalid_api_key() {
        canned_host::answer(
            &[
                ("base_url", "https://nab.example.invalid"),
                ("api_key", "synthetic-key"),
            ],
            200,
            r#"<?xml version="1.0"?><error code="100" description="Incorrect user credentials"/>"#,
        );
        let request = SearchRequest {
            query: "Synthetic Title".to_string(),
            ..SearchRequest::default()
        };

        let error = canned_host::run(search(request)).expect_err("the key is rejected");
        assert_eq!(
            plugin_error_of(&error).code,
            PluginErrorCode::from(codes::INVALID_API_KEY)
        );

        let error = canned_host::run(action(PluginActionRequest {
            action: "newznabConnectionTest".to_string(),
            payload: serde_json::Value::Null,
        }))
        .expect_err("the key is rejected");
        assert_eq!(
            plugin_error_of(&error).code,
            PluginErrorCode::from(codes::INVALID_API_KEY)
        );
    }

    #[test]
    fn a_missing_base_url_is_invalid_config() {
        canned_host::answer(&[], 200, "");
        let error = canned_host::run(search(SearchRequest::default())).expect_err("no base URL");
        let error = plugin_error_of(&error);
        assert_eq!(error.code, PluginErrorCode::InvalidConfig);
        assert!(error.public_message.contains("base_url"));
    }
}
