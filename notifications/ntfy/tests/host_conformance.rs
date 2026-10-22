//! Conformance against the real Scryer notification host, run on the RELEASE
//! artifact.
//!
//! The suite itself lives in `scryer-plugin-conformance`. What stays here is
//! what is genuinely this channel's: its configuration, its endpoint, and the
//! JSON body no URL assertion can reach.

// conformance: bespoke

use scryer_plugin_conformance::notification::{
    Check, NotificationConformance, call_notification, instantiate, test_request,
};
use scryer_plugin_conformance::{HttpScript, Script};
use scryer_plugin_sdk::command::{PluginNotificationCommand, PluginNotificationCommandResult};
use scryer_plugin_sdk::{NotificationEventType, PluginErrorCode, PluginResult};

/// ntfy is published to as JSON at the **server root**
/// (docs.ntfy.sh/publish/#publish-as-json), so the topic is a body field rather
/// than a path segment and the notification text is a body field rather than a
/// query parameter. What is pinned is that the endpoint comes from the resolved
/// configuration and is used verbatim.
const EXPECTED_URL: &str = "https://ntfy.test.invalid/";

/// The configured topic must reach ntfy in the JSON body, and the notification
/// text must never travel in the URL: that is the whole point of the move off
/// Sonarr's query-parameter publish, and a regression to it would be invisible
/// to a URL-prefix assertion alone.
const EXPECTED_TOPIC: &str = "scryer";

#[test]
fn ntfy_release_wasm_conforms_to_the_notification_host_contract() {
    // This channel tolerates an unset setting rather than reporting a typed
    // configuration error, so the family's missing-required-setting check does
    // not apply and is opted out of rather than weakened.
    //
    // A single topic is a single request, so its server error is a typed
    // plugin error carrying `UpstreamUnavailable` rather than an unsuccessful
    // delivery; the suite's in-band-delivery-failure check is replaced by the
    // typed check at the end of this test.
    let conformance = NotificationConformance::new(env!("CARGO_MANIFEST_DIR"), "ntfy")
        .wasm("ntfy_notification.wasm")
        .config("server_url", "https://ntfy.test.invalid")
        .config("topics", EXPECTED_TOPIC)
        .expects_url(EXPECTED_URL)
        .without(Check::MissingRequiredSetting)
        .without(Check::UpstreamFailureIsReported);
    let outcome = conformance.run();

    // The topic travels in the JSON body, and the notification text with it.
    let request = outcome
        .send
        .request_to(EXPECTED_URL)
        .expect("the publish request must have been made");
    let payload: serde_json::Value =
        serde_json::from_slice(&request.body).expect("ntfy is published to as JSON");
    assert_eq!(
        payload["topic"], EXPECTED_TOPIC,
        "the configured topic must be a body field: {payload}"
    );
    assert!(
        payload.get("message").is_some(),
        "the notification text must be a body field: {payload}"
    );
    assert!(
        !outcome.send.urls.iter().any(|url| url.contains('?')),
        "no notification content may travel in the URL: {:?}",
        outcome.send.urls
    );

    let wasm = conformance.wasm_path();
    for (status, body, code) in [
        (
            500,
            "synthetic upstream answer",
            PluginErrorCode::UpstreamUnavailable,
        ),
        (
            401,
            r#"{"code":40101,"http":401,"error":"synthetic upstream answer"}"#,
            PluginErrorCode::AuthFailed,
        ),
    ] {
        let script = Script {
            http: HttpScript::status(status, body.as_bytes().to_vec()),
            ..conformance.script()
        };
        let (mut store, plugin) = instantiate(&wasm, script);
        let result = call_notification(
            &mut store,
            &plugin,
            PluginNotificationCommand::Send(test_request(NotificationEventType::Test)),
        );
        let PluginNotificationCommandResult::Send(PluginResult::Err(error)) = result else {
            panic!("HTTP {status} must be a typed plugin error: {result:?}");
        };
        assert_eq!(error.code, code, "HTTP {status}");
        assert!(!error.public_message.contains("synthetic upstream answer"));
    }
}
