use super::*;

#[test]
fn exact_nab_parameters_replace_defaults_and_preserve_encoded_values() {
    let req = SearchRequest {
        nab: Some(scryer_plugin_sdk::PluginNabSearch {
            operation: scryer_plugin_sdk::PluginNabOperation::Book,
            parameters: [
                ("author".to_owned(), "A & B".to_owned()),
                ("title".to_owned(), "Title + subtitle".to_owned()),
            ]
            .into(),
        }),
        ..SearchRequest::default()
    };
    let params =
        raw_nab_additional_params(&req, "author=old&extended=1&apikey=wrong&offset=99").unwrap();
    let pairs: Vec<_> = url::form_urlencoded::parse(params.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    assert_eq!(
        pairs,
        vec![
            ("extended".into(), "1".into()),
            ("author".into(), "A & B".into()),
            ("title".into(), "Title + subtitle".into())
        ]
    );
}

#[test]
fn exact_nab_parameters_cannot_override_credentials_or_paging() {
    for key in ["apikey", "APIKEY", "t", "q", "cat", "limit", "offset", "o"] {
        let req = SearchRequest {
            nab: Some(scryer_plugin_sdk::PluginNabSearch {
                operation: scryer_plugin_sdk::PluginNabOperation::Search,
                parameters: [(key.to_owned(), "wrong".to_owned())].into(),
            }),
            ..SearchRequest::default()
        };
        assert!(raw_nab_additional_params(&req, "").is_err(), "{key}");
    }
}

#[test]
fn exact_nab_query_keeps_ampersands_and_spacing() {
    let mut req = SearchRequest {
        query: "  A & B  ".to_owned(),
        nab: Some(scryer_plugin_sdk::PluginNabSearch {
            operation: scryer_plugin_sdk::PluginNabOperation::Search,
            parameters: Default::default(),
        }),
        ..SearchRequest::default()
    };
    assert_eq!(raw_query_for_request(&req), "  A & B  ");
    req.nab = None;
    assert_eq!(raw_query_for_request(&req), "A and B");
}

#[test]
fn exact_nab_values_survive_the_final_url_builder() {
    let req = SearchRequest {
        query: "  A & B  ".into(),
        nab: Some(scryer_plugin_sdk::PluginNabSearch {
            operation: scryer_plugin_sdk::PluginNabOperation::Book,
            parameters: [
                ("author".into(), "  Author & Guest  ".into()),
                ("title".into(), "  001.00  ".into()),
            ]
            .into(),
        }),
        ..Default::default()
    };
    let additional = raw_nab_additional_params(&req, "extended=1").unwrap();
    for query in [req.query.as_str(), "   "] {
        let exact = build_exact_search_url(
            "https://indexer.example/api",
            "book",
            Some(query),
            "fixture-key",
            None,
            None,
            None,
            None,
            None,
            None,
            100,
            None,
            None,
            &additional,
        )
        .unwrap();
        let parsed = Url::parse(&exact).unwrap();
        let pairs: BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
        assert_eq!(pairs["q"], query);
        assert_eq!(pairs["author"], "  Author & Guest  ");
        assert_eq!(pairs["title"], "  001.00  ");
    }
    let normal = build_search_url(
        "https://indexer.example/api",
        "book",
        Some(&req.query),
        "fixture-key",
        None,
        None,
        None,
        None,
        None,
        None,
        100,
        None,
        None,
        &additional,
    )
    .unwrap();
    let parsed = Url::parse(&normal).unwrap();
    let pairs: BTreeMap<_, _> = parsed.query_pairs().into_owned().collect();
    assert_eq!(pairs["q"], "A & B");
    assert_eq!(pairs["author"], "Author & Guest");
}
