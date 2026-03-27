use anyhow::{Context, Result};
use std::path::Path;
use cloud_filter::root::{
    HydrationType, PopulationType, ProtectionMode, SecurityId, SyncRootIdBuilder, SyncRootInfo,
};

/// Unregisters the sync root with Windows Cloud Files API
pub fn unregister_sync_root(
    provider_name: &str,
) -> Result<()> {
    tracing::info!("Unregistering CFAPI Sync Root for '{}'", provider_name);
    
    let user_sid = SecurityId::current_user()
        .context("Failed to get current user security ID for CFAPI")?;

    let sync_root_id = SyncRootIdBuilder::new(provider_name)
        .user_security_id(user_sid)
        .account_name(provider_name)
        .build();

    if sync_root_id.is_registered().unwrap_or(false) {
        sync_root_id
            .unregister()
            .context("Failed to unregister existing sync root")?;
        tracing::info!("Unregistered sync root successfully.");
    } else {
        tracing::info!("Sync root was not registered.");
    }

    Ok(())
}

/// Registers the sync root with Windows Cloud Files API
pub fn register_sync_root(
    provider_name: &str,
    provider_version: &str,
    sync_root_path: &Path,
) -> Result<()> {
    tracing::info!(
        "Registering CFAPI sync root '{}' at {:?}",
        provider_name,
        sync_root_path
    );

    // Get the current user's security ID so the virtual drive belongs to them
    let user_sid = SecurityId::current_user()
        .context("Failed to get current user security ID for CFAPI")?;

    // Create the SyncRootId identifier
    let sync_root_id = SyncRootIdBuilder::new(provider_name)
        .user_security_id(user_sid)
        .account_name(provider_name)
        .build();

    // Clean up any stale registration first
    if sync_root_id.is_registered().unwrap_or(false) {
        tracing::info!("Found existing sync root registration. Unregistering first.");
        let _ = sync_root_id.unregister();
    }

    // Configure the Sync Root virtual drive properties
    let info = SyncRootInfo::default()
        .with_display_name(provider_name)
        .with_path(sync_root_path)?
        .with_version(provider_version)
        // Partial Hydration = Files start as 0-byte placeholders and populate on-demand
        .with_hydration_type(HydrationType::Partial)
        // Full Population = The system will ask us for directory listings when accessed
        .with_population_type(PopulationType::Full)
        // Personal Protection = General standard files (not Enterprise encrypted)
        .with_protection_mode(ProtectionMode::Personal);

    // Register with Windows Explorer
    sync_root_id
        .register(info)
        .context("Failed to register Windows CFAPI Sync Root")?;

    tracing::info!("Successfully registered sync root!");

    Ok(())
}

use cloud_filter::root::{Session, Connection};
use crate::filter::TetherSyncFilter;

/// Connects to an existing registered sync root, establishing the callback message loop
pub fn connect_sync_root(
    sync_root_path: &Path,
) -> Result<Connection<TetherSyncFilter>> {
    tracing::info!("Connecting CFAPI message loop for {:?}", sync_root_path);
    
    let filter = TetherSyncFilter::new(sync_root_path.to_path_buf());
    
    let connection = Session::new()
        .connect(sync_root_path, filter)
        .context("Failed to connect to Windows CFAPI Sync Root")?;
        
    Ok(connection)
}
