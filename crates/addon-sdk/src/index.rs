//! Central repository registry, distinct from an individual release's catalog.
use crate::{Catalog, Package, check_compatibility, valid_id};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AddonIndex {
    pub schema: u32,
    pub addons: Vec<IndexedAddon>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexedAddon {
    pub id: String,
    pub path: String,
    pub versions: Vec<IndexedVersion>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexedVersion {
    pub version: String,
    pub release: String,
    pub status: PublicationStatus,
    pub protocol: Option<u32>,
    pub editor_version_req: Option<String>,
    pub packages: Vec<Package>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationStatus {
    Planned,
    Published,
    Reserved,
}

impl AddonIndex {
    pub fn validate(&self) -> Result<(), String> {
        if self.schema != 1 {
            return Err("Unsupported addon index schema".into());
        }
        let mut ids = HashSet::new();
        for addon in &self.addons {
            if !valid_id(&addon.id)
                || !ids.insert(&addon.id)
                || addon.path != format!("addons/{}", addon.id)
            {
                return Err("Invalid or duplicate addon index identity/path".into());
            }
            let mut versions = HashSet::new();
            for version in &addon.versions {
                let parsed = semver::Version::parse(&version.version).map_err(|e| e.to_string())?;
                if !parsed.pre.is_empty()
                    || !parsed.build.is_empty()
                    || !versions.insert(&version.version)
                    || version.release != format!("addon_{}_{}", addon.id, version.version)
                {
                    return Err("Invalid or duplicate addon index version".into());
                }
                if version.status != PublicationStatus::Reserved {
                    if version.protocol.is_none_or(|protocol| protocol == 0) {
                        return Err("Missing addon protocol in index".into());
                    }
                    let requirement = version
                        .editor_version_req
                        .as_deref()
                        .filter(|value| !value.trim().is_empty())
                        .ok_or("Missing editor compatibility in index")?;
                    semver::VersionReq::parse(requirement).map_err(|e| e.to_string())?;
                }
                if version.status != PublicationStatus::Published {
                    if !version.packages.is_empty() {
                        return Err("Unpublished addon version advertises packages".into());
                    }
                    continue;
                }
                if version.packages.is_empty() {
                    return Err("Published addon version has no packages".into());
                }
                Catalog {
                    schema: 2,
                    release: version.release.clone(),
                    editor_version: String::new(),
                    packages: version.packages.clone(),
                }
                .validate()?;
                for package in &version.packages {
                    if package.id != addon.id
                        || package.version != version.version
                        || Some(package.protocol) != version.protocol
                        || package.editor_version_req != version.editor_version_req
                        || package
                            .source_release
                            .as_ref()
                            .is_some_and(|tag| tag != &version.release)
                    {
                        return Err("Addon index and package identity/compatibility differ".into());
                    }
                }
            }
        }
        Ok(())
    }

    /// Select one highest compatible published version per addon for this host.
    pub fn catalog_for(&self, editor: &str, target: &str) -> Result<Catalog, String> {
        self.validate()?;
        let mut selected = BTreeMap::<String, (semver::Version, Package)>::new();
        for addon in &self.addons {
            for version in &addon.versions {
                if version.status != PublicationStatus::Published {
                    continue;
                }
                let parsed = semver::Version::parse(&version.version).map_err(|e| e.to_string())?;
                for package in &version.packages {
                    if package.target != target
                        || check_compatibility(
                            package.editor_version_req.as_deref(),
                            package.protocol,
                            editor,
                        )
                        .is_err()
                        || selected
                            .get(&addon.id)
                            .is_some_and(|(current, _)| current >= &parsed)
                    {
                        continue;
                    }
                    let mut package = package.clone();
                    package.source_release = Some(version.release.clone());
                    selected.insert(addon.id.clone(), (parsed.clone(), package));
                }
            }
        }
        let catalog = Catalog {
            schema: 2,
            release: "latest".into(),
            editor_version: String::new(),
            packages: selected.into_values().map(|(_, package)| package).collect(),
        };
        catalog.validate()?;
        Ok(catalog)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn index() -> AddonIndex {
        let addons: Vec<_> = ["codex", "github", "lsp-python", "lsp-rust", "opencode"]
            .into_iter()
            .map(|id| {
                let versions: Vec<_> = [("0.5.5", ">=0.4.8, <0.5.0"), ("0.5.4", ">=0.4.8, <0.6.0")]
                    .into_iter()
                    .map(|(version, requirement)| {
                        let packages: Vec<_> = [
                            "x86_64-unknown-linux-gnu",
                            "aarch64-unknown-linux-gnu",
                        ]
                        .into_iter()
                        .map(|target| {
                            serde_json::json!({
                                "id": id, "name": id, "version": version, "protocol": 1,
                                "editor_version_req": requirement, "target": target,
                                "archive": "addon.tar.gz", "size": 10, "sha256": "a".repeat(64),
                            })
                        })
                        .collect();
                        serde_json::json!({
                            "version": version, "release": format!("addon_{id}_{version}"),
                            "status": "published", "protocol": 1,
                            "editor_version_req": requirement, "packages": packages,
                        })
                    })
                    .collect();
                serde_json::json!({"id": id, "path": format!("addons/{id}"), "versions": versions})
            })
            .collect();
        serde_json::from_value(serde_json::json!({"schema": 1, "addons": addons})).unwrap()
    }

    #[test]
    fn imported_repository_history_is_valid() {
        let index: AddonIndex = serde_json::from_str(include_str!("../../../addons.json")).unwrap();
        index.validate().unwrap();
    }

    #[test]
    fn history_selects_newest_compatible_native_release() {
        let index = index();
        let catalog = index
            .catalog_for("0.5.1", "x86_64-unknown-linux-gnu")
            .unwrap();
        assert_eq!(catalog.packages.len(), 5);
        assert!(catalog.packages.iter().all(|p| p.version == "0.5.4"));
        assert!(catalog.packages.iter().all(|p| p.source_release.as_deref()
            == Some(format!("addon_{}_{}", p.id, p.version).as_str())));
        let older = index
            .catalog_for("0.4.9", "aarch64-unknown-linux-gnu")
            .unwrap();
        assert!(older.packages.iter().all(|p| p.version == "0.5.5"));
        assert!(
            index
                .catalog_for("99.0.0", "x86_64-unknown-linux-gnu")
                .unwrap()
                .packages
                .is_empty()
        );
        assert!(
            index
                .catalog_for("0.5.1", "unsupported-platform")
                .unwrap()
                .packages
                .is_empty()
        );
    }

    #[test]
    fn plans_and_reservations_never_appear_and_protocol_is_filtered() {
        let mut index = index();
        let mut planned = index.addons[0].versions[0].clone();
        planned.version = "9.0.0".into();
        planned.release = format!("addon_{}_9.0.0", index.addons[0].id);
        planned.status = PublicationStatus::Planned;
        planned.packages.clear();
        let mut reserved = planned.clone();
        reserved.version = "10.0.0".into();
        reserved.release = format!("addon_{}_10.0.0", index.addons[0].id);
        reserved.status = PublicationStatus::Reserved;
        reserved.protocol = None;
        reserved.editor_version_req = None;
        index.addons[0].versions.extend([planned, reserved]);
        for version in &mut index.addons[1].versions {
            if version.status == PublicationStatus::Published {
                version.protocol = Some(99);
                for package in &mut version.packages {
                    package.protocol = 99;
                }
            }
        }
        let catalog = index
            .catalog_for("0.5.1", "x86_64-unknown-linux-gnu")
            .unwrap();
        assert_eq!(catalog.packages.len(), 4);
        assert!(
            !catalog
                .packages
                .iter()
                .any(|p| p.version == "9.0.0" || p.id == index.addons[1].id)
        );
    }

    #[test]
    fn invalid_history_and_package_metadata_fail_closed() {
        for mutate in [
            |index: &mut AddonIndex| index.schema = 99,
            |index: &mut AddonIndex| index.addons.push(index.addons[0].clone()),
            |index: &mut AddonIndex| index.addons[0].path = "../escape".into(),
            |index: &mut AddonIndex| {
                let duplicate = index.addons[0].versions[0].clone();
                index.addons[0].versions.push(duplicate);
            },
            |index: &mut AddonIndex| index.addons[0].versions[0].release = "wrong-tag".into(),
            |index: &mut AddonIndex| {
                index.addons[0].versions[0].packages[0].sha256 = "invalid".into()
            },
            |index: &mut AddonIndex| {
                index.addons[0].versions[0].packages[0].archive = "../addon.tar.gz".into()
            },
            |index: &mut AddonIndex| {
                index.addons[0].versions[0].packages[0].source_release = Some("another-tag".into())
            },
            |index: &mut AddonIndex| {
                index.addons[0].versions[0].editor_version_req = Some("invalid".into())
            },
        ] {
            let mut index = index();
            mutate(&mut index);
            assert!(
                index
                    .catalog_for("0.5.1", "x86_64-unknown-linux-gnu")
                    .is_err()
            );
        }
    }
}
