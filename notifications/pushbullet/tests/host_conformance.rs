//! Conformance against the real Scryer notification host, run on the RELEASE
//! artifact.
//!
//! The suite itself lives in `scryer-plugin-conformance`. What stays here is
//! what is genuinely this channel's: its configuration, its endpoint, the
//! credential header and JSON body no URL assertion can reach, and the
//! device-list action it implements.

// conformance: bespoke

use scryer_plugin_conformance::notification::{
    Check, NotificationConformance, call_notification, instantiate, test_request,
};
use scryer_plugin_conformance::{HttpReply, HttpRoute, HttpScript, Script};
use scryer_plugin_sdk::command::{
    PluginActionRequest, PluginNotificationCommand, PluginNotificationCommandResult,
};
use scryer_plugin_sdk::{NotificationEventType, PluginErrorCode, PluginResult};

const PUSHES_URL: &str = "https://api.pushbullet.com/v2/pushes";
const ACCESS_TOKEN: &str = "pushbullettoken";

fn conformance() -> NotificationConformance {
    NotificationConformance::new(env!("CARGO_MANIFEST_DIR"), "pushbullet")
        .wasm("pushbullet_notification.wasm")
        .config("api_key", ACCESS_TOKEN)
        .expects_url(PUSHES_URL)
        .required_setting("api_key")
        // A send with no target configured is one push, so a non-success
        // answer is a typed plugin error carrying its code rather than an
        // unsuccessful delivery; the typed check below replaces the suite's
        // in-band one.
        .without(Check::UpstreamFailureIsReported)
}

/// A single push that Pushbullet refuses reports the code the answer means,
/// with the provider's text kept out of the public message.
#[test]
fn a_single_push_failure_is_a_typed_error_with_its_code() {
    let conformance = conformance();
    for (status, code) in [
        (500, PluginErrorCode::UpstreamUnavailable),
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
            PluginNotificationCommand::Send(test_request(NotificationEventType::Grab)),
        );
        let PluginNotificationCommandResult::Send(PluginResult::Err(error)) = result else {
            panic!("HTTP {status} must be a typed plugin error: {result:?}");
        };
        assert_eq!(error.code, code, "HTTP {status}");
        assert!(!error.public_message.contains("synthetic upstream answer"));
        assert!(!error.public_message.contains(ACCESS_TOKEN));
    }
}

#[test]
fn pushbullet_release_wasm_conforms_to_the_notification_host_contract() {
    let outcome = conformance().run();

    // Pushbullet documents one request encoding and one credential header:
    // "All POST requests must use a JSON body with the Content-Type header set
    // to application/json", authenticated with `Access-Token`.
    let request = outcome
        .send
        .requests
        .iter()
        .find(|request| request.url == PUSHES_URL)
        .expect("the push must have been created");
    assert_eq!(request.method.as_deref(), Some("POST"));
    assert_eq!(request.header("Access-Token"), Some(ACCESS_TOKEN));
    assert_eq!(request.header("Content-Type"), Some("application/json"));
    assert!(
        request.header("Authorization").is_none(),
        "the token travels in exactly one place"
    );
    let body: serde_json::Value =
        serde_json::from_slice(&request.body).expect("the push is a JSON body");
    assert!(
        matches!(body["type"].as_str(), Some("note" | "link")),
        "{body}"
    );
    assert!(body["title"].is_string(), "{body}");
    assert!(body["body"].is_string(), "{body}");
    // No target configured: the push is broadcast to the account's devices,
    // which Pushbullet expresses by omitting every target parameter.
    for target in ["device_iden", "channel_tag", "email", "client_iden"] {
        assert!(
            body.get(target).is_none(),
            "{target} must be absent: {body}"
        );
    }
}

/// The only channel-specific action: listing the account's devices so an
/// operator can find the idens `device_ids` needs. It must reach
/// `GET /v2/devices` with the same credential header and answer with the
/// `{id, name}` options shape, active named devices only, sorted by name.
#[test]
fn the_device_list_action_answers_from_the_account_devices() {
    let conformance = conformance();
    let script = Script {
        http: HttpScript::Routed(vec![HttpRoute::contains(
            "/v2/devices",
            HttpReply::ok(
                r#"{"devices":[
                    {"iden":"dev-b","nickname":"tablet","active":true},
                    {"iden":"dev-a","nickname":"Phone","active":true},
                    {"iden":"dev-c","active":true},
                    {"iden":"dev-d","nickname":"retired","active":false}
                ]}"#,
            ),
        )]),
        ..conformance.script()
    };
    let (mut store, plugin) = instantiate(&conformance.wasm_path(), script);
    let result = call_notification(
        &mut store,
        &plugin,
        PluginNotificationCommand::Action(PluginActionRequest {
            action: "getDevices".to_string(),
            payload: serde_json::json!({ "query": {} }),
        }),
    );
    let PluginNotificationCommandResult::Action(PluginResult::Ok(response)) = result else {
        panic!("getDevices must answer in-band: {result:?}");
    };
    assert_eq!(
        response.payload,
        serde_json::json!({
            "options": [
                { "id": "dev-a", "name": "Phone" },
                { "id": "dev-b", "name": "tablet" },
            ]
        })
    );

    let recorded = &store.data().script;
    let request = recorded
        .requests
        .iter()
        .find(|request| request.url.contains("/v2/devices"))
        .expect("the device list must have been fetched");
    assert_eq!(request.header("Access-Token"), Some(ACCESS_TOKEN));
}
