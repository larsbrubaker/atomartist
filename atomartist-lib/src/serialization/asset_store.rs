//! Project asset storage — the bytes referenced by `MeshNode`,
//! `ImageNode`, and future asset-backed node types.
//!
//! Inside an `.atmr` zip these live alongside `graph.json`:
//!
//! ```text
//! project.atmr (zip)
//! ├── graph.json
//! └── assets/
//!     ├── <hash>.<ext>          # raw bytes (3mf / png / svg / ...)
//!     ├── <hash>.<ext>
//!     └── manifest.json         # hash → original filename + label
//! ```
//!
//! ## Identity = content
//!
//! Each asset is keyed by a SHA-256 of its bytes, formatted as
//! `sha256-<64 hex chars>`. Two nodes that reference the same file end
//! up sharing the same entry — zero-cost deduplication. The reference
//! is content-addressed, so renaming or moving the source file on disk
//! doesn't break the project.
//!
//! ## Why a manifest
//!
//! The hash makes the in-zip filename machine-friendly but human-
//! useless: the UI wants to show "bunny.stl", not
//! `sha256-7a5c8f...12.3mf`. `manifest.json` is a small side-car that
//! records the original filename and an optional user-supplied label
//! for every asset. It's never load-bearing — losing the manifest
//! degrades UI labels but doesn't break the graph.
//!
//! ## Why no per-type bucketing
//!
//! All assets sit flat under `assets/`. The file extension carries the
//! type; the storage layer doesn't need to know mesh from image. That
//! keeps this module identical-shape when we add `.png`, `.svg`, or
//! `.csv` later.

use std::collections::HashMap;
use std::io::{Read, Write};

use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

/// Identifier for an embedded asset. Content-addressed: equal bytes
/// always produce equal `AssetRef`s, so two MeshNodes that loaded the
/// same .stl converge on a single zip entry.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AssetRef(String);

impl AssetRef {
    /// Hash the input bytes and produce a reference.
    pub fn from_bytes(bytes: &[u8]) -> Self {
        Self(format!("sha256-{}", sha256_hex(bytes)))
    }

    /// Construct from an already-formatted identifier (e.g. one
    /// recovered from a JSON property). Returns `None` if the string
    /// doesn't look like a well-formed `sha256-…` ref.
    pub fn parse(s: &str) -> Option<Self> {
        if !s.starts_with("sha256-") {
            return None;
        }
        let hex = &s[7..];
        if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        Some(Self(s.to_string()))
    }

    /// Raw identifier string. Use this for serialization and display.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AssetRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One asset's payload + UI metadata.
#[derive(Clone, Debug)]
pub struct AssetEntry {
    pub bytes: Vec<u8>,
    /// Filename the asset was originally imported under, e.g.
    /// `"bunny.stl"`. Used by the inspector and the file menu.
    pub original_filename: String,
    /// Optional author-supplied label. Wins over the original filename
    /// in UI surfaces when set.
    pub label: Option<String>,
    /// Extension stored inside the zip — derived from
    /// `original_filename` unless overridden at insert time (the only
    /// override we use today: meshes always re-encode to `.3mf`).
    pub extension: String,
}

/// In-memory map of an `.atmr` archive's asset payload.
#[derive(Default, Debug)]
pub struct AssetStore {
    assets: HashMap<AssetRef, AssetEntry>,
}

impl AssetStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.assets.is_empty()
    }

    pub fn len(&self) -> usize {
        self.assets.len()
    }

    /// Iterate asset references in deterministic order. Useful for
    /// snapshot tests and for writing the zip in stable layout.
    pub fn refs_sorted(&self) -> Vec<AssetRef> {
        let mut v: Vec<AssetRef> = self.assets.keys().cloned().collect();
        v.sort();
        v
    }

    pub fn get(&self, r: &AssetRef) -> Option<&AssetEntry> {
        self.assets.get(r)
    }

    /// Add bytes to the store. Returns the (possibly pre-existing)
    /// asset reference; the `original_filename` of the first insert
    /// for a given hash wins.
    ///
    /// `extension_override` lets the caller pin the in-zip extension
    /// regardless of what the original filename said. Mesh imports use
    /// this to always store assets as `.3mf` even when the user dropped
    /// a `.stl` or `.obj`.
    pub fn insert(
        &mut self,
        bytes: Vec<u8>,
        original_filename: String,
        label: Option<String>,
        extension_override: Option<String>,
    ) -> AssetRef {
        let r = AssetRef::from_bytes(&bytes);
        let ext = extension_override.unwrap_or_else(|| extract_extension(&original_filename));
        self.assets.entry(r.clone()).or_insert(AssetEntry {
            bytes,
            original_filename,
            label,
            extension: ext,
        });
        r
    }

    /// Update or set a label on an asset. No-op when the ref is unknown.
    pub fn set_label(&mut self, r: &AssetRef, label: Option<String>) {
        if let Some(e) = self.assets.get_mut(r) {
            e.label = label;
        }
    }

    /// Write every asset and the manifest to the supplied `ZipWriter`
    /// under the `assets/` prefix. Compression is `Stored` so already-
    /// compressed payloads (3MF, PNG) don't get re-deflated for nothing.
    pub fn write_into_zip<W: Write + std::io::Seek>(
        &self,
        zw: &mut ZipWriter<W>,
    ) -> Result<(), zip::result::ZipError> {
        if self.assets.is_empty() {
            return Ok(());
        }
        let stored: SimpleFileOptions =
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        let deflated: SimpleFileOptions = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(6));

        for r in self.refs_sorted() {
            let entry = &self.assets[&r];
            let name = format!("assets/{}.{}", &r.0, entry.extension);
            // Pick deflate only when the payload is plausibly
            // compressible (plain text + raw RGBA-ish). 3MF / PNG / JPG
            // / SVG-with-base64 are already compressed.
            let opts = if extension_is_compressible(&entry.extension) {
                deflated
            } else {
                stored
            };
            zw.start_file(&name, opts)?;
            zw.write_all(&entry.bytes)?;
        }

        let manifest = ManifestFile::from_store(self);
        let manifest_json = serde_json::to_string_pretty(&manifest)
            .expect("AssetManifest is always serializable");
        zw.start_file("assets/manifest.json", deflated)?;
        zw.write_all(manifest_json.as_bytes())?;
        Ok(())
    }

    /// Walk an `.atmr` archive and pull every `assets/*` entry into
    /// memory. Archives that pre-date the asset feature simply yield
    /// an empty store (the `assets/` directory is absent).
    pub fn read_from_zip<R: Read + std::io::Seek>(
        archive: &mut ZipArchive<R>,
    ) -> Result<Self, zip::result::ZipError> {
        let mut store = AssetStore::new();
        let mut manifest: Option<ManifestFile> = None;
        let mut raw_bytes: HashMap<String, (Vec<u8>, String)> = HashMap::new();

        for i in 0..archive.len() {
            let mut entry = archive.by_index(i)?;
            let name = entry.name().to_string();
            if !name.starts_with("assets/") {
                continue;
            }
            if name == "assets/manifest.json" {
                let mut s = String::new();
                entry.read_to_string(&mut s)?;
                if let Ok(m) = serde_json::from_str::<ManifestFile>(&s) {
                    manifest = Some(m);
                }
                continue;
            }
            // `assets/<ref>.<ext>`
            let stem_with_ext = &name["assets/".len()..];
            let (stem, ext) = match stem_with_ext.rsplit_once('.') {
                Some((s, e)) => (s.to_string(), e.to_string()),
                None => continue, // ignore files without an extension
            };
            let mut bytes = Vec::with_capacity(entry.size() as usize);
            entry.read_to_end(&mut bytes)?;
            raw_bytes.insert(stem, (bytes, ext));
        }

        let manifest_entries = manifest
            .map(|m| {
                m.assets
                    .into_iter()
                    .map(|e| (e.id.clone(), e))
                    .collect::<HashMap<_, _>>()
            })
            .unwrap_or_default();

        for (stem, (bytes, ext)) in raw_bytes {
            let asset_ref = match AssetRef::parse(&stem) {
                Some(r) => r,
                None => continue, // bogus filename, skip
            };
            let manifest_entry = manifest_entries.get(stem.as_str());
            let original_filename = manifest_entry
                .map(|e| e.original_filename.clone())
                .unwrap_or_else(|| format!("{}.{}", stem, ext));
            let label = manifest_entry.and_then(|e| e.label.clone());
            store.assets.insert(
                asset_ref,
                AssetEntry {
                    bytes,
                    original_filename,
                    label,
                    extension: ext,
                },
            );
        }

        Ok(store)
    }
}

/// SHA-256 implementation. Self-contained so the asset store doesn't
/// pull in a hashing crate just for this — content hashing is the only
/// place atomartist needs SHA-256.
fn sha256_hex(bytes: &[u8]) -> String {
    let h = sha256(bytes);
    let mut s = String::with_capacity(64);
    for b in h {
        s.push_str(&format!("{:02x}", b));
    }
    s
}

fn sha256(input: &[u8]) -> [u8; 32] {
    // Constants per FIPS 180-4.
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    // Pre-processing: pad + append length.
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(input.len() + 64);
    msg.extend_from_slice(input);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    // Process each 512-bit block.
    for block in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            let b = &block[i * 4..i * 4 + 4];
            w[i] = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let (mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh) = (
            h[0], h[1], h[2], h[3], h[4], h[5], h[6], h[7],
        );

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = hh
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);
            hh = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
        h[5] = h[5].wrapping_add(f);
        h[6] = h[6].wrapping_add(g);
        h[7] = h[7].wrapping_add(hh);
    }

    let mut out = [0u8; 32];
    for (i, w) in h.iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&w.to_be_bytes());
    }
    out
}

fn extract_extension(filename: &str) -> String {
    filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_else(|| "bin".to_string())
}

fn extension_is_compressible(ext: &str) -> bool {
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "txt" | "md" | "csv" | "tsv" | "svg" | "json" | "xml" | "log"
    )
}

// --- manifest.json schema ---------------------------------------------------

#[derive(Serialize, Deserialize, Default)]
struct ManifestFile {
    /// Schema version — bump when we make a backwards-incompatible
    /// change. `1` is "single flat list, hash-keyed assets".
    #[serde(default = "default_version")]
    version: u32,
    #[serde(default)]
    assets: Vec<ManifestEntry>,
}

fn default_version() -> u32 {
    1
}

#[derive(Serialize, Deserialize)]
struct ManifestEntry {
    /// The asset reference string, e.g. `sha256-7a5c8f…`. Stored as
    /// `id` rather than `hash` so the format stays meaningful when we
    /// eventually adopt a stronger digest.
    id: String,
    original_filename: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    label: Option<String>,
}

impl ManifestFile {
    fn from_store(store: &AssetStore) -> Self {
        let mut entries: Vec<ManifestEntry> = store
            .refs_sorted()
            .into_iter()
            .map(|r| {
                let e = &store.assets[&r];
                ManifestEntry {
                    id: r.0.clone(),
                    original_filename: e.original_filename.clone(),
                    label: e.label.clone(),
                }
            })
            .collect();
        entries.sort_by(|a, b| a.id.cmp(&b.id));
        Self { version: 1, assets: entries }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn sha256_matches_known_vector() {
        // RFC 6234 test vector for "abc".
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn same_bytes_produce_same_ref() {
        let a = AssetRef::from_bytes(b"hello world");
        let b = AssetRef::from_bytes(b"hello world");
        assert_eq!(a, b);
    }

    #[test]
    fn ref_parse_accepts_well_formed_id() {
        let r = AssetRef::from_bytes(b"x");
        assert_eq!(AssetRef::parse(r.as_str()), Some(r));
        assert!(AssetRef::parse("notahash").is_none());
        assert!(AssetRef::parse("sha256-tooshort").is_none());
    }

    #[test]
    fn insert_dedups_by_content() {
        let mut s = AssetStore::new();
        let a = s.insert(vec![1, 2, 3], "first.bin".into(), None, None);
        let b = s.insert(vec![1, 2, 3], "second.bin".into(), None, None);
        assert_eq!(a, b);
        // First insert's filename wins — second one is ignored.
        assert_eq!(s.get(&a).unwrap().original_filename, "first.bin");
        assert_eq!(s.len(), 1);
    }

    #[test]
    fn extension_override_pins_in_zip_extension() {
        let mut s = AssetStore::new();
        let r = s.insert(
            b"<not really 3mf>".to_vec(),
            "bunny.stl".into(),
            None,
            Some("3mf".into()),
        );
        assert_eq!(s.get(&r).unwrap().extension, "3mf");
        assert_eq!(s.get(&r).unwrap().original_filename, "bunny.stl");
    }

    #[test]
    fn store_round_trips_through_zip() {
        let mut original = AssetStore::new();
        let r1 = original.insert(vec![1, 2, 3, 4], "a.3mf".into(), Some("Bunny".into()), None);
        let r2 = original.insert(b"hello".to_vec(), "notes.md".into(), None, None);

        // Wrap in a zip writer, dump, re-read.
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut zw = ZipWriter::new(cursor);
            original.write_into_zip(&mut zw).unwrap();
            zw.finish().unwrap();
        }
        let cursor = Cursor::new(buf.as_slice());
        let mut archive = ZipArchive::new(cursor).unwrap();
        let recovered = AssetStore::read_from_zip(&mut archive).unwrap();

        assert_eq!(recovered.len(), 2);
        assert_eq!(recovered.get(&r1).unwrap().bytes, vec![1, 2, 3, 4]);
        assert_eq!(recovered.get(&r1).unwrap().original_filename, "a.3mf");
        assert_eq!(recovered.get(&r1).unwrap().label.as_deref(), Some("Bunny"));
        assert_eq!(recovered.get(&r2).unwrap().bytes, b"hello".to_vec());
    }

    #[test]
    fn missing_manifest_recovers_filename_from_zip_entry() {
        // Write only the asset entry, skip the manifest — the loader
        // should fall back to `<hash>.<ext>` as the filename.
        let r = AssetRef::from_bytes(b"raw");
        let mut buf: Vec<u8> = Vec::new();
        {
            let cursor = Cursor::new(&mut buf);
            let mut zw = ZipWriter::new(cursor);
            zw.start_file(
                format!("assets/{}.bin", r.as_str()),
                SimpleFileOptions::default(),
            )
            .unwrap();
            zw.write_all(b"raw").unwrap();
            zw.finish().unwrap();
        }
        let cursor = Cursor::new(buf.as_slice());
        let mut archive = ZipArchive::new(cursor).unwrap();
        let s = AssetStore::read_from_zip(&mut archive).unwrap();
        let entry = s.get(&r).expect("asset survived round trip");
        assert_eq!(entry.bytes, b"raw");
        assert!(
            entry.original_filename.ends_with(".bin"),
            "fallback name should retain the extension"
        );
    }
}
