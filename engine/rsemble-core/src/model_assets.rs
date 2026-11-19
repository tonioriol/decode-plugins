//! Model assets shared by the rsemble CLI and Decode's code and notes search.

use std::path::{Path, PathBuf};

/// The Hugging Face repository for the pinned code embeddings.
pub const CODE_MODEL_REPO: &str = "minishlab/potion-code-16M-v2";
/// The Hugging Face revision used by the Semble Python benchmark.
pub const CODE_MODEL_REVISION: &str = "e9d2a44ca6a05ac6685f3b23709ea57eb7352d5b";
/// The model2vec inputs and their SHA-256 checksums at [`CODE_MODEL_REVISION`].
pub const CODE_MODEL_FILES: &[(&str, &str)] = &[
    (
        "config.json",
        "148e5691a6fcc553437156859701fba017a1ba5d340b170f17e0f3668fb861a7",
    ),
    (
        "tokenizer.json",
        "107bbdcbad4bff1d299b7a4c3a2fb17c52890688b7dd0e4c9deab79d3c4f3d45",
    ),
    (
        "model.safetensors",
        "75cf7a6c2171b230ad19b1e7d8e0b1aee86da5a02af8e7cacedd9921d227623c",
    ),
];

/// The pinned download URL prefix, before the filename.
pub fn code_model_base_url() -> String {
    format!("https://huggingface.co/{CODE_MODEL_REPO}/resolve/{CODE_MODEL_REVISION}")
}

/// Where the verified model is published: `<storage>/semble-model-<revision>`.
pub fn code_model_dir(storage: &Path) -> PathBuf {
    storage.join(format!("semble-model-{CODE_MODEL_REVISION}"))
}
