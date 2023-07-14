use std::collections::HashMap;

use reqwest::Client;
use serde::{Deserialize, Serialize};

use crate::error::RegistryError;

pub enum PackageType {
    Dependency,
    DevDependency,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PackageDistribution {
    pub integrity: String,
    #[serde(alias = "npm-signature")]
    pub npm_signature: Option<String>,
    pub shasum: String,
    pub tarball: String,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PackageVersion {
    #[serde(alias = "_npmVersion")]
    pub npm_version: String,
    #[serde(alias = "_nodeVersion")]
    pub node_version: Option<String>,
    pub dist: PackageDistribution,
    pub dependencies: Option<HashMap<String, String>>,
    #[serde(alias = "devDependencies")]
    pub dev_dependencies: Option<HashMap<String, String>>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Package {
    pub name: String,
    #[serde(alias = "dist-tags")]
    dist_tags: HashMap<String, String>,
    pub versions: HashMap<String, PackageVersion>,
}

impl Package {
    pub async fn from_registry(
        client: &Client,
        package_url: &str,
    ) -> Result<Package, RegistryError> {
        Ok(client
            .get(package_url)
            .header("user-agent", "pacquet-cli")
            .header("content-type", "application/json")
            .send()
            .await?
            .json::<Package>()
            .await?)
    }

    pub fn get_latest_tag(&self) -> Result<&String, RegistryError> {
        self.dist_tags.get("latest").ok_or(RegistryError::MissingLatestTag(self.name.to_owned()))
    }

    pub fn get_latest_version(&self) -> Result<&PackageVersion, RegistryError> {
        let latest_tag = self.get_latest_tag()?;
        self.versions.get(latest_tag).ok_or(RegistryError::MissingVersionRelease(
            latest_tag.to_owned(),
            self.name.to_owned(),
        ))
    }

    pub fn get_tarball_url(&self) -> Result<&str, RegistryError> {
        Ok(self.get_latest_version()?.dist.tarball.as_str())
    }
}
