use anyhow::{Context, Result};
use tracing::info;

const SERVICE_NAME: &str = "Tether";

/// Store a credential in the OS credential manager.
pub fn store_credential(key: &str, value: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE_NAME, key)
        .context("Failed to create keyring entry")?;
    entry
        .set_password(value)
        .context("Failed to store credential")?;
    info!("Stored credential: {key}");
    Ok(())
}

/// Retrieve a credential from the OS credential manager.
pub fn get_credential(key: &str) -> Result<String> {
    let entry = keyring::Entry::new(SERVICE_NAME, key)
        .context("Failed to create keyring entry")?;
    entry
        .get_password()
        .context("Credential not found or inaccessible")
}

/// Delete a credential from the OS credential manager.
pub fn delete_credential(key: &str) -> Result<()> {
    let entry = keyring::Entry::new(SERVICE_NAME, key)
        .context("Failed to create keyring entry")?;
    entry
        .delete_credential()
        .context("Failed to delete credential")?;
    info!("Deleted credential: {key}");
    Ok(())
}
