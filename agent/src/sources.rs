//! Resolves document sources (`data`, `path`, `url`) into bytes.
//!
//! Reading files or fetching URLs on behalf of a client turns the agent into a file-read
//! or request-forgery primitive, so both are disabled unless an administrator lists
//! allowed folders / URL prefixes in `[sources]`. Errors never reveal whether a file
//! outside the allowed folders exists.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

use bytes::Bytes;
use kiln_core::error::{ErrorCode, PrintError, Result};

use crate::api::wire::SourceSpec;
use crate::config::SourcesConfig;

pub async fn resolve(config: &SourcesConfig, spec: SourceSpec, max_bytes: u64) -> Result<Bytes> {
    match spec {
        SourceSpec::Inline { data, encoding } => encoding.decode(&data, max_bytes),
        SourceSpec::Path(path) => {
            let roots = config.allowed_paths.clone();
            blocking(move || read_path(&roots, Path::new(&path), max_bytes)).await
        }
        SourceSpec::Url(url) => {
            let prefixes = config.allowed_url_prefixes.clone();
            let timeout = Duration::from_secs(config.fetch_timeout_secs.max(1));
            blocking(move || fetch_url(&prefixes, &url, timeout, max_bytes)).await
        }
    }
}

async fn blocking<F: FnOnce() -> Result<Bytes> + Send + 'static>(f: F) -> Result<Bytes> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(PrintError::internal)?
}

fn denied(message: &str) -> PrintError {
    PrintError::new(ErrorCode::AccessDenied, message)
}

fn too_large(limit: u64) -> PrintError {
    PrintError::new(
        ErrorCode::PayloadTooLarge,
        format!("document exceeds the {limit}-byte limit"),
    )
}

fn read_path(roots: &[PathBuf], path: &Path, max_bytes: u64) -> Result<Bytes> {
    if roots.is_empty() {
        return Err(denied(
            "printing files by path is disabled on this agent (see sources.allowed_paths)",
        ));
    }
    // One message for "missing", "unreadable" and "outside": no existence oracle.
    let refuse = || denied("the path is not readable or is outside the allowed folders");
    if !path.is_absolute() {
        return Err(PrintError::invalid_payload("path must be absolute"));
    }
    let resolved = path.canonicalize().map_err(|_| refuse())?;
    let inside = roots
        .iter()
        .filter_map(|root| root.canonicalize().ok())
        .any(|root| resolved.starts_with(root));
    if !inside || !resolved.is_file() {
        return Err(refuse());
    }
    let file = std::fs::File::open(&resolved).map_err(|_| refuse())?;
    let mut data = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut data)
        .map_err(|_| refuse())?;
    if data.len() as u64 > max_bytes {
        return Err(too_large(max_bytes));
    }
    Ok(Bytes::from(data))
}

fn url_allowed(prefixes: &[String], url: &str) -> bool {
    // Refuse anything that could make a prefix match misleading.
    if url.contains(['@', '\\', ' ', '\t', '\r', '\n'])
        || url.contains("/../")
        || url.contains("%2e%2e")
        || url.contains("%2E%2E")
    {
        return false;
    }
    let lower = url.to_ascii_lowercase();
    prefixes
        .iter()
        .any(|p| lower.starts_with(&p.to_ascii_lowercase()))
}

fn fetch_url(prefixes: &[String], url: &str, timeout: Duration, max_bytes: u64) -> Result<Bytes> {
    if prefixes.is_empty() {
        return Err(denied(
            "fetching documents by URL is disabled on this agent (see sources.allowed_url_prefixes)",
        ));
    }
    if !url_allowed(prefixes, url) {
        return Err(denied("the URL is not under an allowed prefix"));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(timeout))
        // A redirect could leave the allow-listed prefix.
        .max_redirects(0)
        .http_status_as_error(false)
        .build()
        .into();
    let mut response = agent.get(url).call().map_err(|e| {
        PrintError::new(
            ErrorCode::ConnectionError,
            format!("could not fetch the document: {e}"),
        )
    })?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(PrintError::new(
            ErrorCode::ConnectionError,
            format!("fetching the document returned HTTP {status}"),
        )
        .recoverable(status >= 500));
    }
    let data = response
        .body_mut()
        .with_config()
        .limit(max_bytes + 1)
        .read_to_vec()
        .map_err(|e| {
            PrintError::new(
                ErrorCode::ConnectionError,
                format!("reading the document failed: {e}"),
            )
        })?;
    if data.len() as u64 > max_bytes {
        return Err(too_large(max_bytes));
    }
    Ok(Bytes::from(data))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::wire::DataEncoding;

    fn temp_root() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kiln-sources-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(dir.join("allowed")).expect("mkdir");
        std::fs::write(dir.join("allowed").join("doc.pdf"), b"%PDF-1.7 test").expect("write");
        std::fs::write(dir.join("secret.pdf"), b"%PDF-secret").expect("write");
        dir
    }

    #[tokio::test]
    async fn inline_data_decodes() {
        let spec = SourceSpec::Inline {
            data: "JVBERi0=".into(),
            encoding: DataEncoding::Base64,
        };
        let bytes = resolve(&SourcesConfig::default(), spec, 100)
            .await
            .expect("inline");
        assert_eq!(&bytes[..], b"%PDF-");
    }

    #[tokio::test]
    async fn paths_are_confined_to_allowed_folders() {
        let dir = temp_root();
        let config = SourcesConfig {
            allowed_paths: vec![dir.join("allowed")],
            ..SourcesConfig::default()
        };
        let ok = resolve(
            &config,
            SourceSpec::Path(dir.join("allowed/doc.pdf").display().to_string()),
            100,
        )
        .await;
        assert_eq!(&ok.expect("allowed")[..], b"%PDF-1.7 test");

        let escape = dir.join("allowed").join("..").join("secret.pdf");
        let err = resolve(&config, SourceSpec::Path(escape.display().to_string()), 100)
            .await
            .expect_err("escape");
        let missing = resolve(
            &config,
            SourceSpec::Path(dir.join("nope.pdf").display().to_string()),
            100,
        )
        .await
        .expect_err("missing");
        assert_eq!(err.error_code, ErrorCode::AccessDenied);
        assert_eq!(err.message, missing.message, "no existence oracle");

        let big = resolve(
            &config,
            SourceSpec::Path(dir.join("allowed/doc.pdf").display().to_string()),
            4,
        )
        .await;
        assert_eq!(
            big.expect_err("limit").error_code,
            ErrorCode::PayloadTooLarge
        );

        let disabled = resolve(
            &SourcesConfig::default(),
            SourceSpec::Path(dir.join("allowed/doc.pdf").display().to_string()),
            100,
        )
        .await;
        assert_eq!(
            disabled.expect_err("disabled").error_code,
            ErrorCode::AccessDenied
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn url_prefix_matching_is_strict() {
        let prefixes = vec!["https://files.example.com/labels/".to_owned()];
        assert!(url_allowed(
            &prefixes,
            "https://files.example.com/labels/a.pdf"
        ));
        assert!(url_allowed(
            &prefixes,
            "HTTPS://FILES.example.com/labels/a.pdf"
        ));
        for bad in [
            "https://files.example.com/other/a.pdf",
            "https://files.example.com.evil.net/labels/a.pdf",
            "https://files.example.com/labels/../admin",
            "https://files.example.com/labels/%2e%2e/admin",
            "https://user@files.example.com/labels/a.pdf",
            "http://files.example.com/labels/a.pdf",
        ] {
            assert!(!url_allowed(&prefixes, bad), "{bad}");
        }
    }

    #[tokio::test]
    async fn urls_are_disabled_by_default() {
        let err = resolve(
            &SourcesConfig::default(),
            SourceSpec::Url("https://x.example/a.pdf".into()),
            100,
        )
        .await
        .expect_err("disabled");
        assert_eq!(err.error_code, ErrorCode::AccessDenied);
    }

    #[tokio::test]
    async fn url_fetch_reads_allowed_documents_and_refuses_redirects() {
        use std::io::Write;
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for (i, stream) in listener.incoming().take(2).enumerate() {
                let mut stream = stream.expect("conn");
                let mut buf = [0u8; 1024];
                let _ = stream.read(&mut buf);
                let reply: &[u8] = if i == 0 {
                    b"HTTP/1.1 200 OK\r\nContent-Length: 8\r\nConnection: close\r\n\r\n%PDF-1.7"
                } else {
                    b"HTTP/1.1 302 Found\r\nLocation: http://169.254.169.254/\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                };
                let _ = stream.write_all(reply);
            }
        });
        let config = SourcesConfig {
            allowed_url_prefixes: vec![format!("http://127.0.0.1:{port}/docs/")],
            ..SourcesConfig::default()
        };
        let ok = resolve(
            &config,
            SourceSpec::Url(format!("http://127.0.0.1:{port}/docs/a.pdf")),
            100,
        )
        .await;
        assert_eq!(&ok.expect("fetched")[..], b"%PDF-1.7");
        let redirect = resolve(
            &config,
            SourceSpec::Url(format!("http://127.0.0.1:{port}/docs/b.pdf")),
            100,
        )
        .await;
        assert_eq!(
            redirect.expect_err("redirect").error_code,
            ErrorCode::ConnectionError
        );
    }
}
