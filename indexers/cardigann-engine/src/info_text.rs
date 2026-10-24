//! Read-only notes a definition carries as `info` settings.
//!
//! Cardigann writes these as HTML for Prowlarr's form. Scryer shows them as
//! plain text, so the markup is reduced to line breaks, list bullets and
//! `text (url)` links. The `info_*` variants that Prowlarr fills with its own
//! canned copy get Scryer's wording instead, because Prowlarr's describes
//! Prowlarr's settings.

/// The label and text for an informational setting, or `None` when the
/// setting type is not informational or leaves nothing to show.
pub(crate) fn info_note(
    setting_type: &str,
    label: Option<&str>,
    default_text: Option<&str>,
) -> Option<(String, String)> {
    let canned = |label: &str, text: &str| Some((label.to_string(), text.to_string()));
    match setting_type {
        "info_cookie" => canned(
            "How to get the cookie",
            "Sign in to the tracker in a browser, open the browser's developer tools, \
             and copy the Cookie header of a request to the tracker into the Cookie field. \
             Keep using that browser's user agent for this indexer.",
        ),
        "info_useragent" => canned(
            "User agent",
            "The tracker ties its session to the browser that signed in. \
             Use the user agent of the browser the cookie came from.",
        ),
        "info_flaresolverr" => canned(
            "Challenge protection",
            "This tracker can sit behind a browser challenge. \
             If searches fail with a challenge, assign a challenge-solver proxy to this indexer.",
        ),
        "info_category_8000" => canned(
            "Unmapped categories",
            "Releases in tracker categories the definition does not map are reported \
             as category 8000 (Other).",
        ),
        "info" => {
            let label = label.map(plain_text).unwrap_or_default();
            let text = default_text.map(plain_text).unwrap_or_default();
            match (label.is_empty(), text.is_empty()) {
                (true, true) => None,
                (true, false) => Some(("Note".to_string(), text)),
                (false, _) => Some((label, text)),
            }
        }
        _ => None,
    }
}

/// HTML reduced to readable plain text.
pub(crate) fn plain_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let mut rest = html;
    let mut href: Option<String> = None;
    while let Some(start) = rest.find('<') {
        out.push_str(&decode_entities(&rest[..start]));
        let Some(end) = rest[start..].find('>') else {
            // An unclosed `<` is text, not a tag.
            out.push_str(&decode_entities(&rest[start..]));
            rest = "";
            break;
        };
        let tag = &rest[start + 1..start + end];
        rest = &rest[start + end + 1..];
        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        match (name.as_str(), closing) {
            ("br", _) | ("p" | "div" | "ul" | "ol", true) => out.push('\n'),
            ("li", false) => out.push_str("\n- "),
            ("a", false) => href = attribute(tag, "href"),
            ("a", true) => {
                if let Some(url) = href.take()
                    && !out.trim_end().ends_with(url.as_str())
                {
                    out.push_str(&format!(" ({url})"));
                }
            }
            _ => {}
        }
    }
    out.push_str(&decode_entities(rest));
    tidy_lines(&out)
}

fn attribute(tag: &str, name: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let at = lower.find(&format!("{name}="))? + name.len() + 1;
    let value = &tag[at..];
    let value = match value.chars().next()? {
        quote @ ('"' | '\'') => value[1..].split(quote).next()?,
        _ => value.split_whitespace().next()?,
    };
    let value = decode_entities(value.trim());
    (value.starts_with("http://") || value.starts_with("https://")).then_some(value)
}

fn decode_entities(text: &str) -> String {
    html_escape::decode_html_entities(text).into_owned()
}

/// Trims every line, collapses runs of spaces, and keeps at most one blank
/// line between paragraphs.
fn tidy_lines(text: &str) -> String {
    let mut lines: Vec<String> = Vec::new();
    for line in text.lines() {
        let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
        if line.is_empty() && lines.last().is_none_or(|last| last.is_empty()) {
            continue;
        }
        lines.push(line);
    }
    while lines.last().is_some_and(|last| last.is_empty()) {
        lines.pop();
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_becomes_plain_text() {
        assert_eq!(
            plain_text(
                "Search <b>only</b> the freeleech feed.<br>See <a href=\"https://wiki.example/faq\">the FAQ</a> &amp; rules.<ul><li>one</li><li>two</li></ul>"
            ),
            "Search only the freeleech feed.\nSee the FAQ (https://wiki.example/faq) & rules.\n- one\n- two"
        );
        // A link whose text is already the address is not repeated, and a
        // non-web link keeps only its text.
        assert_eq!(
            plain_text(
                "<a href='https://tracker.example'>https://tracker.example</a> <a href=\"javascript:x\">here</a>"
            ),
            "https://tracker.example here"
        );
        assert_eq!(plain_text("1 < 2 and <unclosed"), "1 < 2 and <unclosed");
    }

    #[test]
    fn info_settings_become_notes() {
        assert_eq!(
            info_note("info", Some("About"), Some("Line one<br><br><br>Line two")),
            Some(("About".to_string(), "Line one\n\nLine two".to_string()))
        );
        assert_eq!(
            info_note("info", None, Some("Text only")),
            Some(("Note".to_string(), "Text only".to_string()))
        );
        assert_eq!(info_note("info", Some(" "), Some("<br>")), None);
        let (label, text) = info_note("info_flaresolverr", Some("ignored"), None).unwrap();
        assert_eq!(label, "Challenge protection");
        assert!(text.contains("challenge-solver proxy"));
        for kind in ["info_cookie", "info_useragent", "info_category_8000"] {
            assert!(info_note(kind, None, None).is_some(), "{kind}");
        }
        assert_eq!(info_note("text", Some("Label"), Some("x")), None);
    }
}
