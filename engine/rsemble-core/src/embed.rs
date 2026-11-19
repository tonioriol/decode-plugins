//! The static embedding model (model2vec), loaded from a local folder.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::io;
use std::os::unix::fs::FileExt;
use std::path::Path;

use model2vec_rs::model::StaticModel;

/// A loaded model2vec model producing L2-normalized embeddings.
pub struct Embedder {
    model: StaticModel,
    dim: usize,
}

impl fmt::Debug for Embedder {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Embedder")
            .field("dim", &self.dim)
            .finish_non_exhaustive()
    }
}

impl Embedder {
    /// Loads the model in `folder` (`config.json`, `tokenizer.json`, `model.safetensors`). The tensors are read
    /// block by block straight into the model's tables rather than through a copy of the whole file: the
    /// allocator keeps a freed buffer of that size resident, which doubled the footprint of every process holding
    /// a model. The embedding table is kept as int8 rows with one scale each, a quarter of the float table.
    pub fn load(folder: &Path) -> Result<Self, String> {
        let config: serde_json::Value = serde_json::from_slice(
            &fs::read(folder.join("config.json")).map_err(|error| format!("config.json: {error}"))?,
        )
        .map_err(|error| format!("config.json: {error}"))?;
        let normalize = config
            .get("normalize")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        let tokenizer = tokenizers::Tokenizer::from_file(folder.join("tokenizer.json"))
            .map_err(|error| format!("tokenizer.json: {error}"))?;
        let tensors = Tensors::open(&folder.join("model.safetensors"))
            .map_err(|error| format!("model.safetensors: {error}"))?;
        let (embeddings, scales, cols) = tensors
            .quantized_rows(&["embeddings", "0", "embedding.weight"])
            .map_err(|error| format!("model.safetensors: {error}"))?;
        let rows = scales.len();
        let weights = tensors.floats("weights").ok();
        let mapping = tensors.indices("mapping").ok();
        // A token's row is scaled by the token's weight and by the row's quantization scale.
        let weights = match (weights, &mapping) {
            (Some(weights), Some(mapping)) => (weights.iter().zip(mapping))
                .map(|(weight, row)| weight * scales.get(*row).copied().unwrap_or(0.0))
                .collect(),
            (Some(weights), None) => weights
                .iter()
                .zip(&scales)
                .map(|(weight, scale)| weight * scale)
                .collect(),
            (None, _) => match &mapping {
                Some(mapping) => mapping
                    .iter()
                    .map(|row| scales.get(*row).copied().unwrap_or(0.0))
                    .collect(),
                None => scales,
            },
        };
        let model = StaticModel::from_owned_i8(
            tokenizer,
            embeddings,
            rows,
            cols,
            normalize,
            Some(weights),
            mapping,
        )
        .map_err(|error| format!("{error:#}"))?;
        let dim = model.dim();
        Ok(Self { model, dim })
    }

    /// Builds a model from the bytes of its `tokenizer.json`, `model.safetensors` and `config.json`.
    pub fn from_bytes(tokenizer: &[u8], model: &[u8], config: &[u8]) -> Result<Self, String> {
        let model =
            StaticModel::from_bytes(tokenizer, model, config, None).map_err(|error| format!("{error:#}"))?;
        let dim = model.dim();
        Ok(Self { model, dim })
    }

    /// The embedding dimension.
    pub fn dim(&self) -> usize {
        self.dim
    }

    /// One normalized row per text, concatenated (`texts.len() * dim` values). Texts are not truncated, as Semble
    /// encodes with `max_length=None`.
    pub fn embed(&self, texts: &[String]) -> Vec<f32> {
        self.embed_up_to(texts, None)
    }

    /// As [`Self::embed`], each text cut to its first `max_tokens` tokens (by characters, before tokenizing): the
    /// tokenizer's working memory grows with the text, so one megabyte-long line costs hundreds of megabytes.
    pub fn embed_truncated(&self, texts: &[String], max_tokens: usize) -> Vec<f32> {
        self.embed_up_to(texts, Some(max_tokens))
    }

    fn embed_up_to(&self, texts: &[String], max_tokens: Option<usize>) -> Vec<f32> {
        let mut rows = self.model.encode_flat_with_args(texts, max_tokens);
        for row in rows.chunks_mut(self.dim) {
            normalize(row);
        }
        rows
    }
}

/// A safetensors file, its tensors read on demand.
struct Tensors {
    file: fs::File,
    /// Where the tensor data starts: after the 8-byte header length and the header.
    base: u64,
    header: BTreeMap<String, serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct TensorInfo {
    dtype: String,
    shape: Vec<usize>,
    data_offsets: [u64; 2],
}

/// Bytes read per block: small enough that the block buffer is not a large allocation.
const BLOCK: usize = 1 << 15;

impl Tensors {
    fn open(path: &Path) -> io::Result<Self> {
        let file = fs::File::open(path)?;
        let mut length = [0; 8];
        file.read_exact_at(&mut length, 0)?;
        let length = u64::from_le_bytes(length);
        let mut header = vec![0; usize::try_from(length).map_err(io::Error::other)?];
        file.read_exact_at(&mut header, 8)?;
        let header = serde_json::from_slice(&header).map_err(io::Error::other)?;
        Ok(Self {
            file,
            base: 8 + length,
            header,
        })
    }

    /// The first of `names` present.
    fn info(&self, names: &[&str]) -> io::Result<TensorInfo> {
        let value = names
            .iter()
            .find_map(|name| self.header.get(*name))
            .ok_or_else(|| io::Error::other(format!("no tensor named {}", names.join(" or "))))?;
        serde_json::from_value(value.clone()).map_err(io::Error::other)
    }

    /// Decodes the tensor's elements of `SIZE` bytes, block by block.
    fn decode<const SIZE: usize, T>(
        &self,
        info: &TensorInfo,
        decode: impl Fn([u8; SIZE]) -> T,
    ) -> io::Result<Vec<T>> {
        let [start, end] = info.data_offsets;
        let length = usize::try_from(end.saturating_sub(start)).map_err(io::Error::other)?;
        let mut out = Vec::with_capacity(length / SIZE);
        let mut block = vec![0; BLOCK - BLOCK % SIZE];
        let mut done = 0;
        while done < length {
            let take = (length - done).min(block.len());
            let part = block.get_mut(..take).unwrap_or_default();
            self.file.read_exact_at(part, self.base + start + done as u64)?;
            out.extend(part.as_chunks::<SIZE>().0.iter().map(|bytes| decode(*bytes)));
            done += part.len();
        }
        Ok(out)
    }

    /// The tensor `name` as `f32` values.
    fn floats(&self, name: &str) -> io::Result<Vec<f32>> {
        let info = self.info(&[name])?;
        #[expect(
            clippy::cast_possible_truncation,
            reason = "model2vec keeps its tables in f32"
        )]
        match info.dtype.as_str() {
            "F32" => self.decode(&info, f32::from_le_bytes),
            "F64" => self.decode(&info, |bytes| f64::from_le_bytes(bytes) as f32),
            other => Err(io::Error::other(format!("unsupported dtype {other}"))),
        }
    }

    /// The first of `names` present, a 2-D `f32` table, as int8 rows with one scale each (`max |x| / 127`, so a
    /// quarter of the memory), and its column count.
    fn quantized_rows(&self, names: &[&str]) -> io::Result<(Vec<i8>, Vec<f32>, usize)> {
        let info = self.info(names)?;
        let (&[rows, cols], "F32") = (info.shape.as_slice(), info.dtype.as_str()) else {
            return Err(io::Error::other("the embeddings are not a 2-D F32 table"));
        };
        let [start, _] = info.data_offsets;
        let row_bytes = cols * 4;
        let per_block = (BLOCK / row_bytes).max(1);
        let mut data = Vec::with_capacity(rows * cols);
        let mut scales = Vec::with_capacity(rows);
        let mut block = vec![0; per_block * row_bytes];
        let mut row = vec![0.0_f32; cols];
        let mut done = 0;
        while done < rows {
            let take = (rows - done).min(per_block);
            let part = block.get_mut(..take * row_bytes).unwrap_or_default();
            self.file
                .read_exact_at(part, self.base + start + (done * row_bytes) as u64)?;
            for bytes in part.chunks_exact(row_bytes) {
                for (value, bytes) in row.iter_mut().zip(bytes.as_chunks::<4>().0) {
                    *value = f32::from_le_bytes(*bytes);
                }
                let scale = row.iter().fold(0.0_f32, |max, value| max.max(value.abs())) / 127.0;
                #[expect(clippy::cast_possible_truncation, reason = "clamped to the i8 range")]
                data.extend(row.iter().map(|value| {
                    if scale > 0.0 {
                        (value / scale).round().clamp(-127.0, 127.0) as i8
                    } else {
                        0
                    }
                }));
                scales.push(scale);
            }
            done += take;
        }
        Ok((data, scales, cols))
    }

    /// The tensor `name` as indices.
    fn indices(&self, name: &str) -> io::Result<Vec<usize>> {
        let info = self.info(&[name])?;
        let index = |value: i64| usize::try_from(value).unwrap_or(usize::MAX);
        match info.dtype.as_str() {
            "I64" => self.decode(&info, |bytes| index(i64::from_le_bytes(bytes))),
            "I32" => self.decode(&info, |bytes| index(i64::from(i32::from_le_bytes(bytes)))),
            other => Err(io::Error::other(format!("unsupported dtype {other}"))),
        }
    }
}

/// A tiny word-level model for tests: each word of `words` embeds along its own axis.
#[cfg(any(test, feature = "test-util"))]
pub fn test_embedder(words: &[&str]) -> Result<Embedder, String> {
    let mut vocab = serde_json::Map::new();
    vocab.insert("[UNK]".into(), 0.into());
    for (id, word) in words.iter().enumerate() {
        vocab.insert((*word).into(), (id + 1).into());
    }
    let tokenizer = serde_json::json!({
        "version": "1.0", "truncation": null, "padding": null, "added_tokens": [],
        "normalizer": {"type": "BertNormalizer", "clean_text": true, "handle_chinese_chars": true, "strip_accents": null, "lowercase": true},
        "pre_tokenizer": {"type": "BertPreTokenizer"}, "post_processor": null, "decoder": null,
        "model": {"type": "WordPiece", "unk_token": "[UNK]", "continuing_subword_prefix": "##", "max_input_chars_per_word": 100, "vocab": vocab},
    });
    let (rows, cols) = (words.len() + 1, words.len());
    let mut data = Vec::with_capacity(rows * cols * 4);
    for row in 0..rows {
        for col in 0..cols {
            data.extend_from_slice(&(if row == col + 1 { 1.0_f32 } else { 0.0 }).to_le_bytes());
        }
    }
    let mut header =
        serde_json::json!({"embeddings": {"dtype": "F32", "shape": [rows, cols], "data_offsets": [0, data.len()]}})
            .to_string();
    while !header.len().is_multiple_of(8) {
        header.push(' ');
    }
    let mut model = (header.len() as u64).to_le_bytes().to_vec();
    model.extend_from_slice(header.as_bytes());
    model.extend_from_slice(&data);
    Embedder::from_bytes(
        tokenizer.to_string().as_bytes(),
        &model,
        br#"{"normalize": true}"#,
    )
}

/// Scales `row` to unit length (vicinity's `normalize`); a zero row stays zero.
pub(crate) fn normalize(row: &mut [f32]) {
    let norm = row.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in row {
            *x /= norm;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Embedder;

    /// A safetensors file of `tensors`, each `(name, dtype, shape, little-endian bytes)`.
    fn safetensors(tensors: &[(&str, &str, Vec<usize>, Vec<u8>)]) -> Vec<u8> {
        let mut header = serde_json::Map::new();
        let mut data = Vec::new();
        for (name, dtype, shape, bytes) in tensors {
            header.insert(
                (*name).into(),
                serde_json::json!({"dtype": dtype, "shape": shape, "data_offsets": [data.len(), data.len() + bytes.len()]}),
            );
            data.extend_from_slice(bytes);
        }
        let header = serde_json::Value::Object(header).to_string();
        let mut file = (header.len() as u64).to_le_bytes().to_vec();
        file.extend_from_slice(header.as_bytes());
        file.extend_from_slice(&data);
        file
    }

    #[test]
    fn loading_from_the_folder_matches_the_float_model() {
        let words = ["zebra", "walrus", "otter", "heron", "bison", "lemur"];
        let mut vocab = serde_json::Map::new();
        vocab.insert("[UNK]".into(), 0.into());
        for (id, word) in (1..).zip(words) {
            vocab.insert(word.into(), id.into());
        }
        let tokenizer = serde_json::json!({
            "version": "1.0", "truncation": null, "padding": null, "added_tokens": [],
            "normalizer": {"type": "BertNormalizer", "clean_text": true, "handle_chinese_chars": true, "strip_accents": null, "lowercase": true},
            "pre_tokenizer": {"type": "BertPreTokenizer"}, "post_processor": null, "decoder": null,
            "model": {"type": "WordPiece", "unk_token": "[UNK]", "continuing_subword_prefix": "##", "max_input_chars_per_word": 100, "vocab": vocab},
        })
        .to_string();
        // Rows of different magnitudes, wider than one read block, shared by two tokens through the mapping, and weighted per token.
        let (rows, cols) = (4_usize, 3000_usize);
        let table: Vec<u8> = (0..rows * cols)
            .flat_map(|at| {
                ((((at * 7919 + at / cols * 131) % 1000) as f32 / 500.0 - 1.0)
                    * (1 + at / cols).pow(3) as f32)
                    .to_le_bytes()
            })
            .collect();
        let mapping: Vec<u8> = [0_i64, 1, 2, 3, 1, 2, 3]
            .iter()
            .flat_map(|row| row.to_le_bytes())
            .collect();
        let weights: Vec<u8> = [1.0_f64, 0.5, 2.0, 0.25, 1.5, 0.75, 3.0]
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .collect();
        let model = safetensors(&[
            ("embeddings", "F32", vec![rows, cols], table),
            ("mapping", "I64", vec![7], mapping),
            ("weights", "F64", vec![7], weights),
        ]);
        let config = br#"{"normalize": true}"#;
        let dir = std::env::temp_dir().join(format!("rsemble_embed_load_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tokenizer.json"), &tokenizer).unwrap();
        std::fs::write(dir.join("model.safetensors"), &model).unwrap();
        std::fs::write(dir.join("config.json"), config).unwrap();

        let loaded = Embedder::load(&dir).unwrap();
        let reference = Embedder::from_bytes(tokenizer.as_bytes(), &model, config).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let texts: Vec<String> = ["zebra lemur", "walrus otter heron", "bison zebra zebra"]
            .iter()
            .map(|text| (*text).to_owned())
            .collect();
        assert_eq!(loaded.dim(), cols);
        // The loaded table is int8, so the rows match the float model's up to rounding.
        let (loaded, reference) = (loaded.embed(&texts), reference.embed(&texts));
        for (loaded, reference) in loaded.chunks(cols).zip(reference.chunks(cols)) {
            let cosine: f32 = loaded.iter().zip(reference).map(|(a, b)| a * b).sum();
            assert!(cosine > 0.9999, "{cosine}");
        }
    }
}
