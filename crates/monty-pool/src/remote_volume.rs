//! Remote volumes a client can ask a server to mount within the sandbox.

use monty_proto::{pb, validate_volume_name};
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
    /// What the sandbox may do to the volume. `Overlay` writes last until the
    /// connection ends, not the feed.
    pub mode: MountSpecMode,
    /// Files to load before the session runs, relative to `virtual_path`: a
    /// file, or a pattern read as Python's `Path.glob` reads one. A trailing
    /// `/` loads everything beneath the directories it names, and `/` alone is
    /// the whole volume. Any other entry that matches no file fails the checkout.
    pub eager: Vec<String>,
    /// Optional label for the volume.
    pub name: Option<String>,
    /// Most bytes the volume may hold; `None` takes the server's default.
    pub size_limit: Option<u64>,
    /// Most write operations the session may make on the volume; `None`
    /// takes the server's default.
    pub write_operations_limit: Option<u64>,
    /// Most read operations the session may make on the volume; `None`
    /// takes the server's default.
    pub read_operations_limit: Option<u64>,
}

impl RemoteVolume {
    /// A mount of `volume_id` at `virtual_path` with no eager entries, no name
    /// and the server's default limits.
    ///
    /// # Errors
    ///
    /// Returns [`PoolError::Runtime`] wrapping a `ValueError` if the virtual
    /// path is not absolute or contains a NUL byte.
    pub fn new(virtual_path: &str, volume_id: MontyUuid, mode: MountSpecMode) -> Result<Self, PoolError> {
        if virtual_path.contains('\0') {
            Err(value_error(format!(
                "virtual path must not contain NUL bytes: {virtual_path:?}"
            )))
        } else if virtual_path.starts_with('/') {
            Ok(Self {
                virtual_path: normalize_virtual_path(virtual_path).into_owned(),
                volume_id,
                mode,
                eager: Vec::new(),
                name: None,
                size_limit: None,
                write_operations_limit: None,
                read_operations_limit: None,
            })
        } else {
            Err(value_error(format!(
                "virtual path must be absolute, got: '{virtual_path}'"
            )))
        }
    }

    /// Sets the files and patterns to load before the session runs; see
    /// [`Self::eager`].
    #[must_use]
    pub fn with_eager(mut self, entries: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.eager = entries.into_iter().map(Into::into).collect();
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
        validate_volume_name(&name).map_err(value_error)?;
        self.name = Some(name);
        Ok(self)
    }

    /// Sets the most bytes the volume may hold.
    #[must_use]
    pub fn with_size_limit(mut self, bytes: u64) -> Self {
        self.size_limit = Some(bytes);
        self
    }

    /// Sets the most write operations the session may make on the volume.
    #[must_use]
    pub fn with_write_operations_limit(mut self, operations: u64) -> Self {
        self.write_operations_limit = Some(operations);
        self
    }

    /// Sets the most read operations the session may make on the volume.
    #[must_use]
    pub fn with_read_operations_limit(mut self, operations: u64) -> Self {
        self.read_operations_limit = Some(operations);
        self
    }
}

impl From<&RemoteVolume> for pb::RemoteVolume {
    fn from(mount: &RemoteVolume) -> Self {
        Self {
            virtual_path: mount.virtual_path.clone(),
            volume_id: Some(pb::Uuid::from(&mount.volume_id)),
            mode: wire_mode(mount.mode).into(),
            eager: mount.eager.clone().into(),
            name: mount.name.clone(),
            size_limit: mount.size_limit,
            write_operations_limit: mount.write_operations_limit,
            read_operations_limit: mount.read_operations_limit,
        }
    }
}

/// The wire spelling of a volume's access mode.
fn wire_mode(mode: MountSpecMode) -> pb::MountMode {
    match mode {
        MountSpecMode::ReadOnly => pb::MountMode::ReadOnly,
        MountSpecMode::ReadWrite => pb::MountMode::ReadWrite,
        MountSpecMode::Overlay => pb::MountMode::Overlay,
    }
}
