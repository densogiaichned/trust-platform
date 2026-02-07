//! Local package registry contracts and workflows.

#![allow(missing_docs)]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::RuntimeBundle;

const REGISTRY_CONFIG_FILE: &str = "registry.toml";
const REGISTRY_INDEX_FILE: &str = "index.json";
const REGISTRY_PACKAGES_DIR: &str = "packages";
const PACKAGE_METADATA_FILE: &str = "metadata.json";
const REGISTRY_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum RegistryVisibility {
    #[default]
    Public,
    Private,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySettings {
    pub version: u32,
    pub visibility: RegistryVisibility,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryEndpoint {
    pub method: String,
    pub path: String,
    pub description: String,
    pub auth: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryMetadataModel {
    pub package_fields: Vec<String>,
    pub file_digest_fields: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryApiProfile {
    pub api_version: String,
    pub schema_version: u32,
    pub endpoints: Vec<RegistryEndpoint>,
    pub metadata_model: RegistryMetadataModel,
    pub private_registry_contract: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageFileDigest {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageMetadata {
    pub name: String,
    pub version: String,
    pub resource_name: String,
    pub bundle_version: u32,
    pub published_at_unix: u64,
    pub total_bytes: u64,
    pub package_sha256: String,
    pub files: Vec<PackageFileDigest>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageSummary {
    pub name: String,
    pub version: String,
    pub resource_name: String,
    pub published_at_unix: u64,
    pub total_bytes: u64,
    pub package_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryIndex {
    pub schema_version: u32,
    pub generated_at_unix: u64,
    pub packages: Vec<PackageSummary>,
}

impl Default for RegistryIndex {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            generated_at_unix: now_secs(),
            packages: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RegistryTomlFile {
    registry: RegistryTomlSection,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct RegistryTomlSection {
    version: u32,
    visibility: RegistryVisibility,
    auth_token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PublishRequest {
    pub registry_root: PathBuf,
    pub bundle_root: PathBuf,
    pub package_name: Option<String>,
    pub version: String,
    pub token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PublishReport {
    pub package_root: PathBuf,
    pub metadata_path: PathBuf,
    pub metadata: PackageMetadata,
}

#[derive(Debug, Clone)]
pub struct DownloadRequest {
    pub registry_root: PathBuf,
    pub name: String,
    pub version: String,
    pub output_root: PathBuf,
    pub token: Option<String>,
    pub verify_before_install: bool,
}

#[derive(Debug, Clone)]
pub struct DownloadReport {
    pub output_root: PathBuf,
    pub metadata: PackageMetadata,
}

#[derive(Debug, Clone)]
pub struct VerifyRequest {
    pub registry_root: PathBuf,
    pub name: String,
    pub version: String,
    pub token: Option<String>,
}

#[derive(Debug, Clone)]
pub struct VerifyReport {
    pub metadata: PackageMetadata,
    pub verified_files: usize,
}

#[derive(Debug, Clone)]
pub struct ListRequest {
    pub registry_root: PathBuf,
    pub token: Option<String>,
}

pub fn registry_api_profile() -> RegistryApiProfile {
    RegistryApiProfile {
        api_version: "v1".to_string(),
        schema_version: REGISTRY_SCHEMA_VERSION,
        endpoints: vec![
            RegistryEndpoint {
                method: "PUT".to_string(),
                path: "/v1/packages/{name}/{version}".to_string(),
                description: "Publish a package payload and metadata".to_string(),
                auth: "required for private registries".to_string(),
            },
            RegistryEndpoint {
                method: "GET".to_string(),
                path: "/v1/packages/{name}/{version}".to_string(),
                description: "Download package payload".to_string(),
                auth: "required for private registries".to_string(),
            },
            RegistryEndpoint {
                method: "GET".to_string(),
                path: "/v1/packages/{name}/{version}/verify".to_string(),
                description: "Verify package integrity against metadata digests".to_string(),
                auth: "required for private registries".to_string(),
            },
            RegistryEndpoint {
                method: "GET".to_string(),
                path: "/v1/index".to_string(),
                description: "List package summaries".to_string(),
                auth: "required for private registries".to_string(),
            },
        ],
        metadata_model: RegistryMetadataModel {
            package_fields: vec![
                "name".to_string(),
                "version".to_string(),
                "resource_name".to_string(),
                "bundle_version".to_string(),
                "published_at_unix".to_string(),
                "total_bytes".to_string(),
                "package_sha256".to_string(),
                "files".to_string(),
            ],
            file_digest_fields: vec![
                "path".to_string(),
                "bytes".to_string(),
                "sha256".to_string(),
            ],
        },
        private_registry_contract: vec![
            "visibility=private requires configured auth_token".to_string(),
            "all read/write actions require token match".to_string(),
            "token is never returned in API payloads".to_string(),
        ],
    }
}

pub fn init_registry(
    registry_root: &Path,
    visibility: RegistryVisibility,
    auth_token: Option<String>,
) -> anyhow::Result<RegistrySettings> {
    ensure_registry_layout(registry_root)?;
    let section = RegistryTomlSection {
        version: REGISTRY_SCHEMA_VERSION,
        visibility,
        auth_token: normalize_token(auth_token),
    };
    enforce_private_contract(&section)?;
    write_registry_config(registry_root, &section)?;
    if !registry_index_path(registry_root).is_file() {
        write_registry_index(registry_root, &RegistryIndex::default())?;
    }
    Ok(RegistrySettings {
        version: section.version,
        visibility: section.visibility,
    })
}

pub fn load_registry_settings(registry_root: &Path) -> anyhow::Result<RegistrySettings> {
    let section = load_registry_config(registry_root)?;
    Ok(RegistrySettings {
        version: section.version,
        visibility: section.visibility,
    })
}

pub fn publish_package(request: PublishRequest) -> anyhow::Result<PublishReport> {
    let section = load_registry_config(&request.registry_root)?;
    ensure_access(&section, request.token.as_deref())?;

    let bundle_root = canonical_or_self(&request.bundle_root);
    let bundle = RuntimeBundle::load(&bundle_root)
        .map_err(|err| anyhow::anyhow!("invalid bundle '{}': {err}", bundle_root.display()))?;

    let package_name = request
        .package_name
        .unwrap_or_else(|| bundle.runtime.resource_name.to_string());
    let package_name = normalize_required_field("package name", package_name.as_str())?;
    validate_identifier("package name", package_name.as_str())?;

    let version = normalize_required_field("package version", request.version.as_str())?;
    validate_identifier("package version", version.as_str())?;

    let package_root = package_root(&request.registry_root, &package_name, &version);
    if package_root.exists() {
        anyhow::bail!("package already exists: {}/{}", package_name, version);
    }
    let bundle_out = package_root.join("bundle");
    copy_dir_recursive(&bundle_root, &bundle_out)?;

    let files = collect_file_digests(&bundle_out)?;
    if files.is_empty() {
        anyhow::bail!("package payload is empty");
    }
    let total_bytes = files.iter().map(|entry| entry.bytes).sum();
    let package_sha256 = aggregate_package_sha(&files);

    let metadata = PackageMetadata {
        name: package_name,
        version,
        resource_name: bundle.runtime.resource_name.to_string(),
        bundle_version: bundle.runtime.bundle_version,
        published_at_unix: now_secs(),
        total_bytes,
        package_sha256,
        files,
    };
    let metadata_path = package_root.join(PACKAGE_METADATA_FILE);
    write_json_file(&metadata_path, &metadata)?;
    update_registry_index(&request.registry_root, &metadata)?;

    Ok(PublishReport {
        package_root,
        metadata_path,
        metadata,
    })
}

pub fn download_package(request: DownloadRequest) -> anyhow::Result<DownloadReport> {
    let section = load_registry_config(&request.registry_root)?;
    ensure_access(&section, request.token.as_deref())?;

    let package_root = package_root(&request.registry_root, &request.name, &request.version);
    let metadata = load_package_metadata(&package_root)?;
    if request.verify_before_install {
        verify_bundle_tree_against_metadata(&package_root.join("bundle"), &metadata)?;
    }

    ensure_empty_output_dir(&request.output_root)?;
    copy_dir_recursive(&package_root.join("bundle"), &request.output_root)?;
    if request.verify_before_install {
        verify_bundle_tree_against_metadata(&request.output_root, &metadata)?;
    }

    Ok(DownloadReport {
        output_root: request.output_root,
        metadata,
    })
}

pub fn verify_package(request: VerifyRequest) -> anyhow::Result<VerifyReport> {
    let section = load_registry_config(&request.registry_root)?;
    ensure_access(&section, request.token.as_deref())?;
    let package_root = package_root(&request.registry_root, &request.name, &request.version);
    let metadata = load_package_metadata(&package_root)?;
    verify_bundle_tree_against_metadata(&package_root.join("bundle"), &metadata)?;
    Ok(VerifyReport {
        verified_files: metadata.files.len(),
        metadata,
    })
}

pub fn list_packages(request: ListRequest) -> anyhow::Result<Vec<PackageSummary>> {
    let section = load_registry_config(&request.registry_root)?;
    ensure_access(&section, request.token.as_deref())?;
    let index = load_registry_index(&request.registry_root)?;
    Ok(index.packages)
}

fn write_registry_config(
    registry_root: &Path,
    section: &RegistryTomlSection,
) -> anyhow::Result<()> {
    let text = toml::to_string_pretty(&RegistryTomlFile {
        registry: section.clone(),
    })?;
    fs::write(registry_config_path(registry_root), text)
        .with_context(|| format!("failed to write {}", REGISTRY_CONFIG_FILE))?;
    Ok(())
}

fn load_registry_config(registry_root: &Path) -> anyhow::Result<RegistryTomlSection> {
    let path = registry_config_path(registry_root);
    if !path.is_file() {
        anyhow::bail!(
            "registry config missing at {} (run `trust-runtime registry init`)",
            path.display()
        );
    }
    let text = fs::read_to_string(&path)
        .with_context(|| format!("failed to read registry config {}", path.display()))?;
    let parsed: RegistryTomlFile = toml::from_str(&text)
        .with_context(|| format!("failed to parse registry config {}", path.display()))?;
    enforce_private_contract(&parsed.registry)?;
    Ok(parsed.registry)
}

fn enforce_private_contract(section: &RegistryTomlSection) -> anyhow::Result<()> {
    if section.version != REGISTRY_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported registry schema version {} (expected {})",
            section.version,
            REGISTRY_SCHEMA_VERSION
        );
    }
    if matches!(section.visibility, RegistryVisibility::Private)
        && section
            .auth_token
            .as_deref()
            .unwrap_or_default()
            .trim()
            .is_empty()
    {
        anyhow::bail!("private registry requires non-empty auth_token");
    }
    Ok(())
}

fn ensure_access(section: &RegistryTomlSection, token: Option<&str>) -> anyhow::Result<()> {
    if !matches!(section.visibility, RegistryVisibility::Private) {
        return Ok(());
    }
    let Some(expected) = section.auth_token.as_deref() else {
        anyhow::bail!("private registry requires auth_token");
    };
    let provided = token.unwrap_or_default().trim();
    if provided != expected {
        anyhow::bail!("unauthorized: invalid registry token");
    }
    Ok(())
}

fn ensure_registry_layout(registry_root: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(registry_root)
        .with_context(|| format!("failed to create registry root {}", registry_root.display()))?;
    fs::create_dir_all(registry_packages_path(registry_root)).with_context(|| {
        format!(
            "failed to create registry packages directory {}",
            registry_packages_path(registry_root).display()
        )
    })?;
    Ok(())
}

fn ensure_empty_output_dir(output_root: &Path) -> anyhow::Result<()> {
    if output_root.is_file() {
        anyhow::bail!("output path is a file: {}", output_root.display());
    }
    if output_root.is_dir() {
        let has_entries = fs::read_dir(output_root)
            .with_context(|| format!("failed to read {}", output_root.display()))?
            .next()
            .is_some();
        if has_entries {
            anyhow::bail!("output directory is not empty: {}", output_root.display());
        }
        return Ok(());
    }
    fs::create_dir_all(output_root).with_context(|| {
        format!(
            "failed to create output directory {}",
            output_root.display()
        )
    })?;
    Ok(())
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dest)
        .with_context(|| format!("failed to create destination {}", dest.display()))?;
    for entry in
        fs::read_dir(source).with_context(|| format!("failed to read {}", source.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let target = dest.join(file_name);
        if path.is_dir() {
            copy_dir_recursive(&path, &target)?;
        } else if path.is_file() {
            fs::copy(&path, &target).with_context(|| {
                format!(
                    "failed to copy '{}' -> '{}'",
                    path.display(),
                    target.display()
                )
            })?;
        }
    }
    Ok(())
}

fn collect_file_digests(root: &Path) -> anyhow::Result<Vec<PackageFileDigest>> {
    let mut files = Vec::new();
    collect_file_digests_inner(root, root, &mut files)?;
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn collect_file_digests_inner(
    root: &Path,
    current: &Path,
    out: &mut Vec<PackageFileDigest>,
) -> anyhow::Result<()> {
    for entry in
        fs::read_dir(current).with_context(|| format!("failed to read {}", current.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_file_digests_inner(root, &path, out)?;
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .with_context(|| format!("failed to relativize {}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        let bytes = path
            .metadata()
            .with_context(|| format!("failed to stat {}", path.display()))?
            .len();
        let sha256 = sha256_file(&path)?;
        out.push(PackageFileDigest {
            path: relative,
            bytes,
            sha256,
        });
    }
    Ok(())
}

fn sha256_file(path: &Path) -> anyhow::Result<String> {
    let mut file =
        fs::File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex_string(&hasher.finalize()))
}

fn aggregate_package_sha(files: &[PackageFileDigest]) -> String {
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update(file.path.as_bytes());
        hasher.update([0_u8]);
        hasher.update(file.sha256.as_bytes());
        hasher.update([0_u8]);
        hasher.update(file.bytes.to_le_bytes());
    }
    hex_string(&hasher.finalize())
}

fn verify_bundle_tree_against_metadata(
    bundle_root: &Path,
    metadata: &PackageMetadata,
) -> anyhow::Result<()> {
    let digests = collect_file_digests(bundle_root)?;
    if digests.len() != metadata.files.len() {
        anyhow::bail!(
            "package verification failed: expected {} files, found {}",
            metadata.files.len(),
            digests.len()
        );
    }
    for (expected, actual) in metadata.files.iter().zip(digests.iter()) {
        if expected.path != actual.path {
            anyhow::bail!(
                "package verification failed: path mismatch '{}' != '{}'",
                expected.path,
                actual.path
            );
        }
        if expected.bytes != actual.bytes {
            anyhow::bail!(
                "package verification failed: '{}' size mismatch {} != {}",
                expected.path,
                expected.bytes,
                actual.bytes
            );
        }
        if expected.sha256 != actual.sha256 {
            anyhow::bail!(
                "package verification failed: '{}' digest mismatch",
                expected.path
            );
        }
    }
    let actual_package_sha = aggregate_package_sha(&digests);
    if metadata.package_sha256 != actual_package_sha {
        anyhow::bail!("package verification failed: package_sha256 mismatch");
    }
    Ok(())
}

fn update_registry_index(registry_root: &Path, metadata: &PackageMetadata) -> anyhow::Result<()> {
    let mut index = load_registry_index(registry_root).unwrap_or_default();
    index
        .packages
        .retain(|entry| !(entry.name == metadata.name && entry.version == metadata.version));
    index.packages.push(PackageSummary {
        name: metadata.name.clone(),
        version: metadata.version.clone(),
        resource_name: metadata.resource_name.clone(),
        published_at_unix: metadata.published_at_unix,
        total_bytes: metadata.total_bytes,
        package_sha256: metadata.package_sha256.clone(),
    });
    index.packages.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then(left.version.cmp(&right.version))
    });
    index.generated_at_unix = now_secs();
    write_registry_index(registry_root, &index)
}

fn load_registry_index(registry_root: &Path) -> anyhow::Result<RegistryIndex> {
    let path = registry_index_path(registry_root);
    if !path.is_file() {
        return Ok(RegistryIndex::default());
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let index: RegistryIndex = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if index.schema_version != REGISTRY_SCHEMA_VERSION {
        anyhow::bail!(
            "unsupported registry index schema version {} (expected {})",
            index.schema_version,
            REGISTRY_SCHEMA_VERSION
        );
    }
    Ok(index)
}

fn write_registry_index(registry_root: &Path, index: &RegistryIndex) -> anyhow::Result<()> {
    write_json_file(&registry_index_path(registry_root), index)
}

fn load_package_metadata(package_root: &Path) -> anyhow::Result<PackageMetadata> {
    let path = package_root.join(PACKAGE_METADATA_FILE);
    if !path.is_file() {
        anyhow::bail!("package metadata missing at {}", path.display());
    }
    let text =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let metadata: PackageMetadata = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(metadata)
}

fn write_json_file(path: &Path, value: &impl Serialize) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let text = serde_json::to_string_pretty(value)?;
    fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn package_root(registry_root: &Path, name: &str, version: &str) -> PathBuf {
    registry_packages_path(registry_root)
        .join(name)
        .join(version)
}

fn registry_config_path(registry_root: &Path) -> PathBuf {
    registry_root.join(REGISTRY_CONFIG_FILE)
}

fn registry_index_path(registry_root: &Path) -> PathBuf {
    registry_root.join(REGISTRY_INDEX_FILE)
}

fn registry_packages_path(registry_root: &Path) -> PathBuf {
    registry_root.join(REGISTRY_PACKAGES_DIR)
}

fn normalize_token(token: Option<String>) -> Option<String> {
    token.and_then(|value| {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    })
}

fn normalize_required_field(label: &str, value: &str) -> anyhow::Result<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("{label} is required");
    }
    Ok(trimmed.to_string())
}

fn validate_identifier(label: &str, value: &str) -> anyhow::Result<()> {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    {
        return Ok(());
    }
    anyhow::bail!("{label} contains unsupported characters (allowed: A-Z a-z 0-9 - _ .)");
}

fn canonical_or_self(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn hex_string(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_profile_covers_required_endpoints() {
        let profile = registry_api_profile();
        assert_eq!(profile.api_version, "v1");
        assert!(profile
            .endpoints
            .iter()
            .any(|endpoint| endpoint.path == "/v1/packages/{name}/{version}"));
        assert!(profile
            .endpoints
            .iter()
            .any(|endpoint| endpoint.path == "/v1/packages/{name}/{version}/verify"));
        assert!(profile
            .metadata_model
            .package_fields
            .iter()
            .any(|field| field == "package_sha256"));
    }
}
