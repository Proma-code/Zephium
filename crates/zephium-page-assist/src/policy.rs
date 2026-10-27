//! When the panel may read a tab at all. Every refusal here means no read
//! and no request.
use crate::ipc::{PageAssistAvailabilityV1 as Availability, PageAssistFaultV1 as Fault};

/// What the shell knows about a tab, without reading it.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TabFacts<'a> {
    /// AI is switched on in Settings.
    pub ai_enabled: bool,
    /// The tab belongs to a private window.
    pub private: bool,
    /// The committed URL; none for internal pages.
    pub url: Option<&'a str>,
    /// The tab shows the browser's own viewer (PDF, image, source).
    pub viewer: bool,
    /// The registrable site is a bank, payment, health or government site.
    pub sensitive: bool,
    /// The page has not finished its first load.
    pub loading: bool,
    /// A model is set up: a Zephium account or a key.
    pub has_model: bool,
    /// This platform can read live tabs.
    pub supported: bool,
}

/// Whether the address is an ordinary web page: `http` or `https` with a
/// host and no credentials in it.
pub fn is_web_url(url: &str) -> bool {
    let lower = url.get(..8).unwrap_or(url).to_ascii_lowercase();
    let rest = if lower.starts_with("https://") {
        &url[8..]
    } else if lower.starts_with("http://") {
        &url[7..]
    } else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    !authority.is_empty() && !authority.contains('@') && !authority.starts_with(':')
}

/// The tab's availability. Order matters: the person's own switches first,
/// then what the page is, then what could still change.
pub fn availability(facts: &TabFacts<'_>) -> Availability {
    if !facts.ai_enabled {
        Availability::AiOff
    } else if facts.private {
        Availability::Private
    } else if !facts.supported {
        Availability::Unsupported
    } else if facts.viewer || !facts.url.is_some_and(is_web_url) {
        Availability::NotWeb
    } else if facts.sensitive {
        Availability::Sensitive
    } else if !facts.has_model {
        Availability::NoModel
    } else if facts.loading {
        Availability::Loading
    } else {
        Availability::Ready
    }
}

/// The fault a refused ask reports; none when the tab is ready.
pub fn refusal(availability: Availability) -> Option<Fault> {
    Some(match availability {
        Availability::Ready => return None,
        Availability::AiOff => Fault::AiOff,
        Availability::Private => Fault::Private,
        Availability::NotWeb => Fault::NotWeb,
        Availability::Sensitive => Fault::Sensitive,
        Availability::NoModel => Fault::NoModel,
        Availability::Loading => Fault::Loading,
        Availability::Unsupported => Fault::Unsupported,
        Availability::Unavailable | Availability::Dormant => Fault::Unavailable,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ready(url: &str) -> TabFacts<'_> {
        TabFacts {
            ai_enabled: true,
            url: Some(url),
            has_model: true,
            supported: true,
            ..TabFacts::default()
        }
    }

    #[test]
    fn only_ordinary_web_pages_are_web() {
        for (url, web) in [
            ("https://example.com/a", true),
            ("http://localhost:8080/", true),
            ("HTTPS://Example.com", true),
            ("https://user:pw@example.com/", false),
            ("https://", false),
            ("https:///path", false),
            ("file:///Users/me/a.html", false),
            ("about:blank", false),
            ("data:text/html,hi", false),
            ("view-source:https://example.com", false),
            ("chrome-extension://abc/page.html", false),
            ("safari-web-extension://abc/page.html", false),
            ("zephium://settings", false),
            ("javascript:alert(1)", false),
            ("", false),
        ] {
            assert_eq!(is_web_url(url), web, "{url}");
        }
    }

    #[test]
    fn refusals_follow_a_fixed_order() {
        let base = ready("https://example.com");
        assert_eq!(availability(&base), Availability::Ready);
        assert_eq!(refusal(availability(&base)), None);
        let cases: [(TabFacts<'_>, Availability); 8] = [
            (
                TabFacts {
                    ai_enabled: false,
                    private: true,
                    ..base
                },
                Availability::AiOff,
            ),
            (
                TabFacts {
                    private: true,
                    sensitive: true,
                    ..base
                },
                Availability::Private,
            ),
            (
                TabFacts {
                    supported: false,
                    ..base
                },
                Availability::Unsupported,
            ),
            (TabFacts { url: None, ..base }, Availability::NotWeb),
            (
                TabFacts {
                    viewer: true,
                    ..base
                },
                Availability::NotWeb,
            ),
            (
                TabFacts {
                    sensitive: true,
                    has_model: false,
                    ..base
                },
                Availability::Sensitive,
            ),
            (
                TabFacts {
                    has_model: false,
                    loading: true,
                    ..base
                },
                Availability::NoModel,
            ),
            (
                TabFacts {
                    loading: true,
                    ..base
                },
                Availability::Loading,
            ),
        ];
        for (facts, expected) in cases {
            assert_eq!(availability(&facts), expected, "{facts:?}");
            assert!(refusal(expected).is_some());
        }
    }
}
