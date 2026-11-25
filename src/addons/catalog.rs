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
fn release_identity(tag: &str) -> Option<(&str, semver::Version)> {
    let (id, version) = tag.strip_prefix("addon_")?.rsplit_once('_')?;
    let version = semver::Version::parse(version).ok()?;
    (focus_addon_sdk::valid_id(id) && version.pre.is_empty() && version.build.is_empty())
        .then_some((id, version))
}

fn compatible(package: &Package) -> bool {
    focus_addon_sdk::check_compatibility(
        package.editor_version_req.as_deref(),
        package.protocol,
        env!("CARGO_PKG_VERSION"),
    )
    .is_ok()
}

fn fetch_latest(client: &reqwest::blocking::Client) -> Result<Catalog, String> {
    let releases = discover_releases(|page| {
        // Paginate explicitly: GitHub's latest endpoint represents one editor release,
        // whereas each addon has its own version history.
        let endpoint = format!(
            "https://api.github.com/repos/franchet-ovh/NewCode/releases?per_page=100&page={page}"
        );
        let response = get(
            client,
            reqwest::Url::parse(&endpoint).unwrap(),
            "release discovery",
        )?
        .error_for_status()
        .map_err(request_error)?;
        let mut bytes = Vec::new();
        response
            .take((focus_addon_sdk::MAX_MESSAGE + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() > focus_addon_sdk::MAX_MESSAGE {
            return Err("Release discovery exceeds size limit".into());
        }
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    })?;
    select_packages(releases, |tag| fetch_tag(client, tag))
}

type Release = (String, semver::Version, String);

fn discover_releases(
    mut page_entries: impl FnMut(usize) -> Result<Vec<serde_json::Value>, String>,
) -> Result<Vec<Release>, String> {
    let mut releases = Vec::new();
    for page in 1..=100 {
        let entries = page_entries(page)?;
        let complete = entries.len() < 100;
        for entry in entries {
            if entry["draft"] == true || entry["prerelease"] == true {
                continue;
            }
            if let Some(tag) = entry["tag_name"].as_str() {
                if let Some((id, version)) = release_identity(tag) {
                    releases.push((id.to_owned(), version, tag.to_owned()));
                }
            }
        }
        if complete {
            break;
        }
        if page == 100 {
            return Err("Too many release pages; select an explicit addon tag".into());
        }
    }
    Ok(releases)
}

fn select_packages(
    mut releases: Vec<Release>,
    mut load: impl FnMut(&str) -> Result<Catalog, String>,
) -> Result<Catalog, String> {
    releases.sort_by(|a, b| b.1.cmp(&a.1));
    let mut packages = std::collections::BTreeMap::<(String, String), Package>::new();
    for (id, version, tag) in releases {
        // Once this addon has a compatible native build, older releases cannot
        // improve its selection. Other platforms are not displayed by this host.
        let native_key = (
            id.clone(),
            super::packages::target().unwrap_or("").to_owned(),
        );
        if packages.get(&native_key).is_some_and(compatible) {
            continue;
        }
        let catalog = load(&tag)?;
        catalog.validate()?;
        if catalog.release != tag {
            return Err("Catalog does not match addon release tag".into());
        }
        if catalog.schema != 2 {
            return Err("Independent addon release requires catalog schema 2".into());
        }
        for mut package in catalog.packages {
            if package.id != id || package.version != version.to_string() {
                return Err("Addon release tag and package identity differ".into());
            }
            if Some(package.target.as_str()) != super::packages::target() {
                continue;
            }
            package.source_release = Some(tag.clone());
            let key = (package.id.clone(), package.target.clone());
            if !packages.contains_key(&key)
                || (!compatible(&packages[&key]) && compatible(&package))
            {
                packages.insert(key, package);
            }
        }
    }
    if packages.is_empty() {
        return Err("No independently released addons are available for this platform. Select an older editor release tag or install a local addon.".into());
    }
    let catalog = Catalog {
        schema: 2,
        release: "latest".into(),
        editor_version: String::new(),
        packages: packages.into_values().collect(),
    };
    catalog.validate()?;
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

    fn catalog_for(tag: &str, requirement: &str) -> Catalog {
        let (id, version) = release_identity(tag).unwrap();
        serde_json::from_value(serde_json::json!({
            "schema": 2, "release": tag, "packages": [{
                "id": id, "name": id, "version": version.to_string(), "protocol": 1,
                "editor_version_req": requirement, "target": super::super::packages::target().unwrap(),
                "archive": "addon.tar.gz", "size": 1, "sha256": "a".repeat(64)
            }]
        })).unwrap()
    }

    #[test]
    fn release_pages_exclude_editor_drafts_and_prereleases() {
        let mut pages = Vec::new();
        let releases = discover_releases(|page| {
            pages.push(page);
            Ok(if page == 1 {
                vec![serde_json::json!({"tag_name": "v0.4.8"}); 100]
            } else {
                vec![
                    serde_json::json!({"tag_name": "addon_my_addon_1.2.3"}),
                    serde_json::json!({"tag_name": "addon_draft_1.0.0", "draft": true}),
                    serde_json::json!({"tag_name": "addon_preview_1.0.0-1", "prerelease": true}),
                ]
            })
        })
        .unwrap();
        assert_eq!(pages, [1, 2]);
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].0, "my_addon");
        assert!(discover_releases(|_| Err("network unavailable".into())).is_err());
    }

    #[test]
    fn discovery_selects_highest_compatible_version_and_retains_download_tag() {
        let compatible_requirement = format!("={}", env!("CARGO_PKG_VERSION"));
        let incompatible_requirement = format!(">{}", env!("CARGO_PKG_VERSION"));
        let tags = [
            "addon_github_0.4.9",
            "addon_codex_2.0.0",
            "addon_github_0.4.11",
            "addon_github_0.4.10",
        ];
        let releases = tags
            .iter()
            .map(|tag| {
                let (id, version) = release_identity(tag).unwrap();
                (id.to_owned(), version, tag.to_string())
            })
            .collect();
        let mut fetched = Vec::new();
        let catalog = select_packages(releases, |tag| {
            fetched.push(tag.to_owned());
            Ok(catalog_for(
                tag,
                if tag.ends_with("0.4.11") {
                    &incompatible_requirement
                } else {
                    &compatible_requirement
                },
            ))
        })
        .unwrap();
        let github = catalog.packages.iter().find(|p| p.id == "github").unwrap();
        assert!(compatible(github));
        assert_eq!(github.version, "0.4.10");
        assert_eq!(
            github.source_release.as_deref(),
            Some("addon_github_0.4.10")
        );
        assert!(!fetched.contains(&"addon_github_0.4.9".to_owned()));
        assert_eq!(catalog.packages.len(), 2);
        assert!(
            url(github.source_release.as_deref().unwrap(), &github.archive)
                .as_str()
                .contains("addon_github_0.4.10/addon.tar.gz")
        );
    }

    #[test]
    fn incompatible_release_remains_visible_but_identity_mismatch_is_rejected() {
        let tag = "addon_github_1.0.0";
        let releases = || {
            vec![(
                "github".to_owned(),
                semver::Version::new(1, 0, 0),
                tag.to_owned(),
            )]
        };
        let incompatible_requirement = format!(">{}", env!("CARGO_PKG_VERSION"));
        let catalog = select_packages(releases(), |_| {
            Ok(catalog_for(tag, &incompatible_requirement))
        })
        .unwrap();
        assert!(!compatible(&catalog.packages[0]));
        assert!(select_packages(releases(), |_| Err("download failed".into())).is_err());
        assert!(
            select_packages(releases(), |_| Ok(catalog_for("addon_other_1.0.0", "*"))).is_err()
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
