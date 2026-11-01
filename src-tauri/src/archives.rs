use std::{
    collections::HashSet,
    fs,
    io::{Cursor, Read, Write},
    path::Path,
};

use crate::files::FileError;
use zip::{write::FileOptions, CompressionMethod, ZipArchive, ZipWriter};

pub const MAX_BYTES: u64 = 75 * 1024 * 1024;
const MAX_ENTRIES: usize = 10_000;

pub struct Entry {
    pub name: String,
    pub bytes: Vec<u8>,
}

fn validate_name(name: &str) -> Result<(), FileError> {
    if name.is_empty()
        || name.starts_with('/')
        || name.contains(['\\', ':', '\0'])
        || name.split('/').any(|part| part == ".." || part == ".")
    {
        return Err(FileError::Message(format!("Unsafe archive path: {name}")));
    }
    Ok(())
}

pub fn read_zip(bytes: &[u8]) -> Result<Vec<Entry>, FileError> {
    let mut archive = ZipArchive::new(Cursor::new(bytes))?;
    if archive.len() > MAX_ENTRIES {
        return Err(FileError::Message("Too many archive entries".into()));
    }
    let mut remaining = MAX_BYTES;
    let mut names = HashSet::new();
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index)?;
        let name = file.name().to_string();
        validate_name(&name)?;
        if !names.insert(name.clone()) {
            return Err(FileError::Message("Duplicate archive path".into()));
        }
        if file
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(FileError::Message(
                "Archive symlinks are unsupported".into(),
            ));
        }
        if file.size() > remaining {
            return Err(FileError::Message("Expanded archive exceeds 75 MB".into()));
        }
        let mut content = Vec::new();
        Read::by_ref(&mut file)
            .take(remaining + 1)
            .read_to_end(&mut content)?;
        if content.len() as u64 > remaining {
            return Err(FileError::Message("Expanded archive exceeds 75 MB".into()));
        }
        remaining -= content.len() as u64;
        entries.push(Entry {
            name,
            bytes: content,
        });
    }
    Ok(entries)
}

pub fn write_zip(entries: &[Entry]) -> Result<Vec<u8>, FileError> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = FileOptions::default().compression_method(CompressionMethod::Deflated);
    let mut names = HashSet::new();
    if entries.len() > MAX_ENTRIES {
        return Err(FileError::Message("Too many archive entries".into()));
    }
    for entry in entries {
        validate_name(&entry.name)?;
        if !names.insert(&entry.name) {
            return Err(FileError::Message("Duplicate archive path".into()));
        }
        if entry.name.ends_with('/') {
            writer.add_directory(&entry.name, options)?;
        } else {
            writer.start_file(&entry.name, options)?;
            writer.write_all(&entry.bytes)?;
        }
    }
    Ok(writer.finish()?.into_inner())
}

fn archive_error(error: impl std::fmt::Display) -> FileError {
    FileError::Message(format!("Archive error: {error}"))
}

pub fn is_archive(name: &str) -> bool {
    archive_suffix(name).is_some()
}

fn archive_suffix(name: &str) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    [
        ".tar.gz",
        ".tar.bz2",
        ".tar.xz",
        ".tar.zst",
        ".tar.lzma",
        ".tgz",
        ".tbz2",
        ".tbz",
        ".txz",
        ".tzst",
        ".zip",
        ".rar",
        ".7z",
        ".tar",
    ]
    .into_iter()
    .find(|suffix| name.ends_with(suffix))
}

pub fn result_filename(name: &str) -> String {
    let stem = archive_suffix(name)
        .map(|suffix| &name[..name.len() - suffix.len()])
        .unwrap_or(name);
    format!("{stem}-translated.7z")
}

fn add_entry(
    entries: &mut Vec<Entry>,
    names: &mut HashSet<String>,
    remaining: &mut u64,
    mut name: String,
    directory: bool,
    reader: &mut dyn Read,
) -> Result<(), FileError> {
    while name.starts_with("./") {
        name = name[2..].to_string();
    }
    if directory && (name.is_empty() || name == ".") {
        return Ok(());
    }
    if directory && !name.ends_with('/') {
        name.push('/');
    }
    validate_name(&name)?;
    if entries.len() >= MAX_ENTRIES || !names.insert(name.clone()) {
        return Err(FileError::Message(
            "Too many entries or duplicate archive path".into(),
        ));
    }
    let mut bytes = Vec::new();
    reader.take(*remaining + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > *remaining {
        return Err(FileError::Message("Expanded archive exceeds 75 MB".into()));
    }
    *remaining -= bytes.len() as u64;
    entries.push(Entry { name, bytes });
    Ok(())
}

pub fn read_archive(filename: &str, bytes: &[u8]) -> Result<Vec<Entry>, FileError> {
    let filename = filename.to_ascii_lowercase();
    if filename.ends_with(".zip") {
        return read_zip(bytes);
    }
    let mut entries = Vec::new();
    let mut names = HashSet::new();
    let mut remaining = MAX_BYTES;
    if filename.ends_with(".7z") {
        let mut archive =
            sevenz_rust2::ArchiveReader::new(Cursor::new(bytes), sevenz_rust2::Password::empty())
                .map_err(archive_error)?;
        if archive.archive().files.len() > MAX_ENTRIES {
            return Err(archive_error("Too many entries"));
        }
        let mut failure = None;
        archive
            .for_each_entries(|entry, reader| {
                if entry.is_anti_item || (entry.windows_attributes >> 16) & 0o170000 == 0o120000 {
                    failure = Some(archive_error(
                        "Archive links and anti-items are unsupported",
                    ));
                    return Ok(false);
                }
                if entry.size > remaining {
                    failure = Some(archive_error("Expanded archive exceeds 75 MB"));
                    return Ok(false);
                }
                match add_entry(
                    &mut entries,
                    &mut names,
                    &mut remaining,
                    entry.name.clone(),
                    entry.is_directory,
                    reader,
                ) {
                    Ok(()) => Ok(true),
                    Err(error) => {
                        failure = Some(error);
                        Ok(false)
                    }
                }
            })
            .map_err(archive_error)?;
        if let Some(error) = failure {
            return Err(error);
        }
    } else if filename.ends_with(".rar") {
        let mut temporary = tempfile::NamedTempFile::new()?;
        temporary.write_all(bytes)?;
        let mut archive = unrar::Archive::new(temporary.path())
            .open_for_processing()
            .map_err(archive_error)?;
        while let Some(header) = archive.read_header().map_err(archive_error)? {
            let entry = header.entry();
            if entry.is_encrypted() || entry.is_split() {
                return Err(archive_error(
                    "Encrypted or multipart RAR archives are unsupported",
                ));
            }
            if entry.unpacked_size > remaining {
                return Err(archive_error("Expanded archive exceeds 75 MB"));
            }
            let name = entry
                .filename
                .to_str()
                .ok_or_else(|| archive_error("Archive path is not UTF-8"))?
                .replace('\\', "/");
            let directory = entry.is_directory();
            validate_name(&name)?;
            let (data, next) = header.read().map_err(archive_error)?;
            add_entry(
                &mut entries,
                &mut names,
                &mut remaining,
                name,
                directory,
                &mut Cursor::new(data),
            )?;
            archive = next;
        }
    } else {
        let reader: Box<dyn Read + '_> = match archive_suffix(&filename) {
            Some(".tar.gz" | ".tgz") => Box::new(flate2::read::MultiGzDecoder::new(bytes)),
            Some(".tar.bz2" | ".tbz" | ".tbz2") => {
                Box::new(bzip2::read::MultiBzDecoder::new(bytes))
            }
            Some(".tar.xz" | ".txz") => {
                Box::new(lzma_rust2::XzReader::new_mem_limit(bytes, true, 256 * 1024))
            }
            Some(".tar.zst" | ".tzst") => Box::new(zstd::stream::read::Decoder::new(bytes)?),
            Some(".tar.lzma") => Box::new(lzma_rust2::LzmaReader::new_mem_limit(
                bytes,
                256 * 1024,
                None,
            )?),
            Some(".tar") => Box::new(bytes),
            _ => return Err(archive_error("Unsupported archive format")),
        };
        // Bound the complete tar stream as well as file contents (including padding and headers).
        let mut archive = tar::Archive::new(reader.take(MAX_BYTES + MAX_ENTRIES as u64 * 2048));
        for file in archive.entries()? {
            let mut file = file?;
            let kind = file.header().entry_type();
            if !kind.is_file() && !kind.is_dir() {
                return Err(archive_error(
                    "Archive links and special files are unsupported",
                ));
            }
            let name = file
                .path()?
                .to_str()
                .ok_or_else(|| archive_error("Archive path is not UTF-8"))?
                .to_string();
            if file.size() > remaining {
                return Err(archive_error("Expanded archive exceeds 75 MB"));
            }
            add_entry(
                &mut entries,
                &mut names,
                &mut remaining,
                name,
                kind.is_dir(),
                &mut file,
            )?;
        }
    }
    Ok(entries)
}

pub fn write_7z(entries: &[Entry]) -> Result<Vec<u8>, FileError> {
    let mut writer =
        sevenz_rust2::ArchiveWriter::new(Cursor::new(Vec::new())).map_err(archive_error)?;
    let mut names = HashSet::new();
    for entry in entries {
        validate_name(&entry.name)?;
        if !names.insert(&entry.name) {
            return Err(archive_error("Duplicate archive path"));
        }
        let directory = entry.name.ends_with('/');
        let item = if directory {
            sevenz_rust2::ArchiveEntry::new_directory(&entry.name)
        } else {
            sevenz_rust2::ArchiveEntry::new_file(&entry.name)
        };
        writer
            .push_archive_entry(
                item,
                if directory {
                    None
                } else {
                    Some(Cursor::new(&entry.bytes))
                },
            )
            .map_err(archive_error)?;
    }
    Ok(writer.finish().map_err(archive_error)?.into_inner())
}

pub fn pack_directory(root: &Path) -> Result<Vec<u8>, FileError> {
    pack_directory_filtered(root, false)
}

pub fn pack_output_directory(root: &Path) -> Result<Vec<u8>, FileError> {
    pack_directory_filtered(root, true)
}

fn pack_directory_filtered(root: &Path, translated_only: bool) -> Result<Vec<u8>, FileError> {
    fn visit(
        root: &Path,
        directory: &Path,
        entries: &mut Vec<Entry>,
        remaining: &mut u64,
        translated_only: bool,
    ) -> Result<(), FileError> {
        let mut children = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(|child| child.file_name());
        for child in children {
            let kind = child.file_type()?;
            if kind.is_symlink() {
                continue;
            }
            if entries.len() >= MAX_ENTRIES {
                return Err(FileError::Message("Too many folder entries".into()));
            }
            let path = child.path();
            let name = path
                .strip_prefix(root)
                .map_err(|error| FileError::Message(error.to_string()))?
                .to_str()
                .ok_or_else(|| FileError::Message("Folder path is not UTF-8".into()))?
                .replace('\\', "/");
            if kind.is_dir() {
                entries.push(Entry {
                    name: format!("{name}/"),
                    bytes: Vec::new(),
                });
                visit(root, &path, entries, remaining, translated_only)?;
            } else if kind.is_file()
                && (!translated_only || crate::files::supported_document(&name))
            {
                let mut bytes = Vec::new();
                fs::File::open(&path)?
                    .take(*remaining + 1)
                    .read_to_end(&mut bytes)?;
                if bytes.len() as u64 > *remaining {
                    return Err(FileError::Message("Folder exceeds 75 MB".into()));
                }
                *remaining -= bytes.len() as u64;
                if translated_only && bytes.is_empty() {
                    continue;
                }
                entries.push(Entry { name, bytes });
            }
        }
        Ok(())
    }
    let mut entries = Vec::new();
    let mut remaining = MAX_BYTES;
    visit(root, root, &mut entries, &mut remaining, translated_only)?;
    write_zip(&entries)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seven_zip_roundtrip_and_archive_names() {
        let entries = vec![
            Entry {
                name: "nested/notes.txt".into(),
                bytes: b"hello".to_vec(),
            },
            Entry {
                name: "empty/".into(),
                bytes: vec![],
            },
        ];
        let bytes = write_7z(&entries).unwrap();
        assert!(bytes.starts_with(b"7z\xbc\xaf\x27\x1c"));
        let result = read_archive("notes.7z", &bytes).unwrap();
        assert_eq!(
            result
                .iter()
                .find(|entry| entry.name == "nested/notes.txt")
                .unwrap()
                .bytes,
            b"hello"
        );
        assert!(result.iter().any(|entry| entry.name == "empty/"));
        for name in [
            "notes.zip",
            "notes.rar",
            "notes.7z",
            "notes.tar",
            "notes.TAR.GZ",
            "notes.tar.bz2",
            "notes.tar.xz",
            "notes.tgz",
            "notes.tar.zst",
            "notes.tar.lzma",
        ] {
            assert!(is_archive(name));
            assert_eq!(result_filename(name), "notes-translated.7z");
        }
        assert!(!is_archive("notes.docx"));
    }

    #[test]
    fn reads_rar_without_an_external_program() {
        // Minimal stored RAR5 created for this repository from the public format specification.
        let result =
            read_archive("notes.rar", include_bytes!("../tests/fixtures/notes.rar")).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "nested/notes.txt");
        assert_eq!(result[0].bytes, b"Hello from RAR");
    }

    #[test]
    fn reads_tar_and_compressed_tar_variants() {
        let mut tar = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_size(5);
        header.set_mode(0o644);
        header.set_cksum();
        tar.append_data(&mut header, "nested/notes.txt", b"hello".as_slice())
            .unwrap();
        let bytes = tar.into_inner().unwrap();
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gzip.write_all(&bytes).unwrap();
        let mut bzip = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::default());
        bzip.write_all(&bytes).unwrap();
        let mut xz =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::default()).unwrap();
        xz.write_all(&bytes).unwrap();
        let zstd = zstd::stream::encode_all(bytes.as_slice(), 1).unwrap();
        let mut lzma = lzma_rust2::LzmaWriter::new_use_header(
            Vec::new(),
            &lzma_rust2::LzmaOptions::default(),
            Some(bytes.len() as u64),
        )
        .unwrap();
        lzma.write_all(&bytes).unwrap();
        let cases = [
            ("notes.tar", bytes),
            ("notes.tar.gz", gzip.finish().unwrap()),
            ("notes.tbz2", bzip.finish().unwrap()),
            ("notes.txz", xz.finish().unwrap()),
            ("notes.tar.zst", zstd),
            ("notes.tar.lzma", lzma.finish().unwrap()),
        ];
        for (name, bytes) in cases {
            let result = read_archive(name, &bytes).unwrap();
            assert_eq!(result.len(), 1, "{name}");
            assert_eq!(result[0].name, "nested/notes.txt");
            assert_eq!(result[0].bytes, b"hello");
        }
    }

    #[test]
    fn folder_walk_preserves_empty_directories_and_relative_paths() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("empty")).unwrap();
        fs::create_dir(root.path().join("nested")).unwrap();
        fs::write(root.path().join("nested/notes.txt"), b"hello").unwrap();
        let result = read_zip(&pack_directory(root.path()).unwrap()).unwrap();
        assert!(result.iter().any(|entry| entry.name == "empty/"));
        assert_eq!(
            result
                .iter()
                .find(|entry| entry.name == "nested/notes.txt")
                .unwrap()
                .bytes,
            b"hello"
        );
    }

    #[test]
    fn zip_roundtrip_preserves_tree_and_binary_files() {
        let entries = vec![
            Entry {
                name: "nested/".into(),
                bytes: vec![],
            },
            Entry {
                name: "nested/a.bin".into(),
                bytes: vec![0, 255, 1],
            },
        ];
        let result = read_zip(&write_zip(&entries).unwrap()).unwrap();
        assert_eq!(result[0].name, "nested/");
        assert_eq!(result[1].bytes, [0, 255, 1]);
    }

    #[test]
    fn zip_rejects_unsafe_and_duplicate_paths() {
        for name in ["../bad.txt", "/absolute.txt", "C:/bad.txt", "a\\b"] {
            assert!(write_zip(&[Entry {
                name: name.into(),
                bytes: vec![]
            }])
            .is_err());
        }
        assert!(write_zip(&[
            Entry {
                name: "a".into(),
                bytes: vec![]
            },
            Entry {
                name: "a".into(),
                bytes: vec![]
            }
        ])
        .is_err());
        let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
        writer
            .start_file("../bad.txt", FileOptions::default())
            .unwrap();
        assert!(read_zip(&writer.finish().unwrap().into_inner()).is_err());
    }
}
