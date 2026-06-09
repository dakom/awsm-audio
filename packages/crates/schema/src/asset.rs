//! Shared, by-reference assets. PCM buffers and WASM modules are bulky and
//! frequently reused, so nodes reference them by [`AssetId`] and the bytes live
//! once in the [`AssetTable`]. (Wave-shaper curves and custom-oscillator waves
//! are small + generated from a few parameters, so they live inline on the node
//! instead of as shared assets.)

use serde::{Deserialize, Serialize};

use crate::ids::AssetId;

/// Every shared asset a [`SampleLibrary`](crate::SampleLibrary) draws on.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AssetTable {
    /// Decoded/decodable audio, referenced by
    /// [`AudioBufferSourceNode`](crate::AudioBufferSourceNode) (`buffer`) and
    /// [`ConvolverNode`](crate::ConvolverNode) (impulse response).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub buffers: Vec<BufferAsset>,

    /// WASM DSP modules, referenced by
    /// [`AudioWorkletNode`](crate::AudioWorkletNode) (`module`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub wasm_modules: Vec<WasmAsset>,
}

/// A WASM DSP module for an [`AudioWorkletNode`]: either a URL the player
/// fetches, or inline base64-encoded bytes (keeps a project self-contained).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WasmAsset {
    pub id: AssetId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub source: WasmSource,
}

/// Where a [`WasmAsset`]'s bytes come from. Adjacently tagged (`kind` + `data`)
/// so it round-trips cleanly through TOML.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum WasmSource {
    /// Fetched at load time.
    Url(String),
    /// Inline, base64-encoded `.wasm` bytes (standard base64, with padding).
    Base64(String),
    /// A project-relative path to the `.wasm` file (e.g. `assets/<id>.wasm`). Used
    /// only by the directory-based project format; the editor reads the file and
    /// rehydrates it to inline bytes in memory.
    Path(String),
}

/// An audio buffer: either a URL the player fetches + `decodeAudioData`s, or
/// inline raw PCM (one `Vec<f32>` per channel).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BufferAsset {
    pub id: AssetId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub source: AudioSource,
}

/// Where a [`BufferAsset`]'s samples come from. Adjacently tagged (`kind` +
/// `data`) so it round-trips cleanly through TOML.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "data")]
pub enum AudioSource {
    /// Fetched and decoded at load time.
    Url(String),
    /// Inline, base64-encoded *encoded* audio file (mp3/wav/flac/…), re-decoded
    /// at load. Compact + lossless — how the editor embeds a loaded clip on Save.
    Encoded(String),
    /// Inline, already-decoded PCM.
    Pcm {
        sample_rate: f32,
        /// One channel per outer entry; all inner lengths should match.
        channels: Vec<Vec<f32>>,
    },
    /// A project-relative path to the audio file (e.g. `assets/<id>.wav`). Used
    /// only by the directory-based project format; the editor reads the file and
    /// rehydrates it to inline bytes in memory.
    Path(String),
}
