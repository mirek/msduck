//! Bootstrap administrator authentication. SQL-managed principals remain separate.
use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use ring::pbkdf2;
use std::{
    fs::File,
    io::Read,
    num::NonZeroU32,
    path::{Path, PathBuf},
};

const ITERATIONS: NonZeroU32 = NonZeroU32::new(600_000).unwrap();
const MAX_CONFIG: u64 = 4096;

struct Credential {
    name: String,
    salt: [u8; 32],
    digest: [u8; 32],
}
impl Credential {
    fn load(path: &Path) -> Result<Self> {
        let mut bytes = Vec::new();
        File::open(path)
            .context("read administrator credential file")?
            .take(MAX_CONFIG + 1)
            .read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= MAX_CONFIG,
            "administrator credential file is too large"
        );
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid administrator credential configuration"))?;
        let object = value
            .as_object()
            .context("invalid administrator credential configuration")?;
        ensure!(
            object.len() == 2,
            "invalid administrator credential configuration"
        );
        let name = value["userName"]
            .as_str()
            .context("missing administrator name")?;
        ensure!(
            !name.is_empty() && name.encode_utf16().count() <= 128 && !name.contains('\0'),
            "invalid administrator name"
        );
        let hash = value["passwordHash"]
            .as_str()
            .context("missing administrator password hash")?;
        let parts: Vec<_> = hash.split('$').collect();
        ensure!(
            parts.len() == 5 && parts[..3] == ["msduck", "pbkdf2-sha256", "v1"],
            "invalid administrator password hash"
        );
        let decode = |encoded: &str| -> Result<[u8; 32]> {
            URL_SAFE_NO_PAD
                .decode(encoded)
                .ok()
                .and_then(|bytes| bytes.try_into().ok())
                .context("invalid administrator password hash")
        };
        Ok(Self {
            name: name.into(),
            salt: decode(parts[3])?,
            digest: decode(parts[4])?,
        })
    }
    fn password_matches(&self, password: &str) -> bool {
        pbkdf2::verify(
            pbkdf2::PBKDF2_HMAC_SHA256,
            ITERATIONS,
            &self.salt,
            password.as_bytes(),
            &self.digest,
        )
        .is_ok()
    }
}

/// A configured bootstrap administrator; reloads atomically replaced files for
/// each new login. A failed reload denies access and still performs password work.
pub struct Administrator {
    path: PathBuf,
    fallback: Credential,
}
impl Administrator {
    pub fn load(path: &Path) -> Result<Self> {
        Ok(Self {
            path: path.into(),
            fallback: Credential::load(path)?,
        })
    }
    pub fn authenticate(&self, user_name: &str, password: &str) -> Option<String> {
        let loaded = Credential::load(&self.path).ok();
        let credential = loaded.as_ref().unwrap_or(&self.fallback);
        // Unknown names incur the same KDF work. Never compare plaintext secrets.
        let valid_password = credential.password_matches(password);
        (loaded.is_some()
            && valid_password
            && !user_name.contains('\0')
            && user_name.to_lowercase() == credential.name.to_lowercase())
        .then(|| credential.name.clone())
    }
}
