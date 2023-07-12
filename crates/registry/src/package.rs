use std::{collections::HashMap, fs::File, io::Write, path::Path};

use futures_util::StreamExt;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Error {
    #[error("missing latest tag on `{0}`")]
    MissingLatestTag(String),
    #[error("missing version `{0}` on package `${0}`")]
    MissingVersionRelease(String, String),
    #[error("network error while downloading `${0}`")]
    Network(String),
    #[error("filesystem error: `{0}`")]
    FileSystem(String),
}

#[derive(Serialize, Deserialize, Debug)]
struct PackageDistribution {
    pub integrity: String,
    #[serde(alias = "npm-signature")]
    pub npm_signature: Option<String>,
    pub shasum: String,
    pub tarball: String,
}

#[derive(Serialize, Deserialize, Debug)]
struct PackageVersion {
    #[serde(alias = "_npmVersion")]
    pub npm_version: String,
    pub dist: PackageDistribution,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Package {
    name: String,
    #[serde(alias = "dist-tags")]
    dist_tags: HashMap<String, String>,
    versions: HashMap<String, PackageVersion>,
}

impl Package {
    pub async fn new(client: &Client, package_url: &str) -> Package {
        client
            .get(package_url)
            .header("user-agent", "pacquet-cli")
            .header("content-type", "application/json")
            .send()
            .await
            .or(Err(Error::Network(package_url.to_string())))
            .unwrap()
            .json::<Package>()
            .await
            .or(Err(Error::Network(package_url.to_string())))
            .unwrap()
    }

    pub async fn install_tarball(&self, folder: &Path) -> Result<(), Error> {
        let latest_tag = self
            .dist_tags
            .get("latest")
            .ok_or(Error::MissingLatestTag(self.name.to_owned()))
            .unwrap();

        let tarball_path = folder.join(self.name.to_owned() + "@" + latest_tag);

        if tarball_path.exists() {
            // Skip installing the same version since it's already downloaded.
            return Ok(());
        }

        let latest_tag_version = self
            .versions
            .get(latest_tag)
            .ok_or(Error::MissingVersionRelease(latest_tag.to_owned(), self.name.to_owned()))
            .unwrap();

        let mut stream = reqwest::get(&latest_tag_version.dist.tarball)
            .await
            .or(Err(Error::Network(self.name.to_owned())))?
            .bytes_stream();

        let mut file = File::create(tarball_path)
            .or(Err(Error::FileSystem("failed to create file".to_owned())))
            .unwrap();

        while let Some(item) = stream.next().await {
            let chunk = item.or(Err(Error::Network("error while downloading file".to_owned())))?;
            file.write_all(&chunk)
                .or(Err(Error::FileSystem("error while writing to file".to_owned())))?;
        }

        Ok(())
    }
}
