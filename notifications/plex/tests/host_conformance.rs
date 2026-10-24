//! Conformance against the real Scryer notification host, run on the RELEASE
//! artifact.
//!
//! The suite itself lives in `scryer-plugin-conformance`. What stays here is
//! what is genuinely this channel's: a library refresh rather than a message,
//! and the only channel in the family that *implements* actions.

// conformance: bespoke

use scryer_plugin_conformance::notification::{
    Check, NotificationConformance, call_notification, instantiate,
};
use scryer_plugin_sdk::NotificationEventType;
use scryer_plugin_sdk::PluginErrorCode;
use scryer_plugin_sdk::PluginResult;
use scryer_plugin_sdk::command::{
    PluginActionRequest, PluginNotificationCommand, PluginNotificationCommandResult,
};

#[test]
fn plex_release_wasm_conforms_to_the_notification_host_contract() {
    // The delivery is a library refresh, so the notification carries a
    // `Download` rather than the family's `Test`. An unset `base_url` (with no
    // legacy `host` either) is a typed configuration error naming both. The
    // action arm is wired rather than unsupported, so that check is replaced
    // by the stronger one below rather than dropped. A single refresh is a
    // single request, so its server error is a typed plugin error rather than
    // an unsuccessful delivery; the shared suite accepts that typed answer, and
    // the check below pins it.
    let conformance = NotificationConformance::new(env!("CARGO_MANIFEST_DIR"), "plex")
        .wasm("plex_notification.wasm")
        .config("base_url", "http://plex.test.invalid:32400")
        .config("update_library", "true")
        .config("section_ids", "3")
        .config("auth_token", "plextoken")
        .expects_url_prefix(
            "http://plex.test.invalid:32400/library/sections/3/refresh?X-Plex-Client-Identifier=scryer",
        )
        .event_type(NotificationEventType::Download)
        .required_setting("base_url")
        .required_setting_mentions("host")
        .without(Check::ActionIsUnsupported);
    conformance.run();

    assert_an_unknown_action_answers_empty_rather_than_trapping(&conformance);
    assert_a_single_refresh_failure_is_a_typed_error(&conformance);
}

fn assert_a_single_refresh_failure_is_a_typed_error(conformance: &NotificationConformance) {
    use scryer_plugin_conformance::notification::test_request;
    use scryer_plugin_conformance::{HttpScript, Script};

    for (status, code) in [
        (500, PluginErrorCode::UpstreamUnavailable),
        // A token is configured, so Plex refusing it is the token's fault.
        (401, PluginErrorCode::from("invalid_api_key")),
    ] {
        let script = Script {
            http: HttpScript::status(status, b"synthetic upstream answer".to_vec()),
            ..conformance.script()
        };
        let (mut store, plugin) = instantiate(&conformance.wasm_path(), script);
        let result = call_notification(
            &mut store,
            &plugin,
            PluginNotificationCommand::Send(test_request(NotificationEventType::Download)),
        );
        let PluginNotificationCommandResult::Send(PluginResult::Err(error)) = result else {
            panic!("HTTP {status} must be a typed plugin error: {result:?}");
        };
        assert_eq!(error.code, code, "HTTP {status}");
        assert!(!error.public_message.contains("synthetic upstream answer"));
        assert!(!error.public_message.contains("plextoken"));
    }
}

/// This channel *implements* actions, so the assertion is the opposite of the
/// family's usual one: the operation is wired, it reaches the handler with the
/// action name intact, and an action it does not recognise answers with an
/// empty payload rather than trapping.
///
/// The host splits what used to arrive as one JSON blob into
/// `PluginActionRequest::action` and `::payload`; `action_request_value`
/// re-joins them, and this is what proves that join is right — an unrecognised
/// name has to arrive as an unrecognised name, not as a missing one.
fn assert_an_unknown_action_answers_empty_rather_than_trapping(
    conformance: &NotificationConformance,
) {
    let (mut store, plugin) = instantiate(&conformance.wasm_path(), conformance.script());
    let result = call_notification(
        &mut store,
        &plugin,
        PluginNotificationCommand::Action(PluginActionRequest {
            action: "notAnAction".to_string(),
            payload: serde_json::json!({ "query": {} }),
        }),
    );
    let PluginNotificationCommandResult::Action(PluginResult::Ok(response)) = result else {
        panic!("an unknown action must answer in-band: {result:?}");
    };
    assert_eq!(response.payload, serde_json::json!({}));
}

#[test]
fn refresh_requests_stay_scoped_and_preserve_move_fallbacks() {
    use scryer_plugin_conformance::notification::test_request;
    use scryer_plugin_conformance::{HttpReply, HttpRoute, HttpScript};
    use scryer_plugin_sdk::PluginNotificationTitleMove;
    let sections = r#"{"MediaContainer":{"Directory":[{"key":"1","type":"show","Location":[{"path":"/media/TV"}]},{"key":"2","type":"show","Location":[{"path":"/media/Other"}]},{"key":"3","type":"movie","Location":[{"path":"/media/TV"}]}]}}"#;
    let cases = [
        (
            Some("/media/TV/Example"),
            None,
            false,
            false,
            vec![("1", Some("/media/TV/Example"))],
        ),
        (
            Some("/unmatched/Example"),
            None,
            false,
            false,
            vec![("1", None), ("2", None)],
        ),
        (None, None, false, false, vec![("1", None), ("2", None)]),
        (
            Some("/media/TV/Example"),
            None,
            true,
            false,
            vec![("9", Some("/media/TV/Example"))],
        ),
        (
            Some("/downloads/Example"),
            None,
            false,
            true,
            vec![("1", Some("/media/TV/Example"))],
        ),
        (
            Some("/media/TV/Example"),
            Some("/media/Other/Example"),
            false,
            false,
            vec![
                ("1", Some("/media/TV/Example")),
                ("2", Some("/media/Other/Example")),
            ],
        ),
        (
            Some("/media/TV/Example"),
            Some("/unmatched/Example"),
            false,
            false,
            vec![("1", Some("/media/TV/Example"))],
        ),
        (
            Some("/media/TV/Example"),
            Some("/media/TV/Example"),
            false,
            false,
            vec![("1", Some("/media/TV/Example"))],
        ),
    ];
    for version in ["1.20.0.3125-build", "1.19.9.9999", "unknown"] {
        for (path, old, explicit, mapped, expected) in cases.clone() {
            let expected = if version == "1.20.0.3125-build" {
                let mut expected = expected;
                let mut sections: std::collections::BTreeSet<_> = expected
                    .iter()
                    .filter(|(_, path)| path.is_none())
                    .map(|(id, _)| *id)
                    .collect();
                let fallbacks: Vec<_> = expected
                    .iter()
                    .filter_map(|(id, path)| {
                        (path.is_some() && sections.insert(*id)).then_some((*id, None))
                    })
                    .collect();
                expected.extend(fallbacks);
                expected
            } else {
                let mut ids = std::collections::BTreeSet::new();
                expected
                    .into_iter()
                    .filter_map(|(id, _)| ids.insert(id).then_some((id, None)))
                    .collect()
            };
            let mut suite = NotificationConformance::new(env!("CARGO_MANIFEST_DIR"), "plex")
                .wasm("plex_notification.wasm")
                .config("base_url", "http://plex.test.invalid:32400")
                .config("update_library", "true")
                .config("auth_token", "synthetic-token");
            if explicit {
                suite = suite.config("section_ids", "9");
            }
            if mapped {
                suite = suite
                    .config("map_from", "/downloads")
                    .config("map_to", "/media/TV");
            }
            let mut script = suite.script();
            let identity = serde_json::json!({"MediaContainer": {"version": version}}).to_string();
            script.http = HttpScript::Routed(vec![
                HttpRoute::contains("/identity?", HttpReply::ok(&identity)),
                HttpRoute::contains("/refresh?", HttpReply::ok("{}")),
                HttpRoute::contains("/library/sections?", HttpReply::ok(sections)),
            ]);
            let mut request = test_request(if old.is_some() {
                NotificationEventType::TitleMoved
            } else {
                NotificationEventType::Download
            });
            request.title.as_mut().unwrap().facet = "series".into();
            request.title.as_mut().unwrap().path = path.map(str::to_owned);
            request.file = None;
            request.media_files.clear();
            if let Some(old) = old {
                request.title_move = Some(PluginNotificationTitleMove {
                    source_path: Some(old.into()),
                    destination_path: path.map(str::to_owned),
                    ..Default::default()
                });
            }
            let (mut store, plugin) = instantiate(&suite.wasm_path(), script);
            let result = call_notification(
                &mut store,
                &plugin,
                PluginNotificationCommand::Send(request),
            );
            let PluginNotificationCommandResult::Send(PluginResult::Ok(response)) = result else {
                panic!("{result:?}")
            };
            assert!(response.success, "{response:?}");
            for request in &store.data().script.requests {
                let expected_method = if request.url.contains("/refresh?") {
                    "POST"
                } else {
                    "GET"
                };
                assert_eq!(
                    request.method.as_deref(),
                    Some(expected_method),
                    "{}",
                    request.url
                );
            }
            let urls: Vec<_> = store
                .data()
                .script
                .urls
                .iter()
                .filter(|url| url.contains("/refresh?"))
                .collect();
            assert_eq!(urls.len(), expected.len(), "{urls:?}");
            for (url, (section, path)) in urls.iter().zip(expected) {
                assert!(
                    url.contains(&format!("/sections/{section}/refresh?")),
                    "{url}"
                );
                match path {
                    Some(path) => assert!(
                        url.contains(&format!("path={}", path.replace('/', "%2F"))),
                        "{url}"
                    ),
                    None => assert!(!url.contains("path="), "{url}"),
                }
            }
        }
    }
}

#[test]
fn movie_series_and_anime_refresh_only_the_matching_library() {
    use scryer_plugin_conformance::notification::test_request;
    use scryer_plugin_conformance::{HttpReply, HttpRoute, HttpScript};
    use scryer_plugin_sdk::PluginNotificationTitleMove;

    let sections = r#"{"MediaContainer":{"Directory":[{"key":"1","type":"show","Location":[{"path":"/media/Shows"}]},{"key":"2","type":"movie","Location":[{"path":"/media/Movies"}]}]}}"#;
    for (facet, root, section) in [
        ("movie", "/media/Movies", "2"),
        ("series", "/media/Shows", "1"),
        ("anime", "/media/Shows", "1"),
        ("anime", "/media/Movies", "2"),
    ] {
        for moved in [false, true] {
            let suite = NotificationConformance::new(env!("CARGO_MANIFEST_DIR"), "plex")
                .wasm("plex_notification.wasm")
                .config("base_url", "http://plex.test.invalid:32400")
                .config("update_library", "true")
                .config("auth_token", "synthetic-token");
            let mut script = suite.script();
            script.http = HttpScript::Routed(vec![
                HttpRoute::contains(
                    "/identity?",
                    HttpReply::ok(r#"{"MediaContainer":{"version":"1.43.4.10903"}}"#),
                ),
                HttpRoute::contains("/refresh?", HttpReply::ok("{}")),
                HttpRoute::contains("/library/sections?", HttpReply::ok(sections)),
            ]);
            let mut request = test_request(if moved {
                NotificationEventType::TitleMoved
            } else {
                NotificationEventType::Download
            });
            let path = format!("{root}/Example");
            request.title.as_mut().unwrap().facet = facet.into();
            request.title.as_mut().unwrap().path = Some(path.clone());
            request.file = None;
            request.media_files.clear();
            if moved {
                request.title_move = Some(PluginNotificationTitleMove {
                    source_path: Some(format!("{root}/Previous")),
                    destination_path: Some(path),
                    ..Default::default()
                });
            }
            let (mut store, plugin) = instantiate(&suite.wasm_path(), script);
            let result = call_notification(
                &mut store,
                &plugin,
                PluginNotificationCommand::Send(request),
            );
            let PluginNotificationCommandResult::Send(PluginResult::Ok(response)) = result else {
                panic!("{result:?}");
            };
            assert!(response.success, "{facet}: {response:?}");
            let refreshes: Vec<_> = store
                .data()
                .script
                .requests
                .iter()
                .filter(|request| request.url.contains("/refresh?"))
                .collect();
            assert_eq!(refreshes.len(), if moved { 3 } else { 2 }, "{facet}");
            for request in &refreshes {
                assert_eq!(request.method.as_deref(), Some("POST"));
                assert!(
                    request
                        .url
                        .contains(&format!("/sections/{section}/refresh?")),
                    "{}",
                    request.url
                );
            }
            assert_eq!(
                refreshes
                    .iter()
                    .filter(|request| !request.url.contains("path="))
                    .count(),
                1
            );
            assert!(!refreshes.last().unwrap().url.contains("path="));
        }
    }
}
