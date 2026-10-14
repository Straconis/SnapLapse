//! Packing a finished recording's folder into a single ZIP file.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::{Path, PathBuf};

use chrono::{Datelike, Local, Timelike};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

/// Packs every file in `dir` into `<folder name>.zip` beside it, then deletes the folder.
///
/// Images are stored as they are, without compressing them again, because PNG, JPG and WebP are
/// already compressed. That keeps packing fast. If anything fails, the folder is left untouched.
pub fn zip_folder(dir: &Path) -> io::Result<PathBuf> {
    let name = dir
        .file_name()
        .ok_or_else(|| io::Error::other("the folder has no name"))?
        .to_string_lossy()
        .into_owned();
    let zip_path = dir.with_file_name(format!("{name}.zip"));
    let partial = dir.with_file_name(format!("{name}.zip.part"));

    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.is_file())
        .collect();
    files.sort();

    if let Err(e) = write_zip(&files, &partial) {
        let _ = std::fs::remove_file(&partial);
        return Err(e);
    }
    std::fs::rename(&partial, &zip_path)?;
    std::fs::remove_dir_all(dir)?;
    Ok(zip_path)
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

        let zip_path = zip_folder(&dir).unwrap();
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
            zip::ZipArchive::new(File::open(zip_folder(&dir).unwrap()).unwrap()).unwrap();
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
        let zip_path = zip_folder(&dir).unwrap();
        let archive = zip::ZipArchive::new(File::open(zip_path).unwrap()).unwrap();
        assert_eq!(
            archive.file_names().collect::<Vec<_>>(),
            vec!["000001 - Taverne • Drachenhöhle.png"]
        );
    }

    #[test]
    fn a_missing_folder_is_an_error_not_a_crash() {
        assert!(zip_folder(Path::new("/definitely/not/here")).is_err());
    }
}
