//! Token authentication: a locally generated admin token plus pre-registered clients
//! identified by SHA-256 token hashes.

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::Context;
use base64::Engine;
use kiln_core::error::{ErrorCode, PrintError};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::{
    ADMIN_CLIENT_ID, ClientAuthenticator, ConnectionInfo, Credentials, Permission, Principal,
    PrincipalKind, normalise_origin,
};
use crate::config::SecurityConfig;

const TOKEN_PREFIX: &str = "kiln_";

/// 256 bits from the OS CSPRNG, URL-safe base64.
pub fn generate_token() -> anyhow::Result<String> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| anyhow::anyhow!("system RNG unavailable: {e}"))?;
    Ok(format!(
        "{TOKEN_PREFIX}{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    ))
}

pub fn hash_token(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}

/// Reads the admin token, creating it (readable only by the current user) on first run.
pub fn load_or_create_admin_token(path: &Path) -> anyhow::Result<String> {
    if let Ok(existing) = std::fs::read_to_string(path) {
        let token = existing.trim().to_owned();
        anyhow::ensure!(
            token.len() >= 32,
            "admin token in {} is too short",
            path.display()
        );
        return Ok(token);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    let token = generate_token()?;
    write_private(path, &token).with_context(|| format!("writing {}", path.display()))?;
    tracing::info!(target: "kiln::audit", event = "admin_token.created", path = %path.display(), "created local admin token");
    Ok(token)
}

#[cfg(unix)]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents.as_bytes())
}

/// On Windows the per-user local AppData directory is already restricted to the user,
/// SYSTEM and Administrators, and new files inherit that ACL.
#[cfg(not(unix))]
fn write_private(path: &Path, contents: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(contents.as_bytes())
}

struct Entry {
    hash: [u8; 32],
    principal: Principal,
    origins: Vec<String>,
}

pub struct TokenAuthenticator {
    entries: Vec<Entry>,
}

impl std::fmt::Debug for TokenAuthenticator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenAuthenticator")
            .field("clients", &self.entries.len())
            .finish()
    }
}

impl TokenAuthenticator {
    pub fn new(admin_token: Option<&str>, security: &SecurityConfig) -> anyhow::Result<Self> {
        let mut entries = Vec::new();
        if let Some(token) = admin_token {
            entries.push(Entry {
                hash: hash_token(token),
                principal: Principal {
                    client_id: ADMIN_CLIENT_ID.into(),
                    name: "Local administrator".into(),
                    kind: PrincipalKind::Admin,
                    permissions: Permission::ALL.into_iter().collect(),
                    printers: vec!["*".into()],
                    origin: None,
                },
                origins: normalise_all(&security.admin_origins),
            });
        }
        for client in &security.clients {
            let mut hash = [0u8; 32];
            hex::decode_to_slice(&client.token_sha256, &mut hash)
                .with_context(|| format!("client '{}': invalid token_sha256", client.id))?;
            entries.push(Entry {
                hash,
                principal: Principal {
                    client_id: client.id.clone(),
                    name: client.name.clone(),
                    kind: PrincipalKind::Client,
                    permissions: client.permissions.iter().copied().collect::<BTreeSet<_>>(),
                    printers: client.printers.clone(),
                    origin: None,
                },
                origins: normalise_all(&client.origins),
            });
        }
        Ok(Self { entries })
    }
}

fn normalise_all(origins: &[String]) -> Vec<String> {
    origins.iter().filter_map(|o| normalise_origin(o)).collect()
}

impl ClientAuthenticator for TokenAuthenticator {
    fn authenticate(
        &self,
        credentials: &Credentials,
        connection: &ConnectionInfo,
    ) -> Result<Principal, PrintError> {
        let Credentials::Token(token) = credentials;
        let presented = hash_token(token);
        // Compare against every entry without short-circuiting.
        let mut matched: Option<&Entry> = None;
        for entry in &self.entries {
            if bool::from(entry.hash.ct_eq(&presented)) {
                matched = Some(entry);
            }
        }
        let entry = matched.ok_or_else(|| {
            PrintError::new(
                ErrorCode::ClientNotTrusted,
                "the presented credentials are not trusted by this agent",
            )
        })?;
        if let Some(origin) = &connection.origin {
            if !entry.origins.iter().any(|o| o == origin) {
                return Err(PrintError::new(
                    ErrorCode::AccessDenied,
                    format!("origin {origin} is not allowed for this client"),
                ));
            }
        }
        let mut principal = entry.principal.clone();
        principal.origin = connection.origin.clone();
        Ok(principal)
    }

    fn origin_known(&self, origin: &str) -> bool {
        self.entries
            .iter()
            .any(|e| e.origins.iter().any(|o| o == origin))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::ClientConfig;

    fn conn(origin: Option<&str>) -> ConnectionInfo {
        ConnectionInfo {
            peer: "127.0.0.1:1".parse().expect("addr"),
            origin: origin.map(str::to_owned),
            user_agent: None,
        }
    }

    fn authenticator() -> TokenAuthenticator {
        let security = SecurityConfig {
            clients: vec![ClientConfig {
                id: "lab".into(),
                name: "Lab".into(),
                token_sha256: hex::encode(hash_token("lab-secret")),
                origins: vec!["https://lab.example.com".into()],
                permissions: vec![Permission::Print],
                printers: vec!["Zebra".into()],
            }],
            ..SecurityConfig::default()
        };
        TokenAuthenticator::new(Some("admin-secret"), &security).expect("auth")
    }

    #[test]
    fn tokens_are_unique_and_prefixed() {
        let a = generate_token().expect("token");
        let b = generate_token().expect("token");
        assert_ne!(a, b);
        assert!(a.starts_with("kiln_") && a.len() > 40);
    }

    #[test]
    fn admin_token_works_for_native_clients_only() {
        let auth = authenticator();
        let p = auth
            .authenticate(&Credentials::Token("admin-secret".into()), &conn(None))
            .expect("native");
        assert_eq!(p.kind, PrincipalKind::Admin);
        assert!(p.has(Permission::JobsReadAll));
        let err = auth
            .authenticate(
                &Credentials::Token("admin-secret".into()),
                &conn(Some("https://evil.example")),
            )
            .expect_err("browser");
        assert_eq!(err.error_code, ErrorCode::AccessDenied);
    }

    #[test]
    fn client_is_bound_to_its_origins() {
        let auth = authenticator();
        let token = Credentials::Token("lab-secret".into());
        let p = auth
            .authenticate(&token, &conn(Some("https://lab.example.com")))
            .expect("allowed origin");
        assert_eq!(p.client_id, "lab");
        assert_eq!(p.origin.as_deref(), Some("https://lab.example.com"));
        assert!(
            auth.authenticate(&token, &conn(Some("https://other.example.com")))
                .is_err()
        );
        assert!(auth.origin_known("https://lab.example.com"));
        assert!(!auth.origin_known("https://other.example.com"));
    }

    #[test]
    fn unknown_token_is_not_trusted() {
        let err = authenticator()
            .authenticate(&Credentials::Token("guess".into()), &conn(None))
            .expect_err("unknown");
        assert_eq!(err.error_code, ErrorCode::ClientNotTrusted);
    }

    #[test]
    fn admin_token_file_is_created_once() {
        let dir = std::env::temp_dir().join(format!("kiln-token-test-{}", uuid::Uuid::new_v4()));
        let path = dir.join("admin.token");
        let first = load_or_create_admin_token(&path).expect("create");
        let second = load_or_create_admin_token(&path).expect("reload");
        assert_eq!(first, second);
        let _ = std::fs::remove_dir_all(dir);
    }
}
