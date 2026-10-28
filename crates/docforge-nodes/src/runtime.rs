//! Helpers for per-node runtime state that is never saved.

use std::{
    fmt,
    path::{Path, PathBuf},
    sync::{Mutex, PoisonError},
};

use docforge_core::files::{self, Version};

/// Identity of a file version: cheap to obtain (one `stat`) and changes
/// whenever the file is rewritten.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FileStamp {
    pub path: PathBuf,
    version: Version,
}

impl FileStamp {
    pub fn read(path: &Path) -> Option<Self> {
        Some(Self {
            path: path.to_owned(),
            version: files::version(path)?,
        })
    }

    /// True while the file on disk is still exactly this version.
    pub fn is_current(&self) -> bool {
        Self::read(&self.path).as_ref() == Some(self)
    }
}

/// Interior-mutable state a node updates from `evaluate(&self)`, such as what
/// it last read or wrote. It is runtime-only: never serialised (mark the field
/// `#[serde(skip)]`) and ignored by equality, so two nodes with the same
/// parameters compare equal.
#[derive(Default)]
pub(crate) struct RuntimeState<T>(Mutex<T>);

impl<T: Clone> RuntimeState<T> {
    pub fn get(&self) -> T {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn set(&self, value: T) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = value;
    }
}

impl<T: Clone> Clone for RuntimeState<T> {
    fn clone(&self) -> Self {
        Self(Mutex::new(self.get()))
    }
}

impl<T> PartialEq for RuntimeState<T> {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl<T: Clone + fmt::Debug> fmt::Debug for RuntimeState<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(f)
    }
}
