//! Conformance against the real Scryer notification host, run on the RELEASE
//! artifact.
//!
//! The suite itself lives in `scryer-plugin-conformance`. What stays here is
//! what is genuinely this channel's: its configuration, its endpoint, and the
//! one thing no URL assertion can reach — where the credential travels.

// conformance: bespoke

use scryer_plugin_conformance::notification::{
    Check, NotificationConformance, call_notification, instantiate, test_request,
};
use scryer_plugin_conformance::{HttpScript, Script};
use scryer_plugin_sdk::command::{PluginNotificationCommand, PluginNotificationCommandResult};
use scryer_plugin_sdk::{NotificationEventType, PluginErrorCode, PluginResult};

const EXPECTED_URL_PREFIX: &str = "https://gotify.test.invalid/message";

/// The application token travels in Gotify's documented `X-Gotify-Key` header
/// rather than Sonarr's `?token=` query parameter. Both are still accepted by
/// the server (`appTokenHeader`/`appTokenQuery` in gotify/server's REST-API
/// spec), and only the header keeps the token out of reverse-proxy access logs.
/// The token therefore no longer appears in the URL and is asserted on the
/// request headers instead — a stronger check than the query-string prefix it
/// replaces, because it also pins the name of the credential header.
const EXPECTED_TOKEN_HEADER: (&str, &str) = ("x-gotify-key", "apptoken");

#[test]
fn gotify_release_wasm_conforms_to_the_notification_host_contract() {
    let outcome = conformance()
        .without(Check::UpstreamFailureIsReported)
        .run();

    // The application token is a header, not a query parameter: it must never
    // appear in the URL, and it must be sent under the name Gotify documents.
    let request = outcome
        .send
        .request_with_url_prefix(EXPECTED_URL_PREFIX)
        .expect("the message request must have been made");
    let (name, value) = EXPECTED_TOKEN_HEADER;
    assert_eq!(
        request.header(name),
        Some(value),
        "the app token must travel in the {name} header; got {:?}",
        request.headers
    );
    assert!(
        !outcome.send.urls.iter().any(|url| url.contains("apptoken")),
        "the app token must not appear in a URL: {:?}",
        outcome.send.urls
    );

    // A 5xx is now a typed `UpstreamUnavailable` error rather than an in-band
    // failed delivery, so the family's in-band check is replaced by this one.
    assert_a_server_error_is_upstream_unavailable(&conformance());
}

fn conformance() -> NotificationConformance {
    NotificationConformance::new(env!("CARGO_MANIFEST_DIR"), "gotify")
        .wasm("gotify_notification.wasm")
        .config("server", "https://gotify.test.invalid")
        .config("app_token", "apptoken")
        .expects_url_prefix(EXPECTED_URL_PREFIX)
        .required_setting("server")
}

fn assert_a_server_error_is_upstream_unavailable(conformance: &NotificationConformance) {
    let script = Script {
        http: HttpScript::status(500, b"synthetic upstream failure".to_vec()),
        ..conformance.script()
    };
    let (mut store, plugin) = instantiate(&conformance.wasm_path(), script);
    let result = call_notification(
        &mut store,
        &plugin,
        PluginNotificationCommand::Send(test_request(NotificationEventType::Test)),
    );
    let PluginNotificationCommandResult::Send(PluginResult::Err(error)) = result else {
        panic!("a server error must be a typed plugin error: {result:?}");
    };
    assert_eq!(error.code, PluginErrorCode::UpstreamUnavailable);
    assert!(
        !error.public_message.contains("synthetic upstream failure"),
        "the upstream text belongs in the debug message: {error:?}"
    );
}
