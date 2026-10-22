//! Conformance against the real Scryer notification host, run on the RELEASE
//! artifact.
//!
//! The suite itself lives in `scryer-plugin-conformance`, which builds the
//! shipping `wasm32-wasip2` component and drives it the way the host does.
//! What stays here is what is genuinely this channel's: its descriptor
//! identity, the configuration Scryer would have resolved for it, and the
//! endpoint a delivery has to reach.

use scryer_plugin_conformance::notification::{
    Check, NotificationConformance, call_notification, instantiate, test_request,
};
use scryer_plugin_conformance::{HttpScript, Script};
use scryer_plugin_sdk::command::{PluginNotificationCommand, PluginNotificationCommandResult};
use scryer_plugin_sdk::{NotificationEventType, PluginErrorCode, PluginResult};

const BOT_TOKEN: &str = "bottoken";

// A send is one request, so a non-success answer from Telegram is a typed
// plugin error carrying its code (`UpstreamUnavailable` for a 5xx) rather than
// an unsuccessful delivery; the suite's in-band-delivery-failure check is
// replaced by the typed check below.

#[test]
fn telegram_release_wasm_conforms_to_the_notification_host_contract() {
    let conformance = NotificationConformance::new(env!("CARGO_MANIFEST_DIR"), "telegram")
        .wasm("telegram_notification.wasm")
        .config("chat_id", "123456")
        .config("bot_token", BOT_TOKEN)
        .expects_url_prefix("https://api.telegram.org/botbottoken/sendmessage")
        .required_setting("chat_id")
        .without(Check::UpstreamFailureIsReported);
    conformance.run();

    let wasm = conformance.wasm_path();
    for (status, code) in [
        (500, PluginErrorCode::UpstreamUnavailable),
        (401, PluginErrorCode::from("invalid_api_key")),
    ] {
        let script = Script {
            http: HttpScript::status(status, b"synthetic upstream answer".to_vec()),
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
        assert!(!error.public_message.contains(BOT_TOKEN));
    }
}
