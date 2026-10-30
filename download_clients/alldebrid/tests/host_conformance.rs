use scryer_plugin_conformance::download_client::DownloadClientConformance;

#[test]
fn alldebrid_release_wasm_conforms_to_the_download_client_host_contract() {
    DownloadClientConformance::new(env!("CARGO_MANIFEST_DIR"), "alldebrid")
        .wasm("alldebrid_download_client.wasm")
        .run();
}
