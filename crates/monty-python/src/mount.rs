//! Python bindings for filesystem mount configuration.
//!
//! [`PyMountDir`] stores immutable configuration for one mount point. Each
//! feed copies that configuration into a fresh parent-side mount table, so
//! overlay state lasts only for that feed and host paths never reach workers.
//! [`PyRemoteVolume`] describes a remote volume a server is asked to mount.

use std::{fmt::Write as _, path::PathBuf};

use monty_fs::{MountMode, MountRoot};
use monty_pool::{MountSpec, MountSpecMode, PoolError, RemoteVolume};
use monty_proto::python::{exc_monty_to_py, uuid_to_py};
use monty_types::MontyUuid;
use pyo3::{
    exceptions::{PyTypeError, PyValueError},
    intern,
    prelude::*,
    sync::PyOnceLock,
    types::PyTuple,
};

use crate::pool::pool_err_to_py;

// =============================================================================
// MountDir — immutable mount configuration
// =============================================================================

/// A single mount point mapping a virtual path to a host directory.
///
/// Passing one instance to multiple feeds reuses only its configuration;
/// `'overlay'` writes live in each feed's parent-side mount table.
/// Retained overlay data and filesystem results share a configurable memory
/// budget, which defaults to 100 MB.
///
/// Mounts passed to one feed must have distinct virtual paths and cover
/// disjoint host directories: overlap (the same directory, or one inside the
/// other) or a repeated virtual path is rejected when the feed starts with a
/// `MontyRuntimeError` wrapping a `ValueError`, since the stricter mount's
/// mode could otherwise be bypassed through the other mount's paths.
///
/// The `mode` controls sandbox access:
/// - `'read-only'` — sandbox can read but not write
/// - `'read-write'` — sandbox can read and write real host files
/// - `'overlay'` — reads fall through to host; writes are captured in memory
///
/// Warning: with `'read-write'`, files written by sandboxed code persist on
/// the host and are untrusted; do not execute them. Importing counts as
/// executing, so a mount of a directory on `sys.path` (including the cwd) lets
/// sandboxed code write `json.py`, or any module not yet imported, and the
/// next `import` runs it. That includes imports made by `pydantic_monty`
/// itself.
// TODO rename to LocalVolume at v2
#[pyclass(name = "MountDir")]
pub struct PyMountDir {
    /// Validated configuration copied into each feed's mount table. `None`
    /// once closed — the open directory is released, so nothing can mount it.
    spec: Option<MountSpec>,
    /// What the getters report, kept so they still answer after `close()`.
    label: MountLabel,
}

/// The parts of a mount that outlive its descriptor.
struct MountLabel {
    virtual_path: String,
    host_path: String,
    mode: MountSpecMode,
    write_bytes_limit: Option<u64>,
    memory_usage_limit: u64,
}

#[pymethods]
impl PyMountDir {
    /// Creates a new mount directory. All arguments are keyword-only: mount
    /// tools disagree on host-first (docker `-v`) vs virtual-first (nginx
    /// `alias`) ordering, so requiring names removes the ambiguity.
    ///
    /// # Arguments
    /// * `host_path` — path to the real host directory
    /// * `virtual_path` — absolute virtual path prefix (e.g. `"/data"`)
    /// * `mode` — access mode: `"read-only"`, `"read-write"`, or `"overlay"`
    ///   (default). With `"read-write"`, files written by sandboxed code
    ///   persist on the host; see the warning on the type.
    ///
    /// # Raises
    /// `ValueError` if `mode` is not one of the allowed values, the virtual path
    /// is not absolute, or the host path doesn't exist or isn't a directory.
    #[new]
    #[pyo3(signature = (
        *,
        host_path,
        virtual_path,
        mode = "overlay",
        // must stay a literal mirroring monty_fs::DEFAULT_MEMORY_USAGE_LIMIT: a
        // const default renders as `...` in the text signature, breaking stubtest
        write_bytes_limit = None,
        memory_usage_limit = 100_000_000,
    ))]
    #[expect(clippy::needless_pass_by_value)] // PyO3 requires owned PathBuf for conversion from Python str/Path
    fn new(
        py: Python<'_>,
        host_path: PathBuf,
        virtual_path: &str,
        mode: &str,
        write_bytes_limit: Option<u64>,
        memory_usage_limit: u64,
    ) -> PyResult<Self> {
        let mode = parse_mount_mode(mode)?;
        // Held for this object's lifetime: later feeds mount the descriptor
        // rather than re-resolving `host_path`, which the sandbox can redirect.
        let root = MountRoot::open(virtual_path, &host_path).map_err(|e| exc_monty_to_py(py, e.into_exception()))?;
        let mut spec = MountSpec::from_root(root, mode);
        spec.write_bytes_limit = write_bytes_limit;
        spec.memory_usage_limit = memory_usage_limit;
        Ok(Self {
            label: MountLabel {
                virtual_path: spec.virtual_path().to_owned(),
                host_path: spec.host_path().display().to_string(),
                mode: spec.mode,
                write_bytes_limit,
                memory_usage_limit,
            },
            spec: Some(spec),
        })
    }

    /// Releases the open host directory. Later feeds using this mount raise
    /// `ValueError`; the attributes below keep answering. Idempotent.
    ///
    /// Only Windows needs this: it refuses to rename or delete a directory
    /// while a handle to it is open, so a mount left open blocks the host from
    /// touching it. A feed already running keeps its own reference.
    fn close(&mut self) {
        self.spec = None;
    }

    fn __enter__(slf: PyRef<'_, Self>) -> PyRef<'_, Self> {
        slf
    }

    #[pyo3(signature = (*_args))]
    fn __exit__(&mut self, _args: &Bound<'_, PyTuple>) -> bool {
        self.close();
        false
    }

    /// The canonical host directory path.
    #[getter]
    fn host_path(&self) -> &str {
        &self.label.host_path
    }

    /// The normalized virtual path prefix inside the sandbox.
    #[getter]
    fn virtual_path(&self) -> &str {
        &self.label.virtual_path
    }

    /// The access mode: `"read-only"`, `"read-write"`, or `"overlay"`.
    #[getter]
    fn mode(&self) -> &'static str {
        mount_mode_name(self.label.mode)
    }

    /// The optional write bytes limit, or `None` if unlimited.
    #[getter]
    fn write_bytes_limit(&self) -> Option<u64> {
        self.label.write_bytes_limit
    }

    /// The aggregate memory budget for this mount.
    #[getter]
    fn memory_usage_limit(&self) -> u64 {
        self.label.memory_usage_limit
    }

    fn __repr__(&self) -> String {
        format!(
            "MountDir(host_path='{}', virtual_path='{}', mode='{}')",
            self.label.host_path,
            self.label.virtual_path,
            mount_mode_name(self.label.mode)
        )
    }
}

impl PyMountDir {
    /// Copies the validated configuration for a new parent-side mount table.
    ///
    /// # Errors
    ///
    /// Raises `ValueError` if the mount has been closed.
    pub(crate) fn spec(&self) -> PyResult<MountSpec> {
        self.spec
            .clone()
            .ok_or_else(|| PyValueError::new_err(format!("mount '{}' is closed", self.label.virtual_path)))
    }
}

/// Parses the Python spelling of a mount mode, shared by `MountDir` and `RemoteVolume`.
fn parse_mount_mode(mode: &str) -> PyResult<MountSpecMode> {
    match MountMode::from_mode_str(mode).map_err(PyValueError::new_err)? {
        MountMode::ReadOnly => Ok(MountSpecMode::ReadOnly),
        MountMode::ReadWrite => Ok(MountSpecMode::ReadWrite),
        MountMode::OverlayMemory(_) => Ok(MountSpecMode::Overlay),
    }
}

/// Returns the Python spelling of a pool mount mode.
fn mount_mode_name(mode: MountSpecMode) -> &'static str {
    match mode {
        MountSpecMode::ReadOnly => "read-only",
        MountSpecMode::ReadWrite => "read-write",
        MountSpecMode::Overlay => "overlay",
    }
}

// =============================================================================
// RemoteVolume — a remote volume, sent on checkout
// =============================================================================

/// A remote volume a client can ask a server to mount within the sandbox.
///
/// Plain data, unlike `MountDir`: nothing is opened here, so there is no
/// `close()`. The default mode is `'read-only'`, not `MountDir`'s `'overlay'`.
#[pyclass(name = "RemoteVolume", module = "pydantic_monty", frozen, eq)]
#[derive(PartialEq, Eq)]
pub struct PyRemoteVolume(RemoteVolume);

#[pymethods]
impl PyRemoteVolume {
    /// Describes a mount of a volume at `virtual_path`; without an `id` the
    /// volume gets a fresh uuid4, naming a new, empty volume.
    ///
    /// # Raises
    /// `TypeError` if `id` is not a `uuid.UUID`; `ValueError` if `virtual_path`
    /// is not absolute, `mode` is not one of the three words, or `name` is
    /// empty, over 128 bytes or contains a control character.
    #[new]
    #[pyo3(signature = (virtual_path, *, id = None, mode = "read-only", eager = None, name = None))]
    fn new(
        py: Python<'_>,
        virtual_path: &str,
        id: Option<&Bound<'_, PyAny>>,
        mode: &str,
        eager: Option<Vec<String>>,
        name: Option<&str>,
    ) -> PyResult<Self> {
        let mode = parse_mount_mode(mode)?;
        let mount = RemoteVolume::new(virtual_path, volume_id_from_py(py, id)?, mode)
            .map_err(|err| volume_err_to_py(py, err))?
            .with_eager(eager.unwrap_or_default());
        let mount = match name {
            Some(name) => mount.with_name(name).map_err(|err| volume_err_to_py(py, err))?,
            None => mount,
        };
        Ok(Self(mount))
    }

    /// The volume's ID.
    #[getter]
    fn id(&self, py: Python<'_>) -> PyResult<Py<PyAny>> {
        uuid_to_py(py, &self.0.volume_id)
    }

    /// The normalized virtual path the volume appears at.
    #[getter]
    fn virtual_path(&self) -> &str {
        &self.0.virtual_path
    }

    /// The access mode: `"read-only"`, `"read-write"`, or `"overlay"`.
    #[getter]
    fn mode(&self) -> &'static str {
        mount_mode_name(self.0.mode)
    }

    /// The mount-relative paths to load before the session runs.
    #[getter]
    fn eager<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyTuple>> {
        PyTuple::new(py, &self.0.eager)
    }

    /// The volume's label, if any.
    #[getter]
    fn name(&self) -> Option<&str> {
        self.0.name.as_deref()
    }

    fn __repr__(&self) -> String {
        let mount = &self.0;
        let mut repr = format!(
            "RemoteVolume(virtual_path='{}', id='{}', mode='{}'",
            mount.virtual_path,
            mount.volume_id,
            mount_mode_name(mount.mode)
        );
        if !mount.eager.is_empty() {
            // `{:?}` of a `Vec<String>` is a valid Python list literal for ASCII names
            let _ = write!(repr, ", eager={:?}", mount.eager);
        }
        if let Some(name) = &mount.name {
            let _ = write!(repr, ", name='{name}'");
        }
        repr.push(')');
        repr
    }
}

impl PyRemoteVolume {
    /// The mount as the pool sends it on `Configure`.
    pub(crate) fn mount(&self) -> RemoteVolume {
        self.0.clone()
    }
}

/// Raises a `RemoteVolume` builder's `ValueError` as that Python exception
/// itself: it is an argument error here, not a session failure.
fn volume_err_to_py(py: Python<'_>, err: PoolError) -> PyErr {
    match err {
        PoolError::Runtime(exc) => exc_monty_to_py(py, exc),
        other => pool_err_to_py(py, other),
    }
}

/// Reads a volume ID from a `uuid.UUID`, or makes one with `uuid.uuid4()`
/// when none is given.
fn volume_id_from_py(py: Python<'_>, id: Option<&Bound<'_, PyAny>>) -> PyResult<MontyUuid> {
    static UUID_CLASS: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
    static UUID4: PyOnceLock<Py<PyAny>> = PyOnceLock::new();
    let uuid = match id {
        Some(id) if id.is_instance(UUID_CLASS.import(py, "uuid", "UUID")?)? => id.clone(),
        Some(_) => return Err(PyTypeError::new_err("id must be a uuid.UUID or None")),
        None => UUID4.import(py, "uuid", "uuid4")?.call0()?,
    };
    let bytes: [u8; 16] = uuid.getattr(intern!(py, "bytes"))?.extract()?;
    Ok(MontyUuid::from_bytes(bytes))
}
