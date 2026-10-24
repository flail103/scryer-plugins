//! Reporting a failure to Scryer.
//!
//! Every plugin operation ends in a [`PluginResult`]. When it fails, the
//! [`PluginError`] it carries is what the operator sees: its `code` becomes a
//! stable reason the web client translates, its `public_message` is the one
//! sentence shown beside it, and its `debug_message` goes to the host log only.
//! This module is the one way to build that error. A plugin never writes a
//! `PluginError { .. }` literal:
//!
//! ```
//! use scryer_plugin_pdk::error::{codes, plugin_error};
//! use scryer_plugin_pdk::FnResult;
//!
//! fn login(status: u16) -> FnResult<()> {
//!     if status == 403 {
//!         return plugin_error(codes::INVALID_USERNAME_PASSWORD, "the service rejected the username or password")
//!             .debug(format_args!("login answered HTTP {status}"))
//!             .err();
//!     }
//!     Ok(())
//! }
//! # assert!(login(403).is_err());
//! ```
//!
//! # Choosing a code
//!
//! A code says *what kind* of failure this is; it is API once a released
//! plugin sends it, so keep its meaning and spelling. Prefer the most specific
//! code that is true:
//!
//! * A shared open code from [`codes`] when one names the failure exactly —
//!   [`codes::INVALID_API_KEY`] rather than `AuthFailed` when the service said
//!   the API key was wrong.
//! * Otherwise one of the seven well-known [`PluginErrorCode`] variants, which
//!   the host acts on:
//!   * `InvalidConfig` — a setting is missing or unusable. A missing required
//!     setting is `InvalidConfig`, not a code of its own.
//!   * `AuthFailed` — the service rejected the credentials and the plugin
//!     cannot tell which one.
//!   * `RateLimited` — the service asked the plugin to slow down (HTTP 429).
//!     Set [`PluginErrorBuilder::retry_after_seconds`] when the service said
//!     how long.
//!   * `UpstreamUnavailable` — the service could not be reached: connection
//!     refused, DNS failure, HTTP 5xx.
//!   * `Unsupported` — the plugin does not offer what was asked.
//!   * `Temporary` — anything the next attempt will likely clear.
//!   * `Permanent` — anything the next attempt will not clear.
//!
//! Do not invent a plugin-private code when a shared one fits: the web client
//! only translates codes it knows, and the shared spellings in [`codes`] are
//! the ones it is taught.
//!
//! # Host compatibility
//!
//! Scryer hosts older than 0.21.21 cannot decode a code outside the seven
//! well-known ones. A plugin that sends any code from [`codes`] (or any other
//! open code) must declare `min_scryer_version = "0.21.21"` in its
//! `[package.metadata.scryer]`.
//!
//! # Errors a plugin did not classify
//!
//! A handler may still return a bare `anyhow` error. [`to_plugin_result`]
//! classifies it from the PDK's own error types anywhere in its chain — the
//! host HTTP and host-service errors, and [`HttpStatusError`] — and otherwise
//! reports `Temporary` with a neutral sentence. The full error text only ever
//! reaches `debug_message`. See [`plugin_error_from`] for the table.
//!
//! Turn a non-success HTTP status into an error with [`check_http_status`] or
//! [`crate::HttpResponse::error_for_status`] so that table can map 429 and 5xx
//! answers for you.

use std::fmt;

use scryer_plugin_sdk::net::{SocketError, SocketErrorCode};
pub use scryer_plugin_sdk::{PluginError, PluginErrorCode, PluginErrorDetails, PluginResult};

use crate::host::{HostCallError, HostTransportError};
use crate::runtime::HostError;

/// Shared spellings of the open error codes.
///
/// Each constant is the wire string; pass it wherever an
/// `impl Into<PluginErrorCode>` is accepted, or compare with
/// `error.code == PluginErrorCode::from(codes::INVALID_API_KEY)`.
///
/// Sending any of these requires `min_scryer_version = "0.21.21"`; see the
/// [module documentation](self) for when to use a well-known code instead.
pub mod codes {
    /// The service rejected the configured username or password.
    pub const INVALID_USERNAME_PASSWORD: &str = "invalid_username_password";
    /// The service rejected the configured API key.
    pub const INVALID_API_KEY: &str = "invalid_api_key";
    /// The service rejected the configured passkey.
    pub const INVALID_PASSKEY: &str = "invalid_passkey";
    /// A configured cookie was rejected or has expired.
    pub const INVALID_COOKIE: &str = "invalid_cookie";
    /// The service demands a CAPTCHA before it will answer.
    pub const CAPTCHA_REQUIRED: &str = "captcha_required";
    /// The service demands a second authentication factor.
    pub const TWO_FACTOR_REQUIRED: &str = "two_factor_required";
    /// The service says the account is banned.
    pub const ACCOUNT_BANNED: &str = "account_banned";
    /// The service says the account is disabled, suspended or not yet activated.
    pub const ACCOUNT_DISABLED: &str = "account_disabled";
    /// The configured URL answers, but not with this API (for example HTTP 404
    /// on the API root).
    pub const ENDPOINT_NOT_FOUND: &str = "endpoint_not_found";
    /// A secure (TLS) connection to the service could not be established.
    pub const TLS_ERROR: &str = "tls_error";
    /// The service answered with something this plugin cannot parse.
    pub const INVALID_RESPONSE: &str = "invalid_response";
    /// A daily or monthly allowance is used up; distinct from rate limiting.
    pub const QUOTA_EXCEEDED: &str = "quota_exceeded";
    /// The referenced download, torrent or list no longer exists on the service.
    pub const ITEM_NOT_FOUND: &str = "item_not_found";
    /// The client refused to add the item.
    pub const DOWNLOAD_REJECTED: &str = "download_rejected";
    /// The client cannot write to the configured or requested path.
    pub const PATH_NOT_WRITABLE: &str = "path_not_writable";

    /// Every shared code above, in declaration order.
    pub const ALL: &[&str] = &[
        INVALID_USERNAME_PASSWORD,
        INVALID_API_KEY,
        INVALID_PASSKEY,
        INVALID_COOKIE,
        CAPTCHA_REQUIRED,
        TWO_FACTOR_REQUIRED,
        ACCOUNT_BANNED,
        ACCOUNT_DISABLED,
        ENDPOINT_NOT_FOUND,
        TLS_ERROR,
        INVALID_RESPONSE,
        QUOTA_EXCEEDED,
        ITEM_NOT_FOUND,
        DOWNLOAD_REJECTED,
        PATH_NOT_WRITABLE,
    ];
}

/// Start a [`PluginError`].
///
/// `public_message` is shown to the operator: one sentence, no secrets, no
/// URLs with credentials. Put anything else in [`PluginErrorBuilder::debug`].
pub fn plugin_error(
    code: impl Into<PluginErrorCode>,
    public_message: impl Into<String>,
) -> PluginErrorBuilder {
    PluginErrorBuilder {
        error: PluginError {
            code: code.into(),
            public_message: public_message.into(),
            debug_message: None,
            retry_after_seconds: None,
            details: None,
        },
    }
}

/// A [`PluginError`] under construction; see [`plugin_error`].
#[derive(Clone, Debug)]
#[must_use = "a plugin error does nothing until it is returned"]
pub struct PluginErrorBuilder {
    error: PluginError,
}

impl PluginErrorBuilder {
    /// Detail for the host log only. Never shown to the operator.
    ///
    /// Passed through as given: run text that may quote a request URL (a
    /// transport error, an upstream body) through [`redact_secrets`] first.
    pub fn debug(mut self, message: impl fmt::Display) -> Self {
        self.error.debug_message = Some(message.to_string());
        self
    }

    /// How long the host should wait before trying again.
    pub fn retry_after_seconds(mut self, seconds: i64) -> Self {
        self.error.retry_after_seconds = Some(seconds);
        self
    }

    /// Family-specific structured detail, such as an indexer search outcome.
    pub fn details(mut self, details: PluginErrorDetails) -> Self {
        self.error.details = Some(details);
        self
    }

    /// The finished error, for a handler that returns a [`PluginResult`].
    pub fn build(self) -> PluginError {
        self.error
    }

    /// The finished error as a [`crate::Error`] that [`to_plugin_result`]
    /// delivers unchanged, for a handler that returns [`crate::FnResult`].
    pub fn into_error(self) -> crate::Error {
        structured_plugin_error(self.error)
    }

    /// `Err(self.into_error())`, so a handler can `return plugin_error(..).err();`.
    pub fn err<T>(self) -> Result<T, crate::Error> {
        Err(self.into_error())
    }
}

impl From<PluginErrorBuilder> for PluginError {
    fn from(builder: PluginErrorBuilder) -> Self {
        builder.build()
    }
}

impl From<PluginErrorBuilder> for crate::Error {
    fn from(builder: PluginErrorBuilder) -> Self {
        builder.into_error()
    }
}

/// Classify any error as a [`PluginError`] with the given code and sentence.
///
/// The original error becomes the `debug_message`:
///
/// ```
/// use scryer_plugin_pdk::error::{codes, PluginResultExt};
/// use scryer_plugin_pdk::FnResult;
///
/// fn parse(body: &str) -> FnResult<serde_json::Value> {
///     serde_json::from_str(body)
///         .or_plugin_error(codes::INVALID_RESPONSE, "the service answered with something unreadable")
/// }
/// # assert!(parse("{").is_err());
/// ```
pub trait PluginResultExt<T> {
    fn or_plugin_error(
        self,
        code: impl Into<PluginErrorCode>,
        public_message: impl Into<String>,
    ) -> Result<T, crate::Error>;
}

impl<T, E: fmt::Display> PluginResultExt<T> for Result<T, E> {
    fn or_plugin_error(
        self,
        code: impl Into<PluginErrorCode>,
        public_message: impl Into<String>,
    ) -> Result<T, crate::Error> {
        self.map_err(|error| plugin_error(code, public_message).debug(error).into_error())
    }
}

/// A [`PluginError`] carried inside a [`crate::Error`], so that
/// [`to_plugin_result`] delivers it unchanged.
#[derive(Debug)]
pub struct StructuredPluginError(PluginError);

impl StructuredPluginError {
    pub fn plugin_error(&self) -> &PluginError {
        &self.0
    }
}

impl fmt::Display for StructuredPluginError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0.public_message)
    }
}

impl std::error::Error for StructuredPluginError {}

/// Wrap an explicit [`PluginError`] as a [`crate::Error`].
pub fn structured_plugin_error(error: PluginError) -> crate::Error {
    crate::Error::new(StructuredPluginError(error))
}

/// A non-success HTTP status from the service, as an error the fallback
/// classifies: 429 becomes `RateLimited` (with the `Retry-After` delay when
/// the service sent one in seconds), 401 becomes `AuthFailed` and 5xx becomes
/// `UpstreamUnavailable`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpStatusError {
    pub status: u16,
    pub retry_after_seconds: Option<i64>,
}

impl HttpStatusError {
    /// An error for `status`, reading `retry_after` as a `Retry-After` value.
    ///
    /// Only the delta-seconds form is understood; an HTTP-date is ignored, as
    /// the guest has no wall clock it could trust to subtract from it.
    pub fn new(status: u16, retry_after: Option<&str>) -> Self {
        Self {
            status,
            // Delta-seconds cannot be negative; a malformed `-1` is no delay at all.
            retry_after_seconds: retry_after
                .and_then(|value| value.trim().parse::<i64>().ok())
                .filter(|seconds| *seconds >= 0),
        }
    }
}

impl fmt::Display for HttpStatusError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "the service answered HTTP {}", self.status)
    }
}

impl std::error::Error for HttpStatusError {}

/// `Ok(())` for a status below 400, else an [`HttpStatusError`].
///
/// `retry_after` is the response's `Retry-After` header, if any.
pub fn check_http_status(status: u16, retry_after: Option<&str>) -> Result<(), crate::Error> {
    if status < 400 {
        return Ok(());
    }
    Err(crate::Error::new(HttpStatusError::new(status, retry_after)))
}

/// Public sentences of the fallback. Neutral on purpose: they say what failed,
/// never with what.
const REQUEST_FAILED: &str = "the request to the service failed";
const UNREACHABLE: &str = "the service could not be reached";
const TIMED_OUT: &str = "the request to the service timed out";
const TLS_FAILED: &str = "a secure connection to the service could not be established";
const RATE_LIMITED: &str = "the service is rate limiting requests";
const SERVER_ERROR: &str = "the service answered with a server error";
const AUTH_REJECTED: &str = "the service rejected the credentials";
const UNCLASSIFIED: &str = "the plugin could not complete the request";

/// Classify an error a handler returned without a [`PluginError`] of its own.
///
/// The first PDK error type found walking the chain (outermost first) decides:
///
/// | Error in the chain | Code | Public sentence |
/// |---|---|---|
/// | [`StructuredPluginError`] (from [`plugin_error`]) | its own | its own |
/// | [`HttpStatusError`] 429 | `RateLimited`, with `Retry-After` | rate limiting |
/// | [`HttpStatusError`] 401 | `AuthFailed` | rejected the credentials |
/// | [`HttpStatusError`] 5xx | `UpstreamUnavailable` | server error |
/// | [`HttpStatusError`] other | `Temporary` | request failed |
/// | [`HostError::Transport`] | `UpstreamUnavailable` | could not be reached |
/// | [`HostError::Timeout`] | `Temporary` | timed out |
/// | other [`HostError`] | `Temporary` | request failed |
/// | [`HostCallError::Service`] carrying a socket TLS / STARTTLS failure | [`codes::TLS_ERROR`] | secure connection |
/// | [`HostCallError::Service`] carrying a socket DNS / I/O / remote-closed failure | `UpstreamUnavailable` | could not be reached |
/// | [`HostCallError::Service`] carrying a socket connect timeout | `Temporary` | timed out |
/// | [`HostCallError::Service`] carrying a socket auth failure | `AuthFailed` | request failed |
/// | [`HostCallError::Service`], `Temporary`, host detail names TLS / a certificate | [`codes::TLS_ERROR`] | secure connection |
/// | [`HostCallError::Service`], `Temporary`, host detail names a timeout | `Temporary` | timed out |
/// | [`HostCallError::Service`], `Temporary`, host detail names DNS / connecting | `UpstreamUnavailable` | could not be reached |
/// | [`HostCallError::Service`] with any other code | the host's error, unchanged | the host's |
/// | any other [`HostCallError`], [`HostTransportError`] | `Temporary` | request failed |
/// | anything else | `Temporary` | could not complete the request |
///
/// The whole error, chain included, goes to `debug_message` and nowhere else,
/// with any URL secrets in it replaced by [`redact_secrets`].
pub fn plugin_error_from(error: &crate::Error) -> PluginError {
    let debug = redact_secrets(&format!("{error:#}"));
    for cause in error.chain() {
        if let Some(structured) = cause.downcast_ref::<StructuredPluginError>() {
            return structured.0.clone();
        }
        if let Some(status) = cause.downcast_ref::<HttpStatusError>() {
            return from_http_status(status).debug(debug).build();
        }
        if let Some(host) = cause.downcast_ref::<HostError>() {
            return from_host_error(*host).debug(debug).build();
        }
        if let Some(host) = cause.downcast_ref::<HostCallError>() {
            return match host {
                HostCallError::Service(service) => from_host_service(service, debug),
                _ => plugin_error(PluginErrorCode::Temporary, REQUEST_FAILED)
                    .debug(debug)
                    .build(),
            };
        }
        if cause.downcast_ref::<HostTransportError>().is_some() {
            return plugin_error(PluginErrorCode::Temporary, REQUEST_FAILED)
                .debug(debug)
                .build();
        }
    }
    plugin_error(PluginErrorCode::Temporary, UNCLASSIFIED)
        .debug(debug)
        .build()
}

/// Convert a handler's outcome into the SDK's typed result, classifying any
/// error with [`plugin_error_from`].
pub fn to_plugin_result<T>(result: Result<T, crate::Error>) -> PluginResult<T> {
    match result {
        Ok(value) => PluginResult::Ok(value),
        Err(error) => PluginResult::Err(plugin_error_from(&error)),
    }
}

fn from_http_status(status: &HttpStatusError) -> PluginErrorBuilder {
    match status.status {
        429 => {
            let builder = plugin_error(PluginErrorCode::RateLimited, RATE_LIMITED);
            match status.retry_after_seconds {
                Some(seconds) => builder.retry_after_seconds(seconds),
                None => builder,
            }
        }
        401 => plugin_error(PluginErrorCode::AuthFailed, AUTH_REJECTED),
        500..=599 => plugin_error(PluginErrorCode::UpstreamUnavailable, SERVER_ERROR),
        _ => plugin_error(PluginErrorCode::Temporary, REQUEST_FAILED),
    }
}

fn from_host_error(error: HostError) -> PluginErrorBuilder {
    match error {
        HostError::Transport => plugin_error(PluginErrorCode::UpstreamUnavailable, UNREACHABLE),
        HostError::Timeout => plugin_error(PluginErrorCode::Temporary, TIMED_OUT),
        HostError::InvalidRequest
        | HostError::ForbiddenOrigin
        | HostError::Cancelled
        | HostError::ResponseTooLarge
        | HostError::Capacity => plugin_error(PluginErrorCode::Temporary, REQUEST_FAILED),
    }
}

fn from_host_service(service: &PluginError, debug: String) -> PluginError {
    // `HostCallError`'s Display carries only the host's public sentence; the
    // transport detail the host logged beside it belongs in our debug too.
    let debug = match service.debug_message.as_deref() {
        Some(detail) if !detail.is_empty() => format!("{debug} ({})", redact_secrets(detail)),
        _ => debug,
    };
    // The socket door carries its own discriminant as JSON in the host's
    // debug message; it is exact, so it wins over every heuristic below.
    if let Some(socket) = service
        .debug_message
        .as_deref()
        .and_then(|message| serde_json::from_str::<SocketError>(message).ok())
    {
        let builder = match socket.code {
            SocketErrorCode::TlsVerificationFailed | SocketErrorCode::StartTlsFailed => {
                Some(plugin_error(codes::TLS_ERROR, TLS_FAILED))
            }
            SocketErrorCode::DnsFailed
            | SocketErrorCode::IoFailed
            | SocketErrorCode::RemoteClosed => Some(plugin_error(
                PluginErrorCode::UpstreamUnavailable,
                UNREACHABLE,
            )),
            SocketErrorCode::ConnectTimeout => {
                Some(plugin_error(PluginErrorCode::Temporary, TIMED_OUT))
            }
            SocketErrorCode::AuthFailed => {
                Some(plugin_error(PluginErrorCode::AuthFailed, REQUEST_FAILED))
            }
            SocketErrorCode::PermissionDenied
            | SocketErrorCode::ProtocolError
            | SocketErrorCode::Unsupported => None,
        };
        if let Some(builder) = builder {
            return builder.debug(debug).build();
        }
    }

    // A host that chose a specific code (an archive service with no
    // extractor, a denied socket) already said what happened; keep it.
    if service.code != PluginErrorCode::Temporary {
        return service.clone();
    }

    // The host's HTTP service reports every failure as `Temporary` with the
    // transport's own text as detail. Read that text for the few kinds an
    // operator can act on; anything unrecognised stays a plain failure.
    let detail = service
        .debug_message
        .as_deref()
        .unwrap_or_default()
        .to_ascii_lowercase();
    let builder = if ["certificate", "tls", "ssl", "handshake"]
        .iter()
        .any(|needle| detail.contains(needle))
    {
        plugin_error(codes::TLS_ERROR, TLS_FAILED)
    } else if ["timed out", "timeout"]
        .iter()
        .any(|needle| detail.contains(needle))
    {
        plugin_error(PluginErrorCode::Temporary, TIMED_OUT)
    } else if ["dns", "resolve", "connection refused", "connect"]
        .iter()
        .any(|needle| detail.contains(needle))
    {
        plugin_error(PluginErrorCode::UpstreamUnavailable, UNREACHABLE)
    } else {
        plugin_error(PluginErrorCode::Temporary, REQUEST_FAILED)
    };
    builder.debug(debug).build()
}

// ---------------------------------------------------------------------------
// Redacting secrets from debug text
// ---------------------------------------------------------------------------

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
    use std::collections::BTreeSet;

    use anyhow::Context as _;

    use super::*;

    fn classify(error: crate::Error) -> PluginError {
        plugin_error_from(&error)
    }

    fn host_service(code: PluginErrorCode, debug: Option<&str>) -> crate::Error {
        crate::Error::new(HostCallError::Service(PluginError {
            code,
            public_message: "command plugin host service failed".to_string(),
            debug_message: debug.map(str::to_string),
            retry_after_seconds: Some(0),
            details: None,
        }))
    }

    fn socket_service(code: SocketErrorCode) -> crate::Error {
        let socket = SocketError::new(code, "socket detail");
        host_service(
            PluginErrorCode::Permanent,
            Some(&serde_json::to_string(&socket).unwrap()),
        )
    }

    #[test]
    fn builder_sets_every_field() {
        let error = plugin_error(codes::INVALID_API_KEY, "the service rejected the API key")
            .debug("HTTP 401 from /api")
            .retry_after_seconds(30)
            .details(PluginErrorDetails::IndexerSearch(
                scryer_plugin_sdk::IndexerSearchPluginError::Deferred {
                    reason: scryer_plugin_sdk::IndexerSearchIncompleteReason::RateLimited,
                    retry_after_seconds: Some(30),
                },
            ))
            .build();
        assert_eq!(error.code, PluginErrorCode::from("invalid_api_key"));
        assert_eq!(error.public_message, "the service rejected the API key");
        assert_eq!(error.debug_message.as_deref(), Some("HTTP 401 from /api"));
        assert_eq!(error.retry_after_seconds, Some(30));
        assert!(error.details.is_some());
    }

    #[test]
    fn builder_defaults_leave_optional_fields_unset() {
        let error: PluginError = plugin_error(PluginErrorCode::InvalidConfig, "missing").into();
        assert_eq!(error.code, PluginErrorCode::InvalidConfig);
        assert!(error.debug_message.is_none());
        assert!(error.retry_after_seconds.is_none());
        assert!(error.details.is_none());
    }

    #[test]
    fn a_built_error_survives_the_fallback_unchanged_even_under_context() {
        let built = plugin_error(codes::ACCOUNT_BANNED, "the account is banned").debug("detail");
        let via_err: Result<(), crate::Error> = built.clone().err();
        let wrapped = via_err.context("while logging in").unwrap_err();
        let error = classify(wrapped);
        assert_eq!(error.code, PluginErrorCode::from(codes::ACCOUNT_BANNED));
        assert_eq!(error.public_message, "the account is banned");
        assert_eq!(error.debug_message.as_deref(), Some("detail"));

        let converted: crate::Error = built.into();
        assert_eq!(classify(converted).public_message, "the account is banned");
    }

    #[test]
    fn or_plugin_error_keeps_the_original_as_debug() {
        let result: Result<(), &str> = Err("unexpected token at 3");
        let error = classify(
            result
                .or_plugin_error(codes::INVALID_RESPONSE, "unreadable answer")
                .unwrap_err(),
        );
        assert_eq!(error.code, PluginErrorCode::from(codes::INVALID_RESPONSE));
        assert_eq!(error.public_message, "unreadable answer");
        assert_eq!(
            error.debug_message.as_deref(),
            Some("unexpected token at 3")
        );
    }

    #[test]
    fn http_429_is_rate_limited_with_retry_after() {
        let error = classify(check_http_status(429, Some(" 120 ")).unwrap_err());
        assert_eq!(error.code, PluginErrorCode::RateLimited);
        assert_eq!(error.retry_after_seconds, Some(120));
        assert_eq!(error.public_message, RATE_LIMITED);

        let error =
            classify(check_http_status(429, Some("Wed, 21 Oct 2026 07:28:00 GMT")).unwrap_err());
        assert_eq!(error.code, PluginErrorCode::RateLimited);
        assert_eq!(error.retry_after_seconds, None);

        let error = classify(check_http_status(429, Some("-1")).unwrap_err());
        assert_eq!(error.code, PluginErrorCode::RateLimited);
        assert_eq!(error.retry_after_seconds, None);
    }

    #[test]
    fn http_5xx_is_upstream_unavailable() {
        for status in [500, 502, 503, 599] {
            let error = classify(check_http_status(status, None).unwrap_err());
            assert_eq!(error.code, PluginErrorCode::UpstreamUnavailable, "{status}");
            assert_eq!(error.public_message, SERVER_ERROR);
        }
    }

    #[test]
    fn http_401_is_auth_failed() {
        let error = classify(check_http_status(401, None).unwrap_err());
        assert_eq!(error.code, PluginErrorCode::AuthFailed);
        assert_eq!(error.public_message, AUTH_REJECTED);
    }

    #[test]
    fn other_http_errors_are_temporary_and_success_is_ok() {
        let error = classify(check_http_status(404, None).unwrap_err());
        assert_eq!(error.code, PluginErrorCode::Temporary);
        assert_eq!(error.public_message, REQUEST_FAILED);
        assert!(check_http_status(200, None).is_ok());
        assert!(check_http_status(304, None).is_ok());
    }

    #[test]
    fn runtime_host_errors_map_by_kind() {
        let cases = [
            (
                HostError::Transport,
                PluginErrorCode::UpstreamUnavailable,
                UNREACHABLE,
            ),
            (HostError::Timeout, PluginErrorCode::Temporary, TIMED_OUT),
            (
                HostError::InvalidRequest,
                PluginErrorCode::Temporary,
                REQUEST_FAILED,
            ),
            (
                HostError::ForbiddenOrigin,
                PluginErrorCode::Temporary,
                REQUEST_FAILED,
            ),
            (
                HostError::Cancelled,
                PluginErrorCode::Temporary,
                REQUEST_FAILED,
            ),
            (
                HostError::ResponseTooLarge,
                PluginErrorCode::Temporary,
                REQUEST_FAILED,
            ),
            (
                HostError::Capacity,
                PluginErrorCode::Temporary,
                REQUEST_FAILED,
            ),
        ];
        for (host, code, message) in cases {
            let error = classify(crate::Error::new(host).context("searching"));
            assert_eq!(error.code, code, "{host:?}");
            assert_eq!(error.public_message, message, "{host:?}");
            let debug = error.debug_message.unwrap();
            assert!(debug.contains("searching") && debug.contains(&host.to_string()));
        }
    }

    #[test]
    fn socket_failures_map_from_the_host_discriminant() {
        let cases = [
            (
                SocketErrorCode::TlsVerificationFailed,
                PluginErrorCode::from(codes::TLS_ERROR),
            ),
            (
                SocketErrorCode::StartTlsFailed,
                PluginErrorCode::from(codes::TLS_ERROR),
            ),
            (
                SocketErrorCode::DnsFailed,
                PluginErrorCode::UpstreamUnavailable,
            ),
            (
                SocketErrorCode::IoFailed,
                PluginErrorCode::UpstreamUnavailable,
            ),
            (
                SocketErrorCode::RemoteClosed,
                PluginErrorCode::UpstreamUnavailable,
            ),
            (SocketErrorCode::ConnectTimeout, PluginErrorCode::Temporary),
            (SocketErrorCode::AuthFailed, PluginErrorCode::AuthFailed),
            // No mapping of its own: the host's code stands.
            (
                SocketErrorCode::PermissionDenied,
                PluginErrorCode::Permanent,
            ),
            (SocketErrorCode::ProtocolError, PluginErrorCode::Permanent),
        ];
        for (socket, code) in cases {
            assert_eq!(classify(socket_service(socket)).code, code, "{socket:?}");
        }
    }

    #[test]
    fn http_service_failures_map_from_the_host_detail() {
        let cases = [
            (
                "invalid peer certificate: UnknownIssuer",
                PluginErrorCode::from(codes::TLS_ERROR),
                TLS_FAILED,
            ),
            ("operation timed out", PluginErrorCode::Temporary, TIMED_OUT),
            (
                "dns error: failed to lookup address",
                PluginErrorCode::UpstreamUnavailable,
                UNREACHABLE,
            ),
            (
                "tcp connect error: Connection refused",
                PluginErrorCode::UpstreamUnavailable,
                UNREACHABLE,
            ),
            (
                "plugin HTTP worker stopped",
                PluginErrorCode::Temporary,
                REQUEST_FAILED,
            ),
        ];
        for (detail, code, message) in cases {
            let error = classify(host_service(PluginErrorCode::Temporary, Some(detail)));
            assert_eq!(error.code, code, "{detail}");
            assert_eq!(error.public_message, message, "{detail}");
            assert!(error.debug_message.unwrap().contains(detail));
        }
        let error = classify(host_service(PluginErrorCode::Temporary, None));
        assert_eq!(error.code, PluginErrorCode::Temporary);
        assert_eq!(error.public_message, REQUEST_FAILED);
    }

    #[test]
    fn a_host_service_error_with_a_specific_code_is_kept() {
        let error = classify(crate::Error::new(HostCallError::Service(PluginError {
            code: PluginErrorCode::Unsupported,
            public_message: "no archive extractor is installed".to_string(),
            debug_message: None,
            retry_after_seconds: None,
            details: None,
        })));
        assert_eq!(error.code, PluginErrorCode::Unsupported);
        assert_eq!(error.public_message, "no archive extractor is installed");
    }

    #[test]
    fn other_host_call_failures_are_temporary() {
        for host in [
            HostCallError::Unavailable,
            HostCallError::Transport(HostTransportError::Failed),
            HostCallError::ResponseTooLarge(1),
            HostCallError::UnexpectedResponse("http"),
        ] {
            let error = classify(crate::Error::new(host));
            assert_eq!(error.code, PluginErrorCode::Temporary);
            assert_eq!(error.public_message, REQUEST_FAILED);
        }
        let error = classify(crate::Error::new(HostTransportError::InvalidRequest));
        assert_eq!(error.public_message, REQUEST_FAILED);
    }

    #[test]
    fn an_unclassified_error_stays_temporary_and_keeps_its_text_in_debug_only() {
        let error = classify(anyhow::anyhow!("api key abc123 rejected").context("search"));
        assert_eq!(error.code, PluginErrorCode::Temporary);
        assert_eq!(error.public_message, UNCLASSIFIED);
        assert!(!error.public_message.contains("abc123"));
        assert_eq!(
            error.debug_message.as_deref(),
            Some("search: api key abc123 rejected")
        );
        assert!(error.retry_after_seconds.is_none());
    }

    #[test]
    fn to_plugin_result_passes_values_and_classifies_errors() {
        assert!(matches!(
            to_plugin_result(Ok::<_, crate::Error>(7)),
            PluginResult::Ok(7)
        ));
        let PluginResult::<()>::Err(error) =
            to_plugin_result(Err(crate::Error::new(HostError::Transport)))
        else {
            panic!("expected an error");
        };
        assert_eq!(error.code, PluginErrorCode::UpstreamUnavailable);
    }

    #[test]
    fn shared_codes_are_unique_snake_case_and_open() {
        let mut seen = BTreeSet::new();
        for code in codes::ALL {
            assert!(!code.is_empty());
            assert!(
                code.bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_'),
                "{code}"
            );
            assert!(!code.starts_with('_') && !code.ends_with('_') && !code.contains("__"));
            assert!(seen.insert(*code), "duplicate {code}");
            let parsed = PluginErrorCode::from(*code);
            assert!(!parsed.is_known(), "{code} collides with a well-known code");
            assert_eq!(parsed.as_str(), *code);
        }
        assert_eq!(seen.len(), 15);
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
    fn an_open_code_reaches_the_wire_as_its_bare_string() {
        let error = plugin_error(codes::INVALID_USERNAME_PASSWORD, "rejected").build();
        let json = serde_json::to_string(&PluginResult::<()>::Err(error)).unwrap();
        assert!(
            json.contains(r#""code":"invalid_username_password""#),
            "{json}"
        );
    }

    #[test]
    fn a_well_known_code_serialises_exactly_as_before() {
        let error = plugin_error(PluginErrorCode::Temporary, "failed").build();
        let json = serde_json::to_value(&error).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "code": "temporary",
                "public_message": "failed",
                "debug_message": null,
                "retry_after_seconds": null,
                "details": null,
            })
        );
    }
}
