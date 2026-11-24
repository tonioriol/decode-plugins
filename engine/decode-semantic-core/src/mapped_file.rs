//! The sole unsafe boundary: read-only maps of completed search indexes, never mutable reranker files.

use std::fs::File;
use std::io;

use memmap2::Mmap;

/// A read-only mapping of a completed, immutable search index file.
#[derive(Debug)]
pub struct MappedFile(Option<Mmap>);

impl MappedFile {
    /// Maps the already-opened inode, checking its expected length. Its owner must publish replacements by rename,
    /// never overwrite or truncate this inode while a mapping exists. Mutable reranker files are not eligible.
    #[allow(
        unsafe_code,
        reason = "completed search index files remain immutable for the map's lifetime"
    )]
    pub fn open(file: &File, expected_bytes: u64) -> io::Result<Self> {
        if file.metadata()?.len() != expected_bytes {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "a mapped search index file of the wrong size",
            ));
        }
        if expected_bytes == 0 {
            return Ok(Self(None));
        }
        // SAFETY: SegmentWriter closes completed vectors.bin before Segment opens it; merges write new files
        // and unlink old segments. Code/memory writers (including decode_semantic::Engine::persist) write
        // temporary files and rename them onto the published path. None overwrite or truncate a mapped inode.
        // The opened handle pins that inode, even if its path is concurrently replaced. Segment/SourceIndex's
        // shared base owns the read-only map for every snapshot; Unix retains its pages after unlink. Mutable
        // reranker files are deliberately excluded. Bytes are decoded safely, never cast to aligned f32 rows.
        Ok(Self(Some(unsafe { Mmap::map(file)? })))
    }

    /// Asks the kernel to read the mapped pages in ahead of use, without waiting for them.
    pub fn will_need(&self) {
        if let Some(map) = &self.0 {
            // Only a hint: when refused, the pages come in as they are read.
            drop(map.advise(memmap2::Advice::WillNeed));
        }
    }

    /// The mapped bytes, including an empty file's empty slice.
    pub fn bytes(&self) -> &[u8] {
        self.0.as_deref().unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::MappedFile;
    use std::fs::File;

    #[test]
    fn mapped_rows_survive_unlink_and_replacement_of_the_segment_file() {
        let dir = std::env::temp_dir().join(format!("decode_semantic_mapped_rows_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vectors.bin");
        let rows: Vec<u8> = (0..16385).map(|at| (at * 37 % 251) as u8).collect();
        std::fs::write(&path, &rows).unwrap();
        let mapped = MappedFile::open(&File::open(&path).unwrap(), rows.len() as u64).unwrap();
        std::fs::remove_file(&path).unwrap();
        let replacement: Vec<u8> = rows.iter().map(|byte| byte ^ 0xff).collect();
        std::fs::write(&path, &replacement).unwrap();
        let next = MappedFile::open(&File::open(&path).unwrap(), replacement.len() as u64).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(mapped.bytes(), rows);
        assert_eq!(next.bytes(), replacement);
    }

    #[test]
    fn empty_embedding_tables_open_but_truncated_tables_are_rejected() {
        let path = std::env::temp_dir().join(format!("decode_semantic_mapped_empty_{}", std::process::id()));
        std::fs::write(&path, []).unwrap();
        let empty = MappedFile::open(&File::open(&path).unwrap(), 0).unwrap();
        assert!(empty.bytes().is_empty());
        assert_eq!(
            MappedFile::open(&File::open(&path).unwrap(), 5)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::InvalidData
        );
        std::fs::remove_file(&path).unwrap();
    }
}
