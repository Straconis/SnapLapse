//! Packing a finished recording's folder into ZIP files, split into parts if asked.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{Datelike, Local, Timelike};
use serde::{Deserialize, Serialize};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

/// The largest a ZIP part may be. Each part is a normal ZIP that opens on its own, not a spanned
/// archive (those need special tools on Windows).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ZipSplit {
    #[default]
    Off,
    Mb100,
    Mb500,
    Gb1,
    Gb2,
    /// Just under 4 GB, for USB drives formatted as FAT32, which can't hold larger files.
    Gb4,
}

impl ZipSplit {
    pub const ALL: [ZipSplit; 6] = [
        ZipSplit::Off,
        ZipSplit::Mb100,
        ZipSplit::Mb500,
        ZipSplit::Gb1,
        ZipSplit::Gb2,
        ZipSplit::Gb4,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ZipSplit::Off => "Don't split",
            ZipSplit::Mb100 => "100 MB parts",
            ZipSplit::Mb500 => "500 MB parts",
            ZipSplit::Gb1 => "1 GB parts",
            ZipSplit::Gb2 => "2 GB parts",
            ZipSplit::Gb4 => "4 GB parts (FAT32 drives)",
        }
    }

    pub fn max_bytes(self) -> Option<u64> {
        const MB: u64 = 1_000_000;
        match self {
            ZipSplit::Off => None,
            ZipSplit::Mb100 => Some(100 * MB),
            ZipSplit::Mb500 => Some(500 * MB),
            ZipSplit::Gb1 => Some(1_000 * MB),
            ZipSplit::Gb2 => Some(2_000 * MB),
            // FAT32's limit is 4 GiB minus one byte; stay safely below it.
            ZipSplit::Gb4 => Some(4_000 * MB),
        }
    }
}

/// Packs every file in `dir` into ZIP files beside it, then deletes the folder. Returns the ZIP
/// files made, in order.
///
/// With no size limit, or if everything fits in one part, the result is `<folder name>.zip`.
/// Otherwise it's `<folder name> - part 1.zip`, `- part 2.zip` and so on. Files stay in order,
/// and no file is split across parts. A single file bigger than the limit gets a part of its own.
///
/// Images are stored as they are, without compressing them again, because PNG, JPG and WebP are
/// already compressed. That keeps packing fast. If anything fails, every part made so far is
/// removed and the folder is left untouched.
pub fn zip_folder(dir: &Path, max_part_bytes: Option<u64>) -> io::Result<Vec<PathBuf>> {
    let name = dir
        .file_name()
        .ok_or_else(|| io::Error::other("the folder has no name"))?
        .to_string_lossy()
        .into_owned();

    let mut files: Vec<(PathBuf, u64)> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .map(|path| {
            let size = std::fs::metadata(&path).map_or(0, |m| m.len());
            (path, size)
        })
        .collect();
    files.sort();

    let parts = plan_parts(&files, max_part_bytes);
    let names = part_names(&name, parts.len());
    let mut made: Vec<PathBuf> = Vec::new();
    for (part, part_name) in parts.iter().zip(&names) {
        let zip_path = dir.with_file_name(part_name);
        let partial = dir.with_file_name(format!("{part_name}.part"));
        let written = write_zip(part, &partial).and_then(|()| std::fs::rename(&partial, &zip_path));
        if let Err(e) = written {
            let _ = std::fs::remove_file(&partial);
            for done in &made {
                let _ = std::fs::remove_file(done);
            }
            return Err(e);
        }
        made.push(zip_path);
    }
    std::fs::remove_dir_all(dir)?;
    Ok(made)
}

/// Roughly how many bytes a file takes inside a stored (uncompressed) ZIP: its data plus its
/// two headers. The extra margin covers optional header fields.
fn zip_cost(path: &Path, size: u64) -> u64 {
    let name_len = path.file_name().map_or(0, |n| n.to_string_lossy().len()) as u64;
    size + 30 + 46 + 2 * name_len + 64
}

/// The bytes every ZIP needs beyond its files (the end-of-archive record, with room to spare).
const ZIP_OVERHEAD: u64 = 128;

/// Groups files, in order, into parts that each stay under `max` bytes.
fn plan_parts(files: &[(PathBuf, u64)], max: Option<u64>) -> Vec<Vec<PathBuf>> {
    let mut parts: Vec<Vec<PathBuf>> = vec![Vec::new()];
    let mut used = ZIP_OVERHEAD;
    for (path, size) in files {
        let cost = zip_cost(path, *size);
        let current = parts.last_mut().expect("there is always a current part");
        if max.is_some_and(|max| !current.is_empty() && used + cost > max) {
            parts.push(Vec::new());
            used = ZIP_OVERHEAD;
        }
        parts
            .last_mut()
            .expect("there is always a current part")
            .push(path.clone());
        used += cost;
    }
    parts
}

/// `name.zip` for a single part, or `name - part 01.zip`… padded so the parts sort correctly.
fn part_names(name: &str, count: usize) -> Vec<String> {
    if count <= 1 {
        return vec![format!("{name}.zip")];
    }
    let width = count.to_string().len();
    (1..=count)
        .map(|n| format!("{name} - part {n:0width$}.zip"))
        .collect()
}

fn write_zip(files: &[PathBuf], path: &Path) -> io::Result<()> {
    let mut zip = ZipWriter::new(BufWriter::new(File::create(path)?));
    for file in files {
        let entry = file.file_name().unwrap_or_default().to_string_lossy();
        let mut options =
            SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        if let Some(time) = local_modified_time(file) {
            options = options.last_modified_time(time);
        }
        zip.start_file(entry.as_ref(), options)
            .map_err(io::Error::other)?;
        io::copy(&mut File::open(file)?, &mut zip)?;
    }
    zip.finish().map_err(io::Error::other)?.flush()
}

/// When the picture was saved, in local time, which is what ZIP files store and what File
/// Explorer shows.
fn local_modified_time(file: &Path) -> Option<DateTime> {
    let modified: chrono::DateTime<Local> = std::fs::metadata(file).ok()?.modified().ok()?.into();
    DateTime::from_date_and_time(
        u16::try_from(modified.year()).ok()?,
        modified.month() as u8,
        modified.day() as u8,
        modified.hour() as u8,
        modified.minute() as u8,
        modified.second() as u8,
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_every_file_and_removes_the_folder() {
        let base = crate::test_dir("pack");
        let dir = base.join("2026-10-14 19-30-05 - Foundry VTT");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("000002 - Foundry VTT.png"), b"second").unwrap();
        std::fs::write(dir.join("000001 - Foundry VTT.png"), b"first").unwrap();

        let zip_path = zip_folder(&dir, None).unwrap().remove(0);
        assert_eq!(zip_path, base.join("2026-10-14 19-30-05 - Foundry VTT.zip"));
        assert!(!dir.exists());

        let mut archive = zip::ZipArchive::new(File::open(&zip_path).unwrap()).unwrap();
        let names: Vec<_> = archive.file_names().map(String::from).collect();
        assert_eq!(names.len(), 2);
        let mut first = String::new();
        io::Read::read_to_string(
            &mut archive.by_name("000001 - Foundry VTT.png").unwrap(),
            &mut first,
        )
        .unwrap();
        assert_eq!(first, "first");
    }

    #[test]
    fn stores_local_time_for_each_picture() {
        let base = crate::test_dir("pack-time");
        let dir = base.join("run");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("000001 - x.png"), b"x").unwrap();
        let before = Local::now();
        let mut archive =
            zip::ZipArchive::new(File::open(zip_folder(&dir, None).unwrap().remove(0)).unwrap())
                .unwrap();
        let stored = archive.by_index(0).unwrap().last_modified().unwrap();
        assert_eq!(
            (stored.year(), stored.month(), stored.day(), stored.hour()),
            (
                before.year() as u16,
                before.month() as u8,
                before.day() as u8,
                before.hour() as u8
            )
        );
    }

    #[test]
    fn unicode_names_survive() {
        let base = crate::test_dir("pack-unicode");
        let dir = base.join("run");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("000001 - Taverne • Drachenhöhle.png"), b"x").unwrap();
        let zip_path = zip_folder(&dir, None).unwrap().remove(0);
        let archive = zip::ZipArchive::new(File::open(zip_path).unwrap()).unwrap();
        assert_eq!(
            archive.file_names().collect::<Vec<_>>(),
            vec!["000001 - Taverne • Drachenhöhle.png"]
        );
    }

    #[test]
    fn a_missing_folder_is_an_error_not_a_crash() {
        assert!(zip_folder(Path::new("/definitely/not/here"), None).is_err());
    }

    /// Makes a recording folder with `count` pictures of `size` bytes each.
    fn recording(test: &str, count: usize, size: usize) -> PathBuf {
        let dir = crate::test_dir(test).join("2026-10-14 19-30-05 - VTT");
        std::fs::create_dir(&dir).unwrap();
        for n in 1..=count {
            std::fs::write(dir.join(format!("{n:06} - VTT.png")), vec![n as u8; size]).unwrap();
        }
        dir
    }

    fn entries(zip_path: &Path) -> Vec<String> {
        let archive = zip::ZipArchive::new(File::open(zip_path).unwrap()).unwrap();
        archive.file_names().map(String::from).collect()
    }

    #[test]
    fn splits_into_numbered_parts_that_keep_pictures_in_order() {
        let dir = recording("pack-split", 10, 1000);
        let parts = zip_folder(&dir, Some(3500)).unwrap();
        let names: Vec<String> = parts
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(parts.len() >= 3, "{names:?}");
        let expected_names: Vec<String> = (1..=parts.len())
            .map(|n| format!("2026-10-14 19-30-05 - VTT - part {n}.zip"))
            .collect();
        assert_eq!(names, expected_names);
        let all: Vec<String> = parts.iter().flat_map(|p| entries(p)).collect();
        let expected: Vec<String> = (1..=10).map(|n| format!("{n:06} - VTT.png")).collect();
        assert_eq!(all, expected);
        for part in &parts {
            assert!(
                std::fs::metadata(part).unwrap().len() <= 3500,
                "{part:?} is too big"
            );
        }
        assert!(!dir.exists());
    }

    #[test]
    fn a_recording_that_fits_in_one_part_gets_a_plain_name() {
        let dir = recording("pack-fits", 3, 100);
        let parts = zip_folder(&dir, ZipSplit::Mb100.max_bytes()).unwrap();
        assert_eq!(parts.len(), 1);
        assert!(parts[0].ends_with("2026-10-14 19-30-05 - VTT.zip"));
    }

    #[test]
    fn a_picture_bigger_than_the_limit_gets_its_own_part() {
        let dir = recording("pack-oversize", 3, 5000);
        let parts = zip_folder(&dir, Some(1000)).unwrap();
        assert_eq!(parts.len(), 3);
        for part in &parts {
            assert_eq!(entries(part).len(), 1);
        }
    }

    #[test]
    fn many_parts_are_padded_so_they_sort_correctly() {
        let names = part_names("rec", 12);
        assert_eq!(names[0], "rec - part 01.zip");
        assert_eq!(names[11], "rec - part 12.zip");
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(sorted, names);
    }

    #[test]
    fn fat32_parts_stay_under_its_4_gb_limit() {
        assert!(ZipSplit::Gb4.max_bytes().unwrap() < u64::from(u32::MAX));
    }

    mod properties {
        use super::super::*;
        use proptest::prelude::*;

        proptest! {
            #![proptest_config(ProptestConfig::with_cases(2000))]

            /// For any sizes and limit: every file lands in exactly one part, in order, and each
            /// part stays within the limit unless it holds a single oversized file.
            #[test]
            fn planning_keeps_every_file_once_and_respects_the_limit(
                sizes in proptest::collection::vec(0u64..10_000, 0..60),
                max in 200u64..20_000,
            ) {
                let files: Vec<(PathBuf, u64)> = sizes
                    .iter()
                    .enumerate()
                    .map(|(i, s)| (PathBuf::from(format!("{:06} - x.png", i + 1)), *s))
                    .collect();
                let parts = plan_parts(&files, Some(max));
                let flattened: Vec<PathBuf> = parts.iter().flatten().cloned().collect();
                let originals: Vec<PathBuf> = files.iter().map(|(p, _)| p.clone()).collect();
                prop_assert_eq!(flattened, originals);
                for part in &parts {
                    let total: u64 = ZIP_OVERHEAD + part
                        .iter()
                        .map(|p| zip_cost(p, files.iter().find(|(f, _)| f == p).unwrap().1))
                        .sum::<u64>();
                    prop_assert!(total <= max || part.len() == 1, "part of {} files is {total} > {max}", part.len());
                }
            }
        }
    }
}
