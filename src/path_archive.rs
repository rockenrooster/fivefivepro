use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

use crate::crc32::crc32;
use crate::error::{Pro55Error, Result};
use crate::json::{escape_json_string, parse_json, JsonValue};

pub const PATH_ARCHIVE_MAGIC: &[u8; 11] = b"55PROPATH\x1a\n";
pub const PATH_ARCHIVE_VERSION: u8 = 1;
const PATH_HEADER_SIZE: usize = 25;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathArchiveInfo {
    pub root_name: String,
    pub files: usize,
    pub directories: usize,
    pub total_size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Dir {
        path: String,
        mode: u32,
        mtime_ns: i64,
    },
    File {
        path: String,
        mode: u32,
        mtime_ns: i64,
        offset: usize,
        size: usize,
        crc32: u32,
    },
}

fn err<T>(message: impl Into<String>) -> Result<T> {
    Err(Pro55Error::new(message))
}

fn put_u32_le(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u64_le(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}

fn read_u32_at(data: &[u8], pos: &mut usize, what: &str) -> Result<u32> {
    if *pos + 4 > data.len() {
        return err(format!(
            "truncated 5.5pro path archive while reading {what}"
        ));
    }
    let value = u32::from_le_bytes([data[*pos], data[*pos + 1], data[*pos + 2], data[*pos + 3]]);
    *pos += 4;
    Ok(value)
}

fn read_u64_at(data: &[u8], pos: &mut usize, what: &str) -> Result<u64> {
    if *pos + 8 > data.len() {
        return err(format!(
            "truncated 5.5pro path archive while reading {what}"
        ));
    }
    let value = u64::from_le_bytes([
        data[*pos],
        data[*pos + 1],
        data[*pos + 2],
        data[*pos + 3],
        data[*pos + 4],
        data[*pos + 5],
        data[*pos + 6],
        data[*pos + 7],
    ]);
    *pos += 8;
    Ok(value)
}

pub fn is_path_archive(data: &[u8]) -> bool {
    data.starts_with(PATH_ARCHIVE_MAGIC)
}

fn path_to_utf8_name(path: &Path) -> Result<String> {
    path.file_name()
        .and_then(|name| name.to_str())
        .map(|s| s.to_string())
        .ok_or_else(|| Pro55Error::new(format!("path is not valid UTF-8: {}", path.display())))
}

fn mode_from_metadata(metadata: &fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        metadata.permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        if metadata.permissions().readonly() {
            0o444
        } else {
            0o644
        }
    }
}

fn mtime_ns_from_metadata(metadata: &fs::Metadata) -> i64 {
    #[cfg(unix)]
    {
        metadata.mtime().saturating_mul(1_000_000_000) + metadata.mtime_nsec()
    }
    #[cfg(not(unix))]
    {
        metadata
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos().min(i64::MAX as u128) as i64)
            .unwrap_or(0)
    }
}

fn validate_storable_path(rel: &str) -> Result<()> {
    if rel.is_empty() || rel == "." {
        return err("internal path archive contains an empty path");
    }
    if rel.contains('\0') || rel.contains('\\') {
        return err(format!("unsafe path in internal archive: {rel:?}"));
    }
    if rel.starts_with('/') {
        return err(format!("unsafe path in internal archive: {rel:?}"));
    }
    for part in rel.split('/') {
        if part.is_empty() || part == "." || part == ".." {
            return err(format!("unsafe path in internal archive: {rel:?}"));
        }
    }
    Ok(())
}

fn join_rel(root: &Path, rel: &str) -> Result<PathBuf> {
    validate_storable_path(rel)?;
    let mut dest = root.to_path_buf();
    for part in rel.split('/') {
        dest.push(part);
    }
    Ok(dest)
}

fn reject_symlink_parents(root: &Path, dest: &Path) -> Result<()> {
    let rel = dest
        .strip_prefix(root)
        .map_err(|_| Pro55Error::new("path escapes output directory"))?;
    let mut cur = root.to_path_buf();
    let parts: Vec<_> = rel.components().collect();
    for component in parts.iter().take(parts.len().saturating_sub(1)) {
        if let Component::Normal(part) = component {
            cur.push(part);
            if fs::symlink_metadata(&cur)
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
            {
                return err(format!(
                    "refusing to write through symlinked directory: {}",
                    cur.display()
                ));
            }
        }
    }
    Ok(())
}

#[cfg(unix)]
fn apply_metadata(path: &Path, mode: u32, _mtime_ns: i64) {
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode & 0o777));
}

#[cfg(not(unix))]
fn apply_metadata(_path: &Path, _mode: u32, _mtime_ns: i64) {}

pub fn pack_directory(root: impl AsRef<Path>) -> Result<Vec<u8>> {
    let root_path = root.as_ref();
    if !root_path.is_dir() {
        return err(format!("not a directory: {}", root_path.display()));
    }
    let root_path = root_path.canonicalize()?;
    let mut entries = Vec::<Entry>::new();
    let mut data_blob = Vec::<u8>::new();
    walk_pack(&root_path, &root_path, "", &mut entries, &mut data_blob)?;

    let root_name = path_to_utf8_name(&root_path)?;
    let manifest = manifest_json(&root_name, &entries);
    let manifest_bytes = manifest.into_bytes();
    if manifest_bytes.len() > u64::MAX as usize || data_blob.len() > u64::MAX as usize {
        return err("path archive payload too large");
    }

    let mut out = Vec::new();
    out.extend_from_slice(PATH_ARCHIVE_MAGIC);
    out.push(PATH_ARCHIVE_VERSION);
    put_u64_le(&mut out, manifest_bytes.len() as u64);
    put_u64_le(&mut out, data_blob.len() as u64);
    put_u32_le(&mut out, crc32(&manifest_bytes));
    put_u32_le(&mut out, crc32(&data_blob));
    out.extend_from_slice(&manifest_bytes);
    out.extend_from_slice(&data_blob);
    Ok(out)
}

fn walk_pack(
    root: &Path,
    current: &Path,
    rel: &str,
    entries: &mut Vec<Entry>,
    data_blob: &mut Vec<u8>,
) -> Result<()> {
    let metadata = fs::symlink_metadata(current)?;
    if metadata.file_type().is_symlink() {
        return err(format!(
            "refusing to archive symlink: {}",
            current.display()
        ));
    }
    if !metadata.is_dir() {
        return err(format!(
            "refusing to archive non-directory entry: {}",
            current.display()
        ));
    }

    if current != root {
        validate_storable_path(rel)?;
        entries.push(Entry::Dir {
            path: rel.to_string(),
            mode: mode_from_metadata(&metadata),
            mtime_ns: mtime_ns_from_metadata(&metadata),
        });
    }

    let mut dirs = Vec::<(String, PathBuf)>::new();
    let mut files = Vec::<(String, PathBuf)>::new();
    for item in fs::read_dir(current)? {
        let item = item?;
        let path = item.path();
        let name = item
            .file_name()
            .into_string()
            .map_err(|_| Pro55Error::new(format!("path is not valid UTF-8: {}", path.display())))?;
        let child_meta = fs::symlink_metadata(&path)?;
        if child_meta.file_type().is_symlink() {
            return err(format!("refusing to archive symlink: {}", path.display()));
        }
        if child_meta.is_dir() {
            dirs.push((name, path));
        } else if child_meta.is_file() {
            files.push((name, path));
        } else {
            return err(format!(
                "refusing to archive special file: {}",
                path.display()
            ));
        }
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    files.sort_by(|a, b| a.0.cmp(&b.0));

    for (name, path) in files {
        let rel_child = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        validate_storable_path(&rel_child)?;
        let metadata = fs::symlink_metadata(&path)?;
        let offset = data_blob.len();
        let mut file = fs::File::open(&path)?;
        file.read_to_end(data_blob)?;
        let size = data_blob.len() - offset;
        entries.push(Entry::File {
            path: rel_child,
            mode: mode_from_metadata(&metadata),
            mtime_ns: mtime_ns_from_metadata(&metadata),
            offset,
            size,
            crc32: crc32(&data_blob[offset..offset + size]),
        });
    }

    for (name, path) in dirs {
        let rel_child = if rel.is_empty() {
            name
        } else {
            format!("{rel}/{name}")
        };
        walk_pack(root, &path, &rel_child, entries, data_blob)?;
    }
    Ok(())
}

fn manifest_json(root_name: &str, entries: &[Entry]) -> String {
    let mut out = String::new();
    out.push_str("{\"entries\":[");
    for (i, entry) in entries.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        match entry {
            Entry::Dir {
                path,
                mode,
                mtime_ns,
            } => {
                out.push_str(&format!(
                    "{{\"mode\":{},\"mtime_ns\":{},\"path\":\"{}\",\"type\":\"dir\"}}",
                    mode,
                    mtime_ns,
                    escape_json_string(path)
                ));
            }
            Entry::File {
                path,
                mode,
                mtime_ns,
                offset,
                size,
                crc32,
            } => {
                out.push_str(&format!(
                    "{{\"crc32\":{},\"mode\":{},\"mtime_ns\":{},\"offset\":{},\"path\":\"{}\",\"size\":{},\"type\":\"file\"}}",
                    crc32, mode, mtime_ns, offset, escape_json_string(path), size
                ));
            }
        }
    }
    out.push_str("],\"format\":\"5.5pro-path-archive\",\"root_name\":\"");
    out.push_str(&escape_json_string(root_name));
    out.push_str("\",\"version\":1}");
    out
}

fn read_payload(data: &[u8]) -> Result<(String, Vec<Entry>, Vec<u8>)> {
    if !data.starts_with(PATH_ARCHIVE_MAGIC) {
        return err("not a 5.5pro path archive payload");
    }
    let mut pos = PATH_ARCHIVE_MAGIC.len();
    if data.len() < pos + PATH_HEADER_SIZE {
        return err("truncated 5.5pro path archive header");
    }
    let version = data[pos];
    pos += 1;
    if version != PATH_ARCHIVE_VERSION {
        return err(format!("unsupported 5.5pro path archive version {version}"));
    }
    let manifest_len = read_u64_at(data, &mut pos, "manifest length")?;
    let body_len = read_u64_at(data, &mut pos, "data length")?;
    let manifest_crc = read_u32_at(data, &mut pos, "manifest crc32")?;
    let body_crc = read_u32_at(data, &mut pos, "data crc32")?;

    let manifest_len = usize::try_from(manifest_len)
        .map_err(|_| Pro55Error::new("manifest too large for this platform"))?;
    let body_len = usize::try_from(body_len)
        .map_err(|_| Pro55Error::new("body too large for this platform"))?;
    let end_manifest = pos
        .checked_add(manifest_len)
        .ok_or_else(|| Pro55Error::new("path archive length overflow"))?;
    let end_body = end_manifest
        .checked_add(body_len)
        .ok_or_else(|| Pro55Error::new("path archive length overflow"))?;
    if end_manifest > data.len() || end_body > data.len() {
        return err("truncated 5.5pro path archive payload");
    }
    if end_body != data.len() {
        return err("trailing data inside 5.5pro path archive payload");
    }

    let manifest_bytes = &data[pos..end_manifest];
    let body = data[end_manifest..end_body].to_vec();
    if crc32(manifest_bytes) != manifest_crc {
        return err("5.5pro path archive manifest CRC check failed");
    }
    if crc32(&body) != body_crc {
        return err("5.5pro path archive data CRC check failed");
    }

    let doc = parse_json(manifest_bytes)?;
    let object = doc
        .as_object()
        .ok_or_else(|| Pro55Error::new("invalid 5.5pro path archive manifest"))?;
    if object.get("format").and_then(JsonValue::as_str) != Some("5.5pro-path-archive") {
        return err("unsupported 5.5pro path archive manifest");
    }
    if object.get("version").and_then(JsonValue::as_i64) != Some(i64::from(PATH_ARCHIVE_VERSION)) {
        return err("unsupported 5.5pro path archive manifest");
    }
    let root_name = object
        .get("root_name")
        .and_then(JsonValue::as_str)
        .unwrap_or("")
        .to_string();
    let entries_json = object
        .get("entries")
        .and_then(JsonValue::as_array)
        .ok_or_else(|| Pro55Error::new("5.5pro path archive manifest has no entry list"))?;
    let mut entries = Vec::new();
    for value in entries_json {
        entries.push(entry_from_json(value, &body)?);
    }
    Ok((root_name, entries, body))
}

fn entry_from_json(value: &JsonValue, body: &[u8]) -> Result<Entry> {
    let obj = value
        .as_object()
        .ok_or_else(|| Pro55Error::new("invalid 5.5pro path archive entry"))?;
    let typ = obj
        .get("type")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| Pro55Error::new("invalid entry type in 5.5pro path archive"))?;
    let path = obj
        .get("path")
        .and_then(JsonValue::as_str)
        .ok_or_else(|| Pro55Error::new("invalid entry path in 5.5pro path archive"))?
        .to_string();
    validate_storable_path(&path)?;
    let mode_raw = int_field(obj.get("mode"), "mode")?;
    if !(0..=u32::MAX as i64).contains(&mode_raw) {
        return err("invalid mode in path archive manifest");
    }
    let mode = mode_raw as u32;
    let mtime_ns = int_field(obj.get("mtime_ns"), "mtime_ns")?;
    match typ {
        "dir" => Ok(Entry::Dir {
            path,
            mode,
            mtime_ns,
        }),
        "file" => {
            let offset = usize_field(obj.get("offset"), "offset")?;
            let size = usize_field(obj.get("size"), "size")?;
            let crc_raw = int_field(obj.get("crc32"), "crc32")?;
            if !(0..=u32::MAX as i64).contains(&crc_raw) {
                return err("invalid crc32 in path archive manifest");
            }
            let crc = crc_raw as u32;
            let end = offset
                .checked_add(size)
                .ok_or_else(|| Pro55Error::new("file entry range overflow"))?;
            if end > body.len() {
                return err("file entry exceeds 5.5pro path archive data section");
            }
            let chunk = &body[offset..end];
            if crc32(chunk) != crc {
                return err(format!("file CRC check failed inside path archive: {path}"));
            }
            Ok(Entry::File {
                path,
                mode,
                mtime_ns,
                offset,
                size,
                crc32: crc,
            })
        }
        _ => err("unknown entry type in 5.5pro path archive"),
    }
}

fn int_field(value: Option<&JsonValue>, name: &str) -> Result<i64> {
    value.and_then(JsonValue::as_i64).ok_or_else(|| {
        Pro55Error::new(format!(
            "invalid integer field in path archive manifest: {name}"
        ))
    })
}

fn usize_field(value: Option<&JsonValue>, name: &str) -> Result<usize> {
    let raw = int_field(value, name)?;
    if raw < 0 {
        return err(format!(
            "negative integer field in path archive manifest: {name}"
        ));
    }
    usize::try_from(raw).map_err(|_| {
        Pro55Error::new(format!(
            "integer field too large in path archive manifest: {name}"
        ))
    })
}

pub fn inspect_path_archive_payload(data: &[u8]) -> Result<PathArchiveInfo> {
    let (root_name, entries, _body) = read_payload(data)?;
    Ok(path_archive_info(root_name, &entries))
}

fn path_archive_info(root_name: String, entries: &[Entry]) -> PathArchiveInfo {
    let mut files = 0usize;
    let mut directories = 0usize;
    let mut total_size = 0u64;
    for entry in entries {
        match entry {
            Entry::Dir { .. } => directories += 1,
            Entry::File { size, .. } => {
                files += 1;
                total_size = total_size.saturating_add(*size as u64);
            }
        }
    }
    PathArchiveInfo {
        root_name,
        files,
        directories,
        total_size,
    }
}

pub fn extract_path_archive(
    data: &[u8],
    output_dir: impl AsRef<Path>,
    force: bool,
) -> Result<PathArchiveInfo> {
    let (root_name, entries, body) = read_payload(data)?;
    let info = path_archive_info(root_name, &entries);
    let root = output_dir.as_ref();

    if root.exists() {
        let meta = fs::symlink_metadata(root)?;
        if meta.file_type().is_symlink() {
            return err(format!(
                "refusing to extract into symlink: {}",
                root.display()
            ));
        }
        if !meta.is_dir() {
            return err(format!(
                "output path exists and is not a directory: {}",
                root.display()
            ));
        }
        if !force {
            return err(format!("output directory exists: {}", root.display()));
        }
    } else {
        fs::create_dir_all(root)?;
    }

    let mut dirs: Vec<_> = entries
        .iter()
        .filter_map(|e| match e {
            Entry::Dir {
                path,
                mode,
                mtime_ns,
            } => Some((path, *mode, *mtime_ns)),
            _ => None,
        })
        .collect();
    dirs.sort_by_key(|(path, _, _)| path.split('/').count());
    for (rel, mode, mtime_ns) in dirs {
        let dest = join_rel(root, rel)?;
        reject_symlink_parents(root, &dest)?;
        if dest.exists() {
            let meta = fs::symlink_metadata(&dest)?;
            if meta.file_type().is_symlink() {
                return err(format!("refusing to overwrite symlink: {}", dest.display()));
            }
            if !meta.is_dir() {
                return err(format!(
                    "cannot create directory over existing file: {}",
                    dest.display()
                ));
            }
        }
        fs::create_dir_all(&dest)?;
        apply_metadata(&dest, mode, mtime_ns);
    }

    for entry in entries {
        let Entry::File {
            path: rel,
            mode,
            mtime_ns,
            offset,
            size,
            crc32: _,
        } = entry
        else {
            continue;
        };
        let dest = join_rel(root, &rel)?;
        reject_symlink_parents(root, &dest)?;
        if dest.exists() {
            let meta = fs::symlink_metadata(&dest)?;
            if meta.file_type().is_symlink() {
                return err(format!("refusing to overwrite symlink: {}", dest.display()));
            }
            if meta.is_dir() {
                return err(format!(
                    "cannot write file over existing directory: {}",
                    dest.display()
                ));
            }
            if !force {
                return err(format!("output file exists: {}", dest.display()));
            }
        }
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        let end = offset
            .checked_add(size)
            .ok_or_else(|| Pro55Error::new("file entry range overflow"))?;
        if end > body.len() {
            return err("file entry exceeds 5.5pro path archive data section");
        }
        let chunk = &body[offset..end];
        atomic_write(&dest, chunk)?;
        apply_metadata(&dest, mode, mtime_ns);
    }
    Ok(info)
}

fn atomic_write(dest: &Path, data: &[u8]) -> Result<()> {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    let stem = dest
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("output");
    let pid = std::process::id();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    for attempt in 0..1000u32 {
        let tmp = parent.join(format!(".{stem}.{pid}.{nanos}.{attempt}.tmp"));
        match OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(mut file) => {
                if let Err(err) = file.write_all(data).and_then(|_| file.sync_all()) {
                    let _ = fs::remove_file(&tmp);
                    return Err(Pro55Error::from(err));
                }
                #[cfg(windows)]
                {
                    if dest.exists() {
                        fs::remove_file(dest)?;
                    }
                }
                if let Err(err) = fs::rename(&tmp, dest) {
                    let _ = fs::remove_file(&tmp);
                    return Err(Pro55Error::from(err));
                }
                return Ok(());
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(Pro55Error::from(err)),
        }
    }
    err("could not allocate temporary file for atomic write")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_validation() {
        assert!(validate_storable_path("a/b.txt").is_ok());
        assert!(validate_storable_path("../evil").is_err());
        assert!(validate_storable_path("a//b").is_err());
        assert!(validate_storable_path("/abs").is_err());
    }
}
