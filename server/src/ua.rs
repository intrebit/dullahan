//! Coarse User-Agent parsing — browser and OS *family* only, never versions.
//! Hand-rolled (no dependency, no fingerprinting surface). Substring order
//! matters: Edge/Opera/Samsung and Chrome-on-iOS all masquerade as other
//! engines, so the more specific tokens are checked first.

pub fn parse_ua(ua: &str) -> (Option<String>, Option<String>) {
    if ua.trim().is_empty() {
        return (None, None);
    }
    (parse_browser(ua), parse_os(ua))
}

fn parse_browser(ua: &str) -> Option<String> {
    const CHECKS: &[(&str, &str)] = &[
        ("Edg", "Edge"),
        ("OPR", "Opera"),
        ("Opera", "Opera"),
        ("SamsungBrowser", "Samsung Internet"),
        ("CriOS", "Chrome"),
        ("FxiOS", "Firefox"),
        ("Firefox", "Firefox"),
        ("Chrome", "Chrome"),
        ("Safari", "Safari"),
    ];
    CHECKS
        .iter()
        .find(|(needle, _)| ua.contains(needle))
        .map(|(_, name)| (*name).to_string())
}

fn parse_os(ua: &str) -> Option<String> {
    const CHECKS: &[(&str, &str)] = &[
        ("Windows", "Windows"),
        ("iPhone", "iOS"),
        ("iPad", "iOS"),
        ("iPod", "iOS"),
        ("Android", "Android"),
        ("CrOS", "Chrome OS"),
        ("Mac OS X", "macOS"),
        ("Macintosh", "macOS"),
        ("Linux", "Linux"),
    ];
    CHECKS
        .iter()
        .find(|(needle, _)| ua.contains(needle))
        .map(|(_, name)| (*name).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(ua: &str) -> (Option<String>, Option<String>) {
        parse_ua(ua)
    }

    #[test]
    fn empty_ua_yields_nothing() {
        assert_eq!(parse_ua(""), (None, None));
        assert_eq!(parse_ua("   "), (None, None));
    }

    #[test]
    fn chrome_on_windows() {
        let (b, o) = parse(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
        );
        assert_eq!(b.as_deref(), Some("Chrome"));
        assert_eq!(o.as_deref(), Some("Windows"));
    }

    #[test]
    fn safari_on_macos() {
        let (b, o) = parse(
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) Version/17.0 Safari/605.1.15",
        );
        assert_eq!(b.as_deref(), Some("Safari"));
        assert_eq!(o.as_deref(), Some("macOS"));
    }

    #[test]
    fn edge_is_not_misread_as_chrome() {
        let (b, _) = parse(
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
             (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36 Edg/120.0.0.0",
        );
        assert_eq!(b.as_deref(), Some("Edge"));
    }

    #[test]
    fn chrome_on_ios_is_chrome_not_safari() {
        let (b, o) = parse(
            "Mozilla/5.0 (iPhone; CPU iPhone OS 17_0 like Mac OS X) AppleWebKit/605.1.15 \
             (KHTML, like Gecko) CriOS/120.0.0.0 Mobile/15E148 Safari/604.1",
        );
        assert_eq!(b.as_deref(), Some("Chrome"));
        assert_eq!(o.as_deref(), Some("iOS"));
    }

    #[test]
    fn firefox_on_android() {
        let (b, o) = parse("Mozilla/5.0 (Android 14; Mobile; rv:121.0) Gecko/121.0 Firefox/121.0");
        assert_eq!(b.as_deref(), Some("Firefox"));
        assert_eq!(o.as_deref(), Some("Android"));
    }

    #[test]
    fn unknown_ua_is_none() {
        assert_eq!(parse_ua("curl/8.4.0"), (None, None));
    }
}
