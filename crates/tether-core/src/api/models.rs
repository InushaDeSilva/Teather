use serde::{Deserialize, Serialize};

/// JSON:API top-level response wrapper.
#[derive(Debug, Deserialize)]
pub struct JsonApiResponse<T> {
    pub data: T,
    pub links: Option<PaginationLinks>,
}

/// JSON:API list response wrapper.
#[derive(Debug, Deserialize)]
pub struct JsonApiListResponse<T> {
    pub data: Vec<T>,
    pub links: Option<PaginationLinks>,
}

#[derive(Debug, Deserialize)]
pub struct PaginationLinks {
    #[serde(rename = "self")]
    pub self_link: Option<Link>,
    pub next: Option<Link>,
}

#[derive(Debug, Deserialize)]
pub struct Link {
    pub href: String,
}

// ── Hubs ──

#[derive(Debug, Clone, Deserialize)]
pub struct Hub {
    pub id: String,
    #[serde(rename = "type")]
    pub hub_type: String,
    pub attributes: HubAttributes,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HubAttributes {
    pub name: String,
    pub extension: Option<HubExtension>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct HubExtension {
    #[serde(rename = "type")]
    pub type_code: String,
}

// ── Projects ──

#[derive(Debug, Clone, Deserialize)]
pub struct Project {
    pub id: String,
    #[serde(rename = "type")]
    pub project_type: String,
    pub attributes: ProjectAttributes,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProjectAttributes {
    pub name: String,
}

// ── Folders ──

#[derive(Debug, Clone, Deserialize)]
pub struct Folder {
    pub id: String,
    #[serde(rename = "type")]
    pub folder_type: String,
    pub attributes: FolderAttributes,
}

#[derive(Debug, Clone, Deserialize)]
pub struct FolderAttributes {
    #[serde(rename = "displayName")]
    pub display_name: String,
}

// ── Items (files) ──

#[derive(Debug, Clone, Deserialize)]
pub struct Item {
    pub id: String,
    #[serde(rename = "type")]
    pub item_type: String,
    pub attributes: ItemAttributes,
    pub relationships: Option<ItemRelationships>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemAttributes {
    #[serde(rename = "displayName")]
    pub display_name: String,
    #[serde(rename = "createTime")]
    pub create_time: Option<String>,
    #[serde(rename = "lastModifiedTime")]
    pub last_modified_time: Option<String>,
    /// File size in bytes (present for files, absent for folders)
    #[serde(rename = "storageSize", default)]
    pub storage_size: Option<u64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ItemRelationships {
    pub tip: Option<RelationshipData>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RelationshipData {
    pub data: Option<ResourceIdentifier>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ResourceIdentifier {
    pub id: String,
    #[serde(rename = "type")]
    pub resource_type: String,
}

// ── Versions ──

#[derive(Debug, Clone, Deserialize)]
pub struct VersionInfo {
    pub id: String,
    pub attributes: VersionAttributes,
    pub relationships: Option<VersionRelationships>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionAttributes {
    #[serde(rename = "versionNumber")]
    pub version_number: Option<i32>,
    #[serde(rename = "lastModifiedTime")]
    pub last_modified_time: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct VersionRelationships {
    pub storage: Option<StorageRelationship>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageRelationship {
    pub data: Option<ResourceIdentifier>,
    pub meta: Option<StorageMeta>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageMeta {
    pub link: Option<StorageMetaLink>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct StorageMetaLink {
    pub href: Option<String>,
}

// ── Storage / S3 URLs ──

#[derive(Debug, Clone, Deserialize)]
pub struct StorageLocation {
    pub id: String,
    pub bucket_key: String,
    pub object_key: String,
}

#[derive(Debug, Deserialize)]
pub struct SignedS3DownloadResponse {
    pub url: Option<String>,
    pub status: String,
}

#[derive(Debug, Deserialize)]
pub struct SignedS3UploadResponse {
    pub urls: Vec<String>,
    #[serde(rename = "uploadKey")]
    pub upload_key: String,
}

// ── Token (OAuth) ──

#[derive(Debug, Clone, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub expires_in: u64,
}
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct DriveItem {
    pub name: String,
    pub hub_id: String,
    pub project_id: String,
    pub folder_id: String,
    /// 0 = top-level folder, 1 = subfolder inside a top-level folder, etc.
    pub depth: u32,
}
