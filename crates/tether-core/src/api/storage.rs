//! APS Object Storage Service — S3 signed URL upload and download.

use anyhow::{Context, Result};
use reqwest::Client;
use std::path::Path;
use tracing::{debug, info};

use super::models::*;

const BASE_URL: &str = "https://developer.api.autodesk.com";

/// Client for APS OSS (Object Storage Service) — handles S3 signed URL upload/download.
#[derive(Clone)]
pub struct ApsStorageClient {
    http: Client,
}

impl ApsStorageClient {
    pub fn new() -> Self {
        Self {
            http: Client::new(),
        }
    }

    /// Create a storage location for a new file upload.
    /// POST /data/v1/projects/{projectId}/storage
    /// Returns (bucket_key, object_key, full_urn).
    pub async fn create_storage_location(
        &self,
        token: &str,
        project_id: &str,
        folder_id: &str,
        file_name: &str,
    ) -> Result<StorageLocation> {
        let url = format!("{BASE_URL}/data/v1/projects/{project_id}/storage");
        let body = serde_json::json!({
            "jsonapi": { "version": "1.0" },
            "data": {
                "type": "objects",
                "attributes": {
                    "name": file_name
                },
                "relationships": {
                    "target": {
                        "data": { "type": "folders", "id": folder_id }
                    }
                }
            }
        });

        let resp = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await
            .context("Failed to create storage location")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Create storage failed ({}): {}", status, text);
        }

        let json: serde_json::Value = resp.json().await?;
        let urn = json["data"]["id"]
            .as_str()
            .context("Missing storage URN in response")?
            .to_string();

        // Parse URN: urn:adsk.objects:os.object:{bucketKey}/{objectKey}
        let (bucket_key, object_key) = parse_storage_urn(&urn)?;

        Ok(StorageLocation {
            id: urn,
            bucket_key,
            object_key,
        })
    }

    /// Get a signed S3 URL for uploading, then upload the file, then finalize.
    pub async fn upload_file(
        &self,
        token: &str,
        bucket_key: &str,
        object_key: &str,
        file_path: &Path,
    ) -> Result<String> {
        // Step 1: Get signed upload URL
        let url = format!(
            "{BASE_URL}/oss/v2/buckets/{bucket_key}/objects/{object_key}/signeds3upload"
        );
        let resp = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&serde_json::json!({}))
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Get upload URL failed ({}): {}", status, text);
        }

        let upload_resp: SignedS3UploadResponse = resp.json().await?;
        let signed_url = upload_resp.urls.first().context("No upload URL returned")?;

        // Step 2: PUT the file bytes to the signed URL
        let file_bytes = tokio::fs::read(file_path).await?;
        let file_size = file_bytes.len();
        debug!("Uploading {} bytes to S3", file_size);

        self.http
            .put(signed_url)
            .header("Content-Type", "application/octet-stream")
            .body(file_bytes)
            .send()
            .await
            .context("S3 upload failed")?;

        // Step 3: Finalize the upload
        let finalize_url = format!(
            "{BASE_URL}/oss/v2/buckets/{bucket_key}/objects/{object_key}/signeds3upload"
        );
        self.http
            .post(&finalize_url)
            .bearer_auth(token)
            .json(&serde_json::json!({ "uploadKey": upload_resp.upload_key }))
            .send()
            .await
            .context("Upload finalization failed")?;

        info!(
            "Uploaded {} ({} bytes)",
            file_path.display(),
            file_size
        );
        Ok(upload_resp.upload_key)
    }

    /// Download a file using a signed S3 URL.
    pub async fn download_file(
        &self,
        token: &str,
        bucket_key: &str,
        object_key: &str,
        dest_path: &Path,
    ) -> Result<u64> {
        // Step 1: Get signed download URL
        let url = format!(
            "{BASE_URL}/oss/v2/buckets/{bucket_key}/objects/{object_key}/signeds3download"
        );
        let resp = self
            .http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .context("Failed to get download URL")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Get download URL failed ({}): {}", status, text);
        }

        let download_resp: SignedS3DownloadResponse = resp.json().await?;
        let signed_url = download_resp.url.context("No download URL returned")?;

        // Step 2: Download from the signed URL
        let resp = self
            .http
            .get(&signed_url)
            .send()
            .await
            .context("S3 download failed")?;

        let bytes = resp.bytes().await?;
        let size = bytes.len() as u64;

        // Write to disk
        if let Some(parent) = dest_path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(dest_path, &bytes).await?;

        info!("Downloaded {} ({} bytes)", dest_path.display(), size);
        Ok(size)
    }

    /// Download a file and return its raw bytes (for CFAPI hydration).
    pub async fn download_to_bytes(
        &self,
        token: &str,
        bucket_key: &str,
        object_key: &str,
    ) -> Result<Vec<u8>> {
        // Step 1: Get signed download URL
        let url = format!(
            "{BASE_URL}/oss/v2/buckets/{bucket_key}/objects/{object_key}/signeds3download"
        );
        let resp = self
            .http
            .get(&url)
            .bearer_auth(token)
            .send()
            .await
            .context("Failed to get download URL")?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Get download URL failed ({}): {}", status, text);
        }

        let download_resp: SignedS3DownloadResponse = resp.json().await?;
        let signed_url = download_resp.url.context("No download URL returned")?;

        // Step 2: Download from the signed URL
        let resp = self
            .http
            .get(&signed_url)
            .send()
            .await
            .context("S3 download failed")?;

        let bytes = resp.bytes().await?;
        debug!("Downloaded {} bytes to memory", bytes.len());
        Ok(bytes.to_vec())
    }
}

/// Parse a storage URN like "urn:adsk.objects:os.object:{bucketKey}/{objectKey}"
fn parse_storage_urn(urn: &str) -> Result<(String, String)> {
    let prefix = "urn:adsk.objects:os.object:";
    let rest = urn
        .strip_prefix(prefix)
        .context("Invalid storage URN format")?;
    let (bucket, object) = rest
        .split_once('/')
        .context("Invalid storage URN: no '/' separator")?;
    Ok((bucket.to_string(), object.to_string()))
}
