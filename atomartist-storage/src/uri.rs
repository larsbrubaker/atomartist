//! `StorageUri` — the identity of a project or asset.
//!
//! Replaces `PathBuf` everywhere outside a provider implementation. A URI is
//! a `(scheme, path)` pair rendered as `scheme:///path`; the scheme selects a
//! provider in the [`StorageRegistry`](crate::StorageRegistry) and the path is
//! provider-defined (always `/`-separated, always absolute). Application code
//! must treat the path as opaque — ask the provider for listings and metadata
//! instead of parsing it.
//!
//! The one exception is the `file:` scheme, which round-trips losslessly to a
//! `PathBuf` so OS "Open With", CLI arguments, and drag-and-drop keep working
//! on native shells.
//!
//! A URI path can never contain a `.` or `..` segment: every constructor
//! refuses one. Once a provider is rooted (`browser:` OPFS, a per-account
//! cloud prefix) the URI *is* the authorization boundary, and a value that
//! cannot express traversal cannot be used to escape that root — no
//! provider has to remember to re-check. Local paths that legitimately
//! contain `..` are resolved before URI-ification (see
//! [`StorageUri::from_local_path`]).

use std::fmt;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Scheme used by the native filesystem provider.
pub const FILE_SCHEME: &str = "file";

/// Opaque, serializable location of a project or asset.
///
/// `Arc<str>` for both halves: URIs are cloned into recents lists, window
/// titles, and pending-operation records far more often than they are built.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct StorageUri {
    scheme: Arc<str>,
    path: Arc<str>,
}

/// Why a URI string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UriParseError {
    /// No `://` separator, so there is no scheme.
    MissingScheme,
    /// Scheme was empty or contained characters outside `[A-Za-z][A-Za-z0-9+.-]*`.
    InvalidScheme(String),
    /// The portion after `://` did not start with `/` (we only accept
    /// authority-less URIs — `scheme:///path`).
    InvalidPath(String),
    /// The path contained a `.` or `..` segment. A URI is an
    /// authorization boundary for any rooted provider (`browser:` OPFS, a
    /// per-account cloud prefix), so traversal segments are refused at
    /// construction rather than resolved — see
    /// `docs/storage-architecture-plan.md` §13 open question 6.
    TraversalSegment(String),
}

impl fmt::Display for UriParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UriParseError::MissingScheme => {
                write!(f, "storage URI is missing a `scheme://` prefix")
            }
            UriParseError::InvalidScheme(s) => write!(f, "invalid storage URI scheme `{s}`"),
            UriParseError::InvalidPath(p) => {
                write!(f, "storage URI path must be absolute, got `{p}`")
            }
            UriParseError::TraversalSegment(p) => write!(
                f,
                "storage URI path must not contain `.` or `..` segments, got `{p}`"
            ),
        }
    }
}

impl std::error::Error for UriParseError {}

impl StorageUri {
    /// Build a URI from its parts. The scheme is lower-cased and the path is
    /// normalized: backslashes become `/`, and a leading `/` is added if the
    /// caller omitted it.
    ///
    /// # Panics
    ///
    /// If `path` contains a `.` or `..` segment, or `scheme` is not a legal
    /// scheme (`[A-Za-z][A-Za-z0-9+.-]*`). Both are **programmer errors**, in the same class as indexing a slice out of bounds: a URI
    /// is an authorization boundary for a rooted provider, and no
    /// legitimate caller writes a traversal segment into a literal path.
    /// Every path that comes from *outside* the program — a settings file,
    /// a CLI argument, a picker, a provider listing — must go through the
    /// fallible [`try_new`](Self::try_new), [`try_join`](Self::try_join),
    /// [`from_local_path`](Self::from_local_path) or `FromStr` instead.
    pub fn new(scheme: &str, path: &str) -> Self {
        match Self::try_new(scheme, path) {
            Ok(uri) => uri,
            Err(err) => panic!("StorageUri::new({scheme:?}, {path:?}): {err}"),
        }
    }

    /// [`new`](Self::new) for a scheme or path this code did not author:
    /// returns [`UriParseError::InvalidScheme`] /
    /// [`UriParseError::TraversalSegment`] instead of panicking.
    ///
    /// The scheme is validated exactly as `FromStr` validates it, so every
    /// URI built here re-parses from its own [`Display`](fmt::Display) form.
    pub fn try_new(scheme: &str, path: &str) -> Result<Self, UriParseError> {
        if !valid_scheme(scheme) {
            return Err(UriParseError::InvalidScheme(scheme.to_string()));
        }
        Ok(Self {
            scheme: Arc::from(scheme.to_ascii_lowercase().as_str()),
            path: Arc::from(normalize_path(path)?.as_str()),
        })
    }

    pub fn scheme(&self) -> &str {
        &self.scheme
    }

    pub fn path(&self) -> &str {
        &self.path
    }

    /// Last `/`-separated segment, or `None` at the root. Handy for window
    /// titles; providers may still offer a prettier `display_name`.
    pub fn file_name(&self) -> Option<&str> {
        let trimmed = self.path.trim_end_matches('/');
        if trimmed.is_empty() {
            return None;
        }
        trimmed.rsplit('/').next().filter(|s| !s.is_empty())
    }

    /// Containing directory, or `None` when already at the provider root.
    pub fn parent(&self) -> Option<StorageUri> {
        let trimmed = self.path.trim_end_matches('/');
        if trimmed.is_empty() {
            return None;
        }
        let cut = trimmed.rfind('/')?;
        let parent = if cut == 0 { "/" } else { &trimmed[..cut] };
        Some(StorageUri {
            scheme: Arc::clone(&self.scheme),
            path: Arc::from(parent),
        })
    }

    /// Append a child segment (or several, `/`-separated) to this URI.
    ///
    /// # Panics
    ///
    /// If `child` contains a `.` or `..` segment — the same programmer-error
    /// precondition as [`new`](Self::new). Use [`try_join`](Self::try_join)
    /// for a name that came from outside the program.
    pub fn join(&self, child: &str) -> StorageUri {
        match self.try_join(child) {
            Ok(uri) => uri,
            Err(err) => panic!("StorageUri::join({child:?}) on `{self}`: {err}"),
        }
    }

    /// [`join`](Self::join) for a child name this code did not author —
    /// a directory listing from a provider, a name typed by the user.
    pub fn try_join(&self, child: &str) -> Result<StorageUri, UriParseError> {
        let joined = format!("{}/{}", self.path, child);
        Ok(StorageUri {
            scheme: Arc::clone(&self.scheme),
            path: Arc::from(normalize_path(&joined)?.as_str()),
        })
    }

    /// True when `self` is `dir` itself or lives beneath it.
    pub fn starts_with(&self, dir: &StorageUri) -> bool {
        if self.scheme != dir.scheme {
            return false;
        }
        let base = dir.path.trim_end_matches('/');
        if base.is_empty() {
            return true;
        }
        self.path.as_ref() == base
            || self
                .path
                .strip_prefix(base)
                .is_some_and(|rest| rest.starts_with('/'))
    }

    /// Native path for a `file:` URI, or `None` for any other scheme.
    ///
    /// Windows drive letters (`file:///C:/…`) are only unwrapped on Windows.
    /// Elsewhere such a URI has no meaningful local path and this returns
    /// `None`, rather than silently handing back a relative `C:/…`.
    pub fn to_local_path(&self) -> Option<PathBuf> {
        if self.scheme.as_ref() != FILE_SCHEME {
            return None;
        }
        local_path_str(&self.path).map(PathBuf::from)
    }

    /// Inverse of [`to_local_path`](Self::to_local_path), or `None` when
    /// the path has no URI form that maps back to the same file.
    ///
    /// Refused (v1): **UNC** shares (`\\server\share\…`) and every
    /// **verbatim** / device prefix (`\\?\…`, `\\?\UNC\…`, `\\.\…`). The
    /// authority-less `scheme:///path` shape has nowhere to put a host,
    /// so `\\server\share\p.atmr` would normalize to
    /// `file:///server/share/p.atmr` and come back as
    /// `/server/share/p.atmr` — a path Windows resolves against the
    /// *current drive*. That is silent data loss: a save reports success
    /// having written somewhere else. Refusing lets the caller say so;
    /// the supported workaround is to map the share to a drive letter,
    /// which is an ordinary path that round-trips. Proper UNC support
    /// (a host component, or the `file://server/share` form) is open
    /// question 5 in `docs/storage-architecture-plan.md` §13.
    ///
    /// Caveat: paths are carried as UTF-8, so a non-UTF-8 OS path (possible
    /// on Unix and, rarely, Windows) is lossily converted and will not round
    /// -trip. Every path AtomArtist itself produces — pickers, recents, CLI
    /// arguments — is already valid Unicode.
    ///
    /// A real local path may contain `..` — the user navigated up in the
    /// OS file dialog — while a `StorageUri` may not. Such components are
    /// resolved **lexically** here (no filesystem access, so an
    /// as-yet-nonexistent save target works and a symlinked path is not
    /// silently rewritten); a path whose `..`s reach past the start
    /// (`../elsewhere`) has no absolute form without the process's current
    /// directory and is refused like the others. So is one whose `..`s
    /// would pop a Windows drive prefix (`C:\a\..\..\b`): the result would
    /// name a different volume.
    pub fn from_local_path(path: impl AsRef<Path>) -> Option<StorageUri> {
        let path = path.as_ref();
        if !is_round_trippable(path) {
            return None;
        }
        let resolved = resolve_traversal(&path.to_string_lossy())?;
        StorageUri::try_new(FILE_SCHEME, &resolved).ok()
    }
}

/// Whether a native path survives the trip through `file:///…` and back.
///
/// Only a plain relative path or a drive-letter path does. Everything
/// Windows expresses with a `\\` prefix carries information (a host, a
/// device namespace, a "skip normalization" flag) that the URI form
/// cannot represent.
#[cfg(windows)]
fn is_round_trippable(path: &Path) -> bool {
    use std::path::{Component, Prefix};
    match path.components().next() {
        Some(Component::Prefix(prefix)) => matches!(prefix.kind(), Prefix::Disk(_)),
        _ => true,
    }
}

/// Off Windows there is no UNC syntax and no prefix component, but a
/// leading `\\` can only have come from a Windows-shaped path — and path
/// normalization would silently mangle it into segments — so it is
/// refused here too rather than quietly producing a different file.
#[cfg(not(windows))]
fn is_round_trippable(path: &Path) -> bool {
    !path.to_string_lossy().starts_with(r"\\")
}

/// True for `/C:/…` and `/C:` — a Windows drive letter wrapped by the URI's
/// leading slash.
fn has_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':'
}

#[cfg(windows)]
fn local_path_str(path: &str) -> Option<&str> {
    Some(if has_drive_prefix(path) {
        &path[1..]
    } else {
        path
    })
}

#[cfg(not(windows))]
fn local_path_str(path: &str) -> Option<&str> {
    if has_drive_prefix(path) {
        None
    } else {
        Some(path)
    }
}

/// Canonical form of a URI path: `/`-separated, absolute, no empty segments,
/// no trailing slash (except the root `/`).
///
/// Every constructor funnels through here — `StorageUri` equality is about to
/// become the app-wide identity key, so `mem:///x//y/`, `mem:///x/y`, and
/// `mem:///x\y` must be the same URI with the same hash.
///
/// `.` and `..` segments are **rejected**, not resolved: a `StorageUri` that
/// cannot express traversal cannot be used to escape a provider's root, and
/// there is no ordering hazard about *when* resolution happened relative to
/// a root check. Local paths that legitimately contain `..` (the user
/// navigated up in a picker) are resolved by the OS layer first — see
/// [`StorageUri::from_local_path`].
fn normalize_path(path: &str) -> Result<String, UriParseError> {
    let mut out = String::with_capacity(path.len() + 1);
    out.push('/');
    for segment in path.split(['/', '\\']).filter(|s| !s.is_empty()) {
        if segment == "." || segment == ".." {
            return Err(UriParseError::TraversalSegment(path.to_string()));
        }
        if out.len() > 1 {
            out.push('/');
        }
        out.push_str(segment);
    }
    Ok(out)
}

/// Resolve `.` and `..` in a native path *lexically* — without touching the
/// filesystem — into the segment list a URI path can hold.
///
/// Deliberately not `Path::canonicalize`: that hits the disk, so it fails
/// for a save target that does not exist yet and silently rewrites a path
/// the user picked through a symlink. Lexical resolution is a pure string
/// operation and is what the user meant by "up one directory" in a file
/// dialog.
///
/// Returns `None` when the `..`s underflow (`../outside`, `/..`): the
/// result would depend on the process's current directory, which is not
/// something a stored URI may inherit.
///
/// A leading `C:` is a **volume, not a directory**, so it is a floor the
/// `..`s may not cross: `C:\a\..\..\b` used to yield `/b`, a path Windows
/// resolves against whatever the current drive happens to be — a save that
/// reports success while writing to the wrong volume. Windows itself
/// clamps (`C:\..` is `C:\`), but this refuses instead, the same
/// conservative choice made for UNC and verbatim paths above: a path the
/// user actually navigated through an OS dialog comes back already
/// resolved, so only hand-constructed input reaches this branch.
fn resolve_traversal(path: &str) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    // Number of leading segments `..` may never pop: 1 for a drive prefix.
    let mut floor = 0usize;
    for segment in path.split(['/', '\\']).filter(|s| !s.is_empty()) {
        match segment {
            "." => {}
            ".." => {
                if segments.len() <= floor {
                    return None;
                }
                segments.pop();
            }
            other => {
                if segments.is_empty() && is_drive_segment(other) {
                    floor = 1;
                }
                segments.push(other);
            }
        }
    }
    Some(segments.join("/"))
}

/// True for a bare Windows drive designator (`C:`) as a path segment.
fn is_drive_segment(segment: &str) -> bool {
    let bytes = segment.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

impl fmt::Display for StorageUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}://{}", self.scheme, self.path)
    }
}

impl FromStr for StorageUri {
    type Err = UriParseError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (scheme, rest) = s.split_once("://").ok_or(UriParseError::MissingScheme)?;
        if !valid_scheme(scheme) {
            return Err(UriParseError::InvalidScheme(scheme.to_string()));
        }
        if !rest.starts_with('/') {
            return Err(UriParseError::InvalidPath(rest.to_string()));
        }
        // Normalize exactly as `StorageUri::new` does: a URI parsed from a
        // settings file must equal (and hash like) the one built in code —
        // and, like it, refuses traversal segments.
        Ok(StorageUri {
            scheme: Arc::from(scheme.to_ascii_lowercase().as_str()),
            path: Arc::from(normalize_path(rest)?.as_str()),
        })
    }
}

impl Serialize for StorageUri {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for StorageUri {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = String::deserialize(deserializer)?;
        text.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_round_trips_through_parse() {
        for text in [
            "file:///C:/Users/lars/Documents/bracket.atmr",
            "browser:///projects/bracket.atmr",
            "mh:///u/1a2b/projects/bracket.atmr",
            "mem:///",
        ] {
            let uri: StorageUri = text.parse().unwrap();
            assert_eq!(uri.to_string(), text);
        }
    }

    #[test]
    fn parse_rejects_malformed_uris() {
        assert_eq!(
            "no-scheme/here".parse::<StorageUri>(),
            Err(UriParseError::MissingScheme)
        );
        assert_eq!(
            "1bad:///x".parse::<StorageUri>(),
            Err(UriParseError::InvalidScheme("1bad".into()))
        );
        assert_eq!(
            "file://relative".parse::<StorageUri>(),
            Err(UriParseError::InvalidPath("relative".into()))
        );
    }

    #[test]
    fn windows_path_becomes_a_drive_letter_uri_on_every_platform() {
        let uri = StorageUri::from_local_path(Path::new(r"C:\Users\lars\bracket.atmr")).unwrap();
        assert_eq!(uri.to_string(), "file:///C:/Users/lars/bracket.atmr");
    }

    /// Reproduces a silent-data-loss bug: `\\server\share\p.atmr` used to
    /// normalize to `file:///server/share/p.atmr`, whose `to_local_path`
    /// is `/server/share/p.atmr` — a path Windows resolves against the
    /// *current drive*. A save then reported success while writing
    /// somewhere else entirely. Until the URI form grows a host
    /// component, such paths are refused outright.
    #[cfg(windows)]
    #[test]
    fn unc_and_verbatim_paths_are_refused_rather_than_corrupted() {
        for rejected in [
            r"\\server\share\bracket.atmr",
            r"\\server\share",
            r"\\?\C:\Users\lars\bracket.atmr",
            r"\\?\UNC\server\share\bracket.atmr",
            r"\\.\COM1",
        ] {
            assert_eq!(
                StorageUri::from_local_path(Path::new(rejected)),
                None,
                "`{rejected}` has no round-trippable URI form and must be refused",
            );
        }
    }

    /// The supported workaround for a network share: map it to a drive
    /// letter. A mapped drive is an ordinary `Prefix::Disk` path and
    /// round-trips like any other.
    #[cfg(windows)]
    #[test]
    fn mapped_network_drive_round_trips_like_any_disk_path() {
        let uri = StorageUri::from_local_path(Path::new(r"Z:\projects\bracket.atmr")).unwrap();
        assert_eq!(uri.to_string(), "file:///Z:/projects/bracket.atmr");
        let back = uri.to_local_path().unwrap();
        assert_eq!(StorageUri::from_local_path(&back), Some(uri));
    }

    /// Off Windows there is no UNC syntax, but a leading `\\` still can
    /// only have come from a Windows-shaped path (a real Unix file name
    /// containing backslashes would be mangled by normalization), so it
    /// is refused there too.
    #[cfg(not(windows))]
    #[test]
    fn double_backslash_paths_are_refused_off_windows_too() {
        assert_eq!(
            StorageUri::from_local_path(Path::new(r"\\server\share\bracket.atmr")),
            None
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_round_trips_losslessly() {
        let uri = StorageUri::from_local_path(Path::new(r"C:\Users\lars\bracket.atmr")).unwrap();
        let back = uri.to_local_path().unwrap();
        assert_eq!(back, PathBuf::from("C:/Users/lars/bracket.atmr"));
        assert_eq!(StorageUri::from_local_path(&back), Some(uri));
    }

    /// Off Windows a drive letter names nothing local, so `to_local_path`
    /// refuses rather than returning a relative `C:/…`.
    #[cfg(not(windows))]
    #[test]
    fn drive_letter_uri_has_no_local_path_off_windows() {
        let uri: StorageUri = "file:///C:/Users/lars/bracket.atmr".parse().unwrap();
        assert_eq!(uri.to_local_path(), None);
    }

    #[test]
    fn posix_path_round_trips_losslessly() {
        let uri = StorageUri::from_local_path(Path::new("/home/lars/bracket.atmr")).unwrap();
        assert_eq!(uri.to_string(), "file:///home/lars/bracket.atmr");
        let back = uri.to_local_path().unwrap();
        assert_eq!(StorageUri::from_local_path(&back), Some(uri));
    }

    #[test]
    fn non_file_scheme_has_no_local_path() {
        let uri: StorageUri = "browser:///projects/a.atmr".parse().unwrap();
        assert!(uri.to_local_path().is_none());
    }

    #[test]
    fn parent_join_and_file_name() {
        let root = StorageUri::new("mem", "/");
        assert_eq!(root.parent(), None);
        assert_eq!(root.file_name(), None);

        let dir = root.join("projects");
        assert_eq!(dir.to_string(), "mem:///projects");

        let file = dir.join("a.atmr");
        assert_eq!(file.to_string(), "mem:///projects/a.atmr");
        assert_eq!(file.file_name(), Some("a.atmr"));
        assert_eq!(file.parent(), Some(dir.clone()));
        assert_eq!(dir.parent(), Some(root.clone()));
        assert!(file.starts_with(&dir));
        assert!(file.starts_with(&root));
        assert!(!dir.starts_with(&file));
    }

    /// Reproduces two normalization bugs: `from_str` used to store the path
    /// verbatim while `new` translated backslashes, and neither collapsed
    /// repeated slashes — so the same file could yield unequal,
    /// differently-hashing URIs and even a garbage empty-named directory.
    #[test]
    fn every_constructor_normalizes_identically() {
        let canonical = StorageUri::new("mem", "/x/y");

        for equivalent in [
            StorageUri::new("mem", "x/y"),
            StorageUri::new("mem", "/x//y"),
            StorageUri::new("mem", "/x/y/"),
            StorageUri::new("mem", r"\x\y"),
            "mem:///x//y".parse().unwrap(),
            "mem:///x/y/".parse().unwrap(),
            "MEM:///x/y".parse().unwrap(),
            StorageUri::new("mem", "/").join("x").join("/y"),
            StorageUri::new("mem", "/x").join("/y/"),
            StorageUri::new("mem", "/x").join("//y"),
        ] {
            assert_eq!(equivalent, canonical, "`{equivalent}` must normalize");
            assert_eq!(equivalent.to_string(), "mem:///x/y");
            assert_eq!(hash_of(&equivalent), hash_of(&canonical));
        }
    }

    /// Traversal is refused, never resolved: a `StorageUri` that cannot
    /// express `..` cannot be used to escape a rooted provider, whatever
    /// order that provider does its checks in.
    #[test]
    fn every_constructor_rejects_traversal_segments() {
        for text in [
            "mem:///a/../b",
            "mem:///../up",
            "mem:///./x",
            "mem:///..",
            "mem:///.",
            "mem:///a/./b",
            "file:///C:/projects/../../Windows/System32/config",
        ] {
            assert_eq!(
                text.parse::<StorageUri>(),
                Err(UriParseError::TraversalSegment(
                    text.split_once("://").expect("test URIs have a scheme").1.to_string()
                )),
                "`{text}` must not parse",
            );
        }

        assert!(StorageUri::try_new("mem", "/a/../b").is_err());
        assert!(StorageUri::try_new("mem", "a/./b").is_err());
        assert!(StorageUri::try_new("mem", r"a\..\b").is_err());
        assert!(StorageUri::new("mem", "/a").try_join("../b").is_err());
        assert!(StorageUri::new("mem", "/a").try_join("..").is_err());
        assert!(StorageUri::new("mem", "/a").try_join("./b").is_err());

        // Serialized state is external input: a settings file carrying a
        // traversal URI must fail to deserialize, not smuggle one in.
        assert!(serde_json::from_str::<StorageUri>("\"mem:///a/../b\"").is_err());
    }

    /// A segment that merely *contains* dots is an ordinary name.
    #[test]
    fn dotted_names_are_not_traversal() {
        let uri = StorageUri::new("mem", "/..a/b../.hidden/x.atmr");
        assert_eq!(uri.to_string(), "mem:///..a/b../.hidden/x.atmr");
    }

    #[test]
    #[should_panic(expected = "`.` or `..` segments")]
    fn new_panics_on_a_traversal_literal() {
        let _ = StorageUri::new("mem", "/a/../b");
    }

    /// A user who navigates up in the file dialog picks a path with `..`
    /// in it. That resolves lexically — no filesystem access, so it works
    /// for a save target that does not exist yet.
    #[cfg(windows)]
    #[test]
    fn local_paths_resolve_traversal_before_becoming_uris() {
        let uri = StorageUri::from_local_path(Path::new(r"C:\a\b\..\c.atmr")).unwrap();
        assert_eq!(uri.to_string(), "file:///C:/a/c.atmr");

        // The save target does not exist (and neither does its parent), so
        // a `canonicalize`-based resolution would have failed here.
        let target = Path::new(r"C:\a\no-such-dir\..\other\brand-new.atmr");
        assert!(!target.exists());
        assert_eq!(
            StorageUri::from_local_path(target).unwrap().to_string(),
            "file:///C:/a/other/brand-new.atmr"
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn local_paths_resolve_traversal_before_becoming_uris() {
        let uri = StorageUri::from_local_path(Path::new("/a/b/../c.atmr")).unwrap();
        assert_eq!(uri.to_string(), "file:///a/c.atmr");

        let target = Path::new("/a/no-such-dir/../other/brand-new.atmr");
        assert!(!target.exists());
        assert_eq!(
            StorageUri::from_local_path(target).unwrap().to_string(),
            "file:///a/other/brand-new.atmr"
        );
    }

    /// `..`s that reach past the start of the path name a location only
    /// the process's current directory can resolve. A stored URI must not
    /// inherit that, so such paths are refused like UNC ones.
    #[test]
    fn local_paths_whose_traversal_underflows_are_refused() {
        for rejected in ["/..", "../elsewhere", "a/../../b"] {
            assert_eq!(
                StorageUri::from_local_path(Path::new(rejected)),
                None,
                "`{rejected}` escapes its own root and must be refused",
            );
        }
    }

    /// Reproduces a silent wrong-volume bug: lexical resolution treated the
    /// `C:` drive prefix as an ordinary directory segment, so `C:\a\..\..\b`
    /// popped the drive and produced `file:///b` — a path Windows later
    /// resolves against whatever the *current* drive happens to be. Windows
    /// itself clamps (`C:\..` is `C:\`), but rather than emulate that we
    /// refuse, matching how UNC and verbatim paths are handled: only
    /// hand-constructed pathological input reaches here, since a real OS
    /// dialog hands back an already-resolved path.
    ///
    /// Written without `#[cfg(windows)]` on purpose — the resolver splits on
    /// both separator kinds, so these cases behave identically everywhere.
    #[test]
    fn traversal_may_not_pop_a_windows_drive_prefix() {
        for rejected in [r"C:\a\..\..\b", r"C:\..\Windows", r"C:\a\..\.."] {
            assert_eq!(
                StorageUri::from_local_path(Path::new(rejected)),
                None,
                "`{rejected}` traverses past its drive and must be refused",
            );
        }

        // The ordinary case still resolves, drive intact.
        assert_eq!(
            StorageUri::from_local_path(Path::new(r"C:\a\..\b"))
                .unwrap()
                .to_string(),
            "file:///C:/b"
        );
    }

    /// `try_new` must accept exactly the schemes `FromStr` accepts, or a URI
    /// it builds cannot be re-parsed from its own `to_string`.
    #[test]
    fn try_new_validates_the_scheme_like_parsing_does() {
        assert_eq!(
            StorageUri::try_new("1bad", "/x"),
            Err(UriParseError::InvalidScheme("1bad".into()))
        );
        assert_eq!(
            StorageUri::try_new("", "/x"),
            Err(UriParseError::InvalidScheme(String::new()))
        );
        assert_eq!(
            StorageUri::try_new("has space", "/x"),
            Err(UriParseError::InvalidScheme("has space".into()))
        );

        let uri = StorageUri::try_new("mem+v2", "/x/y").unwrap();
        assert_eq!(uri.to_string().parse::<StorageUri>(), Ok(uri));
    }

    fn hash_of(uri: &StorageUri) -> u64 {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut hasher = DefaultHasher::new();
        uri.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn serializes_as_a_plain_string() {
        let uri: StorageUri = "mem:///projects/a.atmr".parse().unwrap();
        let json = serde_json::to_string(&uri).unwrap();
        assert_eq!(json, "\"mem:///projects/a.atmr\"");
        let back: StorageUri = serde_json::from_str(&json).unwrap();
        assert_eq!(back, uri);
    }
}
