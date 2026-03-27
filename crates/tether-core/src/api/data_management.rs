//! APS Data Management API client — browsing hubs, projects, folders, items.

use anyhow::{Context, Result};
use reqwest::Client;
use tracing::debug;

use super::models::*;

const BASE_URL: &str = "https://developer.api.autodesk.com";

/// Client for Autodesk Platform Services Data Management API.
#[derive(Clone)]
pub struct ApsDataManagementClient {
    http: Client,
}

impl ApsDataManagementClient {
    pub fn new() -> Self {
        Self {
            http: Client::new(),
        }
    }

    /// GET /project/v1/hubs
    pub async fn get_hubs(&self, token: &str) -> Result<Vec<Hub>> {
        let url = format!("{BASE_URL}/project/v1/hubs");
        
        let resp = self.http.get(&url).bearer_auth(token).send().await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        
        tracing::info!("RAW HUBS RESPONSE: {}", text);
        
        if !status.is_success() {
            anyhow::bail!("API request failed ({}): {}", status, text);
        }
        
        let resp: JsonApiListResponse<Hub> = serde_json::from_str(&text)
            .context("Failed to parse hubs JSON")?;
            
        Ok(resp.data)
    }

    /// GET /project/v1/hubs/{hubId}/projects
    pub async fn get_projects(&self, token: &str, hub_id: &str) -> Result<Vec<Project>> {
        let url = format!("{BASE_URL}/project/v1/hubs/{hub_id}/projects");
        let resp: JsonApiListResponse<Project> = self
            .get_json(&url, token)
            .await
            .context("Failed to fetch projects")?;
        Ok(resp.data)
    }

    /// GET /project/v1/hubs/{hub_id}/projects/{project_id}/topFolders
    pub async fn get_top_folders(&self, token: &str, hub_id: &str, project_id: &str) -> Result<Vec<Folder>> {
        let url = format!("{BASE_URL}/project/v1/hubs/{hub_id}/projects/{project_id}/topFolders");
        let resp: JsonApiListResponse<Folder> = self
            .get_json(&url, token)
            .await
            .context("Failed to fetch top folders")?;
        Ok(resp.data)
    }

    /// GET /data/v1/projects/{projectId}/folders/{folderId}/contents
    /// Returns both items (files) and sub-folders, with pagination.
    pub async fn get_folder_contents(
        &self,
        token: &str,
        project_id: &str,
        folder_id: &str,
    ) -> Result<Vec<Item>> {
        let mut all_items = Vec::new();
        let mut url = Some(format!(
            "{BASE_URL}/data/v1/projects/{project_id}/folders/{folder_id}/contents"
        ));

        while let Some(current_url) = url {
            let resp: JsonApiListResponse<Item> = self
                .get_json(&current_url, token)
                .await
                .context("Failed to fetch folder contents")?;

            all_items.extend(resp.data);
            url = resp.links.and_then(|l| l.next).map(|n| n.href);
        }

        Ok(all_items)
    }

    /// GET /data/v1/projects/{projectId}/items/{itemId}/versions
    pub async fn get_item_versions(
        &self,
        token: &str,
        project_id: &str,
        item_id: &str,
    ) -> Result<Vec<VersionInfo>> {
        let url = format!("{BASE_URL}/data/v1/projects/{project_id}/items/{item_id}/versions");
        let resp: JsonApiListResponse<VersionInfo> = self
            .get_json(&url, token)
            .await
            .context("Failed to fetch item versions")?;
        Ok(resp.data)
    }

    /// Create a new item in a folder.
    /// POST /data/v1/projects/{projectId}/items
    pub async fn create_item(
        &self,
        token: &str,
        project_id: &str,
        folder_id: &str,
        file_name: &str,
        storage_urn: &str,
    ) -> Result<Item> {
        let url = format!("{BASE_URL}/data/v1/projects/{project_id}/items");
        let body = serde_json::json!({
            "jsonapi": { "version": "1.0" },
            "data": {
                "type": "items",
                "attributes": {
                    "displayName": file_name,
                    "extension": {
                        "type": "items:autodesk.bim360:File",
                        "version": "1.0"
                    }
                },
                "relationships": {
                    "tip": {
                        "data": { "type": "versions", "id": "1" }
                    },
                    "parent": {
                        "data": { "type": "folders", "id": folder_id }
                    }
                }
            },
            "included": [{
                "type": "versions",
                "id": "1",
                "attributes": {
                    "name": file_name,
                    "extension": {
                        "type": "versions:autodesk.bim360:File",
                        "version": "1.0"
                    }
                },
                "relationships": {
                    "storage": {
                        "data": { "type": "objects", "id": storage_urn }
                    }
                }
            }]
        });

        let resp = self
            .http
            .post(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Create item failed ({}): {}", status, text);
        }

        let result: JsonApiResponse<Item> = resp.json().await?;
        Ok(result.data)
    }

    /// Create a new version of an existing item.
    /// POST /data/v1/projects/{projectId}/versions
    pub async fn create_version(
        &self,
        token: &str,
        project_id: &str,
        item_id: &str,
        file_name: &str,
        storage_urn: &str,
    ) -> Result<VersionInfo> {
        let url = format!("{BASE_URL}/data/v1/projects/{project_id}/versions");
        let body = serde_json::json!({
            "jsonapi": { "version": "1.0" },
            "data": {
                "type": "versions",
                "attributes": {
                    "name": file_name,
                    "extension": {
                        "type": "versions:autodesk.bim360:File",
                        "version": "1.0"
                    }
                },
                "relationships": {
                    "item": {
                        "data": { "type": "items", "id": item_id }
                    },
                    "storage": {
                        "data": { "type": "objects", "id": storage_urn }
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
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("Create version failed ({}): {}", status, text);
        }

        let result: JsonApiResponse<VersionInfo> = resp.json().await?;
        Ok(result.data)
    }

    // ── Internal helper ──

    async fn get_json<T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        token: &str,
    ) -> Result<T> {
        debug!("GET {url}");
        let resp = self.http.get(url).bearer_auth(token).send().await?;
        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("API request failed ({}): {}", status, text);
        }
        Ok(resp.json().await?)
    }
}
