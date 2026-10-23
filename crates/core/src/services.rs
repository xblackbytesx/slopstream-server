//! The services Slopstream plays, keyed like the app's `SourceKey`. They stay
//! hard-split here too: every row names its service, and pages show one at a
//! time.

/// A website-session service (Floatplane's platform).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Site {
    pub base_url: &'static str,
    /// The cookie that carries the session.
    pub session_cookie: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Service {
    /// `SourceKey.id` in the app.
    pub key: &'static str,
    pub label: &'static str,
    /// Some for services that need a signed-in session.
    pub site: Option<Site>,
    /// Channels can be added on the website (YouTube: by URL, handle or id).
    pub add_on_web: bool,
    /// Following is the person's own choice. Floatplane's follows mirror the
    /// account instead, so only the account can end them.
    pub can_unsubscribe: bool,
}

pub const YOUTUBE: Service =
    Service { key: "yt", label: "YouTube", site: None, add_on_web: true, can_unsubscribe: true };

pub const FLOATPLANE: Service = Service {
    key: "fp",
    label: "Floatplane",
    site: Some(Site { base_url: "https://www.floatplane.com", session_cookie: "sails.sid" }),
    add_on_web: false,
    can_unsubscribe: false,
};

pub const SAUCEPLUS: Service = Service {
    key: "sp",
    label: "Sauce+",
    site: Some(Site { base_url: "https://www.sauceplus.com", session_cookie: "__Host-sp-sess" }),
    add_on_web: false,
    can_unsubscribe: true,
};

pub const ALL: [Service; 3] = [YOUTUBE, FLOATPLANE, SAUCEPLUS];

pub fn by_key(key: &str) -> Option<&'static Service> {
    ALL.iter().find(|s| s.key == key)
}

/// The services that keep a session on the server.
pub fn with_sessions() -> impl Iterator<Item = &'static Service> {
    ALL.iter().filter(|s| s.site.is_some())
}

/// The cookie value from whatever was pasted: the bare value, `name=...`, or
/// a whole Cookie header. None when nothing usable is there. Same rules as
/// the app's `SiteSignIn.sessionValue`.
pub fn session_value(input: &str, cookie: &str) -> Option<String> {
    let text = input.trim();
    let text = text.strip_prefix("Cookie:").unwrap_or(text).trim();
    let prefix = format!("{cookie}=");
    let named = text.split(';').map(str::trim).find_map(|kv| kv.strip_prefix(prefix.as_str()));
    let value = named.or_else(|| (!text.contains('=') && !text.contains(';')).then_some(text)).unwrap_or("");
    let value = value.trim().trim_matches('"');
    // Also refuses what would break the Cookie header we send.
    let ok = value.len() >= 16
        && value.len() <= 4096
        && value.bytes().all(|b| b.is_ascii_graphic() && b != b';' && b != b',');
    ok.then(|| value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_match_the_app() {
        assert_eq!(ALL.map(|s| s.key), ["yt", "fp", "sp"]);
        assert_eq!(by_key("sp").unwrap().site.unwrap().session_cookie, "__Host-sp-sess");
        assert!(by_key("xx").is_none());
        assert_eq!(with_sessions().map(|s| s.key).collect::<Vec<_>>(), ["fp", "sp"]);
    }

    #[test]
    fn pasted_sessions() {
        let v = "s%3Aabcdefghijklmnop.qrstuvwxyz";
        assert_eq!(session_value(v, "sails.sid").as_deref(), Some(v));
        assert_eq!(session_value(&format!("sails.sid={v}"), "sails.sid").as_deref(), Some(v));
        let header = format!("Cookie: _cfuvid=x; sails.sid=\"{v}\"; muxData=y");
        assert_eq!(session_value(&header, "sails.sid").as_deref(), Some(v));
        assert_eq!(session_value(&format!("__Host-sp-sess={v}"), "sails.sid"), None, "another cookie");
        assert_eq!(session_value("short", "sails.sid"), None);
        assert_eq!(session_value("has a space in it somewhere", "sails.sid"), None);
    }
}
