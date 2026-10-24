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

#[test]
fn emby_release_wasm_conforms_to_the_notification_host_contract() {
    conformance().run();

    // The shared suite accepts a 5xx answered as a typed `UpstreamUnavailable`
    // error; this pins that answer and keeps the upstream text out of the
    // public message.
    assert_a_server_error_is_upstream_unavailable(&conformance());
}

fn conformance() -> NotificationConformance {
    NotificationConformance::new(env!("CARGO_MANIFEST_DIR"), "emby")
        .wasm("emby_notification.wasm")
        .config("base_url", "https://emby.test.invalid")
        .config("api_key", "embykey")
        .expects_url_prefix("https://emby.test.invalid/System/Info")
        .required_setting("base_url")
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
