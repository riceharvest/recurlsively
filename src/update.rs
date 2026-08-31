//! `recurlsively update` — self-update from GitHub Releases.
//!
//! Downloads the newest release asset for the current platform, verifies its
//! SHA-256 checksum against the published SHA256SUMS, and atomically replaces
//! the running binary.

use std::io::Write;
use std::path::PathBuf;

use crate::resolver::SafeResolver;

const REPO: &str = "riceharvest/recurlsively";
const FALLBACK_INSTALL_DIR: &str = ".local/bin";

#[derive(Debug)]
pub enum UpdateError {
    Network(String),
    NoAsset(String),
    Checksum {
        expected: String,
        actual: String,
    },
    Io(std::io::Error),
    /// Already running the latest version.
    UpToDate(String),
}

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network(e) => write!(f, "network error: {e}"),
            Self::NoAsset(message) => write!(f, "{message}"),
            Self::Checksum { expected, actual } => {
                write!(f, "checksum mismatch: expected {expected}, got {actual}")
            }
            Self::Io(e) => write!(f, "I/O error: {e}"),
            Self::UpToDate(v) => write!(f, "already up to date ({v})"),
        }
    }
}

fn client() -> Result<reqwest::Client, UpdateError> {
    reqwest::Client::builder()
        .user_agent(concat!("recurlsively/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .redirect(reqwest::redirect::Policy::limited(10))
        .dns_resolver(std::sync::Arc::new(SafeResolver::new(false)))
        .build()
        .map_err(|e| UpdateError::Network(e.to_string()))
}

#[derive(serde::Deserialize)]
struct LatestRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<ReleaseAsset>,
}

#[derive(serde::Deserialize)]
struct ReleaseAsset {
    name: String,
    #[serde(rename = "browser_download_url")]
    url: String,
}

/// Best-effort platform target matching the release asset naming.
fn target_triple() -> Result<&'static str, UpdateError> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Ok("x86_64-unknown-linux-musl");
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Ok("aarch64-unknown-linux-musl");
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    return Ok("x86_64-apple-darwin");
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    return Ok("aarch64-apple-darwin");
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    return Ok("x86_64-pc-windows-msvc");
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "windows", target_arch = "x86_64")
    )))]
    return Err(UpdateError::NoAsset(
        "no prebuilt asset for this platform; build from source instead".to_owned(),
    ));
}

fn version_tag(version: &str) -> String {
    if version.starts_with('v') {
        version.to_owned()
    } else {
        format!("v{version}")
    }
}

fn extract_tarball(archive: &[u8], directory: &std::path::Path) -> Result<(), UpdateError> {
    let decoder = flate2::read::GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    tar.unpack(directory).map_err(UpdateError::Io)
}

#[cfg(target_os = "windows")]
fn extract_zip(archive: &[u8], directory: &std::path::Path) -> Result<(), UpdateError> {
    let reader = std::io::Cursor::new(archive);
    let mut zip = zip::ZipArchive::new(reader).map_err(|e| {
        UpdateError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            e.to_string(),
        ))
    })?;
    zip.extract(directory).map_err(UpdateError::Io)
}

/// Runs `recurlsively update` and returns a human-readable report line.
pub async fn run_update() -> Result<String, UpdateError> {
    let current = env!("CARGO_PKG_VERSION");
    let target = target_triple()?;

    let http = client()?;
    let release: LatestRelease = http
        .get(&format!(
            "https://api.github.com/repos/{REPO}/releases/latest"
        ))
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| UpdateError::Network(e.to_string()))?
        .error_for_status()
        .map_err(|e| UpdateError::Network(e.to_string()))?
        .json()
        .await
        .map_err(|e| UpdateError::Network(e.to_string()))?;

    if release.tag_name == version_tag(current) {
        return Err(UpdateError::UpToDate(format!("recurlsively {current}")));
    }

    let short_version = release.tag_name.trim_start_matches('v');
    let archive_name = if cfg!(target_os = "windows") {
        format!("recurlsively-{short_version}-{target}.zip")
    } else {
        format!("recurlsively-{short_version}-{target}.tar.gz")
    };
    let checksum_name = "SHA256SUMS";

    let asset = |name: &str| -> Result<&ReleaseAsset, UpdateError> {
        release
            .assets
            .iter()
            .find(|asset| asset.name == name)
            .ok_or_else(|| {
                UpdateError::NoAsset(format!(
                    "release {} has no asset `{name}`",
                    release.tag_name
                ))
            })
    };
    let archive_asset = asset(&archive_name)?;
    let sums_asset = asset(checksum_name)?;

    let sums = http
        .get(&sums_asset.url)
        .send()
        .await
        .map_err(|e| UpdateError::Network(e.to_string()))?
        .error_for_status()
        .map_err(|e| UpdateError::Network(e.to_string()))?
        .text()
        .await
        .map_err(|e| UpdateError::Network(e.to_string()))?;
    let expected = sums
        .lines()
        .filter_map(|line| {
            let (hash, file) = line.split_once("  ")?;
            (file == archive_name).then(|| hash.to_owned())
        })
        .next()
        .ok_or_else(|| {
            UpdateError::NoAsset(format!("{checksum_name} has no entry for {archive_name}"))
        })?;

    let archive = http
        .get(&archive_asset.url)
        .send()
        .await
        .map_err(|e| UpdateError::Network(e.to_string()))?
        .error_for_status()
        .map_err(|e| UpdateError::Network(e.to_string()))?
        .bytes()
        .await
        .map_err(|e| UpdateError::Network(e.to_string()))?;

    use sha2::Digest;
    let actual = sha2::Sha256::digest(&archive)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    if actual != expected {
        return Err(UpdateError::Checksum { expected, actual });
    }

    // Extract to a temp dir, locate the inner binary, atomically replace self.
    let staging = std::env::temp_dir().join(format!(
        "recurlsively-update-{}-{}",
        std::process::id(),
        short_version
    ));
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(UpdateError::Io)?;

    if cfg!(target_os = "windows") {
        #[cfg(target_os = "windows")]
        extract_zip(&archive, &staging)?;
    } else {
        extract_tarball(&archive, &staging)?;
    }

    let inner_binary = staging
        .join(format!("recurlsively-{short_version}-{target}"))
        .join(if cfg!(target_os = "windows") {
            "recurlsively.exe"
        } else {
            "recurlsively"
        });
    if !inner_binary.is_file() {
        return Err(UpdateError::NoAsset(format!(
            "archive did not contain {}",
            inner_binary.display()
        )));
    }

    let current_exe = std::env::current_exe().map_err(UpdateError::Io)?;
    let destination = match std::env::var("RECURSIVELY_INSTALL_DIR") {
        Ok(dir) => PathBuf::from(dir).join(
            current_exe
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
        ),
        Err(_) => current_exe.clone(),
    };

    // Atomic replace: rename over the running binary (POSIX allows this).
    let backup = destination.with_extension("old");
    let _ = std::fs::remove_file(&backup);
    std::fs::rename(&destination, &backup).map_err(UpdateError::Io)?;
    if let Err(error) = std::fs::copy(&inner_binary, &destination) {
        // Roll the old binary back so the CLI never disappears.
        let _ = std::fs::rename(&backup, &destination);
        return Err(UpdateError::Io(error));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&destination, std::fs::Permissions::from_mode(0o755));
    }
    let _ = std::fs::remove_file(&backup);
    let _ = std::fs::remove_dir_all(&staging);

    let new_version = match http
        .get("https://api.github.com/repos/riceharvest/recurlsively/releases/latest")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
    {
        Ok(response) => response
            .json::<LatestRelease>()
            .await
            .map(|release| release.tag_name)
            .unwrap_or_else(|_| release.tag_name.clone()),
        Err(_) => release.tag_name.clone(),
    };

    Ok(format!(
        "updated recurlsively {current} -> {}",
        new_version.trim_start_matches('v')
    ))
}

// Keep unused-import lint quiet on Windows-only deps.
#[allow(dead_code)]
fn touch(w: &mut dyn Write) {
    let _ = w.write_all(&[]);
}
