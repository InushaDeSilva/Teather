//! Build ACC / Docs-style web URLs for **View online** and **Copy link**.

/// Best-effort Autodesk Construction Cloud (ACC) file browser URL.
/// Project and folder IDs are APS Data Management IDs (often `b.{uuid}`).
pub fn acc_view_item_url(project_id: &str, folder_id: &str, item_id: &str) -> String {
    format!(
        "https://acc.autodesk.com/build/files/projects/{project_id}/folders/{folder_id}/items/{item_id}"
    )
}

/// Folder-only view (no item selected).
pub fn acc_view_folder_url(project_id: &str, folder_id: &str) -> String {
    format!(
        "https://acc.autodesk.com/build/files/projects/{project_id}/folders/{folder_id}"
    )
}
