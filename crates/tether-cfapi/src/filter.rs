use cloud_filter::filter::{SyncFilter, Request, ticket, info};
use cloud_filter::error::CloudErrorKind;
use std::path::PathBuf;

pub struct TetherSyncFilter {
    pub root_path: PathBuf,
}

impl TetherSyncFilter {
    pub fn new(root_path: PathBuf) -> Self {
        Self { root_path }
    }
}

impl SyncFilter for TetherSyncFilter {
    fn fetch_placeholders(
        &self,
        _request: Request,
        _ticket: ticket::FetchPlaceholders,
        _info: info::FetchPlaceholders,
    ) -> Result<(), CloudErrorKind> {
        tracing::debug!("CFAPI fetch_placeholders requested");
        // We will call the engine to fetch directory contents
        Ok(())
    }

    fn fetch_data(
        &self,
        _request: Request,
        _ticket: ticket::FetchData,
        _info: info::FetchData,
    ) -> Result<(), CloudErrorKind> {
        tracing::debug!("CFAPI fetch_data requested");
        // We will call the engine to download the S3 chunk and fulfill the request
        Ok(())
    }
}
