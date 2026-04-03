//! APS Data Management API client — browsing hubs, projects, folders, items.

use anyhow::{Context, Result};
use reqwest::{Client, StatusCode};
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

    /// Aggregates top-level folders **and their immediate subfolder children**
    /// from all projects across all hubs, matching the Autodesk Drive web view.
    pub async fn get_drive_view(&self, token: &str) -> Result<Vec<DriveItem>> {
        let hubs = self.get_hubs(token).await?;
        let mut drive_items = Vec::new();

        for hub in hubs {
            let h_id = hub.id.clone();
            match self.get_projects(token, &h_id).await {
                Ok(projects) => {
                    for project in projects {
                        let p_id = project.id.clone();
                        match self.get_top_folders(token, &h_id, &p_id).await {
                            Ok(folders) => {
                                for folder in folders {
                                    let f_id = folder.id.clone();

                                    drive_items.push(DriveItem {
                                        name: folder.attributes.display_name,
                                        hub_id: h_id.clone(),
                                        project_id: p_id.clone(),
                                        folder_id: f_id.clone(),
                                        depth: 0,
                                    });

                                    match self.get_folder_contents(token, &p_id, &f_id).await {
                                        Ok(contents) => {
                                            for item in contents {
                                                if item.item_type == "folders" {
                                                    drive_items.push(DriveItem {
                                                        name: item.attributes.display_name,
                                                        hub_id: h_id.clone(),
                                                        project_id: p_id.clone(),
                                                        folder_id: item.id,
                                                        depth: 1,
                                                    });
                                                }
                                            }
                                        }
                                        Err(e) => {
                                            tracing::warn!(
                                                "Failed to fetch contents of folder {} in project {}: {:#}",
                                                f_id, p_id, e
                                            );
                                        }
                                    }
                                }
                            }
                            Err(e) => {
                                tracing::warn!("Failed to fetch top folders for project {} in hub {}: {:#}", p_id, h_id, e);
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("Failed to fetch projects for hub {}: {:#}", h_id, e);
                }
            }
        }

        Ok(drive_items)
    }

    /// GET /project/v1/hubs (with pagination)
    pub async fn get_hubs(&self, token: &str) -> Result<Vec<Hub>> {
        let mut all_hubs = Vec::new();
        let mut url = Some(format!("{BASE_URL}/project/v1/hubs"));

        while let Some(current_url) = url {
            let resp = self.http.get(&current_url).bearer_auth(token).send().await?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();

            debug!("RAW HUBS RESPONSE: {}", text);

            if !status.is_success() {
                anyhow::bail!("API request failed ({}): {}", status, text);
            }

            let page: JsonApiListResponse<Hub> = serde_json::from_str(&text)
                .context("Failed to parse hubs JSON")?;

            all_hubs.extend(page.data);
            url = page.links.and_then(|l| l.next).map(|n| n.href);
        }

        Ok(all_hubs)
    }

    /// GET /project/v1/hubs/{hubId}/projects (with pagination)
    ///
    /// Returns an empty list when the hub is not API-accessible (e.g. BIM360DM JPN or other
    /// regional endpoints your app is not entitled to). Callers treat this as “no projects”.
    pub async fn get_projects(&self, token: &str, hub_id: &str) -> Result<Vec<Project>> {
        let mut all_projects = Vec::new();
        let mut url = Some(format!("{BASE_URL}/project/v1/hubs/{hub_id}/projects"));

        while let Some(current_url) = url {
            debug!("GET {current_url}");
            let resp = self.http.get(&current_url).bearer_auth(token).send().await?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();

            if Self::is_hub_projects_forbidden(status, &text) {
                tracing::warn!(
                    "Skipping projects for hub {} ({}): {}",
                    hub_id,
                    status,
                    Self::short_error_snippet(&text)
                );
                return Ok(all_projects);
            }

            if !status.is_success() {
                anyhow::bail!("API request failed ({}): {}", status, text);
            }

            let page: JsonApiListResponse<Project> = serde_json::from_str(&text)
                .context("Failed to parse projects JSON")?;
            all_projects.extend(page.data);
            url = page.links.and_then(|l| l.next).map(|n| n.href);
        }

        Ok(all_projects)
    }

    fn is_hub_projects_forbidden(status: StatusCode, body: &str) -> bool {
        if status == StatusCode::FORBIDDEN {
            return true;
        }
        if status != StatusCode::BAD_REQUEST {
            return false;
        }
        let lower = body.to_lowercase();
        lower.contains("permission")
            || lower.contains("bim360dm")
            || lower.contains("unable to get hubs")
    }

    fn short_error_snippet(body: &str) -> String {
        body.chars().take(280).collect::<String>()
    }

    /// Resolve a folder URN by searching across all hubs/projects.
    /// Returns the folder as a DriveItem if found in any project.
    pub async fn resolve_folder_urn(
        &self,
        token: &str,
        folder_urn: &str,
    ) -> Result<DriveItem> {
        let hubs = self.get_hubs(token).await?;

        for hub in &hubs {
            if let Ok(projects) = self.get_projects(token, &hub.id).await {
                for project in &projects {
                    let url = format!(
                        "{BASE_URL}/data/v1/projects/{}/folders/{}",
                        project.id, folder_urn
                    );
                    if let Ok(resp) = self.get_json::<JsonApiResponse<Folder>>(&url, token).await {
                        return Ok(DriveItem {
                            name: resp.data.attributes.display_name,
                            hub_id: hub.id.clone(),
                            project_id: project.id.clone(),
                            folder_id: resp.data.id,
                            depth: 0,
                        });
                    }
                }
            }
        }

        anyhow::bail!("Folder not found in any project: {}", folder_urn)
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

    pub async fn find_folder_entry_by_name(
        &self,
        token: &str,
        project_id: &str,
        folder_id: &str,
        entry_name: &str,
    ) -> Result<Option<Item>> {
        let items = self.get_folder_contents(token, project_id, folder_id).await?;
        Ok(items
            .into_iter()
            .find(|item| item.attributes.display_name.eq_ignore_ascii_case(entry_name)))
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

    /// GET /data/v1/projects/{projectId}/items/{itemId} — includes `parent` folder.
    pub async fn get_item_with_parent_folder(
        &self,
        token: &str,
        project_id: &str,
        item_id: &str,
    ) -> Result<(Item, String)> {
        let enc = urlencoding::encode(item_id);
        let url = format!("{BASE_URL}/data/v1/projects/{project_id}/items/{enc}");
        let item: Item = self
            .get_json::<JsonApiResponse<Item>>(&url, token)
            .await
            .context("Failed to fetch item")?
            .data;
        let parent = item
            .relationships
            .as_ref()
            .and_then(|r| r.parent.as_ref())
            .and_then(|p| p.data.as_ref())
            .map(|d| d.id.clone())
            .context("Item response missing parent folder")?;
        Ok((item, parent))
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

    /// Create a subfolder inside a parent folder.
    /// POST /data/v1/projects/{projectId}/folders
    pub async fn create_folder(
        &self,
        token: &str,
        project_id: &str,
        parent_folder_id: &str,
        folder_name: &str,
    ) -> Result<Folder> {
        let url = format!("{BASE_URL}/data/v1/projects/{project_id}/folders");
        let body = serde_json::json!({
            "jsonapi": { "version": "1.0" },
            "data": {
                "type": "folders",
                "attributes": {
                    "name": folder_name,
                    "extension": {
                        "type": "folders:autodesk.bim360:Folder",
                        "version": "1.0"
                    }
                },
                "relationships": {
                    "parent": {
                        "data": { "type": "folders", "id": parent_folder_id }
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
            anyhow::bail!("Create folder failed ({}): {}", status, text);
        }

        let result: JsonApiResponse<Folder> = resp.json().await?;
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

    /// Mark a file as deleted in BIM 360 / ACC (POST Deleted version).
    /// Tries extension-only first (works for many ACC projects); on `400 BAD_INPUT`, retries with
    /// `name` set (APS blog / older Docs payloads).
    pub async fn delete_item_as_deleted_version(
        &self,
        token: &str,
        project_id: &str,
        item_id: &str,
        display_name: &str,
    ) -> Result<VersionInfo> {
        let url = format!("{BASE_URL}/data/v1/projects/{project_id}/versions");

        let body_minimal = serde_json::json!({
            "jsonapi": { "version": "1.0" },
            "data": {
                "type": "versions",
                "attributes": {
                    "extension": {
                        "type": "versions:autodesk.core:Deleted",
                        "version": "1.0"
                    }
                },
                "relationships": {
                    "item": {
                        "data": { "type": "items", "id": item_id }
                    }
                }
            }
        });

        let (status, text) = self
            .post_versions_jsonapi(token, &url, &body_minimal)
            .await?;
        let (status, text) = if status == StatusCode::BAD_REQUEST {
            debug!(
                "delete Deleted version: retrying with name attribute (first response: {})",
                text.chars().take(200).collect::<String>()
            );
            let body_named = serde_json::json!({
                "jsonapi": { "version": "1.0" },
                "data": {
                    "type": "versions",
                    "attributes": {
                        "name": display_name,
                        "extension": {
                            "type": "versions:autodesk.core:Deleted",
                            "version": "1.0"
                        }
                    },
                    "relationships": {
                        "item": {
                            "data": { "type": "items", "id": item_id }
                        }
                    }
                }
            });
            self.post_versions_jsonapi(token, &url, &body_named).await?
        } else {
            (status, text)
        };

        if !status.is_success() {
            anyhow::bail!("Delete item (Deleted version) failed ({}): {}", status, text);
        }

        let result: JsonApiResponse<VersionInfo> = serde_json::from_str(&text)
            .context("parse delete-version response")?;
        Ok(result.data)
    }

    async fn post_versions_jsonapi(
        &self,
        token: &str,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<(StatusCode, String)> {
        let body_str = serde_json::to_string(body)?;
        let resp = self
            .http
            .post(url)
            .bearer_auth(token)
            .header("Content-Type", "application/vnd.api+json")
            .header("Accept", "application/vnd.api+json")
            .body(body_str)
            .send()
            .await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        Ok((status, text))
    }

    /// Rename a file by PATCHing the latest version's `name`.
    /// PATCH /data/v1/projects/{projectId}/versions/{versionId}
    pub async fn patch_version_name(
        &self,
        token: &str,
        project_id: &str,
        version_id: &str,
        new_name: &str,
    ) -> Result<VersionInfo> {
        let encoded = urlencoding::encode(version_id);
        let url = format!(
            "{BASE_URL}/data/v1/projects/{project_id}/versions/{encoded}"
        );
        let body = serde_json::json!({
            "jsonapi": { "version": "1.0" },
            "data": {
                "type": "versions",
                "id": version_id,
                "attributes": {
                    "name": new_name
                }
            }
        });

        let resp = self
            .http
            .patch(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("PATCH version name failed ({}): {}", status, text);
        }

        let result: JsonApiResponse<VersionInfo> = resp.json().await?;
        Ok(result.data)
    }

    /// Rename a folder via PATCH (display name).
    /// PATCH /data/v1/projects/{projectId}/folders/{folderId}
    pub async fn patch_folder_display_name(
        &self,
        token: &str,
        project_id: &str,
        folder_id: &str,
        new_name: &str,
    ) -> Result<Folder> {
        let url = format!(
            "{BASE_URL}/data/v1/projects/{project_id}/folders/{folder_id}"
        );
        let body = serde_json::json!({
            "jsonapi": { "version": "1.0" },
            "data": {
                "type": "folders",
                "id": folder_id,
                "attributes": {
                    "name": new_name
                }
            }
        });

        let resp = self
            .http
            .patch(&url)
            .bearer_auth(token)
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            anyhow::bail!("PATCH folder name failed ({}): {}", status, text);
        }

        let result: JsonApiResponse<Folder> = resp.json().await?;
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
