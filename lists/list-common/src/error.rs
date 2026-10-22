//! Plugin errors shaped for the host's list failure classes.
//!
//! The host reads the error code, the public message and the retry hint:
//! `AuthFailed` asks the operator to fix the key (and expires a linked member
//! account), `RateLimited` pauses the provider for `retry_after_seconds`,
//! `UpstreamUnavailable` and `Temporary` retry on the next interval, and a
//! `Permanent` error whose message says "not found" marks the list as gone.
//! Every other code, including the shared open codes, is a plain failure.
//!
//! The host's list classes key on the well-known `AuthFailed` code and on a
//! `Permanent` "not found" sentence, so credential rejections and missing
//! lists keep those shapes rather than the open `invalid_api_key` and
//! `item_not_found` codes. Every error is built through the PDK builder; raw
//! upstream detail goes to `debug_message` only.

use std::fmt;

use scryer_plugin_pdk::error::codes;
use scryer_plugin_sdk::host::PluginHttpResponse;
use scryer_plugin_sdk::{PluginError, PluginErrorCode};

use crate::http::header;

/// Retry hint used when a 429 carries no usable `Retry-After`.
pub const DEFAULT_RETRY_AFTER_SECONDS: i64 = 300;

pub fn plugin_error(code: impl Into<PluginErrorCode>, message: impl Into<String>) -> PluginError {
    scryer_plugin_pdk::plugin_error(code, message).build()
}

/// Attach host-log-only detail to an error. Never shown to the operator.
pub fn with_debug(mut error: PluginError, debug: impl fmt::Display) -> PluginError {
    error.debug_message = Some(debug.to_string());
    error
}

/// The list, person, company or feed does not exist (or is not visible).
pub fn not_found(what: impl AsRef<str>) -> PluginError {
    plugin_error(
        PluginErrorCode::Permanent,
        format!("{} not found", what.as_ref()),
    )
}

pub fn rate_limited(retry_after_seconds: Option<i64>) -> PluginError {
    let seconds = retry_after_seconds
        .filter(|seconds| *seconds > 0)
        .unwrap_or(DEFAULT_RETRY_AFTER_SECONDS);
    scryer_plugin_pdk::plugin_error(
        PluginErrorCode::RateLimited,
        format!("rate limited by the provider; retry after {seconds} seconds"),
    )
    .retry_after_seconds(seconds)
    .build()
}

pub fn auth_failed(message: impl Into<String>) -> PluginError {
    plugin_error(PluginErrorCode::AuthFailed, message)
}

pub fn unavailable(message: impl Into<String>) -> PluginError {
    plugin_error(PluginErrorCode::UpstreamUnavailable, message)
}

pub fn invalid_config(message: impl Into<String>) -> PluginError {
    plugin_error(PluginErrorCode::InvalidConfig, message)
}

pub fn permanent(message: impl Into<String>) -> PluginError {
    plugin_error(PluginErrorCode::Permanent, message)
}

/// The provider answered with a body this plugin cannot read. `debug` says
/// what was wrong with it, for the host log.
pub fn invalid_response(debug: impl fmt::Display) -> PluginError {
    with_debug(
        plugin_error(
            codes::INVALID_RESPONSE,
            "the provider answered with something unreadable",
        ),
        debug,
    )
}

pub fn unsupported_source(source_type: &str) -> PluginError {
    plugin_error(
        PluginErrorCode::Unsupported,
        format!("unknown list source type {source_type}"),
    )
}

pub fn missing_param(key: &str) -> PluginError {
    invalid_config(format!("missing required list parameter {key}"))
}

/// How a provider authenticates, which decides what a 401 means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// The request carried the server API key, so a 401 is a bad key.
    ServerKey,
    /// The request was anonymous, so a 401 means the source is private.
    Public,
}

/// Parse a `Retry-After` header given in delta seconds. HTTP-date values
/// fall back to [`DEFAULT_RETRY_AFTER_SECONDS`].
pub fn retry_after_seconds(response: &PluginHttpResponse) -> Option<i64> {
    header(response, "retry-after")?.trim().parse::<i64>().ok()
}

/// Map a non-success status onto the error the host expects. `what` names
/// the thing that was requested, for the not-found message.
pub fn check_status(
    response: &PluginHttpResponse,
    access: Access,
    what: &str,
) -> Result<(), PluginError> {
    let status = response.status;
    let error = match status {
        200..=299 => return Ok(()),
        401 if access == Access::ServerKey => auth_failed("the provider rejected the API key"),
        401 | 403 => not_found(format!("{what} (private or unavailable)")),
        404 | 410 => not_found(what),
        429 => rate_limited(retry_after_seconds(response)),
        408 | 500..=599 => unavailable("the provider is unavailable"),
        _ => permanent("the provider answered with an unexpected HTTP status"),
    };
    Err(with_debug(error, format_args!("{what}: HTTP {status}")))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn response(status: u16, headers: &[(&str, &str)]) -> PluginHttpResponse {
        PluginHttpResponse {
            status,
            headers: headers
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect::<BTreeMap<_, _>>(),
            body: Vec::new(),
        }
    }

    #[test]
    fn statuses_map_to_host_failure_classes() {
        let code = |status, access| {
            check_status(&response(status, &[]), access, "list 7")
                .unwrap_err()
                .code
        };
        assert_eq!(code(401, Access::ServerKey), PluginErrorCode::AuthFailed);
        assert_eq!(code(401, Access::Public), PluginErrorCode::Permanent);
        assert_eq!(code(403, Access::ServerKey), PluginErrorCode::Permanent);
        assert_eq!(code(404, Access::Public), PluginErrorCode::Permanent);
        assert_eq!(code(429, Access::Public), PluginErrorCode::RateLimited);
        assert_eq!(
            code(503, Access::Public),
            PluginErrorCode::UpstreamUnavailable
        );
        assert_eq!(code(418, Access::Public), PluginErrorCode::Permanent);
        assert!(check_status(&response(204, &[]), Access::Public, "x").is_ok());
    }

    #[test]
    fn not_found_messages_are_recognised_by_the_host() {
        for status in [403, 404, 410] {
            let error = check_status(&response(status, &[]), Access::Public, "list 7").unwrap_err();
            assert!(error.public_message.contains("not found"), "{status}");
        }
        let unexpected = check_status(&response(418, &[]), Access::Public, "x").unwrap_err();
        assert!(!unexpected.public_message.contains("not found"));
    }

    #[test]
    fn rate_limit_carries_retry_after() {
        let error = check_status(
            &response(429, &[("Retry-After", "42")]),
            Access::ServerKey,
            "x",
        )
        .unwrap_err();
        assert_eq!(error.retry_after_seconds, Some(42));

        let dated = check_status(
            &response(429, &[("retry-after", "Wed, 21 Oct 2037 07:28:00 GMT")]),
            Access::ServerKey,
            "x",
        )
        .unwrap_err();
        assert_eq!(dated.retry_after_seconds, Some(DEFAULT_RETRY_AFTER_SECONDS));
    }

    #[test]
    fn the_status_goes_to_debug_and_stays_out_of_the_public_message() {
        let error = check_status(&response(401, &[]), Access::ServerKey, "list 7").unwrap_err();
        assert_eq!(error.code, PluginErrorCode::AuthFailed);
        assert!(!error.public_message.contains("401"));
        assert_eq!(error.debug_message.as_deref(), Some("list 7: HTTP 401"));
    }

    #[test]
    fn unreadable_bodies_carry_the_shared_code() {
        let error = invalid_response("expected value at line 1");
        assert_eq!(error.code, PluginErrorCode::from(codes::INVALID_RESPONSE));
        assert!(!error.public_message.contains("line 1"));
        assert_eq!(
            error.debug_message.as_deref(),
            Some("expected value at line 1")
        );
    }
}
