//! The backup file format: a self-describing media file holding one or more
//! backup sets.
//!
//! ```text
//! magic    16 bytes   "MSDUCK BACKUP\0\0" and a format version byte
//! set*     u32 LE     header length, then the header as JSON
//!          u64 LE     payload length, then the payload
//! ```
//!
//! The header describes the media (GUID, name, compression) and the backup
//! set (database, files, dates, flags) in the terms RESTORE HEADERONLY and
//! FILELISTONLY report. The payload is a complete DuckDB database file made
//! with `COPY FROM DATABASE`, so a backup is one transactionally consistent
//! snapshot, and its SHA-256 lets RESTORE detect damage.
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const MAGIC: &[u8; 16] = b"MSDUCK BACKUP\0\0\x01";
/// Headers are small; a larger length means the file is not a backup.
const MAX_HEADER: u32 = 1 << 20;

/// One file of a backed up database, as FILELISTONLY reports it.
#[derive(Clone, Debug, PartialEq)]
pub struct FileEntry {
    pub logical: String,
    pub physical: String,
    /// `D` for data, `L` for log.
    pub kind: char,
    pub size: u64,
}

/// A backup set's header.
#[derive(Clone, Debug, PartialEq)]
pub struct Header {
    pub media_guid: String,
    pub media_name: Option<String>,
    pub media_description: Option<String>,
    pub media_compressed: bool,
    pub name: Option<String>,
    pub description: Option<String>,
    pub compressed: bool,
    pub copy_only: bool,
    pub checksum: bool,
    pub user_name: String,
    pub server_name: String,
    pub database_name: String,
    /// Microseconds since the Unix epoch, local time.
    pub database_creation_date: i64,
    pub backup_start: i64,
    pub backup_finish: i64,
    pub family_guid: String,
    pub database_guid: String,
    pub backup_set_guid: String,
    pub files: Vec<FileEntry>,
    /// SHA-256 of the payload, in hex.
    pub payload_sha256: String,
}

impl Header {
    fn to_json(&self) -> Value {
        json!({
            "media_guid": self.media_guid,
            "media_name": self.media_name,
            "media_description": self.media_description,
            "media_compressed": self.media_compressed,
            "name": self.name,
            "description": self.description,
            "compressed": self.compressed,
            "copy_only": self.copy_only,
            "checksum": self.checksum,
            "user_name": self.user_name,
            "server_name": self.server_name,
            "database_name": self.database_name,
            "database_creation_date": self.database_creation_date,
            "backup_start": self.backup_start,
            "backup_finish": self.backup_finish,
            "family_guid": self.family_guid,
            "database_guid": self.database_guid,
            "backup_set_guid": self.backup_set_guid,
            "files": self.files.iter().map(|file| json!({
                "logical": file.logical,
                "physical": file.physical,
                "kind": file.kind.to_string(),
                "size": file.size,
            })).collect::<Vec<_>>(),
            "payload_sha256": self.payload_sha256,
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let text = |name: &str| value.get(name)?.as_str().map(str::to_owned);
        let optional = |name: &str| match value.get(name) {
            Some(Value::Null) => Some(None),
            Some(Value::String(text)) => Some(Some(text.clone())),
            _ => None,
        };
        let flag = |name: &str| value.get(name)?.as_bool();
        let number = |name: &str| value.get(name)?.as_i64();
        let files = value
            .get("files")?
            .as_array()?
            .iter()
            .map(|file| {
                Some(FileEntry {
                    logical: file.get("logical")?.as_str()?.to_owned(),
                    physical: file.get("physical")?.as_str()?.to_owned(),
                    kind: file.get("kind")?.as_str()?.chars().next()?,
                    size: file.get("size")?.as_u64()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        Some(Self {
            media_guid: text("media_guid")?,
            media_name: optional("media_name")?,
            media_description: optional("media_description")?,
            media_compressed: flag("media_compressed")?,
            name: optional("name")?,
            description: optional("description")?,
            compressed: flag("compressed")?,
            copy_only: flag("copy_only")?,
            checksum: flag("checksum")?,
            user_name: text("user_name")?,
            server_name: text("server_name")?,
            database_name: text("database_name")?,
            database_creation_date: number("database_creation_date")?,
            backup_start: number("backup_start")?,
            backup_finish: number("backup_finish")?,
            family_guid: text("family_guid")?,
            database_guid: text("database_guid")?,
            backup_set_guid: text("backup_set_guid")?,
            files,
            payload_sha256: text("payload_sha256")?,
        })
    }

    pub fn file(&self, kind: char) -> Option<&FileEntry> {
        self.files.iter().find(|file| file.kind == kind)
    }
}

/// A backup set found in a media file.
#[derive(Clone, Debug)]
pub struct Set {
    /// 1-based position on the media.
    pub position: u32,
    pub header: Header,
    /// Where the set's record starts and ends in the file.
    pub start: u64,
    pub end: u64,
    pub payload_offset: u64,
    pub payload_len: u64,
}

/// What reading a media file found.
#[derive(Debug)]
pub enum Media {
    /// The file does not exist.
    Missing,
    /// The file is shorter than the media header: SQL Server's empty volume.
    Empty,
    /// The file is not a well-formed msduck backup.
    Malformed,
    Sets(Vec<Set>),
}

/// Read the backup sets of a media file. I/O errors other than a missing
/// file are returned as errors.
pub fn read(path: &Path) -> Result<Media> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Media::Missing),
        Err(error) => return Err(error.into()),
    };
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        bail!(std::io::Error::from(std::io::ErrorKind::IsADirectory));
    }
    let length = metadata.len();
    if length < MAGIC.len() as u64 {
        return Ok(Media::Empty);
    }
    let mut magic = [0u8; 16];
    file.read_exact(&mut magic)?;
    if &magic != MAGIC {
        return Ok(Media::Malformed);
    }
    let mut sets = Vec::new();
    let mut offset = MAGIC.len() as u64;
    while offset < length {
        let start = offset;
        let mut size = [0u8; 4];
        if length - offset < 4 {
            return Ok(Media::Malformed);
        }
        file.read_exact(&mut size)?;
        let header_len = u32::from_le_bytes(size);
        if header_len > MAX_HEADER || length - offset - 4 < u64::from(header_len) + 8 {
            return Ok(Media::Malformed);
        }
        let mut header = vec![0u8; header_len as usize];
        file.read_exact(&mut header)?;
        let mut size = [0u8; 8];
        file.read_exact(&mut size)?;
        let payload_len = u64::from_le_bytes(size);
        let payload_offset = offset + 4 + u64::from(header_len) + 8;
        let Some(end) = payload_offset
            .checked_add(payload_len)
            .filter(|end| *end <= length)
        else {
            return Ok(Media::Malformed);
        };
        let Some(header) = serde_json::from_slice::<Value>(&header)
            .ok()
            .as_ref()
            .and_then(Header::from_json)
        else {
            return Ok(Media::Malformed);
        };
        sets.push(Set {
            position: sets.len() as u32 + 1,
            header,
            start,
            end,
            payload_offset,
            payload_len,
        });
        file.seek(SeekFrom::Start(end))?;
        offset = end;
    }
    if sets.is_empty() {
        return Ok(Media::Empty);
    }
    Ok(Media::Sets(sets))
}

/// Write a media file: the kept sets of the existing file (by byte range),
/// then a new set whose payload is the file at `payload`. The file is
/// written next to `path` and renamed over it, so a failure leaves any
/// previous media intact.
pub fn write(
    path: &Path,
    existing: Option<(&Path, &[Set])>,
    header: &Header,
    payload: &Path,
) -> Result<u64> {
    let temporary = sibling(path)?;
    let result = (|| -> Result<u64> {
        let mut out = std::io::BufWriter::new(
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)?,
        );
        out.write_all(MAGIC)?;
        if let Some((source, sets)) = existing {
            let mut input = File::open(source)?;
            for set in sets {
                input.seek(SeekFrom::Start(set.start))?;
                let copied = std::io::copy(&mut (&mut input).take(set.end - set.start), &mut out)?;
                if copied != set.end - set.start {
                    bail!("backup media changed while it was being read");
                }
            }
        }
        let header = serde_json::to_vec(&header.to_json())?;
        out.write_all(&(header.len() as u32).to_le_bytes())?;
        out.write_all(&header)?;
        let payload_len = std::fs::metadata(payload)?.len();
        out.write_all(&payload_len.to_le_bytes())?;
        let copied = std::io::copy(&mut File::open(payload)?, &mut out)?;
        if copied != payload_len {
            bail!("backup payload changed while it was being written");
        }
        let file = out.into_inner().map_err(|error| error.into_error())?;
        file.sync_all()?;
        Ok(payload_len)
    })();
    match result {
        Ok(size) => {
            std::fs::rename(&temporary, path)
                .with_context(|| format!("replace {}", path.display()))?;
            Ok(size)
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temporary);
            Err(error)
        }
    }
}

/// Copy a set's payload to `target` and check its digest.
pub fn extract(path: &Path, set: &Set, target: &Path) -> Result<bool> {
    let mut input = File::open(path)?;
    input.seek(SeekFrom::Start(set.payload_offset))?;
    let mut output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)?;
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut remaining = set.payload_len;
    let mut buffer = vec![0u8; 1 << 16];
    while remaining > 0 {
        let chunk = buffer.len().min(remaining as usize);
        input.read_exact(&mut buffer[..chunk])?;
        digest.update(&buffer[..chunk]);
        output.write_all(&buffer[..chunk])?;
        remaining -= chunk as u64;
    }
    output.sync_all()?;
    Ok(hex(digest.finish().as_ref()) == set.header.payload_sha256)
}

/// Whether a set's payload matches its digest, without copying it.
pub fn verify(path: &Path, set: &Set) -> Result<bool> {
    let mut input = File::open(path)?;
    input.seek(SeekFrom::Start(set.payload_offset))?;
    Ok(sha256(&mut input.take(set.payload_len))? == set.header.payload_sha256)
}

/// SHA-256 of a reader's contents, in hex.
pub fn sha256(reader: &mut impl Read) -> Result<String> {
    let mut digest = ring::digest::Context::new(&ring::digest::SHA256);
    let mut buffer = vec![0u8; 1 << 16];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(hex(digest.finish().as_ref()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// A fresh file name next to `path`.
fn sibling(path: &Path) -> Result<PathBuf> {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let name = path
        .file_name()
        .context("backup device needs a file name")?
        .to_string_lossy();
    Ok(path.with_file_name(format!(
        ".{name}.msduck-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header(name: &str) -> Header {
        Header {
            media_guid: "m".into(),
            media_name: None,
            media_description: Some("d".into()),
            media_compressed: true,
            name: Some(name.into()),
            description: None,
            compressed: true,
            copy_only: false,
            checksum: true,
            user_name: "sa".into(),
            server_name: "host".into(),
            database_name: "foo".into(),
            database_creation_date: 1,
            backup_start: 2,
            backup_finish: 3,
            family_guid: "f".into(),
            database_guid: "g".into(),
            backup_set_guid: "s".into(),
            files: vec![FileEntry {
                logical: "foo".into(),
                physical: "/x/foo.mdf".into(),
                kind: 'D',
                size: 5,
            }],
            payload_sha256: String::new(),
        }
    }

    #[test]
    fn media_round_trips_and_appends() {
        let directory = std::env::temp_dir().join(format!("msduck-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("a.bak");
        assert!(matches!(read(&path).unwrap(), Media::Missing));
        let payload = directory.join("p");
        std::fs::write(&payload, b"hello").unwrap();
        let mut first = header("one");
        first.payload_sha256 = sha256(&mut File::open(&payload).unwrap()).unwrap();
        write(&path, None, &first, &payload).unwrap();
        let Media::Sets(sets) = read(&path).unwrap() else {
            panic!("not a backup")
        };
        assert_eq!(sets.len(), 1);
        assert_eq!(sets[0].header, first);
        let mut second = header("two");
        second.payload_sha256 = first.payload_sha256.clone();
        write(&path, Some((&path, &sets)), &second, &payload).unwrap();
        let Media::Sets(sets) = read(&path).unwrap() else {
            panic!("not a backup")
        };
        assert_eq!(
            sets.iter().map(|set| set.position).collect::<Vec<_>>(),
            [1, 2]
        );
        assert_eq!(sets[1].header.name.as_deref(), Some("two"));
        assert!(verify(&path, &sets[1]).unwrap());
        let target = directory.join("out");
        assert!(extract(&path, &sets[0], &target).unwrap());
        assert_eq!(std::fs::read(&target).unwrap(), b"hello");
        // Truncation and foreign files are malformed; short files are empty.
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, &bytes[..bytes.len() - 1]).unwrap();
        assert!(matches!(read(&path).unwrap(), Media::Malformed));
        std::fs::write(&path, b"not a backup file at all").unwrap();
        assert!(matches!(read(&path).unwrap(), Media::Malformed));
        std::fs::write(&path, b"short").unwrap();
        assert!(matches!(read(&path).unwrap(), Media::Empty));
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
