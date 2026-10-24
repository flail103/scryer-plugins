//! Conformance against the real Scryer notification host, run on the RELEASE
//! artifact.
//!
//! The suite itself lives in `scryer-plugin-conformance`, which builds the
//! shipping `wasm32-wasip2` component and drives it the way the host does.
//! What stays here is what is genuinely this channel's: its descriptor
//! identity, the configuration Scryer would have resolved for it, and the
//! endpoint a delivery has to reach.

use scryer_plugin_conformance::notification::{
    NotificationConformance, call_notification, instantiate, test_request,
};
use scryer_plugin_conformance::{HttpScript, Script};
use scryer_plugin_sdk::command::{PluginNotificationCommand, PluginNotificationCommandResult};
use scryer_plugin_sdk::{NotificationEventType, PluginErrorCode, PluginResult};

const API_KEY: &str = "pushoverkey";

// A send is one request, so a non-success answer from Pushover is a typed
// plugin error carrying its code (`UpstreamUnavailable` for a 5xx) rather than
// an unsuccessful delivery; the shared suite accepts that typed answer, and
// the check below pins this channel's codes.

#[test]
fn pushover_release_wasm_conforms_to_the_notification_host_contract() {
    let conformance = NotificationConformance::new(env!("CARGO_MANIFEST_DIR"), "pushover")
        .wasm("pushover_notification.wasm")
        .config("api_key", API_KEY)
        .config("user_key", "pushoveruser")
        .expects_url_prefix("https://api.pushover.net/1/messages.json")
        .required_setting("api_key");
    conformance.run();

    let wasm = conformance.wasm_path();
    // Pushover answers a bad application token with a 400 that flags the
    // `token` parameter (https://pushover.net/api, "Response Format").
    for (status, body, code) in [
        (
            500,
            "synthetic upstream answer",
            PluginErrorCode::UpstreamUnavailable,
        ),
        (
            400,
            r#"{"token":"invalid","errors":["synthetic upstream answer"],"status":0}"#,
            PluginErrorCode::from("invalid_api_key"),
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
            PluginNotificationCommand::Send(test_request(NotificationEventType::Grab)),
        );
        let PluginNotificationCommandResult::Send(PluginResult::Err(error)) = result else {
            panic!("HTTP {status} must be a typed plugin error: {result:?}");
        };
        assert_eq!(error.code, code, "HTTP {status}");
        assert!(!error.public_message.contains("synthetic upstream answer"));
        assert!(!error.public_message.contains(API_KEY));
    }
}
