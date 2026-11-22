// src/blobs.rs
//
// Content-addressed blob store. A file is stored once, under the SHA-256
// of its bytes:
//
//   <coffer contents>/2c/2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824
//                     ^^ first two hex chars: 256 subdirectories, so
//                        no single directory holds every blob
//
// <coffer contents> is the 85-B_contents nest by default (see coffer.rs).
//
// Why by hash, not by name:
//   - The same book dropped in twice, or on two machines, is one blob.
//     Storing it again is a no-op.
//   - The name proves the content. Any copy, on any device, can be checked
//     by re-hashing it. Replication (phase 8) moves hashes, not paths.
//   - Blobs never change, so they are written read-only.
//
// A nest does not hold the blob. It holds a small .ref file saying which
// blob, plus the .meta.json sidecar. Renaming or moving a nest never moves
// the content.
//
// Writing is crash-safe: copy to a temp file in the same directory,
// hashing while copying; refuse if the hash differs from the one recorded
// at intake (the source changed in between); fsync; rename into place;
// fsync the directory. A crash leaves at worst a stray .tmp file, never a
// blob whose name lies about its bytes.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Where a blob lives, relative to the coffer's contents folder.
pub fn rel_path(hash: &str) -> PathBuf {
    PathBuf::from(&hash[..2]).join(hash)
}

/// Copy `src` into the blob store under `expected_hash`. Returns the blob's
/// path relative to `contents`. If the blob already exists, nothing
/// is copied.
pub async fn put(contents: &Path, src: &Path, expected_hash: &str) -> anyhow::Result<PathBuf> {
    check_hash_shape(expected_hash)?;
    let rel = rel_path(expected_hash);
    let dest = contents.join(&rel);
    if dest.exists() {
        tracing::debug!(blob = %rel.display(), "blob already stored");
        return Ok(rel);
    }

    let (src, dest, expected) = (src.to_path_buf(), dest.clone(), expected_hash.to_string());
    // Large files: plain blocking I/O on a blocking thread, not the
    // async executor.
    tokio::task::spawn_blocking(move || copy_verified(&src, &dest, &expected)).await??;

    tracing::info!(blob = %rel.display(), "blob stored");
    Ok(rel)
}

/// `put`, for callers outside the async runtime: the same verified copy,
/// on the current thread.
pub fn put_blocking(contents: &Path, src: &Path, expected_hash: &str) -> anyhow::Result<PathBuf> {
    check_hash_shape(expected_hash)?;
    let rel = rel_path(expected_hash);
    let dest = contents.join(&rel);
    if !dest.exists() {
        copy_verified(src, &dest, expected_hash)?;
    }
    Ok(rel)
}

/// Re-hash a stored blob and compare it with its name.
/// Ok(false) means the bytes on disk no longer match.
pub async fn verify(contents: &Path, hash: &str) -> anyhow::Result<bool> {
    check_hash_shape(hash)?;
    let path = contents.join(rel_path(hash));
    let hash = hash.to_string();
    tokio::task::spawn_blocking(move || {
        let mut file = File::open(&path)?;
        Ok(hash_reader(&mut file, None)? == hash)
    })
    .await?
}

fn copy_verified(src: &Path, dest: &Path, expected: &str) -> anyhow::Result<()> {
    let dir = dest.parent().expect("blob paths always have a parent");
    fs::create_dir_all(dir)?;

    let tmp = dir.join(format!(".{expected}.{}.tmp", uuid::Uuid::new_v4()));
    let result = (|| -> anyhow::Result<()> {
        let mut input = File::open(src)?;
        let mut output = File::create(&tmp)?;
        let actual = hash_reader(&mut input, Some(&mut output))?;
        if actual != expected {
            anyhow::bail!(
                "{} changed after it was received: its hash was {expected} at intake \
                 and is {actual} now. Nothing was stored; send it again",
                src.display()
            );
        }
        output.sync_all()?;
        set_read_only(&output)?;
        drop(output);
        // rename within one directory is atomic: dest either does not
        // exist or is complete.
        fs::rename(&tmp, dest)?;
        File::open(dir)?.sync_all()?;
        Ok(())
    })();

    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// SHA-256 of a file on disk, hex. Blocking.
pub fn hash_file(path: &Path) -> anyhow::Result<String> {
    hash_reader(&mut File::open(path)?, None)
}

/// True if `name` looks like a blob's file name (64 lowercase hex chars).
pub fn is_hash(name: &str) -> bool {
    check_hash_shape(name).is_ok()
}

/// SHA-256 of everything readable from `input`, optionally copying the
/// bytes to `copy_to` on the way. Hex, lowercase.
fn hash_reader(input: &mut impl Read, mut copy_to: Option<&mut File>) -> anyhow::Result<String> {
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        if let Some(out) = copy_to.as_deref_mut() {
            out.write_all(&buf[..n])?;
        }
    }
    Ok(hex::encode(hasher.finalize()))
}

fn check_hash_shape(hash: &str) -> anyhow::Result<()> {
    if hash.len() != 64 || !hash.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        anyhow::bail!("not a SHA-256 hex digest: {hash:?}");
    }
    Ok(())
}

#[cfg(unix)]
fn set_read_only(file: &File) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o444))
}

#[cfg(not(unix))]
fn set_read_only(file: &File) -> std::io::Result<()> {
    let mut perms = file.metadata()?.permissions();
    perms.set_readonly(true);
    file.set_permissions(perms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{Envelope, Payload, Source};

    // sha256("hello"), a published test vector.
    const HELLO: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";

    fn file_with(dir: &Path, name: &str, contents: &str) -> PathBuf {
        let p = dir.join(name);
        fs::write(&p, contents).unwrap();
        p
    }

    #[tokio::test]
    async fn a_file_is_stored_under_its_hash_with_a_two_char_fanout() {
        let lib = tempfile::tempdir().unwrap();
        let src = file_with(lib.path(), "book.txt", "hello");

        let rel = put(lib.path(), &src, HELLO).await.unwrap();

        assert_eq!(rel, PathBuf::from(format!("2c/{HELLO}")));
        assert_eq!(fs::read_to_string(lib.path().join(&rel)).unwrap(), "hello");
        assert!(verify(lib.path(), HELLO).await.unwrap());
        assert!(src.exists(), "the original is copied, not moved");
    }

    #[tokio::test]
    async fn the_same_bytes_from_two_places_make_one_blob() {
        let lib = tempfile::tempdir().unwrap();
        let a = file_with(lib.path(), "a.txt", "hello");
        let b = file_with(lib.path(), "b-renamed.txt", "hello");

        let ra = put(lib.path(), &a, HELLO).await.unwrap();
        let rb = put(lib.path(), &b, HELLO).await.unwrap();

        assert_eq!(ra, rb);
        let blobs: Vec<_> = fs::read_dir(lib.path().join("2c")).unwrap().collect();
        assert_eq!(blobs.len(), 1, "no temp files left, no second copy");
    }

    #[tokio::test]
    async fn a_file_that_changed_since_intake_is_refused_and_nothing_is_left_behind() {
        let lib = tempfile::tempdir().unwrap();
        let src = file_with(lib.path(), "book.txt", "hello, edited");

        let err = put(lib.path(), &src, HELLO).await.unwrap_err();

        assert!(err.to_string().contains("changed after it was received"), "{err}");
        assert!(!lib.path().join(rel_path(HELLO)).exists());
        let leftovers = fs::read_dir(lib.path().join("2c")).map(|d| d.count()).unwrap_or(0);
        assert_eq!(leftovers, 0, "temp file cleaned up");
    }

    #[tokio::test]
    async fn stored_blobs_are_read_only_and_tampering_is_detected() {
        let lib = tempfile::tempdir().unwrap();
        let src = file_with(lib.path(), "book.txt", "hello");
        let rel = put(lib.path(), &src, HELLO).await.unwrap();
        let blob = lib.path().join(rel);

        assert!(fs::metadata(&blob).unwrap().permissions().readonly());

        // Force a change the way only a deliberate edit could.
        let mut perms = fs::metadata(&blob).unwrap().permissions();
        #[allow(clippy::permissions_set_readonly_false)]
        perms.set_readonly(false);
        fs::set_permissions(&blob, perms).unwrap();
        fs::write(&blob, "tampered").unwrap();
        assert!(!verify(lib.path(), HELLO).await.unwrap());
    }

    #[tokio::test]
    async fn a_malformed_hash_is_rejected_before_touching_disk() {
        let lib = tempfile::tempdir().unwrap();
        let src = file_with(lib.path(), "x", "hello");
        assert!(put(lib.path(), &src, "../../etc/passwd").await.is_err());
        assert!(put(lib.path(), &src, &HELLO.to_uppercase()).await.is_err());
        let dirs = fs::read_dir(lib.path()).unwrap().filter(|e| e.as_ref().unwrap().path().is_dir()).count();
        assert_eq!(dirs, 0, "nothing created in the store");
    }

    // Through store::store, the way a Store hop does it.
    #[tokio::test]
    async fn storing_a_file_envelope_writes_a_ref_to_its_blob() {
        let vault = tempfile::tempdir().unwrap();
        crate::coffer::init(vault.path()).unwrap();
        let src = file_with(vault.path(), "intake-book.txt", "hello");
        let envelope = Envelope::new(Source::Manual, "file/book", Payload::FilePath(src.clone()), Some("52-H"));
        let nest_dir = vault.path().join("52_media/52-H_books");

        let written = crate::store::store(&envelope, vault.path(), &nest_dir).await.unwrap();

        assert!(written.content_path.starts_with(nest_dir.join("entries")));
        let reference = fs::read_to_string(&written.content_path).unwrap();
        assert_eq!(
            reference,
            format!("ref: sha256:{HELLO}\noriginal: {}\n", src.display())
        );
        let meta: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&written.meta_path).unwrap()).unwrap();
        assert_eq!(meta["content_hash"], HELLO);
        let contents = crate::coffer::resolve(vault.path()).unwrap().contents;
        assert!(verify(&contents, HELLO).await.unwrap());
    }
}

#[cfg(test)]
mod store_hop_tests {
    use crate::envelope::{Envelope, Payload, Source};
    use crate::route::{Hop, Route, Stop};
    use std::fs;

    // A nest nested inside an area folder, the way Orchard is laid out.
    // Before the fix, Store wrote to <vault>/52-H_books/entries, a folder
    // derived from id + name, instead of the nest's real folder.
    #[tokio::test]
    async fn the_store_hop_writes_into_the_nests_real_folder() {
        let vault = tempfile::tempdir().unwrap();
        crate::coffer::init(vault.path()).unwrap();
        let nest_dir = vault.path().join("52_media/52-H_books");
        fs::create_dir_all(&nest_dir).unwrap();
        fs::write(nest_dir.join("nest"), "id = \"52-H\"\nname = \"books\"\nkind = \"leaf\"\n").unwrap();
        let src = vault.path().join("intake.txt");
        fs::write(&src, "hello").unwrap();

        let envelope = Envelope::new(Source::Manual, "file/book", Payload::FilePath(src), Some("52-H"));
        let mut route = Route::new("t", vec![Stop { nest_id: "52-H".into(), hops: vec![Hop::Store] }]);
        route.run(vault.path(), Some(&envelope)).await.unwrap();

        let refs: Vec<_> = fs::read_dir(nest_dir.join("entries")).unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "ref"))
            .collect();
        assert_eq!(refs.len(), 1);
        assert!(!vault.path().join("52-H_books").exists(), "no stray folder at the vault root");
        assert!(vault.path().join("85_coffer/85-B_contents/2c").exists(), "bytes go to the coffer");
    }
}
