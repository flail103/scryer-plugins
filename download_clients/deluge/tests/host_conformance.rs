//! Conformance against the real Scryer download-client host, run on the
//! RELEASE artifact.
//!
//! The suite itself lives in `scryer-plugin-conformance`, which builds the
//! shipping `wasm32-wasip2` component and drives it the way the host does.
//! What stays here is what is genuinely this plugin's: its descriptor identity
//! and its artifact name.

use scryer_plugin_conformance::download_client::DownloadClientConformance;

#[test]
fn deluge_release_wasm_conforms_to_the_download_client_host_contract() {
    DownloadClientConformance::new(env!("CARGO_MANIFEST_DIR"), "deluge")
        .wasm("deluge_download_client.wasm")
        .run();
}

/// Deluge answers a wrong Web UI password with `{"result": false}` on
/// `auth.login`; the operator must see that the password was refused, not a
/// generic failure the host retries.
#[test]
fn a_refused_password_reports_its_code_through_the_component() {
    use scryer_plugin_conformance::download_client::{call_download_client, instantiate};
    use scryer_plugin_conformance::{
        ConfigSource, HttpReply, HttpRoute, HttpScript, Script, StateSource,
    };
    use scryer_plugin_sdk::command::{
        PluginDownloadClientCommand as Command, PluginDownloadClientCommandResult as Result,
    };
    use scryer_plugin_sdk::{PluginErrorCode, PluginResult};

    let suite = DownloadClientConformance::new(env!("CARGO_MANIFEST_DIR"), "deluge")
        .wasm("deluge_download_client.wasm");
    let script = Script {
        config: ConfigSource::Resolved(
            [
                ("host".into(), "deluge.test.invalid".into()),
                ("password".into(), "synthetic-password".into()),
            ]
            .into(),
        ),
        state: StateSource::Stored(Default::default()),
        http: HttpScript::Routed(vec![HttpRoute::any(HttpReply::ok(
            r#"{"result": false, "error": null, "id": 1}"#,
        ))]),
        ..Default::default()
    };
    let (mut store, plugin) = instantiate(&suite.wasm_path(), script);
    let result = call_download_client(&mut store, &plugin, Command::TestConnection);
    let Result::TestConnection(PluginResult::Err(error)) = result else {
        panic!("a refused password must be a typed plugin error: {result:?}");
    };
    assert_eq!(
        error.code,
        PluginErrorCode::from("invalid_username_password"),
        "{error:?}"
    );
    assert!(!error.public_message.contains("synthetic-password"));
}
