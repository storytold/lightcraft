//! Read the city-list entry from a GeoNames ZIP within a caller-owned output limit.

use craft_zip::{Archive, Limits};

use crate::GeocodeError;

fn bad(what: &str) -> GeocodeError {
    GeocodeError::Invalid(format!("not a readable ZIP archive ({what})"))
}

/// The contents of the first entry whose name ends with `name`, at most `max` bytes.
pub fn extract(zip: &[u8], name: &str, max: usize) -> Result<Vec<u8>, GeocodeError> {
    let max_bytes = u64::try_from(max).map_err(|_| bad("output limit"))?;
    let mut archive =
        Archive::open(zip, Limits { max_entries: u64::from(u16::MAX), max_total_bytes: max_bytes }).map_err(|error| bad(&error.to_string()))?;
    // Exact duplicate names make the shared index's selected entry ambiguous.
    if archive.has_duplicate_names() {
        return Err(bad("duplicate entry name"));
    }
    // Different raw names can decode to the same text. Keep the first matching index.
    let mut selected = None;
    for index in 0..archive.len() {
        let entry = archive.entry_name(index).map_err(|error| bad(&error.to_string()))?;
        if entry.is_some_and(|entry| entry.ends_with(name)) {
            selected = Some(index);
            break;
        }
    }
    let index = selected.ok_or_else(|| GeocodeError::Invalid(format!("{name} is not in the archive")))?;
    let (declared, _) = archive.entry_sizes(index).map_err(|error| bad(&error.to_string()))?.ok_or_else(|| bad("entry size"))?;
    let decoded = archive
        .read_index(index, max_bytes)
        .map_err(
            |error| {
                if error.is_limit() { GeocodeError::Invalid(format!("{name} is larger than {max} bytes")) } else { bad(&error.to_string()) }
            },
        )?
        .ok_or_else(|| bad("entry"))?;
    // A short read can remain within the output budget, so check the exact size too.
    if decoded.len() as u64 != declared {
        return Err(bad("uncompressed size"));
    }
    Ok(decoded)
}

/// A one-file deflated ZIP (test fixtures: the engine's download test serves one). A writer error
/// gives an empty archive, which [`extract`] refuses.
#[doc(hidden)]
pub fn make(name: &str, data: &[u8]) -> Vec<u8> {
    let mut writer = craft_zip::Writer::new();
    if writer.add(name, data, craft_zip::Compression::Deflated).is_err() {
        return Vec::new();
    }
    writer.finish().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use craft_zip::{Compression, Writer};

    #[test]
    fn round_trip_and_junk() {
        let z = make("cities15000.txt", b"hello world");
        assert_eq!(extract(&z, "cities15000.txt", 100).unwrap(), b"hello world");
        assert!(extract(&z, "cities15000.txt", 5).is_err());
        assert!(extract(&z, "other.txt", 100).is_err());
        assert!(extract(b"PK", "x", 10).is_err());
        assert!(extract(b"", "x", 10).is_err());
        for cut in 0..z.len() {
            let _ = extract(&z[..cut], "cities15000.txt", 100);
        }
        for i in 0..z.len() {
            let mut bad = z.clone();
            bad[i] ^= 0xff;
            let _ = extract(&bad, "cities15000.txt", 100);
        }
    }

    #[test]
    fn rejects_deflated_entry_with_inconsistent_directory_size() {
        let mut z = make("cities15000.txt", b"hello world");
        let eocd = z.len() - 22;
        let cd = u32::from_le_bytes([z[eocd + 16], z[eocd + 17], z[eocd + 18], z[eocd + 19]]) as usize;
        let declared_size = cd + 24;
        z[declared_size..declared_size + 4].copy_from_slice(&2u32.to_le_bytes());

        assert!(extract(&z, "cities15000.txt", 100).is_err());
    }

    #[test]
    fn reads_stored_and_deflated_entries_at_the_exact_limit() {
        for compression in [Compression::Stored, Compression::Deflated] {
            let zip = archive(&[("folder/cities15000.txt", b"Paris\n")], compression);
            assert_eq!(extract(&zip, "cities15000.txt", 6).unwrap(), b"Paris\n");
            assert!(extract(&zip, "cities15000.txt", 5).is_err());
            assert_eq!(extract(&zip, "missing.txt", 6).unwrap_err().to_string(), "missing.txt is not in the archive");
        }
    }

    #[test]
    fn reads_the_first_distinct_suffix_match_in_directory_order() {
        let zip = archive(&[("z/cities15000.txt", b"first"), ("a/cities15000.txt", b"second")], Compression::Deflated);
        assert_eq!(extract(&zip, "cities15000.txt", 100).unwrap(), b"first");
    }

    #[test]
    fn reads_first_suffix_match_with_colliding_decoded_names() {
        let mut zip = archive(&[("a/cities15000.txt", b"first"), ("é/cities15000.txt", b"second")], Compression::Stored);
        let first_directory = directory_entries(&zip)[0];
        // Keep the name length, replace 'a' with CP437 'é' and clear the UTF-8 flags.
        zip[30] = 0x82;
        zip[7] &= !0x08;
        zip[first_directory + 46] = 0x82;
        zip[first_directory + 9] &= !0x08;

        assert_eq!(extract(&zip, "cities15000.txt", 100).unwrap(), b"first");
    }

    #[test]
    fn rejects_bad_checksums_without_trying_a_later_suffix_match() {
        for compression in [Compression::Stored, Compression::Deflated] {
            let mut zip = archive(&[("a/cities15000.txt", b"first"), ("b/cities15000.txt", b"second")], compression);
            let directory = directory_entries(&zip);
            zip[14..18].copy_from_slice(&0u32.to_le_bytes());
            zip[directory[0] + 16..directory[0] + 20].copy_from_slice(&0u32.to_le_bytes());
            assert!(extract(&zip, "cities15000.txt", 100).is_err());
        }
    }

    #[test]
    fn rejects_duplicate_exact_member_names() {
        let mut zip = archive(&[("a/cities15000.txt", b"first"), ("b/cities15000.txt", b"second")], Compression::Stored);
        let names: Vec<_> =
            zip.windows(b"b/cities15000.txt".len()).enumerate().filter_map(|(at, bytes)| (bytes == b"b/cities15000.txt").then_some(at)).collect();
        assert_eq!(names.len(), 2);
        for at in names {
            zip[at] = b'a';
        }
        assert!(extract(&zip, "cities15000.txt", 100).is_err());
    }

    #[test]
    fn rejects_shorter_and_longer_declared_sizes() {
        for compression in [Compression::Stored, Compression::Deflated] {
            for declared in [4u32, 6] {
                let mut zip = archive(&[("cities15000.txt", b"Paris")], compression);
                let directory = directory_entries(&zip)[0];
                zip[22..26].copy_from_slice(&declared.to_le_bytes());
                zip[directory + 24..directory + 28].copy_from_slice(&declared.to_le_bytes());
                assert!(extract(&zip, "cities15000.txt", 100).is_err(), "{compression:?}, declared {declared}");
            }
        }
    }

    #[test]
    fn rejects_truncation_and_unsupported_compression() {
        let original = archive(&[("cities15000.txt", b"Paris")], Compression::Stored);
        for cut in [0, 4, 29, original.len() - 1] {
            assert!(extract(&original[..cut], "cities15000.txt", 100).is_err());
        }
        let mut unsupported = original.clone();
        let directory = directory_entries(&unsupported)[0];
        unsupported[8..10].copy_from_slice(&99u16.to_le_bytes());
        unsupported[directory + 10..directory + 12].copy_from_slice(&99u16.to_le_bytes());
        assert!(extract(&unsupported, "cities15000.txt", 100).is_err());

        let mut truncated_payload = original;
        truncated_payload[directory + 20..directory + 24].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(extract(&truncated_payload, "cities15000.txt", 100).is_err());
    }

    fn archive(entries: &[(&str, &[u8])], compression: Compression) -> Vec<u8> {
        let mut writer = Writer::new();
        for (name, bytes) in entries {
            writer.add(name, bytes, compression).unwrap();
        }
        writer.finish().unwrap()
    }

    fn directory_entries(zip: &[u8]) -> Vec<usize> {
        let end = zip.len() - 22;
        assert_eq!(&zip[end..end + 4], b"PK\x05\x06");
        let count = u16::from_le_bytes(zip[end + 10..end + 12].try_into().unwrap());
        let mut at = u32::from_le_bytes(zip[end + 16..end + 20].try_into().unwrap()) as usize;
        let mut entries = Vec::new();
        for _ in 0..count {
            assert_eq!(&zip[at..at + 4], b"PK\x01\x02");
            entries.push(at);
            let name = u16::from_le_bytes(zip[at + 28..at + 30].try_into().unwrap()) as usize;
            let extra = u16::from_le_bytes(zip[at + 30..at + 32].try_into().unwrap()) as usize;
            let comment = u16::from_le_bytes(zip[at + 32..at + 34].try_into().unwrap()) as usize;
            at += 46 + name + extra + comment;
        }
        entries
    }
}
