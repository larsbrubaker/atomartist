//! Package thumbnails: the `Metadata/thumbnail.png` entry shared by
//! `.atmr` projects and OPC-flavoured packages such as `.3mf`.
//!
//! `.atmr` is a zip (see [`super::atmr`]), so it can carry a preview
//! image the same way 3MF does — under `Metadata/thumbnail.png`, with an
//! optional OPC *relationship* in `_rels/.rels` pointing at it. Writing
//! the entry lives in [`super::atmr`] (it is part of encoding a project);
//! *reading* it lives here because the reader is deliberately format
//! agnostic: the file browser wants a preview out of any package it can
//! find one in, project or mesh, without decoding the payload.
//!
//! [`read_thumbnail_from_bytes`] never decodes a graph, an asset, or a
//! mesh: it opens the zip central directory and extracts only the small
//! set of entries a preview can live in — `_rels/.rels` plus the
//! declared and conventional image paths, every one of them size-capped
//! (see [`MAX_THUMBNAIL_BYTES`]). Anything that is not a zip, or a zip
//! without a preview, is `None` —
//! a missing thumbnail is normal (the entry is optional forever), so
//! there is no error type here.

use std::io::{Cursor, Read};

use quick_xml::events::Event;
use quick_xml::Reader;

/// Where a preview image lives inside an `.atmr` (and, by the same OPC
/// convention, inside most 3MF packages).
pub const THUMBNAIL_ENTRY_NAME: &str = "Metadata/thumbnail.png";

/// OPC package-relationships part. 3MF writers are supposed to declare
/// the thumbnail here rather than rely on the conventional path, and
/// some of them put the image somewhere else entirely — so this is
/// consulted first.
const RELS_ENTRY_NAME: &str = "_rels/.rels";

/// Conventional locations checked when the package has no thumbnail
/// relationship. `plate_1.png` is the Bambu/Orca slicer convention that
/// NodeDesigner's local backend also falls back to.
const CONVENTIONAL_PATHS: &[&str] = &[
    THUMBNAIL_ENTRY_NAME,
    "Metadata/thumbnail.jpg",
    "Metadata/plate_1.png",
];

/// Hard ceiling on how many bytes we will extract for a preview image.
/// The zip header's declared size is *not* trustworthy — a few-hundred
/// byte file can claim gigabytes, and a deflate bomb can genuinely
/// inflate to them — so extraction reserves at most this much and stops
/// reading one byte past it. 4 MB is far beyond any real preview (ours
/// is a 256×192 PNG, a few KB) and well inside a comfortable allocation.
const MAX_THUMBNAIL_BYTES: u64 = 4 * 1024 * 1024;

/// The same ceiling for `_rels/.rels`, which is a handful of XML
/// elements in every package that has one. A `.rels` bigger than this
/// is not worth parsing: the conventional paths are still checked.
const MAX_RELS_BYTES: u64 = 64 * 1024;

/// Extract the preview image bytes from a zip package (`.atmr`, `.3mf`,
/// or anything else following the OPC layout), or `None` when there
/// isn't one.
///
/// Lookup order mirrors NodeDesigner's `dfs-local-backend.ts`:
///
/// 1. the OPC thumbnail *relationship* declared in `_rels/.rels`, and
/// 2. the conventional paths ([`THUMBNAIL_ENTRY_NAME`] first).
///
/// The returned bytes are the stored image exactly as written — PNG for
/// everything we produce, possibly JPEG for a foreign package.
pub fn read_thumbnail_from_bytes(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).ok()?;

    // Every thumbnail-typed relationship gets a chance: writers have
    // shipped packages whose first one dangles (a preview that was
    // removed but not de-declared) with a good one right behind it.
    for target in thumbnail_relationship_targets(&mut archive) {
        for candidate in target_candidates(&target) {
            if let Some(found) = read_entry(&mut archive, &candidate, MAX_THUMBNAIL_BYTES) {
                return Some(found);
            }
        }
    }
    for name in CONVENTIONAL_PATHS {
        if let Some(found) = read_entry(&mut archive, name, MAX_THUMBNAIL_BYTES) {
            return Some(found);
        }
    }
    None
}

/// The names to try for one relationship `Target`, most faithful first:
/// the target exactly as written (entry names are case- and
/// separator-sensitive, and a package may genuinely contain a member
/// with a leading `./`), then a normalised form — backslashes turned
/// into `/`, and any leading `/` or `./` stripped, which is how OPC
/// part names relate to zip entry names.
fn target_candidates(target: &str) -> Vec<String> {
    let mut out = Vec::with_capacity(2);
    if !target.is_empty() {
        out.push(target.to_string());
    }
    let mut normalized = target.replace('\\', "/");
    loop {
        let trimmed = normalized
            .strip_prefix("./")
            .or_else(|| normalized.strip_prefix('/'));
        match trimmed {
            Some(t) => normalized = t.to_string(),
            None => break,
        }
    }
    if !normalized.is_empty() && !out.contains(&normalized) {
        out.push(normalized);
    }
    out
}

/// Read one entry by name, extracting **at most `limit` bytes**.
///
/// The zip header's `uncompressed_size` is attacker-controlled and the
/// compressed stream can inflate without bound, so the reservation is
/// clamped and the read is capped one byte past `limit`: an entry that
/// exceeds it is reported as absent rather than allocated. Returns
/// `None` for a missing, oversized, or unreadable entry — a corrupt
/// member must not sink the whole lookup.
fn read_entry<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    limit: u64,
) -> Option<Vec<u8>> {
    if name.is_empty() {
        return None;
    }
    let entry = archive.by_name(name).ok()?;
    let reserve = entry.size().min(limit) as usize;
    let mut buf = Vec::with_capacity(reserve);
    // `limit + 1` so an entry of exactly `limit` bytes still reads
    // whole while anything larger is detectable by the length check.
    entry.take(limit.saturating_add(1)).read_to_end(&mut buf).ok()?;
    if buf.is_empty() || buf.len() as u64 > limit {
        None
    } else {
        Some(buf)
    }
}

/// Parse `_rels/.rels` and return the `Target` of every relationship
/// whose `Type` mentions "thumbnail" (case-insensitively — the OPC type
/// URI is
/// `http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail`,
/// but 3MF writers have shipped several spellings of the host part), in
/// document order. Empty when there is no `.rels`, it is unparseable, or
/// it is implausibly large.
fn thumbnail_relationship_targets<R: Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Vec<String> {
    match read_entry(archive, RELS_ENTRY_NAME, MAX_RELS_BYTES) {
        Some(xml) => parse_thumbnail_targets(&xml),
        None => Vec::new(),
    }
}

fn parse_thumbnail_targets(xml: &[u8]) -> Vec<String> {
    let mut targets = Vec::new();
    let xml = String::from_utf8_lossy(xml);
    let mut reader = Reader::from_str(&xml);
    reader.config_mut().trim_text(true);
    let mut buf = Vec::new();
    loop {
        let event = match reader.read_event_into(&mut buf) {
            Ok(event) => event,
            // Malformed XML: keep whatever we collected before the
            // damage — a truncated `.rels` still names real parts.
            Err(_) => return targets,
        };
        match event {
            Event::Empty(e) | Event::Start(e) => {
                if e.local_name().as_ref() != b"Relationship" {
                    buf.clear();
                    continue;
                }
                let mut rel_type: Option<String> = None;
                let mut target: Option<String> = None;
                for attr in e.attributes().flatten() {
                    // `unescape_value` resolves `&amp;` and friends;
                    // raw attribute bytes would look for an entry whose
                    // name literally contains the entity.
                    let value = match attr.unescape_value() {
                        Ok(v) => v.into_owned(),
                        Err(_) => String::from_utf8_lossy(&attr.value).into_owned(),
                    };
                    match attr.key.local_name().as_ref() {
                        b"Type" => rel_type = Some(value),
                        b"Target" => target = Some(value),
                        _ => {}
                    }
                }
                let is_thumb = rel_type
                    .map(|t| t.to_ascii_lowercase().contains("thumbnail"))
                    .unwrap_or(false);
                if is_thumb {
                    if let Some(target) = target {
                        targets.push(target);
                    }
                }
            }
            Event::Eof => return targets,
            _ => {}
        }
        buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use zip::write::SimpleFileOptions;
    use zip::{CompressionMethod, ZipWriter};

    /// Build a zip in memory from `(name, bytes)` pairs.
    fn zip_with(entries: &[(&str, &[u8])]) -> Vec<u8> {
        zip_with_method(entries, CompressionMethod::Stored)
    }

    fn zip_with_method(entries: &[(&str, &[u8])], method: CompressionMethod) -> Vec<u8> {
        let mut zw = ZipWriter::new(Cursor::new(Vec::new()));
        let opts = SimpleFileOptions::default().compression_method(method);
        for (name, bytes) in entries {
            zw.start_file(*name, opts).expect("start entry");
            zw.write_all(bytes).expect("write entry");
        }
        zw.finish().expect("finish zip").into_inner()
    }

    #[test]
    fn oversized_thumbnail_entry_is_rejected() {
        // A stored entry larger than the extraction cap: no preview is
        // worth 5 MB, and honouring the header's size would let a
        // hostile package dictate our allocation.
        let big = vec![0x41u8; (MAX_THUMBNAIL_BYTES + 1) as usize];
        let bytes = zip_with(&[(THUMBNAIL_ENTRY_NAME, &big)]);
        assert!(read_thumbnail_from_bytes(&bytes).is_none());
    }

    #[test]
    fn compressed_bomb_entry_is_rejected_without_inflating_it_all() {
        // 16 MB of zeros deflates to a few KB. Reading it to the end
        // would allocate 16 MB from a ~16 KB file; the cap stops the
        // inflate a byte past the limit.
        let bomb = vec![0u8; 16 * 1024 * 1024];
        let bytes = zip_with_method(
            &[(THUMBNAIL_ENTRY_NAME, &bomb)],
            CompressionMethod::Deflated,
        );
        assert!(bytes.len() < 64 * 1024, "bomb must be small on disk");
        assert!(read_thumbnail_from_bytes(&bytes).is_none());
    }

    #[test]
    fn entry_at_exactly_the_cap_is_still_accepted() {
        let at_cap = vec![0x42u8; MAX_THUMBNAIL_BYTES as usize];
        let bytes = zip_with(&[(THUMBNAIL_ENTRY_NAME, &at_cap)]);
        assert_eq!(
            read_thumbnail_from_bytes(&bytes).map(|v| v.len()),
            Some(MAX_THUMBNAIL_BYTES as usize)
        );
    }

    #[test]
    fn oversized_rels_is_ignored_and_the_conventional_path_still_wins() {
        // A `.rels` bigger than the parse cap is not worth streaming —
        // the conventional path is a perfectly good answer.
        let mut rels = Vec::new();
        rels.extend_from_slice(b"<Relationships>");
        while rels.len() < (MAX_RELS_BYTES as usize) + 1 {
            rels.extend_from_slice(b"<!-- padding padding padding padding -->");
        }
        rels.extend_from_slice(b"</Relationships>");
        let bytes = zip_with(&[
            ("_rels/.rels", &rels),
            (THUMBNAIL_ENTRY_NAME, b"conventional"),
        ]);
        assert_eq!(
            read_thumbnail_from_bytes(&bytes).as_deref(),
            Some(&b"conventional"[..])
        );
    }

    #[test]
    fn later_relationship_wins_when_the_first_one_dangles() {
        // Two thumbnail-typed relationships, the first pointing at a
        // missing part. No conventional entry exists, so only walking
        // *every* thumbnail relationship finds the image.
        let rels = br#"<Relationships>
  <Relationship Id="a" Type=".../metadata/thumbnail" Target="/Metadata/gone.png"/>
  <Relationship Id="b" Type=".../metadata/thumbnail" Target="/Metadata/plate_9.png"/>
</Relationships>"#;
        let bytes = zip_with(&[("_rels/.rels", rels), ("Metadata/plate_9.png", b"second")]);
        assert_eq!(
            read_thumbnail_from_bytes(&bytes).as_deref(),
            Some(&b"second"[..])
        );
    }

    #[test]
    fn windows_style_and_dot_relative_targets_resolve() {
        let rels = br#"<Relationships><Relationship Id="r" Type=".../metadata/thumbnail" Target="\Metadata\plate_2.png"/></Relationships>"#;
        let bytes = zip_with(&[("_rels/.rels", rels), ("Metadata/plate_2.png", b"backslash")]);
        assert_eq!(
            read_thumbnail_from_bytes(&bytes).as_deref(),
            Some(&b"backslash"[..])
        );

        let rels = br#"<Relationships><Relationship Id="r" Type=".../metadata/thumbnail" Target="./Metadata/plate_3.png"/></Relationships>"#;
        let bytes = zip_with(&[("_rels/.rels", rels), ("Metadata/plate_3.png", b"dotslash")]);
        assert_eq!(
            read_thumbnail_from_bytes(&bytes).as_deref(),
            Some(&b"dotslash"[..])
        );
    }

    #[test]
    fn xml_escapes_in_the_target_are_unescaped() {
        let rels = br#"<Relationships><Relationship Id="r" Type=".../metadata/thumbnail" Target="/Metadata/a&amp;b.png"/></Relationships>"#;
        let bytes = zip_with(&[("_rels/.rels", rels), ("Metadata/a&b.png", b"escaped")]);
        assert_eq!(
            read_thumbnail_from_bytes(&bytes).as_deref(),
            Some(&b"escaped"[..])
        );
    }

    #[test]
    fn conventional_path_thumbnail_is_found() {
        let bytes = zip_with(&[
            ("3D/3dmodel.model", b"<model/>"),
            (THUMBNAIL_ENTRY_NAME, b"\x89PNG-conventional"),
        ]);
        assert_eq!(
            read_thumbnail_from_bytes(&bytes).as_deref(),
            Some(&b"\x89PNG-conventional"[..])
        );
    }

    #[test]
    fn rels_relationship_wins_over_conventional_path() {
        // A 3MF whose declared thumbnail is somewhere else entirely:
        // the relationship must be honoured, not the conventional path.
        let rels = br#"<?xml version="1.0" encoding="UTF-8"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">
  <Relationship Id="rel1" Type="http://schemas.openxmlformats.org/package/2006/relationships/metadata/thumbnail" Target="/Metadata/plate_2.png"/>
  <Relationship Id="rel0" Type="http://schemas.microsoft.com/3dmanufacturing/2013/01/3dmodel" Target="/3D/3dmodel.model"/>
</Relationships>"#;
        let bytes = zip_with(&[
            ("_rels/.rels", rels),
            ("Metadata/plate_2.png", b"declared"),
            (THUMBNAIL_ENTRY_NAME, b"conventional"),
        ]);
        assert_eq!(
            read_thumbnail_from_bytes(&bytes).as_deref(),
            Some(&b"declared"[..])
        );
    }

    #[test]
    fn dangling_relationship_falls_back_to_conventional_path() {
        let rels = br#"<Relationships><Relationship Id="r" Type=".../metadata/thumbnail" Target="/Metadata/missing.png"/></Relationships>"#;
        let bytes = zip_with(&[
            ("_rels/.rels", rels),
            (THUMBNAIL_ENTRY_NAME, b"conventional"),
        ]);
        assert_eq!(
            read_thumbnail_from_bytes(&bytes).as_deref(),
            Some(&b"conventional"[..])
        );
    }

    #[test]
    fn thumbnail_injected_into_a_real_3mf_export_is_found() {
        // Uses the production 3MF encoder, then appends the preview the
        // way a slicer would — the exact shape the file browser meets
        // when it lists a `.3mf` next to a project.
        let mesh = crate::geometry::generate_box(10.0, 10.0, 10.0);
        let three_mf = crate::serialization::export_3mf(&mesh).expect("export 3mf");
        let with_thumb = {
            let mut zw =
                ZipWriter::new_append(Cursor::new(three_mf)).expect("append to 3mf");
            let opts = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
            zw.start_file(THUMBNAIL_ENTRY_NAME, opts).expect("start entry");
            zw.write_all(b"\x89PNG-from-slicer").expect("write entry");
            zw.finish().expect("finish").into_inner()
        };

        assert_eq!(
            read_thumbnail_from_bytes(&with_thumb).as_deref(),
            Some(&b"\x89PNG-from-slicer"[..])
        );
        // The mesh payload is still readable — appending didn't damage it.
        assert!(crate::serialization::import_3mf(&with_thumb).is_ok());
    }

    #[test]
    fn zip_without_thumbnail_returns_none() {
        let bytes = zip_with(&[("graph.json", b"{}")]);
        assert!(read_thumbnail_from_bytes(&bytes).is_none());
    }

    #[test]
    fn non_zip_bytes_return_none_without_panicking() {
        assert!(read_thumbnail_from_bytes(b"not a zip at all").is_none());
        assert!(read_thumbnail_from_bytes(&[]).is_none());
        // Truncated zip: valid local-header magic, nothing else.
        assert!(read_thumbnail_from_bytes(b"PK\x03\x04garbage").is_none());
    }

    #[test]
    fn malformed_rels_xml_does_not_break_the_lookup() {
        let bytes = zip_with(&[
            ("_rels/.rels", b"<Relationships><Relationship "),
            (THUMBNAIL_ENTRY_NAME, b"conventional"),
        ]);
        assert_eq!(
            read_thumbnail_from_bytes(&bytes).as_deref(),
            Some(&b"conventional"[..])
        );
    }
}
