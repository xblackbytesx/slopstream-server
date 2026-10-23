//! Slopstream's sync contract, shared by the server and (through golden JSON)
//! the Android app. Pure: no IO, no clock, no randomness.
//!
//! - [`wire`]: what `/api/v1` sends and receives.
//! - [`merge`]: whether a device's change replaces the server's row.
//! - [`services`]: the services the app knows and what each one allows here.
//! - [`channel`]: reading a YouTube channel reference someone typed.
//! - [`feeds`]: a channel's recent uploads from what YouTube and Floatplane answer.

pub mod channel;
pub mod feeds;
pub mod merge;
pub mod services;
pub mod wire;

use sha2::{Digest, Sha256};

/// How a session value is named without revealing it: lowercase hex SHA-256.
/// The app computes the same (its contract test checks the golden value).
pub fn fingerprint(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn fingerprint_is_hex_sha256() {
        assert_eq!(super::fingerprint("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    }
}
