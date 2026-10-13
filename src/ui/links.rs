//! Finding links in message bodies, for showing them as clickable.

use std::ops::Range;

use gtk::glib;

const SCHEMES: &[&str] = &["https://", "http://"];
const WWW: &str = "www.";

#[derive(Debug, PartialEq, Eq)]
pub struct Link {
    /// Bytes of the body that are the link.
    pub range: Range<usize>,
    /// What it opens: the text itself, or `https://` added for `www.`.
    pub href: String,
}

/// The `http(s)://` URLs and `www.` addresses in `text`.
pub fn find(text: &str) -> Vec<Link> {
    let mut links = Vec::new();
    let mut pos = 0;
    while let Some(c) = text[pos..].chars().next() {
        match link_at(text, pos) {
            Some(link) => {
                pos = link.range.end;
                links.push(link);
            }
            None => pos += c.len_utf8(),
        }
    }
    links
}

/// Pango markup for `text`, with its links as `<a href>`.
pub fn markup(text: &str) -> String {
    let mut markup = String::new();
    let mut pos = 0;
    for link in find(text) {
        markup.push_str(&glib::markup_escape_text(&text[pos..link.range.start]));
        markup.push_str("<a href=\"");
        markup.push_str(&glib::markup_escape_text(&link.href));
        markup.push_str("\">");
        markup.push_str(&glib::markup_escape_text(&text[link.range.clone()]));
        markup.push_str("</a>");
        pos = link.range.end;
    }
    markup.push_str(&glib::markup_escape_text(&text[pos..]));
    markup
}

fn link_at(text: &str, start: usize) -> Option<Link> {
    // Not inside a word, host or path, like `foo.www.bar`.
    if text[..start]
        .chars()
        .next_back()
        .is_some_and(|c| c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | '/' | '@'))
    {
        return None;
    }
    let rest = &text[start..];
    let starts_with = |prefix: &str| {
        rest.get(..prefix.len())
            .is_some_and(|s| s.eq_ignore_ascii_case(prefix))
    };
    let www = starts_with(WWW);
    let prefix = if www {
        WWW
    } else {
        SCHEMES.iter().copied().find(|scheme| starts_with(scheme))?
    };
    let end = rest
        .find(|c: char| c.is_whitespace() || matches!(c, '<' | '>' | '"'))
        .unwrap_or(rest.len());
    let candidate = trim_end(&rest[..end]);
    if candidate.len() <= prefix.len() {
        return None;
    }
    let href = if www {
        format!("https://{candidate}")
    } else {
        candidate.to_owned()
    };
    let url = url::Url::parse(&href).ok()?;
    url.host_str().filter(|host| !host.is_empty())?;
    Some(Link {
        range: start..start + candidate.len(),
        href,
    })
}

/// Leaves out punctuation ending the sentence rather than the link, and
/// closing brackets the link didn't open.
fn trim_end(mut candidate: &str) -> &str {
    while let Some(last) = candidate.chars().next_back() {
        let unbalanced =
            |open: char| candidate.matches(last).count() > candidate.matches(open).count();
        let trailing = match last {
            '.' | ',' | ':' | ';' | '!' | '?' | '\'' => true,
            ')' => unbalanced('('),
            ']' => unbalanced('['),
            '}' => unbalanced('{'),
            _ => false,
        };
        if !trailing {
            break;
        }
        candidate = &candidate[..candidate.len() - last.len_utf8()];
    }
    candidate
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The links' text and hrefs.
    fn hrefs(text: &str) -> Vec<(&str, String)> {
        find(text)
            .into_iter()
            .map(|link| (&text[link.range], link.href))
            .collect()
    }

    fn one<'a>(text: &'a str, href: &str) -> (&'a str, String) {
        (text, href.to_owned())
    }

    #[test]
    fn finds_urls_and_www_addresses() {
        assert_eq!(
            hrefs("see https://example.com/a?b=1&c=2#d and http://x.org"),
            vec![
                one(
                    "https://example.com/a?b=1&c=2#d",
                    "https://example.com/a?b=1&c=2#d"
                ),
                one("http://x.org", "http://x.org"),
            ]
        );
        assert_eq!(
            hrefs("www.example.com/path"),
            vec![one("www.example.com/path", "https://www.example.com/path")]
        );
        assert_eq!(
            hrefs("HTTPS://Example.com WWW.example.com"),
            vec![
                one("HTTPS://Example.com", "HTTPS://Example.com"),
                one("WWW.example.com", "https://WWW.example.com"),
            ]
        );
        assert_eq!(
            hrefs("café: https://münchen.de/straße"),
            vec![one(
                "https://münchen.de/straße",
                "https://münchen.de/straße"
            )]
        );
    }

    #[test]
    fn leaves_out_trailing_punctuation_and_unopened_brackets() {
        assert_eq!(
            hrefs("Look at https://example.com/x."),
            vec![one("https://example.com/x", "https://example.com/x")]
        );
        assert_eq!(
            hrefs("(see https://example.com/a_(b)), ok?"),
            vec![one(
                "https://example.com/a_(b)",
                "https://example.com/a_(b)"
            )]
        );
        assert_eq!(
            hrefs("[https://example.com]!"),
            vec![one("https://example.com", "https://example.com")]
        );
        assert_eq!(
            hrefs("'https://example.com/it's'"),
            vec![one("https://example.com/it's", "https://example.com/it's")]
        );
        assert_eq!(
            hrefs("<https://example.com>"),
            vec![one("https://example.com", "https://example.com")]
        );
    }

    #[test]
    fn ignores_non_links() {
        for text in [
            "https:// alone",
            "www. alone",
            "foo.www.bar.com",
            "xhttps://example.com",
            "ftp://example.com",
            "mail me at me@www.example.com",
            "https://?",
            "",
        ] {
            assert_eq!(hrefs(text), vec![], "{text}");
        }
    }

    #[test]
    fn markup_escapes_text_and_links() {
        assert_eq!(markup("a < b & c"), "a &lt; b &amp; c");
        assert_eq!(
            markup("<b>https://x.org/?a=1&b=\"2\" www.y.org"),
            "&lt;b&gt;<a href=\"https://x.org/?a=1&amp;b=\">https://x.org/?a=1&amp;b=</a>&quot;2&quot; \
             <a href=\"https://www.y.org\">www.y.org</a>"
        );
    }
}
