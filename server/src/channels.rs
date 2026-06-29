//! Marketing-channel classification from the referrer host + UTM tags. Heuristic
//! and intentionally small; the lists below are the common cases, not exhaustive.
//! Pure and side-effect free so it is cheap to unit-test.

/// `utm_medium` values that mean paid acquisition.
const PAID_MEDIA: &[&str] = &[
    "cpc",
    "ppc",
    "paid",
    "paidsearch",
    "paid_search",
    "paid-search",
    "display",
    "cpm",
    "banner",
    "paidsocial",
    "paid-social",
];

/// Search-engine brand labels, matched against any dot-separated host label so
/// subdomains and ccTLDs work (`www.google.co.uk` -> `google`).
const SEARCH_LABELS: &[&str] = &[
    "google",
    "bing",
    "duckduckgo",
    "yahoo",
    "ecosia",
    "baidu",
    "yandex",
    "startpage",
    "brave",
    "qwant",
];

/// Social brand labels, matched per host label.
const SOCIAL_LABELS: &[&str] = &[
    "facebook",
    "instagram",
    "twitter",
    "linkedin",
    "reddit",
    "youtube",
    "tiktok",
    "pinterest",
    "threads",
    "mastodon",
];

/// Social link-shortener / exact hosts that don't carry a brand label.
const SOCIAL_HOSTS: &[&str] = &["t.co", "x.com", "lnkd.in", "fb.me", "youtu.be"];

pub const DIRECT: &str = "Direct";
pub const PAID: &str = "Paid";
pub const CAMPAIGN: &str = "Campaign";
pub const ORGANIC: &str = "Organic Search";
pub const SOCIAL: &str = "Social";
pub const REFERRAL: &str = "Referral";

fn present(v: Option<&str>) -> bool {
    v.map(|s| !s.trim().is_empty()).unwrap_or(false)
}

/// Classify a pageview into a marketing channel. Precedence: paid > campaign >
/// organic search > social > referral > direct.
pub fn classify(
    referrer: Option<&str>,
    utm_source: Option<&str>,
    utm_medium: Option<&str>,
    utm_campaign: Option<&str>,
) -> &'static str {
    let medium = utm_medium.unwrap_or("").trim().to_ascii_lowercase();
    if PAID_MEDIA.contains(&medium.as_str()) {
        return PAID;
    }
    if present(utm_source) || present(utm_medium) || present(utm_campaign) {
        return CAMPAIGN;
    }

    if let Some(host) = referrer {
        let host = host.trim().to_ascii_lowercase();
        if !host.is_empty() {
            if SOCIAL_HOSTS.contains(&host.as_str()) {
                return SOCIAL;
            }
            let labels = host.split('.');
            for label in labels {
                if SEARCH_LABELS.contains(&label) {
                    return ORGANIC;
                }
                if SOCIAL_LABELS.contains(&label) {
                    return SOCIAL;
                }
            }
            return REFERRAL;
        }
    }
    DIRECT
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_when_nothing() {
        assert_eq!(classify(None, None, None, None), DIRECT);
        assert_eq!(classify(Some("  "), Some(""), None, None), DIRECT);
    }

    #[test]
    fn search_engines_are_organic() {
        assert_eq!(classify(Some("www.google.com"), None, None, None), ORGANIC);
        assert_eq!(classify(Some("google.co.uk"), None, None, None), ORGANIC);
        assert_eq!(classify(Some("duckduckgo.com"), None, None, None), ORGANIC);
    }

    #[test]
    fn socials_match_label_and_exact_host() {
        assert_eq!(classify(Some("m.facebook.com"), None, None, None), SOCIAL);
        assert_eq!(classify(Some("old.reddit.com"), None, None, None), SOCIAL);
        assert_eq!(classify(Some("t.co"), None, None, None), SOCIAL);
        assert_eq!(classify(Some("x.com"), None, None, None), SOCIAL);
    }

    #[test]
    fn paid_beats_referrer() {
        assert_eq!(
            classify(Some("google.com"), Some("google"), Some("cpc"), None),
            PAID
        );
    }

    #[test]
    fn any_utm_is_campaign() {
        assert_eq!(
            classify(None, Some("newsletter"), Some("email"), None),
            CAMPAIGN
        );
        assert_eq!(classify(None, None, None, Some("spring_sale")), CAMPAIGN);
    }

    #[test]
    fn other_referrers_are_referral() {
        assert_eq!(
            classify(Some("news.ycombinator.com"), None, None, None),
            REFERRAL
        );
        // a brand substring that isn't its own label must not match
        assert_eq!(
            classify(Some("notgoogleish.com"), None, None, None),
            REFERRAL
        );
    }
}
