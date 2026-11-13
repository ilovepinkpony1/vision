//! Id helpers: session ids and content hashes. Ticket keys and turn ids are in `paths`,
//! because they name folders.

use std::fmt::Write;

use sha2::{Digest, Sha256};

/// A new random session id for `claude --session-id`.
pub fn new_session_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// The SHA-256 of `bytes` as 64 lowercase hex digits, such as the hash of a plan or a role file.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(64);
    for byte in digest.iter() {
        let _ = write!(text, "{byte:02x}");
    }
    text
}

/// The SHA-256 of a file as 64 lowercase hex digits, read in parts, so that a large file never
/// sits in memory whole.
pub fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1 << 20];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut text = String::with_capacity(64);
    for byte in hasher.finalize().iter() {
        let _ = write!(text, "{byte:02x}");
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_file_matches_the_hash_of_the_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"abc").unwrap();
        assert_eq!(sha256_file(&path).unwrap(), sha256_hex(b"abc"));
    }

    #[test]
    fn sha256_hex_matches_the_known_value() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn session_ids_are_uuids() {
        let id = new_session_id();
        assert_eq!(id.len(), 36);
        assert_ne!(id, new_session_id());
    }
}
