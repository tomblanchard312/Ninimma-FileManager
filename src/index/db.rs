// Ninimma modification: independent project branding and integration.
//! SQLCipher connections and the index's reads and writes.
//!
//! Every connection is keyed before anything is read, must report a
//! SQLCipher version, and is refused otherwise: there is no unencrypted
//! fallback. All values go through bound parameters.

use super::schema::{self, MigrationError};
use crate::security::permissions::{check_private_file, create_private_file, PermError};
use crate::security::secret::{to_hex, SecretBytes};
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;
use zeroize::Zeroizing;

pub const METADATA_DB: &str = "metadata.db";
/// Files SQLite keeps next to a database.
pub const SIDE_FILES: &[&str] = &["-wal", "-shm", "-journal"];

#[derive(Debug, Clone, PartialEq)]
pub enum DbError {
    /// This build or library can't encrypt, so the index stays off.
    EncryptionUnavailable,
    /// The key doesn't open it, or it's damaged.
    WrongKeyOrCorrupt,
    /// Made by a newer Ninimma.
    NewerSchema(u32),
    Permissions(PermError),
    /// A database failure; the text is SQLite's error category, never data.
    Sqlite(String),
}

impl fmt::Display for DbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbError::EncryptionUnavailable => {
                f.write_str("encrypted storage isn't available, so the index can't be created")
            }
            DbError::WrongKeyOrCorrupt => {
                f.write_str("the index can't be opened with its key, or it's damaged")
            }
            DbError::NewerSchema(v) => write!(
                f,
                "the index was made by a newer version of Ninimma (format {v})"
            ),
            DbError::Permissions(e) => e.fmt(f),
            DbError::Sqlite(e) => write!(f, "database error: {e}"),
        }
    }
}

impl From<rusqlite::Error> for DbError {
    fn from(e: rusqlite::Error) -> Self {
        match e.sqlite_error_code() {
            Some(rusqlite::ErrorCode::NotADatabase) => DbError::WrongKeyOrCorrupt,
            Some(rusqlite::ErrorCode::DatabaseCorrupt) => DbError::WrongKeyOrCorrupt,
            // The error code's name only: messages can quote values.
            Some(code) => DbError::Sqlite(format!("{code:?}")),
            None => DbError::Sqlite("unexpected result".to_string()),
        }
    }
}

impl From<PermError> for DbError {
    fn from(e: PermError) -> Self {
        DbError::Permissions(e)
    }
}

/// Opens (or with `create`, creates) the encrypted database at `path`.
/// `read_only` connections are for searching.
pub fn open(
    path: &Path,
    key: &SecretBytes,
    create: bool,
    read_only: bool,
) -> Result<Connection, DbError> {
    open_with(path, key, create, read_only, &schema::METADATA)
}

/// As `open`, for a database with its own schema (e.g. semantic.db).
pub fn open_with(
    path: &Path,
    key: &SecretBytes,
    create: bool,
    read_only: bool,
    schema: &schema::Schema,
) -> Result<Connection, DbError> {
    let exists = std::fs::symlink_metadata(path).is_ok();
    if exists {
        check_private_file(path)?;
    } else if create {
        // Created here, empty and private, so SQLite never makes it with
        // looser permissions. SQLite gives the -wal/-shm files the same mode.
        create_private_file(path).map_err(|e| DbError::Permissions(e.into()))?;
    } else {
        return Err(DbError::WrongKeyOrCorrupt);
    }

    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut conn = Connection::open_with_flags(path, flags)?;
    apply_key(&conn, key)?;

    // A plain SQLite library would ignore the key and write plaintext.
    let cipher: Option<String> = conn
        .query_row("PRAGMA cipher_version", [], |r| r.get(0))
        .optional()
        .ok()
        .flatten();
    if cipher.is_none_or(|v| v.trim().is_empty()) {
        return Err(DbError::EncryptionUnavailable);
    }
    // The first read fails here if the key is wrong.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })?;

    conn.busy_timeout(Duration::from_secs(5))?;
    conn.pragma_update(None, "foreign_keys", "ON")?;
    // Sorting and temporary tables stay in memory, never in temp files.
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    // WAL lets searches read while the scanner writes. SQLCipher encrypts
    // the WAL too; `no_plaintext_in_any_file` checks that.
    conn.pragma_update_and_check(None, "journal_mode", "WAL", |_| Ok(()))?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;

    if read_only {
        conn.pragma_update(None, "query_only", "ON")?;
    } else {
        schema::migrate_schema(&mut conn, schema).map_err(|e| match e {
            MigrationError::Newer(v) => DbError::NewerSchema(v),
            MigrationError::Failed => DbError::Sqlite("migration failed".to_string()),
        })?;
    }
    if read_only && schema::version(&conn)? > schema.version() {
        return Err(DbError::NewerSchema(schema::version(&conn)?));
    }
    for suffix in SIDE_FILES {
        let side = side_file(path, suffix);
        if std::fs::symlink_metadata(&side).is_ok() {
            check_private_file(&side)?;
        }
    }
    Ok(conn)
}

/// Keys the connection through SQLCipher's API, as a raw 256-bit key (the
/// `x'…'` form), which skips SQLCipher's passphrase stretching: the key is
/// already random.
fn apply_key(conn: &Connection, key: &SecretBytes) -> Result<(), DbError> {
    let hex = Zeroizing::new(to_hex(key.expose()));
    let mut raw = Zeroizing::new(String::with_capacity(hex.len() + 3));
    raw.push_str("x'");
    raw.push_str(&hex);
    raw.push('\'');
    // SAFETY: the handle is live for the call; the key buffer and its
    // length are valid, and SQLCipher copies what it needs.
    let rc = unsafe {
        rusqlite::ffi::sqlite3_key(
            conn.handle(),
            raw.as_ptr().cast(),
            raw.len() as std::ffi::c_int,
        )
    };
    if rc == rusqlite::ffi::SQLITE_OK {
        Ok(())
    } else {
        Err(DbError::EncryptionUnavailable)
    }
}

pub fn side_file(db: &Path, suffix: &str) -> PathBuf {
    let mut name = db.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

// ----------------------------------------------------------------------
// Paths as stored: the OS's own bytes, so they round-trip exactly.
// ----------------------------------------------------------------------

#[cfg(unix)]
pub fn path_to_native(path: &Path) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt;
    path.as_os_str().as_bytes().to_vec()
}

#[cfg(unix)]
pub fn native_to_path(bytes: &[u8]) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

/// Windows: UTF-16 code units, little-endian, unpaired surrogates kept.
#[cfg(windows)]
pub fn path_to_native(path: &Path) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .flat_map(u16::to_le_bytes)
        .collect()
}

#[cfg(windows)]
pub fn native_to_path(bytes: &[u8]) -> PathBuf {
    use std::os::windows::ffi::OsStringExt;
    let wide: Vec<u16> = bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    PathBuf::from(std::ffi::OsString::from_wide(&wide))
}

/// The byte range of paths strictly inside `folder`, for range queries.
pub fn descendant_range(folder: &Path) -> (Vec<u8>, Vec<u8>) {
    let mut low = path_to_native(folder);
    let sep = path_to_native(Path::new(std::path::MAIN_SEPARATOR_STR));
    if !low.ends_with(&sep) {
        low.extend_from_slice(&sep);
    }
    // Everything starting with `low` sorts below `low` with its last byte
    // incremented (the separator is never 0xff).
    let mut high = low.clone();
    *high.last_mut().expect("separator") += 1;
    (low, high)
}

// ----------------------------------------------------------------------
// Writes, used by the scanner thread.
// ----------------------------------------------------------------------

/// One file or folder as the scanner found it.
pub struct FileRecord {
    pub path: PathBuf,
    pub volume_id: String,
    pub is_dir: bool,
    pub size: u64,
    pub modified_ns: i64,
    pub created_ns: Option<i64>,
    pub identity: Option<Vec<u8>>,
}

pub fn root_id(conn: &Connection, root: &Path) -> Result<i64, DbError> {
    let native = path_to_native(root);
    conn.execute(
        "INSERT INTO roots (native_path, display_path) VALUES (?1, ?2) ON CONFLICT(native_path) DO NOTHING",
        params![native, root.to_string_lossy()],
    )?;
    Ok(conn.query_row(
        "SELECT id FROM roots WHERE native_path = ?1",
        [&native],
        |r| r.get(0),
    )?)
}

/// Starts a new scan of a root and returns its generation number.
pub fn begin_scan(conn: &Connection, root_id: i64) -> Result<i64, DbError> {
    conn.execute(
        "UPDATE roots SET scan_generation = scan_generation + 1 WHERE id = ?1",
        [root_id],
    )?;
    Ok(conn.query_row(
        "SELECT scan_generation FROM roots WHERE id = ?1",
        [root_id],
        |r| r.get(0),
    )?)
}

/// Writes a batch in one transaction.
pub fn upsert_files(
    conn: &mut Connection,
    root_id: i64,
    generation: i64,
    batch: &[FileRecord],
) -> Result<(), DbError> {
    let now = unix_now();
    let tx = conn.transaction()?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO files (root_id, volume_id, native_path, display_path, filename, filename_folded,
                                extension, is_dir, size_bytes, modified_ns, created_ns, file_identity,
                                indexed_at, scan_generation)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
             ON CONFLICT(native_path) DO UPDATE SET
                root_id = excluded.root_id, volume_id = excluded.volume_id,
                display_path = excluded.display_path, filename = excluded.filename,
                filename_folded = excluded.filename_folded, extension = excluded.extension,
                is_dir = excluded.is_dir, size_bytes = excluded.size_bytes,
                modified_ns = excluded.modified_ns, created_ns = excluded.created_ns,
                file_identity = excluded.file_identity, scan_generation = excluded.scan_generation,
                indexed_at = CASE WHEN files.modified_ns = excluded.modified_ns
                                   AND files.size_bytes = excluded.size_bytes
                              THEN files.indexed_at ELSE excluded.indexed_at END,
                -- Changed contents are read again.
                extraction_state = CASE WHEN files.modified_ns = excluded.modified_ns
                                         AND files.size_bytes = excluded.size_bytes
                                    THEN files.extraction_state ELSE 0 END,
                ocr_state = CASE WHEN files.modified_ns = excluded.modified_ns
                                  AND files.size_bytes = excluded.size_bytes
                             THEN files.ocr_state ELSE 0 END,
                picture_state = CASE WHEN files.modified_ns = excluded.modified_ns
                                      AND files.size_bytes = excluded.size_bytes
                                 THEN files.picture_state ELSE 0 END",
        )?;
        for r in batch {
            let name = r
                .path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let extension = (!r.is_dir)
                .then(|| {
                    r.path
                        .extension()
                        .map(|e| e.to_string_lossy().to_lowercase())
                })
                .flatten();
            stmt.execute(params![
                root_id,
                r.volume_id,
                path_to_native(&r.path),
                r.path.to_string_lossy(),
                name,
                name.to_lowercase(),
                extension,
                r.is_dir,
                r.size as i64,
                r.modified_ns,
                r.created_ns,
                r.identity,
                now,
                generation,
            ])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// After a complete scan: forgets what the scan didn't find (deleted,
/// moved, now excluded or unreadable).
pub fn finish_scan(conn: &Connection, root_id: i64, generation: i64) -> Result<usize, DbError> {
    let removed = conn.execute(
        "DELETE FROM files WHERE root_id = ?1 AND scan_generation < ?2",
        params![root_id, generation],
    )?;
    conn.execute(
        "UPDATE roots SET last_scan_at = ?2 WHERE id = ?1",
        params![root_id, unix_now()],
    )?;
    Ok(removed)
}

/// Forgets roots no longer in the settings, and everything under them.
pub fn remove_other_roots(conn: &Connection, keep: &[PathBuf]) -> Result<(), DbError> {
    let keep: Vec<Vec<u8>> = keep.iter().map(|p| path_to_native(p)).collect();
    let mut stmt = conn.prepare("SELECT id, native_path FROM roots")?;
    let roots: Vec<(i64, Vec<u8>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    for (id, native) in roots {
        if !keep.contains(&native) {
            conn.execute("DELETE FROM roots WHERE id = ?1", [id])?;
        }
    }
    Ok(())
}

/// Removes single entries, e.g. results found missing or unreadable.
pub fn remove_paths(conn: &Connection, paths: &[PathBuf]) -> Result<(), DbError> {
    let mut stmt = conn.prepare_cached("DELETE FROM files WHERE native_path = ?1")?;
    for p in paths {
        stmt.execute([path_to_native(p)])?;
    }
    Ok(())
}

/// Forgets everything at or below `path` (deleted or moved away).
pub fn remove_tree(conn: &Connection, path: &Path) -> Result<usize, DbError> {
    let (low, high) = descendant_range(path);
    let n = conn.execute(
        "DELETE FROM files WHERE native_path = ?1",
        [path_to_native(path)],
    )?;
    Ok(n + conn.execute(
        "DELETE FROM files WHERE native_path >= ?1 AND native_path < ?2",
        params![low, high],
    )?)
}

/// The root containing `path`: its id, current scan generation and path.
pub fn root_of(conn: &Connection, path: &Path) -> Result<Option<(i64, i64, PathBuf)>, DbError> {
    let mut stmt = conn.prepare_cached("SELECT id, native_path, scan_generation FROM roots")?;
    let roots: Vec<(i64, Vec<u8>, i64)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    Ok(roots
        .into_iter()
        .filter(|(_, native, _)| path.starts_with(native_to_path(native)))
        .max_by_key(|(_, native, _)| native.len())
        .map(|(id, native, generation)| (id, generation, native_to_path(&native))))
}

/// A file waiting to have its contents read.
pub struct Pending {
    pub id: i64,
    pub path: PathBuf,
    pub is_dir: bool,
    pub size: u64,
}

/// The next files whose contents need reading, most recently changed first.
pub fn pending_extraction(conn: &Connection, limit: usize) -> Result<Vec<Pending>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, native_path, is_dir, size_bytes FROM files WHERE extraction_state = 0
         ORDER BY modified_ns DESC LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(Pending {
            id: r.get(0)?,
            path: native_to_path(&r.get::<_, Vec<u8>>(1)?),
            is_dir: r.get(2)?,
            size: r.get::<_, i64>(3)? as u64,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn pending_count(conn: &Connection) -> Result<u64, DbError> {
    Ok(conn.query_row(
        "SELECT count(*) FROM files WHERE extraction_state = 0",
        [],
        |r| r.get::<_, i64>(0),
    )? as u64)
}

/// What was read from one file.
pub struct Contents<'a> {
    pub state: i64,
    /// A short failure category, never file contents.
    pub error: Option<&'static str>,
    pub text: &'a str,
    pub chunks: &'a [(usize, usize, String)],
    pub taken_ns: Option<i64>,
    pub gps: Option<(f64, f64)>,
    /// (shown, searchable)
    pub place: Option<(&'a str, &'a str)>,
}

/// Stores what was read from a file, replacing what was there, in one
/// transaction: its search text, its chunks, and its photo details.
pub fn store_contents(conn: &mut Connection, id: i64, c: &Contents) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    tx.execute(
        "UPDATE files SET extraction_state = ?2, extraction_error = ?3, taken_ns = ?4, place = ?5,
                          latitude = ?6, longitude = ?7
         WHERE id = ?1",
        params![
            id,
            c.state,
            c.error,
            c.taken_ns,
            c.place.map(|p| p.0),
            c.gps.map(|g| g.0),
            c.gps.map(|g| g.1)
        ],
    )?;
    tx.execute("DELETE FROM document_chunks WHERE file_id = ?1", [id])?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO document_chunks (file_id, chunk_index, content, start_offset, end_offset, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (i, (start, end, text)) in c.chunks.iter().enumerate() {
            use blake2::Digest;
            let hash = blake2::Blake2s256::digest(text.as_bytes()).to_vec();
            stmt.execute(params![
                id,
                i as i64,
                text,
                *start as i64,
                *end as i64,
                hash
            ])?;
        }
    }
    tx.execute("DELETE FROM file_fts WHERE rowid = ?1", [id])?;
    tx.execute(
        "INSERT INTO file_fts (rowid, filename, display_path, place, content)
         SELECT id, filename, display_path, ?2, ?3 FROM files WHERE id = ?1",
        params![id, c.place.map(|p| p.1).unwrap_or(""), c.text],
    )?;
    tx.commit()?;
    Ok(())
}

/// A piece of document text waiting for its vector.
pub struct ChunkToEmbed {
    pub id: i64,
    pub file_id: i64,
    pub hash: Vec<u8>,
    pub text: String,
}

pub fn chunks_to_embed(conn: &Connection, limit: usize) -> Result<Vec<ChunkToEmbed>, DbError> {
    let mut stmt = conn.prepare_cached(
        "SELECT id, file_id, content_hash, content FROM document_chunks WHERE embedded = 0 ORDER BY id LIMIT ?1",
    )?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(ChunkToEmbed {
            id: r.get(0)?,
            file_id: r.get(1)?,
            hash: r.get(2)?,
            text: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn chunks_to_embed_count(conn: &Connection) -> Result<u64, DbError> {
    Ok(conn.query_row(
        "SELECT count(*) FROM document_chunks WHERE embedded = 0",
        [],
        |r| r.get::<_, i64>(0),
    )? as u64)
}

pub fn mark_embedded(conn: &Connection, ids: &[i64]) -> Result<(), DbError> {
    let mut stmt = conn.prepare_cached("UPDATE document_chunks SET embedded = 1 WHERE id = ?1")?;
    for id in ids {
        stmt.execute([id])?;
    }
    Ok(())
}

/// Every chunk needs a new vector (the model changed, or semantic.db was
/// rebuilt).
pub fn reset_embedded(conn: &Connection) -> Result<(), DbError> {
    conn.execute("UPDATE document_chunks SET embedded = 0", [])?;
    Ok(())
}

/// Whether `file_id` still has a chunk made from exactly this text. A
/// vector is only trusted if it does (ids can be reused after deletions).
pub fn chunk_is_current(conn: &Connection, file_id: i64, hash: &[u8]) -> Result<bool, DbError> {
    let mut stmt = conn
        .prepare_cached("SELECT 1 FROM document_chunks WHERE file_id = ?1 AND content_hash = ?2")?;
    Ok(stmt.exists(params![file_id, hash])?)
}

pub fn live_file_ids(conn: &Connection) -> Result<std::collections::HashSet<i64>, DbError> {
    let mut stmt = conn.prepare("SELECT DISTINCT file_id FROM document_chunks")?;
    let ids = stmt.query_map([], |r| r.get(0))?;
    Ok(ids.collect::<Result<_, _>>()?)
}

/// Files whose pictures have been looked at (and are still indexed).
pub fn picture_file_ids(conn: &Connection) -> Result<std::collections::HashSet<i64>, DbError> {
    let mut stmt = conn.prepare("SELECT id FROM files WHERE picture_state = 1")?;
    let ids = stmt.query_map([], |r| r.get(0))?;
    Ok(ids.collect::<Result<_, _>>()?)
}

/// Image and PDF extensions OCR considers.
pub const OCR_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "tif", "tiff", "webp", "bmp", "pdf"];

/// Files to try OCR on, newest first: images, and PDFs that had no text of
/// their own (scans), once their other details have been read. Only those
/// modified at or after `since_ns` (`i64::MIN` for all).
pub fn ocr_candidates(
    conn: &Connection,
    limit: usize,
    since_ns: i64,
) -> Result<Vec<Pending>, DbError> {
    let list = OCR_EXTENSIONS
        .iter()
        .map(|e| format!("'{e}'"))
        .collect::<Vec<_>>()
        .join(",");
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT f.id, f.native_path, f.is_dir, f.size_bytes FROM files f
         WHERE f.ocr_state = 0 AND f.extraction_state IN (1, 5) AND f.is_dir = 0
           AND f.extension IN ({list})
           AND (f.extension != 'pdf' OR NOT EXISTS (SELECT 1 FROM document_chunks c WHERE c.file_id = f.id))
           AND f.modified_ns >= ?2
         ORDER BY f.modified_ns DESC LIMIT ?1"
    ))?;
    let rows = stmt.query_map([limit as i64, since_ns], |r| {
        Ok(Pending {
            id: r.get(0)?,
            path: native_to_path(&r.get::<_, Vec<u8>>(1)?),
            is_dir: r.get(2)?,
            size: r.get::<_, i64>(3)? as u64,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn ocr_candidate_count(conn: &Connection, since_ns: i64) -> Result<u64, DbError> {
    let list = OCR_EXTENSIONS
        .iter()
        .map(|e| format!("'{e}'"))
        .collect::<Vec<_>>()
        .join(",");
    Ok(conn.query_row(
        &format!(
            "SELECT count(*) FROM files f WHERE f.ocr_state = 0 AND f.extraction_state IN (1, 5) AND f.is_dir = 0
               AND f.extension IN ({list})
               AND (f.extension != 'pdf' OR NOT EXISTS (SELECT 1 FROM document_chunks c WHERE c.file_id = f.id))
               AND f.modified_ns >= ?1"
        ),
        [since_ns],
        |r| r.get::<_, i64>(0),
    )? as u64)
}

/// Photo and video extensions whose pictures are searched by what they show.
pub const PICTURE_EXTENSIONS: &[&str] =
    &["jpg", "jpeg", "png", "webp", "bmp", "tif", "tiff", "gif"];
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mp4", "mov", "m4v", "mkv", "webm", "avi", "wmv", "mpg", "mpeg", "3gp",
];

fn picture_filter(videos: bool) -> String {
    let exts: Vec<String> = PICTURE_EXTENSIONS
        .iter()
        .chain(if videos { VIDEO_EXTENSIONS } else { &[] })
        .map(|e| format!("'{e}'"))
        .collect();
    format!(
        "f.picture_state = 0 AND f.extraction_state != 0 AND f.is_dir = 0 AND f.extension IN ({})",
        exts.join(",")
    )
}

/// Photos (and videos, when they can be read) still to look at, newest first.
pub fn picture_candidates(
    conn: &Connection,
    videos: bool,
    limit: usize,
) -> Result<Vec<Pending>, DbError> {
    let mut stmt = conn.prepare_cached(&format!(
        "SELECT f.id, f.native_path, f.is_dir, f.size_bytes FROM files f WHERE {}
         ORDER BY f.modified_ns DESC LIMIT ?1",
        picture_filter(videos)
    ))?;
    let rows = stmt.query_map([limit as i64], |r| {
        Ok(Pending {
            id: r.get(0)?,
            path: native_to_path(&r.get::<_, Vec<u8>>(1)?),
            is_dir: r.get(2)?,
            size: r.get::<_, i64>(3)? as u64,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

pub fn picture_candidate_count(conn: &Connection, videos: bool) -> Result<u64, DbError> {
    Ok(conn.query_row(
        &format!(
            "SELECT count(*) FROM files f WHERE {}",
            picture_filter(videos)
        ),
        [],
        |r| r.get::<_, i64>(0),
    )? as u64)
}

pub fn set_picture_state(conn: &Connection, ids: &[i64], state: i64) -> Result<(), DbError> {
    let tx = conn.unchecked_transaction()?;
    {
        let mut stmt = tx.prepare_cached("UPDATE files SET picture_state = ?2 WHERE id = ?1")?;
        for id in ids {
            stmt.execute(params![id, state])?;
        }
    }
    tx.commit()?;
    Ok(())
}

/// Every picture is looked at again (a new picture model, or a rebuild).
pub fn reset_pictures(conn: &Connection) -> Result<(), DbError> {
    conn.execute("UPDATE files SET picture_state = 0", [])?;
    Ok(())
}

pub fn set_ocr_state(conn: &Connection, id: i64, state: i64) -> Result<(), DbError> {
    conn.execute(
        "UPDATE files SET ocr_state = ?2 WHERE id = ?1",
        params![id, state],
    )?;
    Ok(())
}

/// Stores text read by OCR as the file's searchable text (keeping its
/// name, folder and place) and as chunks for smart search.
pub fn store_ocr_text(
    conn: &mut Connection,
    id: i64,
    text: &str,
    chunks: &[(usize, usize, String)],
) -> Result<(), DbError> {
    let tx = conn.transaction()?;
    let place: String = rusqlite::OptionalExtension::optional(tx.query_row(
        "SELECT place FROM file_fts WHERE rowid = ?1",
        [id],
        |r| r.get::<_, Option<String>>(0),
    ))?
    .flatten()
    .unwrap_or_default();
    tx.execute("DELETE FROM file_fts WHERE rowid = ?1", [id])?;
    tx.execute(
        "INSERT INTO file_fts (rowid, filename, display_path, place, content)
         SELECT id, filename, display_path, ?2, ?3 FROM files WHERE id = ?1",
        params![id, place, text],
    )?;
    tx.execute("DELETE FROM document_chunks WHERE file_id = ?1", [id])?;
    {
        let mut stmt = tx.prepare_cached(
            "INSERT INTO document_chunks (file_id, chunk_index, content, start_offset, end_offset, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for (i, (start, end, chunk)) in chunks.iter().enumerate() {
            use blake2::Digest;
            let hash = blake2::Blake2s256::digest(chunk.as_bytes()).to_vec();
            stmt.execute(params![
                id,
                i as i64,
                chunk,
                *start as i64,
                *end as i64,
                hash
            ])?;
        }
    }
    tx.execute("UPDATE files SET ocr_state = 1 WHERE id = ?1", [id])?;
    tx.commit()?;
    Ok(())
}

/// Forgets all text read by OCR (OCR turned off), so it's read again if
/// OCR is turned back on.
pub fn clear_ocr(conn: &mut Connection) -> Result<(), DbError> {
    let ids: Vec<i64> = conn
        .prepare("SELECT id FROM files WHERE ocr_state = 1")?
        .query_map([], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    let tx = conn.transaction()?;
    for id in ids {
        let place: String = rusqlite::OptionalExtension::optional(tx.query_row(
            "SELECT place FROM file_fts WHERE rowid = ?1",
            [id],
            |r| r.get::<_, Option<String>>(0),
        ))?
        .flatten()
        .unwrap_or_default();
        tx.execute("DELETE FROM file_fts WHERE rowid = ?1", [id])?;
        tx.execute(
            "INSERT INTO file_fts (rowid, filename, display_path, place, content)
             SELECT id, filename, display_path, ?2, '' FROM files WHERE id = ?1",
            params![id, place],
        )?;
        tx.execute("DELETE FROM document_chunks WHERE file_id = ?1", [id])?;
    }
    tx.execute("UPDATE files SET ocr_state = 0", [])?;
    tx.commit()?;
    Ok(())
}

/// Marks a file as failed, so it isn't tried again until it changes.
pub fn mark_failed(conn: &Connection, id: i64) -> Result<(), DbError> {
    conn.execute(
        "UPDATE files SET extraction_state = 3, extraction_error = 'could not be stored' WHERE id = ?1",
        [id],
    )?;
    Ok(())
}

/// Reads PDFs again once a PDF reader is installed.
pub fn retry_needing_tool(conn: &Connection, state: i64) -> Result<usize, DbError> {
    Ok(conn.execute(
        "UPDATE files SET extraction_state = 0 WHERE extraction_state = ?1",
        [state],
    )?)
}

/// Drops all document text and photo details (back to names only), and
/// marks every file to be read again if text is turned back on.
pub fn clear_contents(conn: &Connection) -> Result<(), DbError> {
    conn.execute_batch(
        "DELETE FROM document_chunks;
         DELETE FROM file_fts;
         UPDATE files SET extraction_state = 0, extraction_error = NULL, taken_ns = NULL,
                          place = NULL, latitude = NULL, longitude = NULL;",
    )?;
    Ok(())
}

pub fn file_count(conn: &Connection) -> Result<u64, DbError> {
    Ok(conn.query_row("SELECT count(*) FROM files", [], |r| r.get::<_, i64>(0))? as u64)
}

/// Whether any of `roots` was never scanned, or not since `since`.
pub fn needs_scan(conn: &Connection, roots: &[PathBuf], since: i64) -> Result<bool, DbError> {
    let mut stmt = conn.prepare_cached("SELECT last_scan_at FROM roots WHERE native_path = ?1")?;
    for root in roots {
        let at: Option<Option<i64>> = stmt
            .query_row([path_to_native(root)], |r| r.get(0))
            .optional()?;
        if at.flatten().is_none_or(|t| t < since) {
            return Ok(true);
        }
    }
    Ok(false)
}

pub fn last_scan(conn: &Connection) -> Result<Option<i64>, DbError> {
    Ok(conn.query_row("SELECT min(last_scan_at) FROM roots", [], |r| r.get(0))?)
}

/// Bytes the database occupies, including its WAL.
/// Folders indexed under `root`, counting the root; None if it hasn't
/// finished a scan yet (so the number isn't known).
pub fn folder_count(conn: &Connection, root: &Path) -> Result<Option<u64>, DbError> {
    let row: Option<(i64, Option<i64>)> = conn
        .query_row(
            "SELECT id, last_scan_at FROM roots WHERE native_path = ?1",
            [path_to_native(root)],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((id, Some(_))) = row else {
        return Ok(None);
    };
    let n: i64 = conn.query_row(
        "SELECT count(*) FROM files WHERE root_id = ?1 AND is_dir = 1",
        [id],
        |r| r.get(0),
    )?;
    Ok(Some(n as u64 + 1))
}

/// Bytes the index really uses: its pages in use plus the write-ahead
/// log. Pages freed by deletions don't count (SQLite keeps the file's size
/// until it's vacuumed), so removing a folder makes room at once.
pub fn used_bytes(conn: &Connection, db_path: &Path) -> Result<u64, DbError> {
    let pragma = |name: &str| pragma_number(conn, name);
    let pages = pragma("page_count")?.saturating_sub(pragma("freelist_count")?);
    let wal = std::fs::metadata(side_file(db_path, "-wal")).map_or(0, |m| m.len());
    Ok(pages * pragma("page_size")? + wal)
}

/// A PRAGMA's numeric value. SQLCipher answers some (`page_size`) as
/// text, so either form is accepted.
fn pragma_number(conn: &Connection, name: &str) -> Result<u64, DbError> {
    let value: rusqlite::types::Value =
        conn.query_row(&format!("PRAGMA {name}"), [], |r| r.get(0))?;
    match value {
        rusqlite::types::Value::Integer(n) => Ok(n.max(0) as u64),
        rusqlite::types::Value::Text(t) => t
            .trim()
            .parse()
            .map_err(|_| DbError::Sqlite(format!("PRAGMA {name} isn't a number"))),
        _ => Err(DbError::Sqlite(format!("PRAGMA {name} isn't a number"))),
    }
}

/// Gives freed space back to the disk once it's worth it: more than
/// `threshold` bytes and more than a quarter of the file. Returns whether
/// it vacuumed. VACUUM rewrites the file (keeping its encryption), so it's
/// done only after large deletions, e.g. a removed folder.
pub fn reclaim_space(conn: &Connection, threshold: u64) -> Result<bool, DbError> {
    let pragma = |name: &str| pragma_number(conn, name);
    let (pages, free, size) = (
        pragma("page_count")?,
        pragma("freelist_count")?,
        pragma("page_size")?,
    );
    if free * size <= threshold || free * 4 <= pages {
        return Ok(false);
    }
    conn.execute_batch("VACUUM")?;
    checkpoint(conn)?;
    Ok(true)
}

/// Moves the write-ahead log into the database and empties it. (A PRAGMA
/// that returns a row: `execute_batch` would fail on it, silently if the
/// error is ignored.)
pub fn checkpoint(conn: &Connection) -> Result<(), DbError> {
    conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    Ok(())
}

pub fn size_on_disk(db_path: &Path) -> u64 {
    let len = |p: &Path| std::fs::metadata(p).map_or(0, |m| m.len());
    len(db_path)
        + SIDE_FILES
            .iter()
            .map(|s| len(&side_file(db_path, s)))
            .sum::<u64>()
}

pub fn unix_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::security::secret::KEY_LEN;

    /// A fixture name that must never appear unencrypted on disk.
    const FIXTURE: &str = "TOPSECRET-fixture-quarterly-salaries";

    pub fn open_test_db(dir: &Path, key: &SecretBytes) -> Connection {
        open(&dir.join(METADATA_DB), key, true, false).unwrap()
    }

    fn record(path: &str, generation_hint: u64) -> FileRecord {
        FileRecord {
            path: PathBuf::from(path),
            volume_id: "1".into(),
            is_dir: false,
            size: generation_hint,
            modified_ns: 1,
            created_ns: None,
            identity: None,
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn the_right_key_opens_and_a_wrong_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        drop(open_test_db(dir.path(), &key));
        let path = dir.path().join(METADATA_DB);
        assert!(open(&path, &key, false, false).is_ok());
        let wrong = SecretBytes::random(KEY_LEN).unwrap();
        assert_eq!(
            open(&path, &wrong, false, false).err(),
            Some(DbError::WrongKeyOrCorrupt)
        );
    }

    #[test]
    fn sqlcipher_is_really_in_use() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let conn = open_test_db(dir.path(), &key);
        let version: String = conn
            .query_row("PRAGMA cipher_version", [], |r| r.get(0))
            .unwrap();
        assert!(!version.is_empty());
        // FTS5 is compiled in, for the full-text phase.
        conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(x); DROP TABLE t;")
            .unwrap();
    }

    #[test]
    fn a_new_database_is_not_plain_sqlite() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        drop(open_test_db(dir.path(), &key));
        let bytes = std::fs::read(dir.path().join(METADATA_DB)).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3"));
        // Without the key, SQLite sees no database at all.
        let conn = Connection::open(dir.path().join(METADATA_DB)).unwrap();
        assert!(conn
            .query_row("SELECT count(*) FROM sqlite_master", [], |r| r
                .get::<_, i64>(0))
            .is_err());
    }

    #[test]
    fn no_plaintext_in_any_file() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(dir.path(), &key);
        let root = root_id(&conn, Path::new("/home/a")).unwrap();
        let generation = begin_scan(&conn, root).unwrap();
        let batch: Vec<FileRecord> = (0..200)
            .map(|i| record(&format!("/home/a/{FIXTURE}-{i}.xlsx"), i))
            .collect();
        upsert_files(&mut conn, root, generation, &batch).unwrap();

        let check = |when: &str| {
            for entry in std::fs::read_dir(dir.path()).unwrap() {
                let path = entry.unwrap().path();
                let bytes = std::fs::read(&path).unwrap();
                assert!(
                    !contains(&bytes, FIXTURE.as_bytes()),
                    "{when}: plaintext found in {}",
                    path.display()
                );
            }
        };
        // While open, the data sits in the WAL.
        assert!(side_file(&dir.path().join(METADATA_DB), "-wal").exists());
        check("with the WAL in use");
        drop(conn);
        check("after closing");
    }

    #[cfg(unix)]
    #[test]
    fn the_database_and_its_side_files_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(dir.path(), &key);
        let root = root_id(&conn, Path::new("/x")).unwrap();
        upsert_files(&mut conn, root, 1, &[record("/x/a", 1)]).unwrap();
        for entry in std::fs::read_dir(dir.path()).unwrap() {
            let path = entry.unwrap().path();
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "{}", path.display());
        }
    }

    #[test]
    fn a_rescan_forgets_what_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(dir.path(), &key);
        let root = root_id(&conn, Path::new("/r")).unwrap();
        let g1 = begin_scan(&conn, root).unwrap();
        upsert_files(&mut conn, root, g1, &[record("/r/a", 1), record("/r/b", 1)]).unwrap();
        finish_scan(&conn, root, g1).unwrap();
        let g2 = begin_scan(&conn, root).unwrap();
        upsert_files(&mut conn, root, g2, &[record("/r/a", 2)]).unwrap();
        assert_eq!(finish_scan(&conn, root, g2).unwrap(), 1);
        assert_eq!(file_count(&conn).unwrap(), 1);
        // Removing the root forgets its files too.
        remove_other_roots(&conn, &[]).unwrap();
        assert_eq!(file_count(&conn).unwrap(), 0);
    }

    #[test]
    fn a_removed_folder_frees_its_space_for_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(dir.path(), &key);
        let path = dir.path().join(METADATA_DB);
        // A big root (like a whole old Windows drive) and a small one.
        let big = root_id(&conn, Path::new("/big")).unwrap();
        let g = begin_scan(&conn, big).unwrap();
        let files: Vec<FileRecord> = (0..20_000)
            .map(|i| record(&format!("/big/some/long/folder/name/file-{i:06}.txt"), 1))
            .collect();
        upsert_files(&mut conn, big, g, &files).unwrap();
        finish_scan(&conn, big, g).unwrap();
        let small = root_id(&conn, Path::new("/small")).unwrap();
        let g = begin_scan(&conn, small).unwrap();
        upsert_files(&mut conn, small, g, &[record("/small/a", 1)]).unwrap();
        finish_scan(&conn, small, g).unwrap();
        checkpoint(&conn).unwrap();
        let before = size_on_disk(&path);
        assert_eq!(folder_count(&conn, Path::new("/small")).unwrap(), Some(1));
        assert_eq!(
            folder_count(&conn, Path::new("/never-scanned")).unwrap(),
            None
        );

        remove_other_roots(&conn, &[PathBuf::from("/small")]).unwrap();
        checkpoint(&conn).unwrap();
        // The file keeps its size, but the space in use drops at once...
        assert!(size_on_disk(&path) >= before);
        let used = used_bytes(&conn, &path).unwrap();
        assert!(used * 4 < before, "used {used} of {before}");
        // ...and reclaiming gives it back to the disk, still encrypted.
        assert!(
            !reclaim_space(&conn, u64::MAX).unwrap(),
            "below the threshold: nothing done"
        );
        assert!(reclaim_space(&conn, 0).unwrap());
        assert!(
            size_on_disk(&path) * 4 < before,
            "{} of {before}",
            size_on_disk(&path)
        );
        assert!(!std::fs::read(&path)
            .unwrap()
            .starts_with(b"SQLite format 3"));
        assert_eq!(file_count(&conn).unwrap(), 1);
        drop(conn);
        assert!(open(&path, &key, false, false).is_ok());
    }

    #[test]
    fn contents_are_stored_searched_and_forgotten_with_their_file() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(dir.path(), &key);
        let root = root_id(&conn, Path::new("/r")).unwrap();
        upsert_files(&mut conn, root, 1, &[record("/r/dir/note.txt", 1)]).unwrap();
        let pending = pending_extraction(&conn, 10).unwrap();
        assert_eq!(pending.len(), 1);
        let chunks = [(0usize, 12usize, "walrus notes".to_string())];
        store_contents(
            &mut conn,
            pending[0].id,
            &Contents {
                state: 1,
                error: None,
                text: "walrus notes",
                chunks: &chunks,
                taken_ns: None,
                gps: None,
                place: None,
            },
        )
        .unwrap();
        assert_eq!(pending_count(&conn).unwrap(), 0);
        let hits = |conn: &Connection| -> i64 {
            conn.query_row(
                "SELECT count(*) FROM file_fts WHERE file_fts MATCH 'walrus'",
                [],
                |r| r.get(0),
            )
            .unwrap()
        };
        assert_eq!(hits(&conn), 1);
        // A changed file is read again.
        upsert_files(&mut conn, root, 1, &[record("/r/dir/note.txt", 2)]).unwrap();
        assert_eq!(pending_count(&conn).unwrap(), 1);
        // Deleting its folder forgets the file, its text and its chunks.
        remove_tree(&conn, Path::new("/r/dir")).unwrap();
        assert_eq!(hits(&conn), 0);
        let chunks_left: i64 = conn
            .query_row("SELECT count(*) FROM document_chunks", [], |r| r.get(0))
            .unwrap();
        assert_eq!(chunks_left, 0);
    }

    #[test]
    fn clearing_contents_keeps_names() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(dir.path(), &key);
        let root = root_id(&conn, Path::new("/r")).unwrap();
        upsert_files(&mut conn, root, 1, &[record("/r/a.txt", 1)]).unwrap();
        let id = pending_extraction(&conn, 1).unwrap()[0].id;
        store_contents(
            &mut conn,
            id,
            &Contents {
                state: 1,
                error: None,
                text: "walrus",
                chunks: &[],
                taken_ns: Some(5),
                gps: Some((1.0, 2.0)),
                place: Some(("Here", "Here")),
            },
        )
        .unwrap();
        clear_contents(&conn).unwrap();
        assert_eq!(file_count(&conn).unwrap(), 1);
        let place: Option<String> = conn
            .query_row("SELECT place FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(place, None);
        let fts: i64 = conn
            .query_row("SELECT count(*) FROM file_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts, 0);
    }

    #[test]
    fn descendant_ranges_cover_subfolders_only() {
        let (low, high) = descendant_range(Path::new("/home/a"));
        let inside = path_to_native(Path::new("/home/a/x/y"));
        let sibling = path_to_native(Path::new("/home/ab"));
        let itself = path_to_native(Path::new("/home/a"));
        assert!(inside >= low && inside < high);
        assert!(!(sibling >= low && sibling < high));
        assert!(!(itself >= low && itself < high));
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_paths_survive_exactly() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"/home/a/caf\xe9-latin1.txt");
        let path = Path::new(raw);
        assert_eq!(native_to_path(&path_to_native(path)), path);

        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut conn = open_test_db(dir.path(), &key);
        let root = root_id(&conn, Path::new("/home/a")).unwrap();
        let rec = FileRecord {
            path: path.to_path_buf(),
            ..record("/unused", 1)
        };
        upsert_files(&mut conn, root, 1, &[rec]).unwrap();
        let stored: Vec<u8> = conn
            .query_row("SELECT native_path FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(native_to_path(&stored), path);
    }
}
