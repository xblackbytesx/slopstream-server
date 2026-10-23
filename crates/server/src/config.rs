use anyhow::{Context, Result, bail};
use base64::Engine as _;

pub struct Config {
    pub database_url: String,
    pub bind: String,
    /// Master key for service sessions at rest (32 bytes).
    pub secret_key: [u8; 32],
    /// `Secure` on the web session cookie. On by default; turn off only for a
    /// plain-http LAN setup, or browsers will not send the cookie back.
    pub cookie_secure: bool,
    /// This server as devices reach it (`https://slopstream.example.com`),
    /// shown on a pairing TV. Without it the request's own host is used.
    pub public_url: Option<String>,
    /// `SLOPSTREAM_FEEDS`: fetch the video lists here (default on).
    pub feeds: bool,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            database_url: var("DATABASE_URL").context("DATABASE_URL is required")?,
            bind: var("SLOPSTREAM_BIND").unwrap_or_else(|| "0.0.0.0:8080".into()),
            secret_key: secret_key(var("SLOPSTREAM_SECRET_KEY").as_deref())?,
            cookie_secure: !matches!(var("SLOPSTREAM_COOKIE_SECURE").as_deref(), Some("false" | "0" | "no")),
            public_url: public_url(var("SLOPSTREAM_PUBLIC_URL").as_deref())?,
            feeds: !matches!(var("SLOPSTREAM_FEEDS").as_deref(), Some("false" | "0" | "no")),
        })
    }
}

/// Declared, never generated on the fly: a key made at startup and lost with
/// the container would silently orphan every stored session.
pub fn secret_key(v: Option<&str>) -> Result<[u8; 32]> {
    let Some(v) = v else {
        bail!("SLOPSTREAM_SECRET_KEY is required: 32 random bytes, base64 (`head -c 32 /dev/urandom | base64`)")
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(v)
        .context("SLOPSTREAM_SECRET_KEY must be base64 (`head -c 32 /dev/urandom | base64`)")?;
    bytes.try_into().map_err(|b: Vec<u8>| anyhow::anyhow!("SLOPSTREAM_SECRET_KEY must be 32 bytes, got {}", b.len()))
}

pub fn public_url(v: Option<&str>) -> Result<Option<String>> {
    let Some(v) = v else { return Ok(None) };
    if !(v.starts_with("https://") || v.starts_with("http://")) {
        bail!("SLOPSTREAM_PUBLIC_URL must start with https:// (or http:// on a LAN)");
    }
    Ok(Some(v.trim_end_matches('/').to_string()))
}

/// Trimmed; empty means unset.
fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().map(|v| v.trim().to_string()).filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_key_must_be_32_base64_bytes() {
        assert!(secret_key(None).is_err());
        assert!(secret_key(Some("not base64!")).is_err());
        assert!(secret_key(Some("c2hvcnQ=")).is_err());
        assert_eq!(
            secret_key(Some("ZGV2LW9ubHktZGV2LW9ubHktZGV2LW9ubHktZGV2LW8=")).unwrap(),
            *b"dev-only-dev-only-dev-only-dev-o"
        );
    }

    #[test]
    fn public_url_is_a_base_url() {
        assert_eq!(public_url(None).unwrap(), None);
        assert_eq!(public_url(Some("https://s.example.com/")).unwrap().as_deref(), Some("https://s.example.com"));
        assert!(public_url(Some("s.example.com")).is_err());
    }
}
