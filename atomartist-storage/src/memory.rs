//! `MemoryProvider` — a complete, in-process storage backend.
//!
//! It is the reference implementation of [`StorageProvider`]: it exercises
//! every corner of the contract (directories, stamps, preconditions, error
//! shapes) with no platform dependencies, so it works identically on native
//! and wasm. Tests across the workspace use it in place of the filesystem,
//! and `crate::conformance` runs its whole suite against it.
//!
//! Every operation completes synchronously via `Job::ready`, matching the
//! plan's requirement that local providers keep the zero-latency path.

use std::collections::BTreeMap;
use std::sync::Mutex;

use crate::error::{StorageError, StorageResult};
use crate::job::Job;
use crate::provider::{
    Blob, Bytes, Capabilities, Entry, ModifiedMs, Precondition, Stamp, StorageProvider,
};
use crate::uri::StorageUri;

struct FileRec {
    bytes: Vec<u8>,
    stamp: Stamp,
    modified: ModifiedMs,
}

#[derive(Default)]
struct Store {
    /// Absolute, normalized (`/`-prefixed, no trailing `/`) file paths.
    files: BTreeMap<String, FileRec>,
    /// Directory paths, including the root `/`.
    dirs: BTreeMap<String, ()>,
    /// Monotonic counter behind both stamps and the fake clock, so results
    /// are deterministic and reproducible in tests.
    tick: u64,
}

/// In-memory [`StorageProvider`].
pub struct MemoryProvider {
    scheme: String,
    display_name: String,
    store: Mutex<Store>,
}

impl MemoryProvider {
    pub fn new(scheme: impl Into<String>, display_name: impl Into<String>) -> Self {
        let mut store = Store::default();
        store.dirs.insert("/".to_string(), ());
        MemoryProvider {
            // Lower-cased to match `StorageUri`'s scheme normalization, so
            // `MEM:///x` resolves against a provider built as `"mem"`.
            scheme: scheme.into().to_ascii_lowercase(),
            display_name: display_name.into(),
            store: Mutex::new(store),
        }
    }

    /// Root URI of this provider, the natural starting point for listings.
    pub fn root(&self) -> StorageUri {
        StorageUri::new(&self.scheme, "/")
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Store> {
        self.store
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reject URIs belonging to a different provider before touching state.
    fn key(&self, uri: &StorageUri) -> StorageResult<String> {
        if !uri.scheme().eq_ignore_ascii_case(&self.scheme) {
            return Err(StorageError::Unsupported);
        }
        Ok(normalize(uri.path()))
    }

    fn uri_for(&self, key: &str) -> StorageUri {
        StorageUri::new(&self.scheme, key)
    }

    fn entry_for(&self, store: &Store, key: &str) -> Option<Entry> {
        if let Some(rec) = store.files.get(key) {
            return Some(Entry {
                uri: self.uri_for(key),
                name: file_name(key),
                is_dir: false,
                size: Some(rec.bytes.len() as u64),
                modified: Some(rec.modified),
                stamp: Some(rec.stamp.clone()),
            });
        }
        if store.dirs.contains_key(key) {
            return Some(Entry {
                uri: self.uri_for(key),
                name: file_name(key),
                is_dir: true,
                size: None,
                modified: None,
                stamp: None,
            });
        }
        None
    }

    fn do_list(&self, dir: &StorageUri) -> StorageResult<Vec<Entry>> {
        let key = self.key(dir)?;
        let store = self.lock();
        if !store.dirs.contains_key(&key) {
            return Err(StorageError::NotFound);
        }
        // Collected by path, so a key can never be emitted twice even if the
        // store's file/dir invariant were ever violated.
        let mut out: BTreeMap<String, Entry> = BTreeMap::new();
        let children = store
            .dirs
            .keys()
            .chain(store.files.keys())
            .filter(|child| *child != &key && parent_of(child).as_deref() == Some(key.as_str()))
            .cloned()
            .collect::<Vec<_>>();
        for child in children {
            if let Some(entry) = self.entry_for(&store, &child) {
                out.insert(child, entry);
            }
        }
        Ok(out.into_values().collect())
    }

    fn do_read(&self, at: &StorageUri) -> StorageResult<Blob> {
        let key = self.key(at)?;
        let store = self.lock();
        store
            .files
            .get(&key)
            .map(|rec| rec.bytes.clone())
            .ok_or(StorageError::NotFound)
    }

    fn do_write(&self, at: &StorageUri, bytes: Bytes, pre: Precondition) -> StorageResult<Stamp> {
        let key = self.key(at)?;
        let mut store = self.lock();
        if store.dirs.contains_key(&key) {
            return Err(StorageError::Io(format!("`{key}` is a directory")));
        }
        // A file may not become a directory by implication: reject before any
        // state changes so the store keeps its "a path is a file XOR a
        // directory" invariant.
        if let Some(blocking) = file_ancestor(&store, &key) {
            return Err(StorageError::Io(format!(
                "cannot write `{key}`: ancestor `{blocking}` is a file"
            )));
        }
        let current = store.files.get(&key).map(|rec| rec.stamp.clone());
        match pre {
            Precondition::None => {}
            Precondition::IfAbsent => {
                if current.is_some() {
                    return Err(StorageError::Conflict {
                        expected: None,
                        actual: current,
                    });
                }
            }
            Precondition::IfMatch(expected) => {
                if current.as_ref() != Some(&expected) {
                    return Err(StorageError::Conflict {
                        expected: Some(expected),
                        actual: current,
                    });
                }
            }
        }

        // Materialize any missing ancestors so a fresh store behaves like an
        // object store rather than demanding create_dir first.
        let mut ancestor = parent_of(&key);
        while let Some(dir) = ancestor {
            store.dirs.insert(dir.clone(), ());
            ancestor = parent_of(&dir);
        }

        store.tick += 1;
        let stamp = Stamp::new(format!("v{}", store.tick));
        let modified = store.tick;
        store.files.insert(
            key,
            FileRec {
                bytes,
                stamp: stamp.clone(),
                modified,
            },
        );
        Ok(stamp)
    }

    fn do_delete(&self, at: &StorageUri) -> StorageResult<()> {
        let key = self.key(at)?;
        let mut store = self.lock();
        if store.files.remove(&key).is_some() {
            return Ok(());
        }
        if key != "/" && store.dirs.contains_key(&key) {
            let has_children = store
                .files
                .keys()
                .chain(store.dirs.keys())
                .any(|other| other != &key && is_under(other, &key));
            if has_children {
                return Err(StorageError::Io(format!("directory `{key}` is not empty")));
            }
            store.dirs.remove(&key);
            return Ok(());
        }
        Err(StorageError::NotFound)
    }

    fn do_stat(&self, at: &StorageUri) -> StorageResult<Option<Entry>> {
        let key = self.key(at)?;
        let store = self.lock();
        Ok(self.entry_for(&store, &key))
    }

    fn do_create_dir(&self, at: &StorageUri) -> StorageResult<()> {
        let key = self.key(at)?;
        let mut store = self.lock();
        if store.files.contains_key(&key) {
            return Err(StorageError::Io(format!("`{key}` is a file")));
        }
        if let Some(blocking) = file_ancestor(&store, &key) {
            return Err(StorageError::Io(format!(
                "cannot create `{key}`: ancestor `{blocking}` is a file"
            )));
        }
        let mut dir = Some(key);
        while let Some(path) = dir {
            store.dirs.insert(path.clone(), ());
            dir = parent_of(&path);
        }
        Ok(())
    }
}

impl StorageProvider for MemoryProvider {
    fn scheme(&self) -> &str {
        &self.scheme
    }

    fn display_name(&self) -> &str {
        &self.display_name
    }

    fn capabilities(&self) -> Capabilities {
        Capabilities {
            versioned: true,
            ..Capabilities::default()
        }
    }

    fn list(&self, dir: &StorageUri) -> Job<Vec<Entry>> {
        Job::from_result(self.do_list(dir))
    }

    fn read(&self, at: &StorageUri) -> Job<Blob> {
        Job::from_result(self.do_read(at))
    }

    fn write(&self, at: &StorageUri, bytes: Bytes, pre: Precondition) -> Job<Stamp> {
        Job::from_result(self.do_write(at, bytes, pre))
    }

    fn delete(&self, at: &StorageUri) -> Job<()> {
        Job::from_result(self.do_delete(at))
    }

    fn stat(&self, at: &StorageUri) -> Job<Option<Entry>> {
        Job::from_result(self.do_stat(at))
    }

    fn create_dir(&self, at: &StorageUri) -> Job<()> {
        Job::from_result(self.do_create_dir(at))
    }
}

/// `/a/b/` and `a/b` both become `/a/b`; the root stays `/`.
fn normalize(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

/// Nearest ancestor of `key` that is stored as a file, if any. Such an
/// ancestor blocks both writes and directory creation beneath it.
fn file_ancestor(store: &Store, key: &str) -> Option<String> {
    let mut ancestor = parent_of(key);
    while let Some(dir) = ancestor {
        if store.files.contains_key(&dir) {
            return Some(dir);
        }
        ancestor = parent_of(&dir);
    }
    None
}

fn parent_of(key: &str) -> Option<String> {
    if key == "/" {
        return None;
    }
    let cut = key.rfind('/')?;
    Some(if cut == 0 {
        "/".to_string()
    } else {
        key[..cut].to_string()
    })
}

fn file_name(key: &str) -> String {
    match key.rsplit('/').next() {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => "/".to_string(),
    }
}

fn is_under(candidate: &str, dir: &str) -> bool {
    let base = if dir == "/" { "" } else { dir };
    candidate
        .strip_prefix(base)
        .is_some_and(|rest| rest.starts_with('/'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::JobState;

    fn provider() -> MemoryProvider {
        MemoryProvider::new("mem", "Memory")
    }

    #[test]
    fn completes_every_job_synchronously() {
        let p = provider();
        let job = p.write(&p.root().join("a.atmr"), b"hi".to_vec(), Precondition::None);
        assert_eq!(job.poll(), JobState::Ready);
    }

    #[test]
    fn rejects_uris_from_another_scheme() {
        let p = provider();
        let foreign: StorageUri = "other:///a.atmr".parse().unwrap();
        assert_eq!(p.read(&foreign).error(), Some(StorageError::Unsupported));
    }

    #[test]
    fn write_creates_missing_parent_directories() {
        let p = provider();
        let deep = p.root().join("a/b/c.atmr");
        p.write(&deep, b"x".to_vec(), Precondition::None)
            .take()
            .unwrap()
            .unwrap();
        let listing = p.list(&p.root()).take().unwrap().unwrap();
        assert_eq!(listing.len(), 1);
        assert!(listing[0].is_dir);
        assert_eq!(listing[0].name, "a");
    }

    #[test]
    fn non_empty_directory_cannot_be_deleted() {
        let p = provider();
        let dir = p.root().join("d");
        p.create_dir(&dir).take().unwrap().unwrap();
        p.write(&dir.join("f.atmr"), b"x".to_vec(), Precondition::None)
            .take()
            .unwrap()
            .unwrap();
        assert!(matches!(p.delete(&dir).error(), Some(StorageError::Io(_))));
        p.delete(&dir.join("f.atmr")).take().unwrap().unwrap();
        p.delete(&dir).take().unwrap().unwrap();
        assert!(p.list(&p.root()).take().unwrap().unwrap().is_empty());
    }

    /// Reproduces the bug where writing `mem:///a/b` under the *file*
    /// `mem:///a` silently created a directory named `a` as well, so the
    /// root listed `a` twice — once as a file, once as a directory.
    #[test]
    fn a_file_ancestor_blocks_writes_beneath_it() {
        let p = provider();
        let file = p.root().join("a");
        p.write(&file, b"x".to_vec(), Precondition::None)
            .take()
            .unwrap()
            .unwrap();

        let nested = file.join("b");
        assert!(
            matches!(
                p.write(&nested, b"y".to_vec(), Precondition::None).error(),
                Some(StorageError::Io(_))
            ),
            "writing under a file ancestor must fail"
        );

        let listing = p.list(&p.root()).take().unwrap().unwrap();
        assert_eq!(listing.len(), 1, "root must still list `a` exactly once");
        assert!(!listing[0].is_dir, "`a` must still be a file");
        assert_eq!(
            p.stat(&nested).take().unwrap().unwrap(),
            None,
            "the rejected write must not have created anything"
        );
    }

    /// Same invariant from the `create_dir` side, including the case where
    /// the offending file is a *grandparent* rather than the leaf.
    #[test]
    fn a_file_ancestor_blocks_directory_creation() {
        let p = provider();
        let file = p.root().join("a");
        p.write(&file, b"x".to_vec(), Precondition::None)
            .take()
            .unwrap()
            .unwrap();

        assert!(matches!(
            p.create_dir(&file).error(),
            Some(StorageError::Io(_))
        ));
        assert!(
            matches!(
                p.create_dir(&file.join("b/c")).error(),
                Some(StorageError::Io(_))
            ),
            "a file grandparent must block deep create_dir too"
        );

        let listing = p.list(&p.root()).take().unwrap().unwrap();
        assert_eq!(listing.len(), 1);
        assert!(!listing[0].is_dir);
    }

    /// A stray slash in a URI must not create an empty-named directory key.
    #[test]
    fn repeated_slashes_do_not_create_phantom_directories() {
        let p = provider();
        let sloppy = p.root().join("x//y.bin");
        p.write(&sloppy, b"z".to_vec(), Precondition::None)
            .take()
            .unwrap()
            .unwrap();

        let listing = p.list(&p.root()).take().unwrap().unwrap();
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].name, "x");
        assert!(listing[0].is_dir);

        let inner = p.list(&p.root().join("x")).take().unwrap().unwrap();
        assert_eq!(inner.len(), 1);
        assert_eq!(inner[0].name, "y.bin");
        assert_eq!(inner[0].uri, p.root().join("x/y.bin"));
    }

    #[test]
    fn if_absent_conflict_expects_nothing_stored() {
        let p = provider();
        let at = p.root().join("a.bin");
        let stamp = p
            .write(&at, b"x".to_vec(), Precondition::None)
            .take()
            .unwrap()
            .unwrap();
        assert_eq!(
            p.write(&at, b"y".to_vec(), Precondition::IfAbsent).error(),
            Some(StorageError::Conflict {
                expected: None,
                actual: Some(stamp)
            })
        );
    }

    #[test]
    fn if_match_against_a_missing_file_conflicts_with_no_actual() {
        let p = provider();
        let at = p.root().join("missing.bin");
        let expected = Stamp::new("v1");
        assert_eq!(
            p.write(&at, b"x".to_vec(), Precondition::IfMatch(expected.clone()))
                .error(),
            Some(StorageError::Conflict {
                expected: Some(expected),
                actual: None
            })
        );
    }

    #[test]
    fn scheme_is_lower_cased_to_match_uri_normalization() {
        let p = MemoryProvider::new("MEM", "Memory");
        assert_eq!(p.scheme(), "mem");
        let uri: StorageUri = "MEM:///a.bin".parse().unwrap();
        assert!(p
            .write(&uri, b"x".to_vec(), Precondition::None)
            .error()
            .is_none());
    }

    #[test]
    fn listing_a_missing_directory_is_not_found() {
        let p = provider();
        assert_eq!(
            p.list(&p.root().join("nope")).error(),
            Some(StorageError::NotFound)
        );
    }
}
