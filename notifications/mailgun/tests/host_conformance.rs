//! Conformance against the real Scryer notification host, run on the RELEASE
//! artifact.
//!
//! The suite itself lives in `scryer-plugin-conformance`. What stays here is
//! what is genuinely this channel's: its configuration, its endpoint, and the
//! form body no URL assertion can reach.

// conformance: bespoke

use scryer_plugin_conformance::notification::{
    Check, NotificationConformance, call_notification, instantiate, test_request,
};
use scryer_plugin_conformance::{HttpScript, Script};
use scryer_plugin_sdk::command::{PluginNotificationCommand, PluginNotificationCommandResult};
use scryer_plugin_sdk::{NotificationEventType, PluginErrorCode, PluginResult};

/// A prefix rather than a whole URL: what is pinned is that the sending domain
/// and the region both come from the resolved configuration and are used
/// verbatim in the path. The message itself is a URL-encoded form body,
/// asserted separately below.
const EXPECTED_URL_PREFIX: &str = "https://api.mailgun.net/v3/mg.test.invalid/messages";

#[test]
fn mailgun_release_wasm_conforms_to_the_notification_host_contract() {
    let outcome = conformance()
        .without(Check::UpstreamFailureIsReported)
        .run();

    // Mailgun is posted to as a URL-encoded form, and every field that decides
    // where the mail goes travels in that body — never in the URL.
    let request = outcome
        .send
        .request_with_url_prefix(EXPECTED_URL_PREFIX)
        .expect("the message request must have been made");
    let form = String::from_utf8_lossy(&request.body).to_string();
    for field in [
        "from=scryer%40test.invalid",
        "to=ops%40test.invalid",
        "subject=",
        "text=",
        "html=",
    ] {
        assert!(
            form.contains(field),
            "the form body must carry {field}: {form}"
        );
    }
    assert!(
        !outcome.send.urls.iter().any(|url| url.contains('?')),
        "no message content may travel in the URL: {:?}",
        outcome.send.urls
    );

    // A 5xx is now a typed `UpstreamUnavailable` error rather than an in-band
    // failed delivery, so the family's in-band check is replaced by this one.
    assert_a_server_error_is_upstream_unavailable(&conformance());
}

fn conformance() -> NotificationConformance {
    NotificationConformance::new(env!("CARGO_MANIFEST_DIR"), "mailgun")
        .wasm("mailgun_notification.wasm")
        .config("sender_domain", "mg.test.invalid")
        .config("from", "scryer@test.invalid")
        .config("recipients", "ops@test.invalid")
        .config("api_key", "mailgunkey")
        .expects_url_prefix(EXPECTED_URL_PREFIX)
        .required_setting("sender_domain")
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
