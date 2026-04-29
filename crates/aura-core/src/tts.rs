use lazy_static::lazy_static;
use regex::Regex;
use std::collections::HashMap;

lazy_static! {
    static ref URL_REGEX: Regex =
        Regex::new(r"https?://(?:www\.)?([a-zA-Z0-9.-]+\.[a-z]{2,})[^\s]*").unwrap();
    static ref BRAND_MAP: HashMap<&'static str, &'static str> = {
        let mut m = HashMap::new();
        m.insert("youtube.com", "YouTube");
        m.insert("github.com", "GitHub");
        m.insert("google.com", "Google");
        m.insert("discord.com", "Discord");
        m.insert("twitter.com", "Twitter");
        m.insert("x.com", "X");
        m.insert("reddit.com", "Reddit");
        m
    };
}

#[derive(uniffi::Record, Default)]
pub struct TtsSettings {
    pub enabled: bool,
    pub volume: f32, // 0.0 to 1.0
    pub rate: f32,   // 0.0 to 1.0
    pub speak_chat: bool,
    pub speak_join_leave: bool,
}

#[derive(uniffi::Object, Default)]
pub struct TtsFormatter;

#[uniffi::export]
impl TtsFormatter {
    #[uniffi::constructor]
    pub fn new() -> Self {
        Self
    }

    /// Sanitizes text for TTS, replacing URLs with spoken descriptions.
    pub fn sanitize(&self, text: &str) -> String {
        URL_REGEX
            .replace_all(text, |caps: &regex::Captures| {
                let domain = &caps[1];

                // 1. Check brand map
                if let Some(brand) = BRAND_MAP.get(domain) {
                    return format!("{} link", brand);
                }

                // 2. Check if domain is short enough to say
                if domain.len() < 20 {
                    return format!("link to {}", domain);
                }

                // 3. Fallback
                "link".to_string()
            })
            .to_string()
    }

    /// Formats a join event for speech
    pub fn format_join(&self, name: &str) -> String {
        format!("{} joined the channel", name)
    }

    /// Formats a leave event for speech
    pub fn format_leave(&self, name: &str) -> String {
        format!("{} left the channel", name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_urls() {
        let formatter = TtsFormatter::new();

        // Brand mapping
        assert_eq!(
            formatter.sanitize("Check this out: https://youtube.com/watch?v=123"),
            "Check this out: YouTube link"
        );

        // Short domain
        assert_eq!(
            formatter.sanitize("Go to http://example.com/foo"),
            "Go to link to example.com"
        );

        // Long domain fallback
        assert_eq!(
            formatter.sanitize(
                "Visit http://very-long-and-super-suspicious-domain-name-that-is-too-long.com/path"
            ),
            "Visit link"
        );

        // Multiple URLs
        assert_eq!(
            formatter.sanitize("Search on https://google.com or code on https://github.com"),
            "Search on Google link or code on GitHub link"
        );
    }
}
