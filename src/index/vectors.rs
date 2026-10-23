// Ninimma modification: independent project branding and integration.
//! Vectors for smart search, in their own encrypted database.
//!
//! `semantic.db` is SQLCipher like the metadata index and opened the same
//! way. Its key is derived from the metadata key, so there is still one
//! key to store, and deleting it makes both unreadable. Keeping the vectors
//! apart means they can be deleted and rebuilt (a new model, a size limit)
//! without touching the rest of the index.
//!
//! [`VectorIndex`] hides how vectors are stored and searched. This version
//! keeps 8-bit vectors in a plain table and compares them all, which is
//! exact. sqlite-vec was evaluated: its `vec0` tables also compare
//! exhaustively at this version (0.1.x), and loading a pre-1.0 extension
//! into SQLCipher on every platform (FreeBSD included) adds risk for no
//! speed gain at the sizes a personal index reaches (tens of thousands of
//! vectors). An approximate index can replace this behind the same trait.

use super::db::{self, DbError};
use super::schema;
use crate::embedding::{quantize, similarity};
use crate::security::secret::SecretBytes;
use rusqlite::{params, Connection};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const SEMANTIC_DB: &str = "semantic.db";

/// Vectors' storage format; a change means a rebuild.
const ENCODING: &str = "int8-scaled-cosine-v1";

/// semantic.db's key: derived from the metadata key with a fixed label.
pub fn semantic_key(metadata_key: &SecretBytes) -> SecretBytes {
    use blake2::Digest;
    let mut hasher = blake2::Blake2s256::new();
    hasher.update(b"ninimma semantic.db key v1\0");
    hasher.update(metadata_key.expose());
    SecretBytes::from_vec(hasher.finalize().to_vec())
}

pub trait VectorIndex {
    /// The model and size the stored vectors were made with.
    fn model(&self) -> Result<Option<(String, usize)>, DbError>;
    /// Empties the index for a (new) model.
    fn reset(&mut self, model_id: &str, dimensions: usize) -> Result<(), DbError>;
    fn insert(&mut self, items: &[(i64, Vec<u8>, Vec<f32>)]) -> Result<(), DbError>;
    /// Files whose best chunk is most similar to `query`, best first, at
    /// most `k`, with the chunk's text hash.
    fn search(
        &self,
        query: &[f32],
        k: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<(i64, Vec<u8>, f32)>, DbError>;
    /// Drops vectors of files no longer indexed.
    fn retain_files(&mut self, live: &HashSet<i64>) -> Result<usize, DbError>;
    fn count(&self) -> Result<u64, DbError>;
}

pub struct SqliteVectors {
    conn: Connection,
    path: PathBuf,
}

impl SqliteVectors {
    pub fn open(dir: &Path, metadata_key: &SecretBytes, read_only: bool) -> Result<Self, DbError> {
        let path = dir.join(SEMANTIC_DB);
        let key = semantic_key(metadata_key);
        let conn = db::open_with(&path, &key, !read_only, read_only, &schema::SEMANTIC)?;
        Ok(Self { conn, path })
    }

    pub fn size_on_disk(&self) -> u64 {
        db::size_on_disk(&self.path)
    }

    pub fn delete_files(dir: &Path) {
        let path = dir.join(SEMANTIC_DB);
        let _ = std::fs::remove_file(&path);
        for suffix in db::SIDE_FILES {
            let _ = std::fs::remove_file(db::side_file(&path, suffix));
        }
    }
}

/// Points for what pictures show (see `crate::pictures`), kept beside the
/// text vectors in the same encrypted database.
impl SqliteVectors {
    /// The picture model the stored points were made with.
    pub fn picture_model(&self) -> Result<Option<String>, DbError> {
        Ok(rusqlite::OptionalExtension::optional(self.conn.query_row(
            "SELECT value FROM picture_metadata WHERE key = 'model_id'",
            [],
            |r| r.get(0),
        ))?)
    }

    /// Empties the picture points for a (new) model.
    pub fn reset_pictures(&mut self, model_id: &str) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM picture_vectors", [])?;
        tx.execute("DELETE FROM picture_metadata", [])?;
        tx.execute(
            "INSERT INTO picture_metadata (key, value) VALUES ('model_id', ?1)",
            [model_id],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn insert_pictures(
        &mut self,
        items: &[(i64, crate::pictures::Picture)],
    ) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO picture_vectors (file_id, vector, baseline) VALUES (?1, ?2, ?3)",
            )?;
            for (file_id, p) in items {
                stmt.execute(params![file_id, quantize(&p.vector), p.baseline as f64])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Pictures matching a search's point by at least `floor`, best first.
    pub fn search_pictures(
        &self,
        query: &[f32],
        floor: f32,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<(i64, f32)>, DbError> {
        let mut out: Vec<(i64, f32)> = Vec::new();
        let mut stmt = self
            .conn
            .prepare_cached("SELECT file_id, vector, baseline FROM picture_vectors")?;
        let mut rows = stmt.query([])?;
        let mut seen = 0usize;
        while let Some(row) = rows.next()? {
            seen += 1;
            if seen.is_multiple_of(4096) && cancelled() {
                return Ok(Vec::new());
            }
            let vector: Vec<u8> = row.get(1)?;
            let Some(similarity) = similarity(query, &vector) else {
                continue;
            };
            let score = similarity - row.get::<_, f64>(2)? as f32;
            if score >= floor {
                out.push((row.get(0)?, score));
            }
        }
        out.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        Ok(out)
    }

    /// Forgets the points of files no longer indexed.
    pub fn retain_pictures(&mut self, live: &HashSet<i64>) -> Result<usize, DbError> {
        let stored: Vec<i64> = self
            .conn
            .prepare("SELECT file_id FROM picture_vectors")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let tx = self.conn.transaction()?;
        let mut removed = 0;
        for id in stored.into_iter().filter(|id| !live.contains(id)) {
            removed += tx.execute("DELETE FROM picture_vectors WHERE file_id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(removed)
    }

    pub fn picture_count(&self) -> Result<u64, DbError> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM picture_vectors", [], |r| {
                r.get::<_, i64>(0)
            })? as u64)
    }
}

impl VectorIndex for SqliteVectors {
    fn model(&self) -> Result<Option<(String, usize)>, DbError> {
        let get = |key: &str| -> Result<Option<String>, DbError> {
            Ok(rusqlite::OptionalExtension::optional(self.conn.query_row(
                "SELECT value FROM semantic_metadata WHERE key = ?1",
                [key],
                |r| r.get(0),
            ))?)
        };
        match (get("model_id")?, get("dimensions")?, get("encoding")?) {
            (Some(model), Some(dims), Some(enc)) if enc == ENCODING => {
                Ok(dims.parse().ok().map(|d| (model, d)))
            }
            _ => Ok(None),
        }
    }

    fn reset(&mut self, model_id: &str, dimensions: usize) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM vectors", [])?;
        tx.execute("DELETE FROM semantic_metadata", [])?;
        for (k, v) in [
            ("model_id", model_id.to_string()),
            ("dimensions", dimensions.to_string()),
            ("encoding", ENCODING.into()),
        ] {
            tx.execute(
                "INSERT INTO semantic_metadata (key, value) VALUES (?1, ?2)",
                params![k, v],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    fn insert(&mut self, items: &[(i64, Vec<u8>, Vec<f32>)]) -> Result<(), DbError> {
        let tx = self.conn.transaction()?;
        {
            let mut stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO vectors (file_id, content_hash, vector) VALUES (?1, ?2, ?3)",
            )?;
            for (file_id, hash, vector) in items {
                stmt.execute(params![file_id, hash, quantize(vector)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    fn search(
        &self,
        query: &[f32],
        k: usize,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<Vec<(i64, Vec<u8>, f32)>, DbError> {
        let mut best: HashMap<i64, (Vec<u8>, f32)> = HashMap::new();
        let mut stmt = self
            .conn
            .prepare_cached("SELECT file_id, content_hash, vector FROM vectors")?;
        let mut rows = stmt.query([])?;
        let mut seen = 0usize;
        while let Some(row) = rows.next()? {
            seen += 1;
            if seen.is_multiple_of(4096) && cancelled() {
                return Ok(Vec::new());
            }
            let vector: Vec<u8> = row.get(2)?;
            let Some(score) = similarity(query, &vector) else {
                continue;
            };
            let file_id: i64 = row.get(0)?;
            if best.get(&file_id).is_none_or(|(_, s)| score > *s) {
                best.insert(file_id, (row.get(1)?, score));
            }
        }
        let mut out: Vec<(i64, Vec<u8>, f32)> =
            best.into_iter().map(|(f, (h, s))| (f, h, s)).collect();
        out.sort_by(|a, b| b.2.total_cmp(&a.2).then(a.0.cmp(&b.0)));
        out.truncate(k);
        Ok(out)
    }

    fn retain_files(&mut self, live: &HashSet<i64>) -> Result<usize, DbError> {
        let stored: Vec<i64> = self
            .conn
            .prepare("SELECT DISTINCT file_id FROM vectors")?
            .query_map([], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        let tx = self.conn.transaction()?;
        let mut removed = 0;
        for id in stored.into_iter().filter(|id| !live.contains(id)) {
            removed += tx.execute("DELETE FROM vectors WHERE file_id = ?1", [id])?;
        }
        tx.commit()?;
        Ok(removed)
    }

    fn count(&self) -> Result<u64, DbError> {
        Ok(self
            .conn
            .query_row("SELECT count(*) FROM vectors", [], |r| r.get::<_, i64>(0))?
            as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::embedding::normalize;
    use crate::security::secret::KEY_LEN;

    fn unit(seed: usize) -> Vec<f32> {
        let mut v: Vec<f32> = (0..384)
            .map(|i| (((i + 1) * (seed + 3)) % 17) as f32 - 8.0)
            .collect();
        normalize(&mut v);
        v
    }

    #[test]
    fn vectors_are_found_by_similarity_through_sqlcipher() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut index = SqliteVectors::open(dir.path(), &key, false).unwrap();
        assert_eq!(index.model().unwrap(), None);
        index.reset("model-a", 384).unwrap();
        assert_eq!(index.model().unwrap(), Some(("model-a".to_string(), 384)));
        index
            .insert(&[
                (1, vec![1], unit(1)),
                (1, vec![2], unit(2)),
                (2, vec![3], unit(3)),
                (3, vec![4], unit(4)),
            ])
            .unwrap();
        let found = index.search(&unit(3), 2, &|| false).unwrap();
        assert_eq!(found[0].0, 2, "the identical vector comes first");
        assert!((found[0].2 - 1.0).abs() < 0.02);
        assert_eq!(found.len(), 2);
        // One result per file: its best chunk.
        let all = index.search(&unit(1), 10, &|| false).unwrap();
        assert_eq!(all.iter().filter(|r| r.0 == 1).count(), 1);
        assert_eq!(all.iter().find(|r| r.0 == 1).unwrap().1, vec![1]);

        // Files no longer indexed lose their vectors.
        index.retain_files(&HashSet::from([1, 3])).unwrap();
        assert_eq!(index.count().unwrap(), 3);
        // A new model empties it.
        index.reset("model-b", 384).unwrap();
        assert_eq!(index.count().unwrap(), 0);
    }

    #[test]
    fn pictures_are_stored_found_and_forgotten_beside_text_vectors() {
        use crate::pictures::Picture;
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        let mut index = SqliteVectors::open(dir.path(), &key, false).unwrap();
        index.reset("text-model", 384).unwrap();
        index.insert(&[(9, vec![1], unit(1))]).unwrap();
        assert_eq!(index.picture_model().unwrap(), None);
        index.reset_pictures("picture-model").unwrap();
        assert_eq!(
            index.picture_model().unwrap().as_deref(),
            Some("picture-model")
        );
        index
            .insert_pictures(&[
                (
                    1,
                    Picture {
                        vector: unit(1),
                        baseline: 0.2,
                    },
                ),
                (
                    2,
                    Picture {
                        vector: unit(2),
                        baseline: 0.2,
                    },
                ),
                (
                    3,
                    Picture {
                        vector: unit(3),
                        baseline: 0.99,
                    },
                ),
            ])
            .unwrap();
        // The same point as picture 1 matches it by 1 - 0.2; picture 3's
        // high baseline (it matches everything) keeps it out.
        let found = index.search_pictures(&unit(1), 0.5, &|| false).unwrap();
        assert_eq!(found.iter().map(|f| f.0).collect::<Vec<_>>(), [1]);
        assert!((found[0].1 - 0.8).abs() < 0.02);
        // Only pictures of files still indexed are kept; text vectors aren't touched.
        index.retain_pictures(&HashSet::from([1, 2])).unwrap();
        assert_eq!(index.picture_count().unwrap(), 2);
        assert_eq!(index.count().unwrap(), 1);
        // A text-model change leaves pictures alone, and the other way round.
        index.reset("other-text-model", 384).unwrap();
        assert_eq!(index.picture_count().unwrap(), 2);
        index.reset_pictures("other-picture-model").unwrap();
        assert_eq!(index.picture_count().unwrap(), 0);
    }

    #[test]
    fn semantic_db_is_encrypted_with_a_derived_key() {
        let dir = tempfile::tempdir().unwrap();
        let key = SecretBytes::random(KEY_LEN).unwrap();
        drop(SqliteVectors::open(dir.path(), &key, false).unwrap());
        let bytes = std::fs::read(dir.path().join(SEMANTIC_DB)).unwrap();
        assert!(!bytes.starts_with(b"SQLite format 3"));
        // Not the metadata key itself, and stable.
        assert!(semantic_key(&key) != SecretBytes::from_vec(key.expose().to_vec()));
        assert!(semantic_key(&key) == semantic_key(&key));
        // Another key can't open it.
        let other = SecretBytes::random(KEY_LEN).unwrap();
        assert!(SqliteVectors::open(dir.path(), &other, false).is_err());
    }
}
