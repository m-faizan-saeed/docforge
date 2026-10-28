//! File access for nodes.
//!
//! On desktop this is the real file system: thin, inlined wrappers around
//! `std::fs`. A web page (wasm32) has no file system, so there files live in
//! an in-memory store instead: the app puts in the files the user opens or
//! drops, nodes read and write them by path as usual, and the app offers
//! written files as downloads.

use std::{error::Error, path::Path};

pub use imp::*;

use crate::NodeError;

/// What an encoder passed to [`write_atomic`] may fail with.
pub type EncodeError = Box<dyn Error + Send + Sync>;

#[cfg(not(target_arch = "wasm32"))]
mod imp {
    use std::{
        fs::{self, File},
        io::{self, BufWriter, Write},
        path::Path,
        time::SystemTime,
    };

    use super::EncodeError;
    use crate::NodeError;

    fn io_error(path: &Path, source: io::Error) -> NodeError {
        NodeError::Io {
            path: path.to_owned(),
            source,
        }
    }

    /// Where [`write_atomic`] encodes to.
    pub type Sink = BufWriter<File>;

    /// Identity of a file version: cheap to obtain (one `stat`) and different
    /// whenever the file is rewritten.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Version {
        len: u64,
        modified: Option<SystemTime>,
    }

    #[inline]
    pub fn read(path: &Path) -> io::Result<Vec<u8>> {
        fs::read(path)
    }

    #[inline]
    pub fn exists(path: &Path) -> bool {
        path.exists()
    }

    #[inline]
    pub fn create_dir_all(path: &Path) -> io::Result<()> {
        fs::create_dir_all(path)
    }

    pub fn version(path: &Path) -> Option<Version> {
        let meta = fs::metadata(path).ok()?;
        Some(Version {
            len: meta.len(),
            modified: meta.modified().ok(),
        })
    }

    /// Writes through a temporary sibling and renames it over `target`, so
    /// the target is either the old file or the complete new one, never
    /// partial.
    pub fn write_atomic(
        target: &Path,
        encode: impl FnOnce(&mut Sink) -> Result<(), EncodeError>,
    ) -> Result<(), NodeError> {
        let file_name = target.file_name().unwrap_or_default().to_string_lossy();
        let tmp = target.with_file_name(format!(".{file_name}.{}.tmp", std::process::id()));
        let io_err = |source| io_error(target, source);

        let result = File::create(&tmp).map_err(io_err).and_then(|file| {
            let mut writer = BufWriter::new(file);
            encode(&mut writer).map_err(|source| NodeError::Encode {
                path: target.to_owned(),
                source,
            })?;
            writer.flush().map_err(io_err)?;
            let file = writer.into_inner().map_err(|e| io_err(e.into_error()))?;
            file.sync_data().map_err(io_err)?;
            fs::rename(&tmp, target).map_err(io_err)
        });
        if result.is_err() {
            let _ = fs::remove_file(&tmp);
        }
        result
    }
}

#[cfg(target_arch = "wasm32")]
mod imp {
    use std::{
        io::{self, Cursor},
        path::Path,
        sync::{Arc, Mutex, PoisonError},
    };

    use super::{EncodeError, memory::MemoryStore};
    use crate::NodeError;

    pub use super::memory::{Version, normalize};

    /// Where [`write_atomic`] encodes to.
    pub type Sink = Cursor<Vec<u8>>;

    static STORE: Mutex<MemoryStore> = Mutex::new(MemoryStore::new());

    fn with<R>(f: impl FnOnce(&mut MemoryStore) -> R) -> R {
        f(&mut STORE.lock().unwrap_or_else(PoisonError::into_inner))
    }

    pub fn read(path: &Path) -> io::Result<Vec<u8>> {
        with(|s| s.get(path)).map(|b| b.to_vec()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "not opened in this browser tab: open or drop the file first",
            )
        })
    }

    pub fn exists(path: &Path) -> bool {
        with(|s| s.exists(path))
    }

    pub fn create_dir_all(_: &Path) -> io::Result<()> {
        Ok(())
    }

    pub fn version(path: &Path) -> Option<Version> {
        with(|s| s.version(path))
    }

    pub fn write_atomic(
        target: &Path,
        encode: impl FnOnce(&mut Sink) -> Result<(), EncodeError>,
    ) -> Result<(), NodeError> {
        let mut sink = Cursor::new(Vec::new());
        encode(&mut sink).map_err(|source| NodeError::Encode {
            path: target.to_owned(),
            source,
        })?;
        insert(target, sink.into_inner());
        Ok(())
    }

    /// Adds or replaces a file (e.g. one the user opened).
    pub fn insert(path: &Path, bytes: impl Into<Arc<[u8]>>) {
        with(|s| s.insert(path, bytes.into()));
    }

    /// The contents of the file at `path`, without copying.
    pub fn get(path: &Path) -> Option<Arc<[u8]>> {
        with(|s| s.get(path))
    }
}

/// The in-memory file store behind [`read`] and friends in a web page.
/// Compiled everywhere so it is tested with the rest of the engine.
pub mod memory {
    use std::{
        collections::BTreeMap,
        path::{Component, Path, PathBuf},
        sync::Arc,
    };

    /// Identity of a file version in the store.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct Version {
        len: u64,
        generation: u64,
    }

    struct Entry {
        bytes: Arc<[u8]>,
        generation: u64,
    }

    #[derive(Default)]
    pub struct MemoryStore {
        files: BTreeMap<PathBuf, Entry>,
        generation: u64,
    }

    /// `a/./b/../c` → `a/c`, so differently spelled paths find one file.
    pub fn normalize(path: &Path) -> PathBuf {
        let mut out = PathBuf::new();
        for c in path.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    out.pop();
                }
                c => out.push(c),
            }
        }
        out
    }

    impl MemoryStore {
        pub const fn new() -> Self {
            Self {
                files: BTreeMap::new(),
                generation: 0,
            }
        }

        /// Adds or replaces the file at `path`.
        pub fn insert(&mut self, path: &Path, bytes: Arc<[u8]>) {
            self.generation += 1;
            let entry = Entry {
                bytes,
                generation: self.generation,
            };
            self.files.insert(normalize(path), entry);
        }

        /// Exactly this path (outputs must not match other files).
        pub fn exists(&self, path: &Path) -> bool {
            self.files.contains_key(&normalize(path))
        }

        /// The file at `path`, or else the only file with the same name: a
        /// project's relative paths then find files the user opened from
        /// anywhere.
        fn find(&self, path: &Path) -> Option<&Entry> {
            let path = normalize(path);
            self.files.get(&path).or_else(|| {
                let name = path.file_name()?;
                let mut same = self
                    .files
                    .iter()
                    .filter(|(p, _)| p.file_name() == Some(name));
                let (_, only) = same.next()?;
                same.next().is_none().then_some(only)
            })
        }

        pub fn get(&self, path: &Path) -> Option<Arc<[u8]>> {
            self.find(path).map(|e| Arc::clone(&e.bytes))
        }

        pub fn version(&self, path: &Path) -> Option<Version> {
            self.find(path).map(|e| Version {
                len: e.bytes.len() as u64,
                generation: e.generation,
            })
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn bytes(b: &[u8]) -> Arc<[u8]> {
            Arc::from(b)
        }

        #[test]
        fn paths_are_normalised_and_versions_change_on_write() {
            let mut store = MemoryStore::new();
            store.insert(Path::new("/web/projects/../pdfs/./a.pdf"), bytes(b"one"));
            assert!(store.exists(Path::new("/web/pdfs/a.pdf")));
            let before = store.version(Path::new("/web/pdfs/a.pdf")).unwrap();
            store.insert(Path::new("/web/pdfs/a.pdf"), bytes(b"two"));
            let after = store.version(Path::new("/web/pdfs/a.pdf")).unwrap();
            assert_ne!(before, after, "same length, new version");
            assert_eq!(&*store.get(Path::new("/web/pdfs/a.pdf")).unwrap(), b"two");
        }

        #[test]
        fn inputs_fall_back_to_a_unique_file_name_but_outputs_do_not() {
            let mut store = MemoryStore::new();
            store.insert(Path::new("/browser/photo.jpg"), bytes(b"jpg"));
            // A project's `inputs/photo.jpg` finds the file the user opened.
            let wanted = Path::new("/web/test-projects/inputs/photo.jpg");
            assert_eq!(&*store.get(wanted).unwrap(), b"jpg");
            assert!(store.version(wanted).is_some());
            assert!(!store.exists(wanted), "existence is exact (for outputs)");

            // Two files of that name: ambiguous, so no fallback.
            store.insert(Path::new("/elsewhere/photo.jpg"), bytes(b"other"));
            assert!(store.get(wanted).is_none());
            assert!(store.get(Path::new("/missing.png")).is_none());
        }
    }
}

/// Convenience for the common case of writing bytes that are already encoded.
pub fn write_bytes_atomic(target: &Path, bytes: &[u8]) -> Result<(), NodeError> {
    use std::io::Write as _;
    write_atomic(target, |w| w.write_all(bytes).map_err(Into::into))
}
