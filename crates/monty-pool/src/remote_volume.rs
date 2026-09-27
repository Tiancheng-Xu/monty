//! Remote volumes a client can ask a server to mount within the sandbox.

use monty_proto::pb;
use monty_types::{MontyUuid, normalize_virtual_path};

use crate::{
    PoolError,
    checkout::{MountSpecMode, value_error},
};

/// A remote volume a client can ask a server to mount within the sandbox.
// non_exhaustive: mount options may be added
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct RemoteVolume {
    /// Absolute, normalized sandbox path the volume appears at.
    pub virtual_path: String,
    /// Identifies the volume.
    pub volume_id: MontyUuid,
    /// What the sandbox may do to the volume.
    pub mode: MountSpecMode,
    /// Mount-relative paths to load before the session runs; a trailing `/`
    /// names a directory.
    pub eager: Vec<String>,
    /// Optional label for the volume.
    pub name: Option<String>,
}

impl RemoteVolume {
    /// A mount of `volume_id` at `virtual_path` with no eager entries and no name.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Runtime`] wrapping a `ValueError` if the virtual
    /// path is not absolute.
    pub fn new(virtual_path: &str, volume_id: MontyUuid, mode: MountSpecMode) -> Result<Self, PoolError> {
        if virtual_path.starts_with('/') {
            Ok(Self {
                virtual_path: normalize_virtual_path(virtual_path).into_owned(),
                volume_id,
                mode,
                eager: Vec::new(),
                name: None,
            })
        } else {
            Err(value_error(format!(
                "virtual path must be absolute, got: '{virtual_path}'"
            )))
        }
    }

    /// Sets the mount-relative paths to load before the session runs.
    #[must_use]
    pub fn with_eager(mut self, paths: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.eager = paths.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the volume's label.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Runtime`] wrapping a `ValueError` if the name is
    /// empty, longer than 128 bytes, or contains a control character.
    pub fn with_name(mut self, name: impl Into<String>) -> Result<Self, PoolError> {
        let name = name.into();
        if name.is_empty() || name.len() > VOLUME_NAME_MAX {
            Err(value_error(format!(
                "volume name must be 1 to {VOLUME_NAME_MAX} bytes, got {} bytes",
                name.len()
            )))
        } else if name.chars().any(char::is_control) {
            Err(value_error(
                "volume name must not contain control characters".to_owned(),
            ))
        } else {
            self.name = Some(name);
            Ok(self)
        }
    }
}

/// Longest [`RemoteVolume::name`], in bytes.
const VOLUME_NAME_MAX: usize = 128;

impl From<&RemoteVolume> for pb::RemoteVolume {
    fn from(mount: &RemoteVolume) -> Self {
        Self {
            virtual_path: mount.virtual_path.clone(),
            volume_id: Some(pb::Uuid::from(&mount.volume_id)),
            mode: wire_mode(mount.mode).into(),
            eager: mount.eager.clone().into(),
            name: mount.name.clone(),
        }
    }
}

/// The wire spelling of a volume's access mode.
fn wire_mode(mode: MountSpecMode) -> pb::VolumeMode {
    match mode {
        MountSpecMode::ReadOnly => pb::VolumeMode::ReadOnly,
        MountSpecMode::ReadWrite => pb::VolumeMode::ReadWrite,
        MountSpecMode::Overlay => pb::VolumeMode::Overlay,
    }
}
