//! Clearing old recordings out of the save folder.
//!
//! Only things SnapLapse made are ever touched: recording folders and ZIP files named like
//! `2026-10-14 19-30-05 - Foundry VTT`, and loose screenshots like `000001 - Foundry VTT.png`
//! that older versions saved straight into the folder. Anything else in the folder is left alone.

use std::path::{Path, PathBuf};

use crate::encode::Format;

/// What a cleanup would remove from a folder.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Cleanup {
    /// Every file and folder to delete, sorted by name.
    pub items: Vec<PathBuf>,
    /// Their total size on disk, in bytes.
    pub bytes: u64,
}

impl Cleanup {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Lists what SnapLapse made in `folder`. A missing folder has nothing to clean.
pub fn scan(folder: &Path) -> Cleanup {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Cleanup::default();
    };
    let mut items: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            match entry.file_type() {
                Ok(t) if t.is_dir() => is_recording_name(&name),
                Ok(t) if t.is_file() => is_recording_file(&name) || is_screenshot_file(&name),
                _ => false,
            }
        })
        .map(|entry| entry.path())
        .collect();
    items.sort();
    let bytes = items.iter().map(|p| size_on_disk(p)).sum();
    Cleanup { items, bytes }
}

/// Deletes everything in `cleanup`. Returns how many items were removed and the first error, if
/// any. Items that are already gone count as removed.
pub fn delete(cleanup: &Cleanup) -> (usize, Option<String>) {
    let mut removed = 0;
    let mut first_error = None;
    for path in &cleanup.items {
        let result = if path.is_dir() {
            std::fs::remove_dir_all(path)
        } else {
            std::fs::remove_file(path)
        };
        match result {
            Ok(()) => removed += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => removed += 1,
            Err(e) => {
                first_error.get_or_insert_with(|| {
                    let name = path.file_name().unwrap_or_default().to_string_lossy();
                    format!("couldn't delete {name}: {e}")
                });
            }
        }
    }
    (removed, first_error)
}

/// A size such as `2.4 GB`, for the confirmation message.
pub fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} bytes");
    }
    let mut size = bytes as f64 / 1024.0;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    format!("{size:.1} {}", UNITS[unit])
}

/// Whether a name starts with a recording's start time, as in `2026-10-14 19-30-05 - VTT`.
fn is_recording_name(name: &str) -> bool {
    let Some((time, title)) = name.split_once(" - ") else {
        return false;
    };
    let pattern = "0000-00-00 00-00-00";
    !title.is_empty()
        && time.len() == pattern.len()
        && time.bytes().zip(pattern.bytes()).all(|(c, p)| match p {
            b'0' => c.is_ascii_digit(),
            _ => c == p,
        })
}

/// A recording's ZIP file (or one part of a split ZIP), or one left half-written.
fn is_recording_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    is_recording_name(name) && (lower.ends_with(".zip") || lower.ends_with(".zip.part"))
}

/// A screenshot such as `000001 - VTT.png`, or one left half-written as `….png.part`.
fn is_screenshot_file(name: &str) -> bool {
    let Some((digits, rest)) = name.split_once(" - ") else {
        return false;
    };
    if digits.len() < 6 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let rest = rest.strip_suffix(".part").unwrap_or(rest);
    Path::new(rest)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| Format::from_extension(e).is_some())
}

fn size_on_disk(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .map(|entries| entries.flatten().map(|e| size_on_disk(&e.path())).sum())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(cleanup: &Cleanup) -> Vec<String> {
        cleanup
            .items
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn finds_only_what_snaplapse_made() {
        let dir = crate::test_dir("cleanup-scan");
        let rec = dir.join("2026-10-14 19-30-05 - Foundry VTT");
        std::fs::create_dir(&rec).unwrap();
        std::fs::write(rec.join("000001 - Foundry VTT.png"), [0u8; 100]).unwrap();
        for file in [
            "2026-10-14 20-00-00 - VTT.zip",
            "2026-10-14 21-00-00 - VTT - part 1.zip",
            "2026-10-14 21-00-00 - VTT - part 2.zip.part",
            "000042 - Old style.jpg",
            "000043 - Old style.webp.part",
        ] {
            std::fs::write(dir.join(file), [0u8; 10]).unwrap();
        }
        // None of these are SnapLapse's, so they must survive.
        std::fs::create_dir(dir.join("Holiday photos")).unwrap();
        std::fs::create_dir(dir.join("2026-10-14 - notes")).unwrap();
        for file in [
            "notes.txt",
            "2026-10-14 19-30-05 - VTT.txt",
            "000001 - report.pdf",
            "12 - short number.png",
            "photo.png",
        ] {
            std::fs::write(dir.join(file), b"keep").unwrap();
        }

        let found = scan(&dir);
        assert_eq!(
            names(&found),
            [
                "000042 - Old style.jpg",
                "000043 - Old style.webp.part",
                "2026-10-14 19-30-05 - Foundry VTT",
                "2026-10-14 20-00-00 - VTT.zip",
                "2026-10-14 21-00-00 - VTT - part 1.zip",
                "2026-10-14 21-00-00 - VTT - part 2.zip.part",
            ]
        );
        assert_eq!(found.bytes, 100 + 5 * 10);

        assert_eq!(delete(&found), (6, None));
        assert!(scan(&dir).is_empty());
        let mut left: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(
            left,
            [
                "000001 - report.pdf",
                "12 - short number.png",
                "2026-10-14 - notes",
                "2026-10-14 19-30-05 - VTT.txt",
                "Holiday photos",
                "notes.txt",
                "photo.png",
            ]
        );
    }

    #[test]
    fn a_missing_folder_has_nothing_to_clean() {
        let dir = crate::test_dir("cleanup-missing").join("not here");
        assert!(scan(&dir).is_empty());
    }

    #[test]
    fn already_deleted_items_count_as_removed() {
        let dir = crate::test_dir("cleanup-gone");
        let cleanup = Cleanup {
            items: vec![dir.join("2026-10-14 19-30-05 - VTT.zip")],
            bytes: 0,
        };
        assert_eq!(delete(&cleanup), (1, None));
    }

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_size(0), "0 bytes");
        assert_eq!(human_size(1023), "1023 bytes");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(human_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }
}
