//! Generic webhook notifications, as a WASI Preview 2 component.
//!
//! The plugin implements `scryer:notification/notification@1.0.0`: two exports
//! carrying UTF-8 JSON (`describe` returns a `PluginDescriptor`, `process`
//! exchanges a `PluginCommandRequest` for a `PluginCommandResponse`), plus the
//! shared `scryer:host/services@1.0.0` import that carries config and HTTP.
//!
//! The delivery logic is untouched. What changed is the transport: the two
//! exported entry points collapse into one `process` export dispatching the SDK's
//! `PluginNotificationCommand`, and `config::get` / `http::request` now reach
//! Scryer through [`scryer_plugin_pdk`] rather than the removed core-module
//! host ABI.

mod redact;

use scryer_plugin_pdk::sdk::command::{PluginNotificationCommand, PluginNotificationCommandResult};
use scryer_plugin_pdk::{HttpRequest, codes, config, http};
use scryer_plugin_sdk::current_sdk_constraint;
use scryer_plugin_sdk::{
    ConfigFieldDef, ConfigFieldOption, ConfigFieldType, NotificationCapabilities,
    NotificationDeliveryMode, NotificationDescriptor, NotificationEventType,
    NotificationPayloadFormat, PluginDescriptor, PluginError, PluginErrorCode,
    PluginNotificationRequest, PluginNotificationResponse, PluginResult, ProviderDescriptor,
    SDK_VERSION, to_webhook_json,
};

wit_bindgen::generate!({
    // Fully qualified: `path` resolves two packages, so a bare world name is
    // ambiguous even though only one of them declares a world.
    world: "scryer:notification/notification@1.0.0",
    // Two packages, two paths, matching the host's own bindgen: the shared
    // `scryer:host` package is listed first so the family package's
    // `import scryer:host/services@1.0.0` resolves against it.
    path: ["../../pdk/scryer-plugin-pdk/wit/host-v1.0.0", "../../pdk/scryer-plugin-pdk/wit/notification-v1.0.0"],
    // The shared host package lives in its own WIT package, so wit-bindgen
    // asks explicitly whether to generate for it. Yes: the PDK holds only a
    // `fn` pointer and the entry macro binds it to this module's
    // `scryer::host::services::host-call`.
    generate_all,
});

scryer_plugin_pdk::scryer_notification_component_main!(
    descriptor = build_descriptor,
    handler = handle_notification_command,
);

const PROVIDER_TYPE: &str = "webhook";

// ---------------------------------------------------------------------------
// Plugin exports
// ---------------------------------------------------------------------------

/// The world's single `process` entry, dispatching the SDK's notification
/// command enum.
///
/// One arm per operation this plugin exports. `action` is not one of them: the descriptor advertises no action, so the host does not route
/// one here and the arm answers **in-band** with `Unsupported` rather than
/// trapping. A trap under a component costs the whole instance and replaces the
/// plugin's own diagnosis with a generic ABI failure.
fn handle_notification_command(
    command: PluginNotificationCommand,
) -> PluginNotificationCommandResult {
    match command {
        PluginNotificationCommand::Send(request) => {
            PluginNotificationCommandResult::Send(match send_notification(&request) {
                Ok(response) => PluginResult::Ok(response),
                Err(error) => PluginResult::Err(error),
            })
        }
        PluginNotificationCommand::Action(_) => {
            PluginNotificationCommandResult::Action(PluginResult::Err(
                plugin_error(
                    PluginErrorCode::Unsupported,
                    format!("{PROVIDER_TYPE} does not implement notification actions"),
                )
                .build(),
            ))
        }
    }
}

fn plugin_error(
    code: impl Into<PluginErrorCode>,
    public_message: impl Into<String>,
) -> scryer_plugin_pdk::PluginErrorBuilder {
    scryer_plugin_pdk::plugin_error(code, public_message)
}

/// The typed error for a non-success answer from the webhook endpoint.
///
/// A generic webhook carries no credential setting of its own (any secret
/// lives in the configured URL), so a 401/403 cannot name which credential
/// was wrong and is `AuthFailed`. The public sentence never carries the
/// status or the endpoint's text; both go to the host log.
fn http_failure(status: u16, retry_after: Option<&str>, body: &str) -> PluginError {
    let builder = match status {
        401 | 403 => plugin_error(
            PluginErrorCode::AuthFailed,
            "the webhook endpoint rejected the request as unauthorized",
        ),
        404 => plugin_error(
            codes::ENDPOINT_NOT_FOUND,
            "the configured webhook URL does not exist",
        ),
        429 => {
            let builder = plugin_error(
                PluginErrorCode::RateLimited,
                "the webhook endpoint is rate limiting requests",
            );
            match retry_after_seconds(retry_after) {
                Some(seconds) => builder.retry_after_seconds(seconds),
                None => builder,
            }
        }
        500..=599 => plugin_error(
            PluginErrorCode::UpstreamUnavailable,
            "the webhook endpoint answered with a server error",
        ),
        _ => plugin_error(
            PluginErrorCode::Permanent,
            "the webhook endpoint refused the notification",
        ),
    };
    builder
        .debug(redact::redact_secrets(&format!("HTTP {status}: {body}")))
        .build()
}

/// A `Retry-After` value in delta-seconds, rounded up to at least one second.
fn retry_after_seconds(value: Option<&str>) -> Option<i64> {
    value
        .and_then(|value| value.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| (seconds.ceil() as i64).max(1))
}

fn build_descriptor() -> PluginDescriptor {
    PluginDescriptor {
        id: "webhook".to_string(),
        name: "Webhook".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        sdk_version: SDK_VERSION.to_string(),
        sdk_constraint: current_sdk_constraint(),
        socket_permissions: vec![],
        settings: Vec::new(),
        provider: ProviderDescriptor::Notification(NotificationDescriptor {
            provider_type: "webhook".to_string(),
            provider_aliases: vec![],
            default_base_url: None,
            allowed_hosts: vec![],
            capabilities: NotificationCapabilities {
                supports_rich_text: false,
                supports_images: false,
                supports_test: true,
                supports_batch: false,
                supports_coalescing: false,
                requires_host_filesystem: false,
                requires_host_process: false,
                delivery_modes: vec![NotificationDeliveryMode::Webhook],
                payload_formats: vec![
                    NotificationPayloadFormat::StructuredJson,
                    NotificationPayloadFormat::PlainText,
                ],
                supported_events: general_notification_events(),
                event_options: Default::default(),
            },
            config_fields: vec![
                ConfigFieldDef {
                    key: "webhook_url".to_string(),
                    label: "Webhook URL".to_string(),
                    field_type: ConfigFieldType::String,
                    required: true,
                    default_value: None,
                    value_source: Default::default(),
                    host_binding: None,
                    role: None,
                    options: vec![],
                    help_text: Some("The URL to POST notification payloads to.".to_string()),
                    ..Default::default()
                },
                ConfigFieldDef {
                    key: "method".to_string(),
                    label: "HTTP Method".to_string(),
                    field_type: ConfigFieldType::Select,
                    required: false,
                    default_value: Some("POST".to_string()),
                    value_source: Default::default(),
                    host_binding: None,
                    role: None,
                    options: vec![
                        ConfigFieldOption {
                            value: "POST".to_string(),
                            label: "POST".to_string(),
                            // Added by SDK 3.10. No notification channel drives
                            // dependent fields from a select option, so the default
                            // empty map keeps this descriptor byte-identical.
                            config_overrides: Default::default(),
                        },
                        ConfigFieldOption {
                            value: "PUT".to_string(),
                            label: "PUT".to_string(),
                            // Added by SDK 3.10. No notification channel drives
                            // dependent fields from a select option, so the default
                            // empty map keeps this descriptor byte-identical.
                            config_overrides: Default::default(),
                        },
                    ],
                    help_text: None,
                    ..Default::default()
                },
                ConfigFieldDef {
                    key: "content_type".to_string(),
                    label: "Content Type".to_string(),
                    field_type: ConfigFieldType::Select,
                    required: false,
                    default_value: Some("application/json".to_string()),
                    value_source: Default::default(),
                    host_binding: None,
                    role: None,
                    options: vec![
                        ConfigFieldOption {
                            value: "application/json".to_string(),
                            label: "application/json".to_string(),
                            // Added by SDK 3.10. No notification channel drives
                            // dependent fields from a select option, so the default
                            // empty map keeps this descriptor byte-identical.
                            config_overrides: Default::default(),
                        },
                        ConfigFieldOption {
                            value: "text/plain".to_string(),
                            label: "text/plain".to_string(),
                            // Added by SDK 3.10. No notification channel drives
                            // dependent fields from a select option, so the default
                            // empty map keeps this descriptor byte-identical.
                            config_overrides: Default::default(),
                        },
                    ],
                    help_text: None,
                    ..Default::default()
                },
            ],
        }),
    }
}

fn general_notification_events() -> Vec<NotificationEventType> {
    vec![
        NotificationEventType::Grab,
        NotificationEventType::Download,
        NotificationEventType::Upgrade,
        NotificationEventType::ImportComplete,
        NotificationEventType::ImportRejected,
        NotificationEventType::Rename,
        NotificationEventType::TitleAdded,
        NotificationEventType::TitleDeleted,
        NotificationEventType::TitleMoved,
        NotificationEventType::FileDeleted,
        NotificationEventType::FileDeletedForUpgrade,
        NotificationEventType::PostProcessingCompleted,
        NotificationEventType::SubtitleDownloaded,
        NotificationEventType::SubtitleSearchFailed,
        NotificationEventType::MediaRequestSubmitted,
        NotificationEventType::MediaRequestApproved,
        NotificationEventType::MediaRequestRejected,
        NotificationEventType::MediaRequestCanceled,
    ]
}

fn send_notification(
    req: &PluginNotificationRequest,
) -> Result<PluginNotificationResponse, PluginError> {
    // Read config values injected by the host
    let webhook_url = config::get("webhook_url")
        .ok()
        .flatten()
        .unwrap_or_default();
    if webhook_url.is_empty() {
        return Err(plugin_error(
            PluginErrorCode::InvalidConfig,
            "the webhook_url setting is not configured",
        )
        .build());
    }

    let method = config::get("method")
        .ok()
        .flatten()
        .unwrap_or_else(|| "POST".to_string());
    let content_type = config::get("content_type")
        .ok()
        .flatten()
        .unwrap_or_else(|| "application/json".to_string());

    let body = if content_type == "text/plain" {
        format!(
            "[{}] {}: {}",
            req.event_type.as_str(),
            req.summary_title,
            req.summary_message
        )
    } else {
        serde_json::to_string(&to_webhook_json(req)).map_err(|error| {
            plugin_error(
                PluginErrorCode::InvalidConfig,
                "the webhook payload could not be built",
            )
            .debug(error)
            .build()
        })?
    };

    // Make the HTTP request through the host-services import
    let http_req = HttpRequest::new(&webhook_url)
        .with_method(&method)
        .with_header("Content-Type", &content_type)
        .with_header("User-Agent", "scryer-webhook-plugin/0.1");

    match http::request::<Vec<u8>>(&http_req, Some(body.into())) {
        Ok(res) => {
            let status = res.status_code();
            if (200..300).contains(&status) {
                let resp = PluginNotificationResponse {
                    success: true,
                    error: None,
                    delivery_id: None,
                    provider_status: None,
                    retry_after_seconds: None,
                    warnings: Vec::new(),
                    target_results: Vec::new(),
                };
                Ok(resp)
            } else {
                let body_text = String::from_utf8_lossy(&res.body()).to_string();
                Err(http_failure(status, res.header("Retry-After"), &body_text))
            }
        }
        // A request that never got an HTTP answer: the PDK reads the host's
        // transport detail (TLS, timeout, DNS/connect) into the right code.
        Err(error) => {
            let mut error = scryer_plugin_pdk::error::plugin_error_from(&error);
            error.debug_message = error
                .debug_message
                .map(|debug| redact::redact_secrets(&debug));
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unauthorized_answer_is_auth_failed_with_the_detail_in_debug_only() {
        for status in [401, 403] {
            let error = http_failure(status, None, "signature mismatch for hook-synthetic");
            assert_eq!(error.code, PluginErrorCode::AuthFailed, "{status}");
            assert!(!error.public_message.contains("hook-synthetic"));
            assert!(!error.public_message.contains(&status.to_string()));
            assert_eq!(
                error.debug_message.as_deref(),
                Some(format!("HTTP {status}: signature mismatch for hook-synthetic").as_str())
            );
        }
    }

    #[test]
    fn http_failures_map_to_their_codes() {
        assert_eq!(
            http_failure(404, None, "").code,
            PluginErrorCode::from(codes::ENDPOINT_NOT_FOUND)
        );
        let limited = http_failure(429, Some(" 12 "), "slow down");
        assert_eq!(limited.code, PluginErrorCode::RateLimited);
        assert_eq!(limited.retry_after_seconds, Some(12));
        assert_eq!(http_failure(429, None, "").retry_after_seconds, None);
        for status in [500, 502, 503] {
            assert_eq!(
                http_failure(status, None, "").code,
                PluginErrorCode::UpstreamUnavailable
            );
        }
        assert_eq!(
            http_failure(400, None, "bad payload").code,
            PluginErrorCode::Permanent
        );
    }

    #[test]
    fn describe_produces_valid_json() {
        let descriptor = build_descriptor();
        let result = serde_json::to_string(&descriptor).unwrap();
        let desc: serde_json::Value = serde_json::from_str(&result).unwrap();
        assert_eq!(desc["provider"]["kind"], "notification");
        assert_eq!(desc["provider"]["provider_type"], "webhook");
        assert_eq!(
            desc["provider"]["config_fields"].as_array().unwrap().len(),
            3
        );
        assert!(desc["provider"]["capabilities"].is_object());
    }

    #[test]
    fn descriptor_supports_media_request_lifecycle_events() {
        let notification = match build_descriptor().provider {
            ProviderDescriptor::Notification(notification) => notification,
            provider => panic!("expected notification provider, got {provider:?}"),
        };
        assert!(
            notification
                .capabilities
                .supported_events
                .contains(&NotificationEventType::MediaRequestSubmitted)
        );
        assert!(
            notification
                .capabilities
                .supported_events
                .contains(&NotificationEventType::MediaRequestApproved)
        );
        assert!(
            notification
                .capabilities
                .supported_events
                .contains(&NotificationEventType::MediaRequestRejected)
        );
        assert!(
            notification
                .capabilities
                .supported_events
                .contains(&NotificationEventType::MediaRequestCanceled)
        );
    }

    #[test]
    fn a_title_move_is_delivered_with_its_move_block() {
        let notification = match build_descriptor().provider {
            ProviderDescriptor::Notification(notification) => notification,
            provider => panic!("expected notification provider, got {provider:?}"),
        };
        assert!(
            notification
                .capabilities
                .supported_events
                .contains(&NotificationEventType::TitleMoved)
        );

        let mut request: PluginNotificationRequest = serde_json::from_value(serde_json::json!({
            "event_type": "title_moved",
            "summary_title": "Title moved: Cinder Line",
            "summary_message": "Moved 'Cinder Line' from Library A to Library B.",
            "app": { "name": "Scryer", "version": "test" },
        }))
        .expect("minimal notification request deserializes");
        request.title_move = Some(scryer_plugin_sdk::PluginNotificationTitleMove {
            source_path: Some("/media/a/Cinder Line".to_string()),
            destination_path: Some("/media/b/Cinder Line".to_string()),
            ..Default::default()
        });

        let parsed = serde_json::to_value(to_webhook_json(&request)).unwrap();
        assert_eq!(parsed["event_type"], "title_moved");
        assert_eq!(
            parsed["title_move"]["destination_path"],
            "/media/b/Cinder Line"
        );
        assert_eq!(parsed["title_move"]["source_path"], "/media/a/Cinder Line");
    }

    #[test]
    fn webhook_payload_serialization() {
        let payload = PluginNotificationRequest {
            schema_version: 1,
            event_type: scryer_plugin_sdk::NotificationEventType::Test,
            event_id: Some("evt-1".to_string()),
            occurred_at: Some("2026-04-29T12:00:00Z".to_string()),
            correlation_id: Some("corr-1".to_string()),
            actor: None,
            severity: None,
            is_test: true,
            summary_title: "Test Notification".to_string(),
            summary_message: "This is a test.".to_string(),
            app: scryer_plugin_sdk::PluginNotificationApp {
                name: "Scryer".to_string(),
                version: "test".to_string(),
            },
            title: Some(scryer_plugin_sdk::PluginNotificationTitle {
                id: None,
                name: "Cinder Line".to_string(),
                facet: "tv".to_string(),
                year: Some(2008),
                slug: None,
                path: None,
                overview: None,
                sort_title: None,
                background_url: None,
                poster_url: None,
                tags: Vec::new(),
                aliases: Vec::new(),
                original_language: None,
                original_country: None,
                external_ids: Default::default(),
            }),
            episode: None,
            episodes: Vec::new(),
            release: None,
            download: None,
            import: None,
            health: None,
            file: None,
            media_files: Vec::new(),
            application_update: None,
            manual_interaction: None,
            media_request: None,
            title_move: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["schema_version"], 1);
        assert_eq!(parsed["event_type"], "test");
        assert_eq!(parsed["title"]["name"], "Cinder Line");
        assert!(parsed.get("provider_extra").is_none());
    }
}
