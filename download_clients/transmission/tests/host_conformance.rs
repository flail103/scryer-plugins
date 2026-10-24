//! Conformance against the real Scryer download-client host, run on the
//! RELEASE artifact.
//!
//! The suite itself lives in `scryer-plugin-conformance`, which builds the
//! shipping `wasm32-wasip2` component and drives it the way the host does.
//! What stays here is what is genuinely this plugin's: its descriptor identity
//! and its artifact name.

use scryer_plugin_conformance::download_client::DownloadClientConformance;

#[test]
fn transmission_release_wasm_conforms_to_the_download_client_host_contract() {
    DownloadClientConformance::new(env!("CARGO_MANIFEST_DIR"), "transmission")
        .wasm("transmission_download_client.wasm")
        .run();
}

#[test]
fn session_versions_and_handshake_work_through_the_component() {
    use scryer_plugin_conformance::download_client::{call_download_client, instantiate_with};
    use scryer_plugin_conformance::{
        ConfigSource, HostErrorKind, HostResponder, HttpReply, HttpRoute, HttpScript, Script,
        StateSource, default_respond,
    };
    use scryer_plugin_sdk::PluginResult;
    use scryer_plugin_sdk::command::{
        PluginDownloadClientCommand as Command, PluginDownloadClientCommandResult as Result,
    };
    use scryer_plugin_sdk::host::{PluginHostRequest, PluginHostResponse};

    struct RpcHost {
        body: String,
    }
    impl HostResponder for RpcHost {
        fn respond(
            &mut self,
            request: PluginHostRequest,
            script: &mut Script,
        ) -> std::result::Result<PluginHostResponse, HostErrorKind> {
            if matches!(&request, PluginHostRequest::Http(_)) {
                let reply = if script.requests.is_empty() {
                    HttpReply::new(409, vec![])
                        .with_header("X-Transmission-Session-Id", "synthetic-session")
                } else {
                    HttpReply::ok(&self.body)
                };
                script.http = HttpScript::Routed(vec![HttpRoute::any(reply)]);
            }
            default_respond(request, script)
        }
    }
    let suite = DownloadClientConformance::new(env!("CARGO_MANIFEST_DIR"), "transmission")
        .wasm("transmission_download_client.wasm");
    for (protocol, version) in [
        (serde_json::json!(19), Some("4.1.3 (synthetic)")),
        (serde_json::json!("19"), Some("4.1.3 (synthetic)")),
        (serde_json::json!("14"), None),
    ] {
        let body = serde_json::json!({"result":"success","arguments":{
            "rpc-version":protocol,"version":version,"download-dir":"/downloads","torrents":[]
        }})
        .to_string();
        let script = Script {
            config: ConfigSource::Resolved(
                [("host".into(), "transmission.test.invalid".into())].into(),
            ),
            state: StateSource::Stored(Default::default()),
            ..Default::default()
        };
        let (mut store, plugin) = instantiate_with(&suite.wasm_path(), script, RpcHost { body });
        for command in [
            Command::TestConnection,
            Command::Status,
            Command::ListQueue,
            Command::ListHistory,
        ] {
            let result = call_download_client(&mut store, &plugin, command);
            assert!(
                matches!(
                    result,
                    Result::TestConnection(PluginResult::Ok(_))
                        | Result::Status(PluginResult::Ok(_))
                        | Result::ListQueue(PluginResult::Ok(_))
                        | Result::ListHistory(PluginResult::Ok(_))
                ),
                "{result:?}"
            );
        }
        let requests = &store.data().script.requests;
        assert!(requests.len() >= 5);
        assert_eq!(requests[0].body, requests[1].body);
        assert_eq!(
            requests[1].header("X-Transmission-Session-Id"),
            Some("synthetic-session")
        );
        assert!(requests[1..].iter().all(
            |request| request.header("X-Transmission-Session-Id") == Some("synthetic-session")
        ));
    }
}

/// Transmission answers wrong RPC credentials with 401; the operator must see
/// that the username or password was refused, not a retried generic failure.
#[test]
fn rejected_rpc_credentials_report_their_code_through_the_component() {
    use scryer_plugin_conformance::download_client::{call_download_client, instantiate};
    use scryer_plugin_conformance::{
        ConfigSource, HttpReply, HttpRoute, HttpScript, Script, StateSource,
    };
    use scryer_plugin_sdk::command::{
        PluginDownloadClientCommand as Command, PluginDownloadClientCommandResult as Result,
    };
    use scryer_plugin_sdk::{PluginErrorCode, PluginResult};

    let suite = DownloadClientConformance::new(env!("CARGO_MANIFEST_DIR"), "transmission")
        .wasm("transmission_download_client.wasm");
    let script = Script {
        config: ConfigSource::Resolved(
            [
                ("host".into(), "transmission.test.invalid".into()),
                ("username".into(), "synthetic-user".into()),
                ("password".into(), "synthetic-password".into()),
            ]
            .into(),
        ),
        state: StateSource::Stored(Default::default()),
        http: HttpScript::Routed(vec![HttpRoute::any(HttpReply::new(
            401,
            b"<h1>401: Unauthorized</h1>".to_vec(),
        ))]),
        ..Default::default()
    };
    let (mut store, plugin) = instantiate(&suite.wasm_path(), script);
    let result = call_download_client(&mut store, &plugin, Command::TestConnection);
    let Result::TestConnection(PluginResult::Err(error)) = result else {
        panic!("rejected credentials must be a typed plugin error: {result:?}");
    };
    assert_eq!(
        error.code,
        PluginErrorCode::from("invalid_username_password"),
        "{error:?}"
    );
    assert!(!error.public_message.contains("synthetic-password"));
}
