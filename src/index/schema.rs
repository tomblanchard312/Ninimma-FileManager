// Ninimma modification: independent project branding and integration.
//! The metadata index's schema and its migrations.
//!
//! The schema version lives in SQLite's `user_version`. Code never opens a
//! database written by a newer version: it reports it and offers a rebuild.

use rusqlite::Connection;

/// The version this code writes. Bump it and add a step to `MIGRATIONS`
/// for every schema change.
pub const SCHEMA_VERSION: u32 = 5;

/// `MIGRATIONS[n]` upgrades a version-n database to n+1.
const MIGRATIONS: &[&str] = &[
    // 0 -> 1: roots, files and settings.
    "
    CREATE TABLE roots (
        id                  INTEGER PRIMARY KEY,
        native_path         BLOB NOT NULL UNIQUE,
        display_path        TEXT NOT NULL,
        scan_generation     INTEGER NOT NULL DEFAULT 0,
        last_scan_at        INTEGER
    );

    -- Paths are stored as the OS's own bytes (native_path) so names that
    -- aren't valid UTF-8 survive; display_path is only for showing and
    -- searching, never for opening.
    CREATE TABLE files (
        id                  INTEGER PRIMARY KEY,
        root_id             INTEGER NOT NULL REFERENCES roots(id) ON DELETE CASCADE,
        volume_id           TEXT NOT NULL,
        native_path         BLOB NOT NULL UNIQUE,
        display_path        TEXT NOT NULL,
        filename            TEXT NOT NULL,
        filename_folded     TEXT NOT NULL,
        extension           TEXT,
        mime_type           TEXT,
        is_dir              INTEGER NOT NULL,
        size_bytes          INTEGER NOT NULL,
        modified_ns         INTEGER NOT NULL,
        created_ns          INTEGER,
        file_identity       BLOB,
        extraction_state    INTEGER NOT NULL DEFAULT 0,
        extraction_error    TEXT,
        indexed_at          INTEGER NOT NULL,
        scan_generation     INTEGER NOT NULL
    );
    CREATE INDEX files_root_generation ON files(root_id, scan_generation);
    CREATE INDEX files_extension ON files(extension);
    CREATE INDEX files_modified ON files(modified_ns);

    CREATE TABLE index_metadata (
        key                 TEXT PRIMARY KEY,
        value               TEXT NOT NULL
    );
    ",
    // 1 -> 2: document text, photo date and place, full-text search.
    "
    -- When a photo was taken (EXIF), and where (nearest town, state,
    -- country, worked out offline from its GPS position).
    ALTER TABLE files ADD COLUMN taken_ns INTEGER;
    ALTER TABLE files ADD COLUMN place TEXT;
    ALTER TABLE files ADD COLUMN latitude REAL;
    ALTER TABLE files ADD COLUMN longitude REAL;

    -- A few representative pieces of each document's text, bounded, for
    -- the meaning-based search later; content_hash says when to redo them.
    CREATE TABLE document_chunks (
        id                  INTEGER PRIMARY KEY,
        file_id             INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
        chunk_index         INTEGER NOT NULL,
        content             TEXT NOT NULL,
        start_offset        INTEGER,
        end_offset          INTEGER,
        content_hash        BLOB,
        UNIQUE(file_id, chunk_index)
    );

    -- Full-text search over names, folders, places and document text.
    -- The rowid is files.id. Porter stemming makes 'receipts' match
    -- 'receipt'; accents are ignored.
    CREATE VIRTUAL TABLE file_fts USING fts5(
        filename, display_path, place, content,
        tokenize = 'porter unicode61 remove_diacritics 2'
    );
    -- Forget a file's text along with the file.
    CREATE TRIGGER files_forget_text AFTER DELETE ON files BEGIN
        DELETE FROM file_fts WHERE rowid = old.id;
    END;
    ",
    // 2 -> 3: which chunks have vectors in semantic.db.
    "
    ALTER TABLE document_chunks ADD COLUMN embedded INTEGER NOT NULL DEFAULT 0;
    CREATE INDEX chunks_to_embed ON document_chunks(id) WHERE embedded = 0;
    ",
    // 3 -> 4: OCR of photographed documents and scanned PDFs.
    // ocr_state: 0 to do, 1 read, 2 not a document, 3 failed.
    "
    ALTER TABLE files ADD COLUMN ocr_state INTEGER NOT NULL DEFAULT 0;
    CREATE INDEX files_to_ocr ON files(id) WHERE ocr_state = 0;
    ",
    // 4 -> 5: what's in pictures (a point per photo or video in semantic.db).
    // picture_state: 0 to do, 1 done, 2 couldn't be read.
    "
    ALTER TABLE files ADD COLUMN picture_state INTEGER NOT NULL DEFAULT 0;
    CREATE INDEX files_to_picture ON files(id) WHERE picture_state = 0;
    ",
];

/// The separate vector database (smart search). Kept apart so it can be
/// deleted and rebuilt without touching the metadata index.
const SEMANTIC_MIGRATIONS: &[&str] = &[
    // 0 -> 1
    "
    -- One vector per document chunk, identified by its file and the hash
    -- of the text it came from, so a vector is only ever used for the
    -- exact text it was made from.
    CREATE TABLE vectors (
        file_id             INTEGER NOT NULL,
        content_hash        BLOB NOT NULL,
        vector              BLOB NOT NULL,
        PRIMARY KEY (file_id, content_hash)
    ) WITHOUT ROWID;
    -- model_id, dimensions, encoding: a change means a rebuild.
    CREATE TABLE semantic_metadata (
        key                 TEXT PRIMARY KEY,
        value               TEXT NOT NULL
    );
    ",
    // 1 -> 2: one point per photo or video (what it shows), and how close
    // it is to a photo of anything (see `pictures`).
    "
    CREATE TABLE picture_vectors (
        file_id             INTEGER PRIMARY KEY,
        vector              BLOB NOT NULL,
        baseline            REAL NOT NULL
    );
    CREATE TABLE picture_metadata (
        key                 TEXT PRIMARY KEY,
        value               TEXT NOT NULL
    );
    ",
];

/// A database's migrations.
pub struct Schema {
    migrations: &'static [&'static str],
}

impl Schema {
    pub fn version(&self) -> u32 {
        self.migrations.len() as u32
    }
}

pub const METADATA: Schema = Schema {
    migrations: MIGRATIONS,
};
pub const SEMANTIC: Schema = Schema {
    migrations: SEMANTIC_MIGRATIONS,
};

#[derive(Debug, PartialEq, Eq)]
pub enum MigrationError {
    /// Written by a newer version of Ninimma.
    Newer(u32),
    Failed,
}

pub fn version(conn: &Connection) -> rusqlite::Result<u32> {
    conn.pragma_query_value(None, "user_version", |row| row.get(0))
}

/// Brings the database up to `SCHEMA_VERSION`, each step in a transaction,
/// so a failure leaves the previous version intact.
#[cfg(test)]
pub fn migrate(conn: &mut Connection) -> Result<(), MigrationError> {
    migrate_schema(conn, &METADATA)
}

pub fn migrate_schema(conn: &mut Connection, schema: &Schema) -> Result<(), MigrationError> {
    let current = version(conn).map_err(|_| MigrationError::Failed)?;
    if current > schema.version() {
        return Err(MigrationError::Newer(current));
    }
    for (from, sql) in schema.migrations.iter().enumerate().skip(current as usize) {
        let tx = conn.transaction().map_err(|_| MigrationError::Failed)?;
        tx.execute_batch(sql).map_err(|_| MigrationError::Failed)?;
        // user_version can't be bound as a parameter; it's our own integer.
        tx.pragma_update(None, "user_version", from as u32 + 1)
            .map_err(|_| MigrationError::Failed)?;
        tx.commit().map_err(|_| MigrationError::Failed)?;
    }
    debug_assert_eq!(MIGRATIONS.len() as u32, SCHEMA_VERSION);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_database_reaches_the_current_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn).unwrap(), SCHEMA_VERSION);
        // Running again is harmless.
        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn a_phase_1_index_upgrades_and_keeps_its_data() {
        let mut conn = Connection::open_in_memory().unwrap();
        // Version 1 as phase 1 left it, with a file in it.
        conn.execute_batch(MIGRATIONS[0]).unwrap();
        conn.pragma_update(None, "user_version", 1).unwrap();
        conn.execute_batch(
            "INSERT INTO roots (id, native_path, display_path) VALUES (1, x'2f72', '/r');
             INSERT INTO files (root_id, volume_id, native_path, display_path, filename, filename_folded,
                                is_dir, size_bytes, modified_ns, indexed_at, scan_generation)
             VALUES (1, '1', x'2f722f61', '/r/a', 'a', 'a', 0, 1, 1, 1, 1);",
        )
        .unwrap();
        migrate(&mut conn).unwrap();
        assert_eq!(version(&conn).unwrap(), SCHEMA_VERSION);
        let name: String = conn
            .query_row("SELECT filename FROM files", [], |r| r.get(0))
            .unwrap();
        assert_eq!(name, "a");
        // The new columns exist and start empty; the file awaits reading.
        let (place, state): (Option<String>, i64) = conn
            .query_row("SELECT place, extraction_state FROM files", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!((place, state), (None, 0));
        // Deleting a file deletes its searchable text.
        conn.execute("INSERT INTO file_fts (rowid, filename, display_path, place, content) VALUES (1, 'a', '/r/a', '', 'zebra')", [])
            .unwrap();
        conn.execute("DELETE FROM files", []).unwrap();
        let left: i64 = conn
            .query_row(
                "SELECT count(*) FROM file_fts WHERE file_fts MATCH 'zebra'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(left, 0);
    }

    #[test]
    fn a_newer_database_is_never_opened() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();
        assert_eq!(
            migrate(&mut conn),
            Err(MigrationError::Newer(SCHEMA_VERSION + 1))
        );
    }

    #[test]
    fn a_failed_step_leaves_the_old_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        // A table in the way makes step 0 -> 1 fail part-way.
        conn.execute_batch("CREATE TABLE files (x)").unwrap();
        assert_eq!(migrate(&mut conn), Err(MigrationError::Failed));
        assert_eq!(version(&conn).unwrap(), 0);
        let roots: i64 = conn
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = 'roots'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(roots, 0, "the failed step was rolled back");
    }
}
