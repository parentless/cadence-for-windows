//! Portable validation for the public GitHub release feed used on Windows.
#![cfg_attr(
    not(target_os = "windows"),
    allow(
        dead_code,
        reason = "Windows transport shares its validation tests with macOS"
    )
)]
use anyhow::{Context as _, Result, ensure};
use futures::StreamExt as _;
use serde::Deserialize;

const METADATA_LIMIT: usize = 1024 * 1024;
const CHECKSUM_LIMIT: usize = 1024;

#[derive(Clone, Debug)]
pub(crate) struct Release {
    pub(crate) version: String,
    pub(crate) url: String,
    pub(crate) checksum: String,
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    assets: Vec<GithubAsset>,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
}
#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

fn version(value: &str) -> Option<(u64, u64, u64)> {
    let mut parts = value.strip_prefix('v').unwrap_or(value).split('.');
    let mut number = || {
        let part = parts.next()?;
        if part.is_empty()
            || !part.bytes().all(|b| b.is_ascii_digit())
            || (part.len() > 1 && part.starts_with('0'))
        {
            return None;
        }
        part.parse().ok()
    };
    let value = (number()?, number()?, number()?);
    if parts.next().is_some() {
        None
    } else {
        Some(value)
    }
}

fn release_assets(
    metadata: &[u8],
    repository: &str,
    current: &str,
) -> Result<Option<(String, String, String)>> {
    let release: GithubRelease = serde_json::from_slice(metadata)?;
    if release.draft || release.prerelease {
        return Ok(None);
    }
    let newest = version(&release.tag_name).context("invalid stable release version")?;
    if newest <= version(current).context("invalid installed version")? {
        return Ok(None);
    }
    let name = format!(
        "cadence-{}-windows-x86_64.exe",
        release
            .tag_name
            .strip_prefix('v')
            .unwrap_or(&release.tag_name)
    );
    let url = format!(
        "https://github.com/{repository}/releases/download/{}/{name}",
        release.tag_name
    );
    let checksum_url = format!("{url}.sha256");
    for (name, expected) in [
        (name.clone(), &url),
        (format!("{name}.sha256"), &checksum_url),
    ] {
        let asset = release
            .assets
            .iter()
            .find(|a| a.name == name)
            .context("release is missing a Windows executable or checksum")?;
        ensure!(
            &asset.browser_download_url == expected,
            "release asset is outside this repository or tag"
        );
    }
    Ok(Some((release.tag_name, url, checksum_url)))
}

async fn read_limited(response: reqwest::Response, limit: usize) -> Result<Vec<u8>> {
    ensure!(
        response
            .content_length()
            .is_none_or(|size| size <= limit as u64),
        "release response exceeds size limit"
    );
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk?;
        ensure!(
            bytes.len().saturating_add(chunk.len()) <= limit,
            "release response exceeds size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

fn checksum(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes)?;
    let value = text
        .split_whitespace()
        .next()
        .context("empty update checksum")?;
    ensure!(
        value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()),
        "invalid update checksum"
    );
    Ok(value.to_owned())
}

pub(crate) async fn latest_release(
    client: &reqwest::Client,
    repository: &str,
    current: &str,
) -> Result<Option<Release>> {
    let response = client
        .get(format!(
            "https://api.github.com/repos/{repository}/releases/latest"
        ))
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(None);
    }
    let metadata = read_limited(response.error_for_status()?, METADATA_LIMIT).await?;
    let Some((version, url, checksum_url)) = release_assets(&metadata, repository, current)? else {
        return Ok(None);
    };
    let bytes = read_limited(
        client.get(checksum_url).send().await?.error_for_status()?,
        CHECKSUM_LIMIT,
    )
    .await?;
    Ok(Some(Release {
        version,
        url,
        checksum: checksum(&bytes)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn metadata() -> serde_json::Value {
        serde_json::json!({"tag_name":"v0.9.0", "assets":[
            {"name":"cadence-0.9.0-windows-x86_64.exe", "browser_download_url":"https://github.com/owner/cadence-for-windows/releases/download/v0.9.0/cadence-0.9.0-windows-x86_64.exe"},
            {"name":"cadence-0.9.0-windows-x86_64.exe.sha256", "browser_download_url":"https://github.com/owner/cadence-for-windows/releases/download/v0.9.0/cadence-0.9.0-windows-x86_64.exe.sha256"}
        ]})
    }
    fn parse(value: serde_json::Value, current: &str) -> Result<Option<(String, String, String)>> {
        release_assets(
            &serde_json::to_vec(&value)?,
            "owner/cadence-for-windows",
            current,
        )
    }
    #[test]
    fn stable_updates_are_numeric_and_never_downgrade() {
        assert!(parse(metadata(), "0.8.0").unwrap().is_some());
        assert!(parse(metadata(), "0.9.0").unwrap().is_none());
        assert!(parse(metadata(), "0.10.0").unwrap().is_none());
        for invalid in ["0.8", "01.0.0", "1.0.0-beta", "+1.0.0", "1.2.3.4"] {
            assert!(version(invalid).is_none());
        }
        for field in ["draft", "prerelease"] {
            let mut value = metadata();
            value[field] = true.into();
            assert!(parse(value, "0.8.0").unwrap().is_none());
        }
    }
    #[test]
    fn asset_names_repository_and_tag_must_match() {
        let mut value = metadata();
        value["assets"][0]["browser_download_url"] =
            "https://github.com/another/repository/releases/download/v0.9.0/evil.exe".into();
        assert!(parse(value, "0.8.0").is_err());
        let mut value = metadata();
        value["assets"][0]["name"] = "cadence-0.9.0-windows-x86_64-debug.exe".into();
        assert!(parse(value, "0.8.0").is_err());
        let mut value = metadata();
        value["assets"] = serde_json::json!([]);
        assert!(parse(value, "0.8.0").is_err());
        assert!(checksum(b"invalid").is_err());
        assert!(checksum(format!("{}  app.exe\r\n", "a".repeat(64)).as_bytes()).is_ok());
    }
    #[tokio::test]
    async fn release_responses_are_bounded_even_without_content_length() {
        use std::io::{Read as _, Write as _};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = server.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let _ = socket.read(&mut [0; 4096]);
            let _ = socket.write_all(b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n123456789");
        });
        let client = reqwest::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .get(format!("http://{address}/"))
            .send()
            .await
            .unwrap();
        assert!(read_limited(response, 4).await.is_err());
        worker.join().unwrap();
    }
}
