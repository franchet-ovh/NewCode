//! Anonymous HTTPS access to the official release catalog; no stored credentials are read.
use focus_addon_sdk::{Catalog, Package};
use std::{
    error::Error,
    io::{Read, Write},
    path::Path,
    time::Duration,
};
const REPOSITORY: &str = "https://github.com/franchet-ovh/NewCode";
fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .https_only(true)
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            log::debug!(
                "Addon HTTPS redirect hop={} destination={}",
                attempt.previous().len(),
                diagnostic_url(attempt.url())
            );
            if attempt.previous().len() >= 5 {
                attempt.error("Addon HTTPS redirect limit exceeded")
            } else {
                attempt.follow()
            }
        }))
        .user_agent("focus-editor-addon-manager")
        .build()
        .map_err(|e| e.to_string())
}
// Signed GitHub asset URLs contain temporary credentials in their query strings.
fn diagnostic_url(url: &reqwest::Url) -> String {
    let mut url = url.clone();
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.to_string()
}
fn request_error(error: reqwest::Error) -> String {
    let error = error.without_url();
    let mut message = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    crate::logging::redaction::sanitize(&message).join("; ")
}
fn get(
    client: &reqwest::blocking::Client,
    url: reqwest::Url,
    operation: &str,
) -> Result<reqwest::blocking::Response, String> {
    let started = std::time::Instant::now();
    let destination = diagnostic_url(&url);
    log::info!("Addon {operation}: GET {destination}");
    let response = client.get(url).send().map_err(|error| {
        format!(
            "Addon {operation} request to {destination} failed after {} ms: {}",
            started.elapsed().as_millis(),
            request_error(error)
        )
    })?;
    log::info!(
        "Addon {operation}: HTTP {} final_url={} elapsed_ms={}",
        response.status(),
        diagnostic_url(response.url()),
        started.elapsed().as_millis()
    );
    for name in [
        "content-type",
        "content-length",
        "x-github-request-id",
        "x-ratelimit-remaining",
        "x-ratelimit-reset",
        "retry-after",
    ] {
        if let Some(value) = response.headers().get(name) {
            log::debug!("Addon {operation} response {name}={value:?}");
        }
    }
    Ok(response)
}
fn url(tag: &str, asset: &str) -> reqwest::Url {
    let mut url = reqwest::Url::parse(REPOSITORY).unwrap();
    url.path_segments_mut()
        .unwrap()
        .extend(["releases", "download", tag, asset]);
    url
}
pub(super) fn fetch(tag: &str) -> Result<Catalog, String> {
    let client = client()?;
    if tag.trim() == "latest" {
        return fetch_latest(&client);
    }
    fetch_tag(&client, tag.trim())
}
fn fetch_tag(client: &reqwest::blocking::Client, tag: &str) -> Result<Catalog, String> {
    if tag.is_empty() {
        return Err("Choose a release tag or latest".into());
    }
    log::info!("Fetching addon catalog for release {tag:?}");
    let response = get(client, url(tag, "addons.json"), "catalog fetch")?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Err("This release has no available addon catalog. Select a release published with addon packages.".into());
    }
    let response = response
        .error_for_status()
        .map_err(|e| format!("Cannot fetch addon catalog: {}", request_error(e)))?;
    let mut bytes = Vec::new();
    response
        .take((focus_addon_sdk::MAX_MESSAGE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > focus_addon_sdk::MAX_MESSAGE {
        return Err("Addon catalog exceeds size limit".into());
    }
    let mut catalog: Catalog = serde_json::from_slice(&bytes)
        .map_err(|e| format!("Cannot parse addon catalog for {tag}: {e}"))?;
    catalog
        .validate()
        .map_err(|e| format!("Invalid addon catalog for {tag}: {e}"))?;
    if catalog.release != tag {
        return Err("Catalog does not match the selected release".into());
    }
    for package in &mut catalog.packages {
        package.source_release = Some(tag.to_owned());
    }
    log::info!(
        "Loaded addon catalog release={} packages={} bytes={}",
        catalog.release,
        catalog.packages.len(),
        bytes.len()
    );
    Ok(catalog)
}
const INDEX_URL: &str = "https://raw.githubusercontent.com/franchet-ovh/NewCode/main/addons.json";

fn fetch_latest(client: &reqwest::blocking::Client) -> Result<Catalog, String> {
    let response = get(
        client,
        reqwest::Url::parse(INDEX_URL).unwrap(),
        "index fetch",
    )?
    .error_for_status()
    .map_err(|e| format!("Cannot fetch addon index: {}", request_error(e)))?;
    index_catalog(response)
}

fn index_catalog(reader: impl Read) -> Result<Catalog, String> {
    let mut bytes = Vec::new();
    reader
        .take((focus_addon_sdk::MAX_MESSAGE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > focus_addon_sdk::MAX_MESSAGE {
        return Err("Addon index exceeds size limit".into());
    }
    let index: focus_addon_sdk::AddonIndex =
        serde_json::from_slice(&bytes).map_err(|e| format!("Cannot parse addon index: {e}"))?;
    let catalog = index.catalog_for(
        env!("CARGO_PKG_VERSION"),
        super::packages::target().unwrap_or("unsupported"),
    )?;
    log::info!(
        "Loaded addon index packages={} bytes={}",
        catalog.packages.len(),
        bytes.len()
    );
    Ok(catalog)
}

pub(super) fn download(
    tag: &str,
    package: &Package,
    path: &Path,
    mut progress: impl FnMut(String),
) -> Result<(), String> {
    let mut response = get(&client()?, url(tag, &package.archive), "package download")?
        .error_for_status()
        .map_err(request_error)?;
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut total = 0u64;
    let mut buffer = [0u8; 64 * 1024];
    let mut notified = std::time::Instant::now();
    loop {
        let count = response.read(&mut buffer).map_err(|e| e.to_string())?;
        if count == 0 {
            break;
        }
        total += count as u64;
        if total > package.size || total > super::packages::MAX_ARCHIVE {
            return Err("Downloaded addon exceeds its declared size".into());
        }
        file.write_all(&buffer[..count])
            .map_err(|e| e.to_string())?;
        if notified.elapsed() > Duration::from_millis(200) {
            progress(format!(
                "Downloading {}: {}%",
                package.name,
                total * 100 / package.size
            ));
            notified = std::time::Instant::now();
        }
    }
    if total != package.size {
        return Err("Addon download was interrupted or incomplete".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn central_index_filters_before_installation_and_rejects_bad_responses() {
        let catalog = index_catalog(include_bytes!("../../addons.json").as_slice()).unwrap();
        assert!(catalog.packages.iter().all(|package| {
            Some(package.target.as_str()) == super::super::packages::target()
                && focus_addon_sdk::check_compatibility(
                    package.editor_version_req.as_deref(),
                    package.protocol,
                    env!("CARGO_PKG_VERSION"),
                )
                .is_ok()
                && package.source_release.is_some()
        }));
        assert!(index_catalog(b"invalid JSON".as_slice()).is_err());
        assert!(index_catalog(b"{\"schema\":99,\"addons\":[]}".as_slice()).is_err());
        assert!(
            index_catalog(std::io::repeat(b' '))
                .unwrap_err()
                .contains("size limit")
        );
    }

    #[test]
    fn diagnostic_urls_omit_credentials_and_signed_queries() {
        let url = reqwest::Url::parse(
            "https://user:password@release-assets.githubusercontent.com/file?sig=private#secret",
        )
        .unwrap();
        assert_eq!(
            diagnostic_url(&url),
            "https://release-assets.githubusercontent.com/file"
        );
    }

    #[test]
    fn transport_errors_include_causes_without_request_url() {
        let client = reqwest::blocking::Client::builder()
            .https_only(true)
            .build()
            .unwrap();
        let error = client
            .get("http://example.invalid/catalog?sig=private")
            .send()
            .unwrap_err();
        let detail = request_error(error);
        assert!(detail.contains(": "), "Expected underlying cause: {detail}");
        assert!(!detail.contains("sig=private"));
        assert!(!detail.contains("example.invalid"));
    }
}
