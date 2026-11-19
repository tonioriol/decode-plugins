//! The embedding model: the pinned revision of `minishlab/potion-code-16M-v2`, downloaded from Hugging Face on first
//! use into `<storage>/semble-model-<revision>` (where Decode keeps it too), checked against its SHA-256 before it is published, and rechecked on every
//! later use. A failed download or check publishes nothing.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use decode_semantic_core::model_assets::{CODE_MODEL_FILES, code_model_base_url};
use sha2::{Digest, Sha256};

pub use decode_semantic_core::model_assets::{
    CODE_MODEL_REVISION as MODEL_REVISION, code_model_dir as model_dir,
};
/// Hosts a download may be redirected to.
const TRUSTED_DOMAINS: &[&str] = &["huggingface.co", "hf.co"];
/// Cap on one model file.
const MAX_BYTES: u64 = 70 * 1024 * 1024;
const MAX_REDIRECTS: u32 = 5;
const TIMEOUT: Duration = Duration::from_secs(120);

/// Where the model comes from and what it must hash to.
#[derive(Debug, Clone)]
struct Source {
    /// URL prefix joined with each file name.
    base_url: String,
    /// `(file name, lowercase hex SHA-256)`.
    files: Vec<(String, String)>,
    trusted_domains: Vec<String>,
    /// Tests only: also accept `http://` loopback URLs whose host is trusted.
    allow_loopback_http: bool,
}

impl Source {
    fn pinned() -> Self {
        Self {
            base_url: code_model_base_url(),
            files: CODE_MODEL_FILES
                .iter()
                .map(|(name, sha)| ((*name).to_owned(), (*sha).to_owned()))
                .collect(),
            trusted_domains: TRUSTED_DOMAINS
                .iter()
                .map(|domain| (*domain).to_owned())
                .collect(),
            allow_loopback_http: false,
        }
    }
}

/// The verified model folder under `storage`, downloaded first when it is missing. An existing install is
/// rechecked, never trusted.
pub fn ensure_model(storage: &Path) -> Result<PathBuf, String> {
    ensure(storage, &Source::pinned())
}

/// `(scheme, host, rest)` of an absolute URL, `rest` starting at the path.
fn split_url(url: &str) -> Option<(&str, &str, &str)> {
    let (scheme, after) = url.split_once("://")?;
    let (authority, rest) = after.find('/').map_or((after, ""), |at| after.split_at(at));
    let host = authority.rsplit('@').next()?;
    let host = host
        .rsplit_once(':')
        .filter(|(_, port)| port.bytes().all(|b| b.is_ascii_digit()))
        .map_or(host, |(name, _)| name);
    Some((scheme, host, rest))
}

fn is_trusted(url: &str, source: &Source) -> bool {
    let Some((scheme, host, _)) = split_url(url) else {
        return false;
    };
    let loopback = matches!(host, "127.0.0.1" | "localhost");
    let scheme_ok = scheme == "https" || (source.allow_loopback_http && scheme == "http" && loopback);
    scheme_ok
        && source
            .trusted_domains
            .iter()
            .any(|domain| host == domain || host.ends_with(&format!(".{domain}")))
}

/// The URL a redirect leads to, refused when the redirects are used up, the `Location` is missing or relative to
/// nothing we understand, or the target is not trusted.
fn redirect_target(
    url: &str,
    location: Option<&str>,
    remaining: u32,
    source: &Source,
) -> Result<String, String> {
    if remaining == 0 {
        return Err("too many model download redirects".to_owned());
    }
    let location = location.ok_or("model download redirect has no Location header")?;
    let next = if location.contains("://") {
        location.to_owned()
    } else if location.starts_with('/') {
        let (scheme, _, _) = split_url(url).ok_or("bad model URL")?;
        let authority = url
            .split_once("://")
            .and_then(|(_, after)| after.split('/').next())
            .ok_or("bad model URL")?;
        format!("{scheme}://{authority}{location}")
    } else {
        return Err("model download redirect is not an absolute URL or path".to_owned());
    };
    if !is_trusted(&next, source) {
        return Err("model download redirected to an untrusted host".to_owned());
    }
    Ok(next)
}

fn download(agent: &ureq::Agent, url: &str, destination: &Path, source: &Source) -> Result<(), String> {
    let mut url = url.to_owned();
    let mut remaining = MAX_REDIRECTS;
    loop {
        if !is_trusted(&url, source) {
            return Err(format!("model download URL is not a trusted HTTPS host: {url}"));
        }
        let response = match agent.get(&url).call() {
            Ok(response) | Err(ureq::Error::Status(_, response)) => response,
            Err(error) => return Err(error.to_string()),
        };
        let status = response.status();
        if matches!(status, 301 | 302 | 303 | 307 | 308) {
            url = redirect_target(&url, response.header("location"), remaining, source)?;
            remaining -= 1;
            continue;
        }
        if status != 200 {
            return Err(format!("model download failed with HTTP {status}"));
        }
        let declared = response
            .header("content-length")
            .and_then(|value| value.parse::<u64>().ok());
        if declared.is_some_and(|size| size > MAX_BYTES) {
            return Err("model file exceeds the download size limit".to_owned());
        }
        let mut file = fs::File::create_new(destination).map_err(|error| error.to_string())?;
        let copied = io::copy(&mut response.into_reader().take(MAX_BYTES + 1), &mut file)
            .map_err(|error| error.to_string())?;
        if copied > MAX_BYTES {
            return Err("model file exceeds the download size limit".to_owned());
        }
        return Ok(());
    }
}

fn sha256_hex(file: &Path) -> io::Result<String> {
    let mut reader = fs::File::open(file)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1 << 16];
    loop {
        let read = reader.read(&mut buffer)?;
        let Some(chunk) = buffer.get(..read).filter(|chunk| !chunk.is_empty()) else {
            break;
        };
        hasher.update(chunk);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

/// Every model file is a regular file with its pinned checksum.
fn verify(folder: &Path, files: &[(String, String)]) -> Result<(), String> {
    for (name, expected) in files {
        let file = folder.join(name);
        let meta = fs::symlink_metadata(&file).map_err(|error| format!("{name}: {error}"))?;
        if !meta.is_file() {
            return Err(format!("model file is not regular: {name}"));
        }
        if sha256_hex(&file).map_err(|error| format!("{name}: {error}"))? != *expected {
            return Err(format!("model checksum mismatch: {name}"));
        }
    }
    Ok(())
}

static STAGING: AtomicU64 = AtomicU64::new(0);

fn ensure(storage: &Path, source: &Source) -> Result<PathBuf, String> {
    fs::create_dir_all(storage).map_err(|error| error.to_string())?;
    let folder = model_dir(storage);
    match fs::symlink_metadata(&folder) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.to_string()),
        Ok(meta) if !meta.is_dir() => return Err("model path is not a directory".to_owned()),
        Ok(_) => {
            verify(&folder, &source.files)?;
            return Ok(folder);
        }
    }
    // Each attempt stages into its own folder, so concurrent installs never share files.
    let staging = storage.join(format!(
        ".model-new-{}-{}",
        std::process::id(),
        STAGING.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&staging).map_err(|error| error.to_string())?;
    let agent = ureq::AgentBuilder::new()
        .redirects(0)
        .timeout_connect(TIMEOUT)
        .timeout_read(TIMEOUT)
        .build();
    let result = (|| {
        for (name, _) in &source.files {
            download(
                &agent,
                &format!("{}/{name}", source.base_url),
                &staging.join(name),
                source,
            )?;
        }
        verify(&staging, &source.files)?;
        fs::rename(&staging, &folder).map_err(|error| error.to_string())
    })();
    if result.is_err() {
        drop(fs::remove_dir_all(&staging));
    }
    result.map(|()| folder)
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;

    use super::*;

    const FIXTURES: &[(&str, &[u8])] = &[
        ("config.json", b"cfg"),
        ("tokenizer.json", b"tok"),
        ("model.safetensors", b"weights"),
    ];

    fn sha(bytes: &[u8]) -> String {
        Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect()
    }

    /// Serves `respond(path) -> (status, location, body)` for `expected` requests on a loopback port.
    fn serve(
        respond: impl Fn(&str) -> (u16, Option<String>, Vec<u8>) + Send + 'static,
        expected: usize,
    ) -> u16 {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            for _ in 0..expected {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk) {
                        Ok(0) | Err(_) => break,
                        Ok(read) => request.extend_from_slice(chunk.get(..read).unwrap()),
                    }
                }
                let text = String::from_utf8_lossy(&request).into_owned();
                let path = text.split(' ').nth(1).unwrap_or_default().to_owned();
                let (status, location, body) = respond(&path);
                let mut reply = format!(
                    "HTTP/1.1 {status} X\r\ncontent-length: {}\r\nconnection: close\r\n",
                    body.len()
                );
                if let Some(location) = location {
                    reply.push_str(&format!("location: {location}\r\n"));
                }
                reply.push_str("\r\n");
                drop(stream.write_all(reply.as_bytes()));
                drop(stream.write_all(&body));
            }
        });
        port
    }

    fn test_source(port: u16, corrupt: bool) -> Source {
        Source {
            base_url: format!("http://127.0.0.1:{port}/m"),
            files: FIXTURES
                .iter()
                .map(|(name, body)| {
                    (
                        (*name).to_owned(),
                        if corrupt { sha(b"other") } else { sha(body) },
                    )
                })
                .collect(),
            trusted_domains: vec!["127.0.0.1".to_owned()],
            allow_loopback_http: true,
        }
    }

    fn respond_with_fixtures(path: &str) -> (u16, Option<String>, Vec<u8>) {
        FIXTURES
            .iter()
            .find(|(name, _)| path.ends_with(name))
            .map_or((404, None, Vec::new()), |(_, body)| (200, None, body.to_vec()))
    }

    #[test]
    fn downloads_verifies_publishes_and_rechecks_the_cache() {
        let storage = tempfile::tempdir().unwrap();
        let port = serve(respond_with_fixtures, 3);
        let source = test_source(port, false);
        let folder = ensure(storage.path(), &source).unwrap();
        assert_eq!(fs::read(folder.join("tokenizer.json")).unwrap(), b"tok");
        // Cached: no server is left to answer, so a download attempt would fail.
        assert_eq!(ensure(storage.path(), &source).unwrap(), folder);
        fs::write(folder.join("config.json"), b"tampered").unwrap();
        assert!(
            ensure(storage.path(), &source)
                .unwrap_err()
                .contains("checksum mismatch")
        );
    }

    #[test]
    fn a_bad_checksum_publishes_nothing() {
        let storage = tempfile::tempdir().unwrap();
        let port = serve(respond_with_fixtures, 3);
        let error = ensure(storage.path(), &test_source(port, true)).unwrap_err();
        assert!(error.contains("checksum mismatch"), "{error}");
        assert_eq!(fs::read_dir(storage.path()).unwrap().count(), 0);
    }

    #[test]
    fn redirects_stay_on_trusted_hosts() {
        let source = Source::pinned();
        let url = "https://huggingface.co/a/b";
        assert_eq!(
            redirect_target(url, Some("https://cdn-lfs.hf.co/x"), 5, &source).unwrap(),
            "https://cdn-lfs.hf.co/x"
        );
        assert_eq!(
            redirect_target(url, Some("/api/y"), 5, &source).unwrap(),
            "https://huggingface.co/api/y"
        );
        assert!(redirect_target(url, Some("https://evil.example/x"), 5, &source).is_err());
        assert!(redirect_target(url, Some("https://nothuggingface.co/x"), 5, &source).is_err());
        assert!(redirect_target(url, Some("http://huggingface.co/x"), 5, &source).is_err());
        assert!(redirect_target(url, Some("https://huggingface.co/x"), 0, &source).is_err());
        assert!(redirect_target(url, None, 5, &source).is_err());
    }
}
