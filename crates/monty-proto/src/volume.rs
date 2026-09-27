//! Validation for the remote volumes carried by `Configure.volumes`, shared
//! by clients and servers so both apply the same rule.

/// Longest `RemoteVolume.name`, in bytes.
pub const VOLUME_NAME_MAX: usize = 128;

/// Checks a `RemoteVolume.name`: 1 to [`VOLUME_NAME_MAX`] bytes with no
/// control characters.
pub fn validate_volume_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > VOLUME_NAME_MAX {
        Err(format!(
            "volume name must be 1 to {VOLUME_NAME_MAX} bytes, got {} bytes",
            name.len()
        ))
    } else if name.chars().any(char::is_control) {
        Err("volume name must not contain control characters".to_owned())
    } else {
        Ok(())
    }
}
