//! Shared building blocks for Scryer's notification channels.
//!
//! # What this crate is
//!
//! Every channel is a `scryer:notification/notification@1.0.0` component, and
//! each one is a thin body: parse config, build a request, render a payload.
//! Everything those bodies have in common lives here — the config accessors,
//! the payload and delivery types, the typed error constructors, the retry and
//! redaction helpers, and [`process_exec`] for the one channel family that
//! needs to run a host executable.
//!
//! # How it reaches Scryer
//!
//! A component reaches the host through exactly one import,
//! `scryer:host/services@1.0.0`, which the family entry macro binds to
//! [`scryer_plugin_pdk::host`]. This crate re-exports the PDK's `config`,
//! `http` and `var` shapes over that transport, and [`process_exec`] rides the
//! same door as `PluginHostRequest::ProcessExec` — the family that needs
//! authority beyond HTTP imports the same one function as the families that do
//! not.
//!
//! # Nothing here names a world
//!
//! `wit_bindgen::generate!` has to live in the plugin crate: bindings are
//! generated per world, and a shared crate that named one would drag that
//! world's import into every component linking it (the failure mode
//! `subtitles/amenzb` hit through `newznab-common`). This crate only ever
//! touches the PDK's injected `fn`-pointer transport, so it is world-agnostic
//! by construction and links into any family world unchanged.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use scryer_plugin_sdk::current_sdk_constraint;

/// The PDK's host-service shapes, re-exported for channel bodies.
///
/// Re-exported so `use notify_common::*;` keeps supplying `config::get`,
/// `http::request`, `HttpRequest` and friends to every channel body.
pub use scryer_plugin_pdk::{Error, FnResult, HttpRequest, HttpResponse, config, http, var};
/// The shared open error codes and the PDK's error builder, for channel bodies.
pub use scryer_plugin_pdk::{PluginErrorBuilder, PluginResultExt, codes};
pub use scryer_plugin_sdk::command::{
    PluginActionRequest, PluginActionResponse, PluginNotificationCommand,
    PluginNotificationCommandResult,
};
pub use scryer_plugin_sdk::{
    ConfigFieldDef, ConfigFieldOption, ConfigFieldRole, ConfigFieldType, NotificationCapabilities,
    NotificationDeliveryMode, NotificationEventType, NotificationPayloadFormat, PluginDescriptor,
    PluginError, PluginErrorCode, PluginNotificationMediaFile, PluginNotificationRequest,
    PluginNotificationResponse, PluginNotificationTitleMove, PluginResult, ProviderDescriptor,
    SDK_VERSION,
};

// Pre-existing shape, not introduced by the component migration: this is a
// flat descriptor constructor whose arguments are the descriptor's own fields,
// so a parameter struct would only move the same list one level out.
#[allow(clippy::too_many_arguments)]
pub fn build_notification_descriptor(
    id: &str,
    name: &str,
    version: &str,
    provider_type: &str,
    delivery_modes: Vec<NotificationDeliveryMode>,
    payload_formats: Vec<NotificationPayloadFormat>,
    config_fields: Vec<ConfigFieldDef>,
    supports_rich_text: bool,
    supports_images: bool,
) -> PluginDescriptor {
    PluginDescriptor {
        id: id.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        sdk_version: SDK_VERSION.to_string(),
        sdk_constraint: current_sdk_constraint(),
        socket_permissions: vec![],
        settings: Vec::new(),
        provider: ProviderDescriptor::Notification(scryer_plugin_sdk::NotificationDescriptor {
            provider_type: provider_type.to_string(),
            provider_aliases: vec![],
            default_base_url: None,
            allowed_hosts: vec![],
            capabilities: NotificationCapabilities {
                supports_rich_text,
                supports_images,
                supports_test: true,
                supports_batch: false,
                supports_coalescing: false,
                requires_host_filesystem: false,
                requires_host_process: false,
                delivery_modes,
                payload_formats,
                supported_events: general_notification_events(),
                event_options: Default::default(),
            },
            config_fields,
        }),
    }
}

pub fn add_notification_allowed_hosts(descriptor: &mut PluginDescriptor, hosts: &[&str]) {
    if let ProviderDescriptor::Notification(notification) = &mut descriptor.provider {
        notification
            .allowed_hosts
            .extend(hosts.iter().map(|host| (*host).to_string()));
        notification.allowed_hosts.sort();
        notification.allowed_hosts.dedup();
    }
}

pub fn general_notification_events() -> Vec<NotificationEventType> {
    vec![
        NotificationEventType::Grab,
        NotificationEventType::Download,
        NotificationEventType::Upgrade,
        NotificationEventType::ImportComplete,
        NotificationEventType::ImportRejected,
        NotificationEventType::Rename,
        NotificationEventType::TitleAdded,
        NotificationEventType::TitleDeleted,
        NotificationEventType::FileDeleted,
        NotificationEventType::FileDeletedForUpgrade,
        NotificationEventType::PostProcessingCompleted,
        NotificationEventType::SubtitleDownloaded,
        NotificationEventType::SubtitleSearchFailed,
        NotificationEventType::MediaRequestSubmitted,
        NotificationEventType::MediaRequestApproved,
        NotificationEventType::MediaRequestRejected,
        NotificationEventType::MediaRequestCanceled,
        NotificationEventType::HealthIssue,
        NotificationEventType::HealthRestored,
        NotificationEventType::ApplicationUpdate,
        NotificationEventType::ManualInteractionRequired,
        NotificationEventType::TitleMoved,
        NotificationEventType::Test,
    ]
}

pub fn field(
    key: &str,
    label: &str,
    field_type: ConfigFieldType,
    required: bool,
    default_value: Option<&str>,
    help_text: Option<&str>,
) -> ConfigFieldDef {
    ConfigFieldDef {
        key: key.to_string(),
        label: label.to_string(),
        field_type,
        required,
        default_value: default_value.map(str::to_string),
        value_source: Default::default(),
        host_binding: None,
        role: None,
        options: vec![],
        help_text: help_text.map(str::to_string),
        ..Default::default()
    }
}

pub fn connection_field(
    key: &str,
    label: &str,
    required: bool,
    default_value: Option<&str>,
    help_text: Option<&str>,
) -> ConfigFieldDef {
    ConfigFieldDef {
        role: Some(ConfigFieldRole::ConnectionUrl),
        ..field(
            key,
            label,
            ConfigFieldType::String,
            required,
            default_value,
            help_text,
        )
    }
}

pub fn select_field(
    key: &str,
    label: &str,
    default_value: Option<&str>,
    options: &[(&str, &str)],
) -> ConfigFieldDef {
    ConfigFieldDef {
        options: options
            .iter()
            .map(|(value, label)| ConfigFieldOption {
                value: (*value).to_string(),
                label: (*label).to_string(),
                // Added by SDK 3.10; no notification channel drives dependent
                // fields from a select option, so the default empty map keeps
                // the descriptors byte-identical to what 3.2 produced.
                config_overrides: Default::default(),
            })
            .collect(),
        ..field(
            key,
            label,
            ConfigFieldType::Select,
            false,
            default_value,
            None,
        )
    }
}

pub fn config_value(key: &str) -> Option<String> {
    config::get(key)
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn required_config(key: &str) -> Result<String, Error> {
    config_value(key).ok_or_else(|| Error::msg(format!("{key} is not configured")))
}

pub fn config_bool(key: &str) -> bool {
    config_value(key)
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

pub fn config_i64(key: &str, default_value: i64) -> i64 {
    config_value(key)
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(default_value)
}

pub fn config_csv(key: &str) -> Vec<String> {
    config_value(key)
        .map(|value| {
            value
                .split([',', '\n', ';'])
                .map(str::trim)
                .filter(|part| !part.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

pub fn notification_title(req: &PluginNotificationRequest) -> String {
    req.summary_title.trim().to_string()
}

pub fn script_environment(
    req: &PluginNotificationRequest,
) -> std::collections::BTreeMap<String, String> {
    scryer_plugin_sdk::notification::to_script_environment(req)
}

pub fn notification_body(req: &PluginNotificationRequest) -> String {
    req.summary_message.trim().to_string()
}

pub fn title_and_body(req: &PluginNotificationRequest) -> (String, String) {
    (notification_title(req), notification_body(req))
}

pub fn poster_url(req: &PluginNotificationRequest) -> Option<String> {
    req.title
        .as_ref()
        .and_then(|title| {
            title
                .poster_url
                .clone()
                .or_else(|| title.background_url.clone())
        })
        .filter(|url| !url.trim().is_empty())
}

/// Where a `title_moved` title came from: its old folder when the host reported
/// one, otherwise the library it left.
pub fn title_move_source(req: &PluginNotificationRequest) -> Option<String> {
    let title_move = req.title_move.as_ref()?;
    non_empty(title_move.source_path.as_deref())
        .or_else(|| non_empty(title_move.source_library_name.as_deref()))
}

/// Where a `title_moved` title went: its new folder when the host reported one,
/// otherwise the library it joined.
pub fn title_move_destination(req: &PluginNotificationRequest) -> Option<String> {
    let title_move = req.title_move.as_ref()?;
    non_empty(title_move.destination_path.as_deref())
        .or_else(|| non_empty(title_move.destination_library_name.as_deref()))
}

/// The warning a `title_moved` notification carries when the move finished with
/// warnings, preferring the host's own detail. `None` for a clean move.
pub fn title_move_warning(req: &PluginNotificationRequest) -> Option<String> {
    let title_move = req.title_move.as_ref()?;
    if !title_move.completed_with_warnings {
        return None;
    }
    Some(
        non_empty(title_move.detail.as_deref())
            .unwrap_or_else(|| "Completed with warnings".to_string()),
    )
}

fn non_empty(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn ok_response() -> PluginNotificationResponse {
    PluginNotificationResponse {
        success: true,
        error: None,
        delivery_id: None,
        provider_status: None,
        retry_after_seconds: None,
        warnings: Vec::new(),
        target_results: Vec::new(),
    }
}

pub fn error_response(
    error: impl Into<String>,
    provider_status: Option<String>,
) -> PluginNotificationResponse {
    PluginNotificationResponse {
        success: false,
        error: Some(redact_secrets(&error.into())),
        delivery_id: None,
        provider_status,
        retry_after_seconds: None,
        warnings: Vec::new(),
        target_results: Vec::new(),
    }
}

pub fn merge_responses(responses: Vec<PluginNotificationResponse>) -> PluginNotificationResponse {
    let mut merged = ok_response();
    for response in responses {
        if !response.success {
            merged.success = false;
        }
        if let Some(error) = response.error {
            merged.warnings.push(error);
        }
        if let Some(status) = response.provider_status {
            merged.warnings.push(status);
        }
    }
    if !merged.success && merged.error.is_none() {
        merged.error = Some("one or more notification targets failed".to_string());
    }
    merged
}

pub fn basic_auth_header(username: &str, password: &str) -> String {
    format!(
        "Basic {}",
        STANDARD.encode(format!("{username}:{password}").as_bytes())
    )
}

pub fn append_query(url: &str, params: &[(&str, String)]) -> String {
    let separator = if url.contains('?') { '&' } else { '?' };
    let query = params
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&");
    if query.is_empty() {
        url.to_string()
    } else {
        format!("{url}{separator}{query}")
    }
}

pub fn path_segment(value: &str) -> String {
    urlencoding::encode(value).into_owned()
}

pub fn form_body(params: &[(String, String)]) -> Vec<u8> {
    params
        .iter()
        .map(|(key, value)| {
            format!(
                "{}={}",
                urlencoding::encode(key),
                urlencoding::encode(value)
            )
        })
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

pub fn send_json(
    url: &str,
    method: &str,
    headers: &[(&str, String)],
    body: serde_json::Value,
) -> PluginNotificationResponse {
    send_bytes(
        url,
        method,
        &[headers, &[("Content-Type", "application/json".to_string())]].concat(),
        serde_json::to_vec(&body).unwrap_or_default(),
    )
}

pub fn send_form(
    url: &str,
    method: &str,
    headers: &[(&str, String)],
    params: &[(String, String)],
) -> PluginNotificationResponse {
    send_bytes(
        url,
        method,
        &[
            headers,
            &[(
                "Content-Type",
                "application/x-www-form-urlencoded".to_string(),
            )],
        ]
        .concat(),
        form_body(params),
    )
}

pub fn send_bytes(
    url: &str,
    method: &str,
    headers: &[(&str, String)],
    body: Vec<u8>,
) -> PluginNotificationResponse {
    let mut request = HttpRequest::new(url)
        .with_method(method)
        .with_header("User-Agent", "scryer-notification-plugin/0.1");
    for (key, value) in headers {
        request = request.with_header(*key, value);
    }

    match http::request::<Vec<u8>>(&request, Some(body)) {
        Ok(response) => {
            let status = response.status_code();
            if (200..300).contains(&status) {
                ok_response()
            } else {
                let body_text = String::from_utf8_lossy(&response.body()).to_string();
                error_response(
                    format!("HTTP {}: {}", status, body_text),
                    Some(format!("http_{status}")),
                )
            }
        }
        Err(error) => error_response(format!("request failed: {error}"), None),
    }
}

pub fn html_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProcessExecRequest {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_directory: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdin_base64: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProcessExecResponse {
    pub status_code: Option<i32>,
    pub stdout_base64: String,
    pub stderr_base64: String,
    /// Always `false` since the move to host services.
    ///
    /// The predecessor host function reported a timeout as a *successful*
    /// response with this flag set. `PluginHostRequest::ProcessExec` has no
    /// such field:
    /// a host-enforced timeout is a typed `PluginError` and therefore arrives
    /// on the `Err` arm of [`process_exec`], carrying the host's message. The
    /// field is kept because it is part of this crate's published shape and
    /// because a future host may populate it again.
    pub timed_out: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(bound(deserialize = "T: serde::Deserialize<'de>"))]
pub struct ProcessResponse<T> {
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ProcessError>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProcessError {
    pub code: String,
    pub message: String,
}

/// Run one allowlisted host executable.
///
/// This used to be a dedicated host-function extern exchanging JSON strings.
/// It is now `PluginHostRequest::ProcessExec` over the same
/// `scryer:host/services@1.0.0` import every other service uses — which is the
/// whole point of the notification world's design note: the family that needs
/// authority beyond HTTP imports the same one function as the families that do
/// not.
///
/// Authority is unchanged and still entirely host-side. The descriptor's
/// `requires_host_process` capability plus the loader's first-party gate decide
/// whether the allowlist is populated; a community channel that asks for this
/// gets `permission_denied` per call, in-band.
pub fn process_exec(request: ProcessExecRequest) -> Result<ProcessExecResponse, ProcessError> {
    let stdin = match &request.stdin_base64 {
        Some(encoded) => STANDARD.decode(encoded).map_err(|error| ProcessError {
            code: "protocol_error".to_string(),
            message: format!("failed to decode process stdin: {error}"),
        })?,
        None => Vec::new(),
    };

    let response =
        scryer_plugin_pdk::host::process_exec(scryer_plugin_sdk::host::PluginProcessExecRequest {
            command: request.command,
            args: request.args,
            env: request.env,
            cwd: request.working_directory,
            stdin,
            timeout_ms: request.timeout_ms,
        })
        .map_err(process_error)?;

    Ok(ProcessExecResponse {
        status_code: Some(response.exit_code),
        stdout_base64: STANDARD.encode(&response.stdout),
        stderr_base64: STANDARD.encode(&response.stderr),
        timed_out: false,
    })
}

/// Translate a host-services failure into this crate's process error shape.
///
/// A host with no process service configured answers **in-band** with a typed
/// `PluginError`, so that arrives as [`scryer_plugin_pdk::host::HostCallError::Service`]
/// and keeps the host's own diagnosis — `permission_denied`, an allowlist miss,
/// or a timeout — instead of collapsing to a generic ABI fault the way the
/// predecessor host function did.
fn process_error(error: scryer_plugin_pdk::host::HostCallError) -> ProcessError {
    match error {
        scryer_plugin_pdk::host::HostCallError::Service(error) => ProcessError {
            code: process_error_code(error.code),
            message: error.public_message,
        },
        scryer_plugin_pdk::host::HostCallError::Unavailable => ProcessError {
            code: "unsupported".to_string(),
            message: "this host provides no process execution service".to_string(),
        },
        error => ProcessError {
            code: "protocol_error".to_string(),
            message: error.to_string(),
        },
    }
}

fn process_error_code(code: PluginErrorCode) -> String {
    let code = match code {
        PluginErrorCode::InvalidConfig => "invalid_config",
        PluginErrorCode::AuthFailed => "permission_denied",
        PluginErrorCode::RateLimited => "rate_limited",
        PluginErrorCode::UpstreamUnavailable => "upstream_unavailable",
        PluginErrorCode::Unsupported => "unsupported",
        PluginErrorCode::Temporary => "temporary",
        PluginErrorCode::Permanent => "permanent",
        // An open code from a newer host keeps its own spelling.
        PluginErrorCode::Other(code) => return code,
    };
    code.to_string()
}

/// The in-band answer a channel with no action operation gives.
///
/// Every notification world carries `send` and `action`; most channels
/// implement only `send`. The host reads that from the descriptor and does not
/// route an action here, so this arm exists to *answer* rather than to trap —
/// a guest trap would cost the host the plugin's own diagnosis and, under a
/// component, the whole instance.
pub fn unsupported_action(provider: &str) -> PluginResult<PluginActionResponse> {
    PluginResult::Err(
        scryer_plugin_pdk::plugin_error(
            PluginErrorCode::Unsupported,
            format!("{provider} does not implement notification actions"),
        )
        .build(),
    )
}

/// Turn a configuration-resolution failure into a typed plugin error.
///
/// These used to be hard ABI faults: the host saw a string and a generic
/// failure, and could not tell a misconfigured channel from a broken one. They are now an ordinary `PluginResult::Err` on the operation's
/// own result, which is what lets Scryer surface "this channel is not
/// configured" to the operator instead of "the plugin crashed".
pub fn config_error(error: impl std::fmt::Display) -> PluginError {
    scryer_plugin_pdk::plugin_error(PluginErrorCode::InvalidConfig, error.to_string()).build()
}

/// Which configured credential a channel's service checks, so a rejection can
/// name the one the operator has to fix.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Credential {
    /// An API key, token or webhook secret.
    ApiKey,
    /// A username and password.
    UsernamePassword,
    /// The service rejected the request without saying which credential.
    Unknown,
}

impl Credential {
    /// The error code a rejection of this credential carries.
    pub fn code(self) -> PluginErrorCode {
        match self {
            Self::ApiKey => codes::INVALID_API_KEY.into(),
            Self::UsernamePassword => codes::INVALID_USERNAME_PASSWORD.into(),
            Self::Unknown => PluginErrorCode::AuthFailed,
        }
    }

    fn public_message(self) -> &'static str {
        match self {
            Self::ApiKey => "the service rejected the configured key or token",
            Self::UsernamePassword => "the service rejected the configured username or password",
            Self::Unknown => "the service rejected the configured credentials",
        }
    }
}

/// The service rejected `credential`. `debug` goes to the host log only.
pub fn rejected_credential(credential: Credential, debug: impl std::fmt::Display) -> PluginError {
    scryer_plugin_pdk::plugin_error(credential.code(), credential.public_message())
        .debug(redact_secrets(&debug.to_string()))
        .build()
}

/// The typed error for a non-success HTTP answer a channel has no more
/// specific reading of.
///
/// | Status | Code |
/// |---|---|
/// | 401, 403 | `credential`'s code |
/// | 404 | `endpoint_not_found` |
/// | 429 | `RateLimited`, with `retry_after_seconds` |
/// | 5xx | `UpstreamUnavailable` |
/// | any other | `Permanent` |
///
/// The public sentence never carries the status or the service's own text;
/// `debug` (for example the status and response body) goes to the host log,
/// through [`redact_secrets`].
pub fn http_failure(
    status: u16,
    retry_after_seconds: Option<i64>,
    credential: Credential,
    debug: impl std::fmt::Display,
) -> PluginError {
    let builder = match status {
        401 | 403 => {
            scryer_plugin_pdk::plugin_error(credential.code(), credential.public_message())
        }
        404 => scryer_plugin_pdk::plugin_error(
            codes::ENDPOINT_NOT_FOUND,
            "the configured address does not answer as this service",
        ),
        429 => {
            let builder = scryer_plugin_pdk::plugin_error(
                PluginErrorCode::RateLimited,
                "the service is rate limiting requests",
            );
            match retry_after_seconds {
                Some(seconds) => builder.retry_after_seconds(seconds),
                None => builder,
            }
        }
        500..=599 => scryer_plugin_pdk::plugin_error(
            PluginErrorCode::UpstreamUnavailable,
            "the service answered with a server error",
        ),
        _ => scryer_plugin_pdk::plugin_error(
            PluginErrorCode::Permanent,
            "the service refused the notification",
        ),
    };
    builder.debug(redact_secrets(&debug.to_string())).build()
}

/// The typed error for a request that never got an HTTP answer (refused
/// egress, DNS, connection or TLS failure, timeout), classified by the PDK.
///
/// URL secrets in the debug text (the host's transport error usually quotes the
/// request URL) are replaced by [`redact_secrets`].
pub fn request_failure(error: &Error) -> PluginError {
    let mut error = scryer_plugin_pdk::error::plugin_error_from(error);
    error.debug_message = error.debug_message.map(|debug| redact_secrets(&debug));
    error
}

/// A `Retry-After` header in delta-seconds, if the response carried one.
pub fn retry_after_header(headers: &std::collections::BTreeMap<String, String>) -> Option<i64> {
    headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| value.trim().parse::<f64>().ok())
        .filter(|seconds| seconds.is_finite() && *seconds >= 0.0)
        .map(|seconds| (seconds.ceil() as i64).max(1))
}

// ---------------------------------------------------------------------------
// Redacting secrets from debug text
// ---------------------------------------------------------------------------
//
// The same helper as `scryer_plugin_pdk::error::redact_secrets` in PDK 0.8.1.
// The channels build against PDK 0.8.0 from crates.io, so it is carried here
// until they take a PDK that exports it; keep the two in step.

/// What a redacted secret is replaced with.
pub const REDACTED: &str = "<redacted>";

/// Path segments after which every further segment of a URL is a secret: the
/// webhook id and token of Discord (`/api/webhooks/<id>/<token>`), Slack
/// (`/services/T…/B…/<secret>`), Mattermost and Rocket.Chat
/// (`/hooks/<id>[/<token>]`), Home Assistant (`/api/webhook/<id>`) and an
/// Apprise configuration key (`/notify/<key>`).
const SECRET_PATH_MARKERS: &[&str] = &["webhooks", "webhook", "hooks", "services", "notify"];

/// The shortest path segment the shape heuristic treats as an opaque token.
const MIN_TOKEN_SEGMENT: usize = 20;

/// Replace the secrets a URL can carry with [`REDACTED`], everywhere a URL
/// appears in `text`.
///
/// Meant for debug text — a transport error, an upstream body — that may quote
/// the request URL of a channel whose URL *is* the credential. Inside every
/// `scheme://…` it redacts:
///
/// * userinfo (`https://user:pass@host` → `https://<redacted>@host`);
/// * every query value (`?token=…`, `?key=…`, `?apikey=…`, and any other) and
///   the fragment;
/// * every path segment after a known hook path (`webhooks`, `webhook`,
///   `hooks`, `services`, `notify`);
/// * a Telegram bot token segment (`/bot<id>:<secret>/…`);
/// * any other path segment that looks like an opaque token: at least
///   20 characters of `[A-Za-z0-9-_.~:=+%]` mixing letters and digits.
///
/// Text outside URLs is returned unchanged.
pub fn redact_secrets(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(separator) = rest.find("://") {
        // Past the whole delimiter: it may be multibyte punctuation such as `：`.
        let scheme_start = rest[..separator]
            .char_indices()
            .rfind(|&(_, c)| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.')))
            .map_or(0, |(index, c)| index + c.len_utf8());
        if scheme_start == separator {
            // `://` with no scheme in front of it is not a URL.
            out.push_str(&rest[..separator + 3]);
            rest = &rest[separator + 3..];
            continue;
        }
        let end = rest[separator + 3..]
            .find(|c: char| {
                c.is_whitespace() || matches!(c, '"' | '\'' | '`' | '<' | '>' | ')' | ']' | '}')
            })
            .map_or(rest.len(), |index| separator + 3 + index);
        out.push_str(&rest[..scheme_start]);
        out.push_str(&redact_url(&rest[scheme_start..end]));
        rest = &rest[end..];
    }
    out.push_str(rest);
    out
}

fn redact_url(url: &str) -> String {
    let Some(separator) = url.find("://") else {
        return url.to_string();
    };
    let (scheme, rest) = (&url[..separator + 3], &url[separator + 3..]);
    let (before_fragment, fragment) = match rest.split_once('#') {
        Some((before, fragment)) => (before, Some(fragment)),
        None => (rest, None),
    };
    let (before_query, query) = match before_fragment.split_once('?') {
        Some((before, query)) => (before, Some(query)),
        None => (before_fragment, None),
    };
    let (authority, path) = match before_query.find('/') {
        Some(index) => before_query.split_at(index),
        None => (before_query, ""),
    };

    let mut out = String::with_capacity(url.len());
    out.push_str(scheme);
    match authority.rsplit_once('@') {
        Some((_, host)) => {
            out.push_str(REDACTED);
            out.push('@');
            out.push_str(host);
        }
        None => out.push_str(authority),
    }

    let mut after_marker = false;
    for (index, segment) in path.split('/').enumerate() {
        if index > 0 {
            out.push('/');
        }
        if segment.is_empty() {
            continue;
        }
        if after_marker || looks_like_token(segment) {
            out.push_str(REDACTED);
        } else if segment.starts_with("bot") && segment.contains(':') {
            out.push_str("bot");
            out.push_str(REDACTED);
        } else {
            out.push_str(segment);
        }
        if SECRET_PATH_MARKERS
            .iter()
            .any(|marker| segment.eq_ignore_ascii_case(marker))
        {
            after_marker = true;
        }
    }

    if let Some(query) = query {
        out.push('?');
        for (index, pair) in query.split('&').enumerate() {
            if index > 0 {
                out.push('&');
            }
            match pair.split_once('=') {
                Some((key, value)) if !value.is_empty() => {
                    out.push_str(key);
                    out.push('=');
                    out.push_str(REDACTED);
                }
                Some((key, _)) => {
                    out.push_str(key);
                    out.push('=');
                }
                None if looks_like_token(pair) => out.push_str(REDACTED),
                None => out.push_str(pair),
            }
        }
    }
    if let Some(fragment) = fragment {
        out.push('#');
        if !fragment.is_empty() {
            out.push_str(REDACTED);
        }
    }
    out
}

fn looks_like_token(segment: &str) -> bool {
    segment.len() >= MIN_TOKEN_SEGMENT
        && segment.chars().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | ':' | '=' | '+' | '%')
        })
        && segment.chars().any(|c| c.is_ascii_digit())
        && segment.chars().any(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title_moved(title_move: serde_json::Value) -> PluginNotificationRequest {
        serde_json::from_value(serde_json::json!({
            "event_type": "title_moved",
            "summary_title": "Moved: Example Title",
            "summary_message": "Moved 'Example Title'.",
            "app": { "name": "Scryer", "version": "test" },
            "title_move": title_move,
        }))
        .expect("title_moved request deserializes")
    }

    #[test]
    fn credential_rejections_name_the_configured_credential() {
        let key = http_failure(401, None, Credential::ApiKey, "HTTP 401: bad token");
        assert_eq!(key.code, PluginErrorCode::from(codes::INVALID_API_KEY));
        assert_eq!(key.debug_message.as_deref(), Some("HTTP 401: bad token"));
        assert!(!key.public_message.contains("bad token"));

        let login = http_failure(403, None, Credential::UsernamePassword, "HTTP 403");
        assert_eq!(
            login.code,
            PluginErrorCode::from(codes::INVALID_USERNAME_PASSWORD)
        );
        assert_eq!(
            http_failure(401, None, Credential::Unknown, "").code,
            PluginErrorCode::AuthFailed
        );
    }

    #[test]
    fn http_failures_map_to_the_shared_codes() {
        assert_eq!(
            http_failure(404, None, Credential::ApiKey, "").code,
            PluginErrorCode::from(codes::ENDPOINT_NOT_FOUND)
        );
        let limited = http_failure(429, Some(30), Credential::ApiKey, "");
        assert_eq!(limited.code, PluginErrorCode::RateLimited);
        assert_eq!(limited.retry_after_seconds, Some(30));
        assert_eq!(
            http_failure(503, None, Credential::ApiKey, "").code,
            PluginErrorCode::UpstreamUnavailable
        );
        assert_eq!(
            http_failure(400, None, Credential::ApiKey, "").code,
            PluginErrorCode::Permanent
        );
    }

    #[test]
    fn retry_after_reads_delta_seconds_only() {
        let mut headers = std::collections::BTreeMap::new();
        headers.insert("Retry-After".to_string(), "12".to_string());
        assert_eq!(retry_after_header(&headers), Some(12));
        headers.insert(
            "Retry-After".to_string(),
            "Wed, 21 Oct 2026 07:28:00 GMT".to_string(),
        );
        assert_eq!(retry_after_header(&headers), None);
    }

    #[test]
    fn redacts_url_userinfo() {
        assert_eq!(
            redact_secrets("GET https://operator:hunter2@example.invalid/api failed"),
            "GET https://<redacted>@example.invalid/api failed"
        );
    }

    #[test]
    fn a_url_after_multibyte_punctuation_is_still_redacted() {
        assert_eq!(
            redact_secrets("Request failed：https://example.invalid/?token=secret"),
            "Request failed：https://example.invalid/?token=<redacted>"
        );
    }

    #[test]
    fn redacts_every_query_value_and_the_fragment() {
        assert_eq!(
            redact_secrets(
                "https://example.invalid/message?token=abc&key=def&apikey=ghi&empty=&flag#frag"
            ),
            "https://example.invalid/message?token=<redacted>&key=<redacted>&apikey=<redacted>&empty=&flag#<redacted>"
        );
    }

    #[test]
    fn redacts_a_discord_webhook_id_and_token() {
        assert_eq!(
            redact_secrets(
                "error sending request for url (https://discord.invalid/api/webhooks/123/abc)"
            ),
            "error sending request for url (https://discord.invalid/api/webhooks/<redacted>/<redacted>)"
        );
    }

    #[test]
    fn redacts_a_slack_services_path() {
        assert_eq!(
            redact_secrets("https://hooks.slack.invalid/services/T000/B000/XXXX"),
            "https://hooks.slack.invalid/services/<redacted>/<redacted>/<redacted>"
        );
    }

    #[test]
    fn redacts_hooks_webhook_and_notify_paths() {
        assert_eq!(
            redact_secrets("https://chat.invalid/hooks/abc/def"),
            "https://chat.invalid/hooks/<redacted>/<redacted>"
        );
        assert_eq!(
            redact_secrets("http://ha.invalid:8123/api/webhook/abc"),
            "http://ha.invalid:8123/api/webhook/<redacted>"
        );
        assert_eq!(
            redact_secrets("http://apprise.invalid/notify/key1"),
            "http://apprise.invalid/notify/<redacted>"
        );
    }

    #[test]
    fn redacts_a_telegram_bot_token_segment() {
        assert_eq!(
            redact_secrets("https://api.telegram.invalid/bot12:ab/sendMessage"),
            "https://api.telegram.invalid/bot<redacted>/sendMessage"
        );
    }

    #[test]
    fn redacts_an_opaque_token_shaped_segment() {
        assert_eq!(
            redact_secrets("https://push.invalid/a1b2c3d4e5f6a7b8c9d0/notifications/Example"),
            "https://push.invalid/<redacted>/notifications/Example"
        );
        // Short, word-like or digit-only segments are left alone.
        assert_eq!(
            redact_secrets("http://signal.invalid:8080/v2/send"),
            "http://signal.invalid:8080/v2/send"
        );
    }

    #[test]
    fn leaves_text_without_urls_alone_and_handles_several_urls() {
        assert_eq!(
            redact_secrets("HTTP 500: upstream exploded"),
            "HTTP 500: upstream exploded"
        );
        assert_eq!(redact_secrets("odd :// marker"), "odd :// marker");
        assert_eq!(
            redact_secrets("a https://x.invalid/?k=1 b \"https://u:p@y.invalid\" c"),
            "a https://x.invalid/?k=<redacted> b \"https://<redacted>@y.invalid\" c"
        );
    }

    #[test]
    fn shared_error_helpers_redact_their_debug_text() {
        let url = "https://discord.invalid/api/webhooks/1/secret";
        for error in [
            http_failure(500, None, Credential::ApiKey, format!("POST {url}")),
            rejected_credential(Credential::ApiKey, format!("POST {url}")),
        ] {
            let debug = error.debug_message.unwrap();
            assert!(!debug.contains("secret"), "{debug}");
            assert!(
                debug.contains("/api/webhooks/<redacted>/<redacted>"),
                "{debug}"
            );
        }
        let failed = error_response(format!("request failed for {url}"), None);
        assert!(!failed.error.unwrap().contains("secret"));
    }

    #[test]
    fn general_events_include_title_moved() {
        assert!(general_notification_events().contains(&NotificationEventType::TitleMoved));
    }

    #[test]
    fn title_move_prefers_paths_over_library_names() {
        let req = title_moved(serde_json::json!({
            "source_library_name": "Library A",
            "destination_library_name": "Library B",
            "source_path": "/media/a/Example Title",
            "destination_path": "/media/b/Example Title",
        }));
        assert_eq!(
            title_move_source(&req).as_deref(),
            Some("/media/a/Example Title")
        );
        assert_eq!(
            title_move_destination(&req).as_deref(),
            Some("/media/b/Example Title")
        );
        assert_eq!(title_move_warning(&req), None);
    }

    #[test]
    fn title_move_falls_back_to_library_names_and_reports_warnings() {
        let req = title_moved(serde_json::json!({
            "source_library_name": "Library A",
            "destination_library_name": "Library B",
            "source_path": "  ",
            "completed_with_warnings": true,
        }));
        assert_eq!(title_move_source(&req).as_deref(), Some("Library A"));
        assert_eq!(title_move_destination(&req).as_deref(), Some("Library B"));
        assert_eq!(
            title_move_warning(&req).as_deref(),
            Some("Completed with warnings")
        );

        let detailed = title_moved(serde_json::json!({
            "completed_with_warnings": true,
            "detail": "1 subtitle file was left behind",
        }));
        assert_eq!(
            title_move_warning(&detailed).as_deref(),
            Some("1 subtitle file was left behind")
        );
    }

    #[test]
    fn title_move_helpers_are_empty_without_the_block() {
        let mut req = title_moved(serde_json::json!({}));
        req.title_move = None;
        assert_eq!(title_move_source(&req), None);
        assert_eq!(title_move_destination(&req), None);
        assert_eq!(title_move_warning(&req), None);
    }
}
