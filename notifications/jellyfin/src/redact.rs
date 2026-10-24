//! Secrets out of debug text.
//!
//! A private copy of `scryer_plugin_pdk::error::redact_secrets` (PDK 0.8.1),
//! carried here because this channel builds against PDK 0.8.0 from crates.io
//! and does not link `notify-common`, which holds the shared copy. Keep the
//! copies in step until the channels take a PDK that exports it.

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
}
