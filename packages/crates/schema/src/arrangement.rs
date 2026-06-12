//! An **arrangement**: a DAW-style audio timeline. Tracks hold [`Clip`]s, and
//! each clip plays a rendered ("bounced") audio buffer of a [`Sample`](crate::Sample)
//! Sound. Times are in **seconds** (not beats) so the audio never desyncs when the
//! tempo grid changes; `bpm` only drives the ruler + snapping.
//!
//! Arrangements carry no synthesis of their own — they schedule pre-rendered audio
//! buffers (see a sample's [`Bounce`](crate::Bounce)). The editor compiles them to
//! plain `AudioBufferSource` playback.

use serde::{Deserialize, Serialize};

use crate::ids::{AssetId, SampleId};

/// A whole arrangement: a tempo (for the grid), a length in seconds, and tracks.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Arrangement {
    /// Tempo in BPM — used only to draw the bar/beat grid and snap edits.
    pub bpm: f64,
    /// Timeline length in seconds (the ruler extent).
    pub length_secs: f64,
    /// Optional loop/export start marker, in seconds. When both markers are set
    /// (and `end > start`), playback loops this region and export renders exactly
    /// it; otherwise both span the whole timeline (`0..length_secs`). Toggled on
    /// and off from the Arrange-view ruler.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_start: Option<f64>,
    /// Optional loop/export end marker, in seconds. See [`loop_start`](Self::loop_start).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loop_end: Option<f64>,
    /// The tracks, top to bottom.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<ArrTrack>,
    /// Named regions of the timeline ("intro", "main", "outro") — annotation
    /// metadata for navigation/authoring; playback never interprets them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<ArrSection>,
}

/// A named timeline region (annotation only — see [`Arrangement::sections`]).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrSection {
    pub name: String,
    /// Region start, in seconds.
    pub start: f64,
    /// Region end, in seconds (exclusive; `end > start`).
    pub end: f64,
}

impl Default for Arrangement {
    fn default() -> Self {
        Self {
            bpm: 120.0,
            length_secs: 32.0,
            loop_start: None,
            loop_end: None,
            tracks: Vec::new(),
            sections: Vec::new(),
        }
    }
}

impl Arrangement {
    /// An arrangement is "empty" (and skipped on save) when it has no tracks.
    pub fn is_empty(&self) -> bool {
        self.tracks.is_empty()
    }

    /// Whether explicit loop/export markers are active (both set, `end > start`).
    pub fn has_markers(&self) -> bool {
        matches!((self.loop_start, self.loop_end), (Some(s), Some(e)) if e > s)
    }

    /// The effective `(start, end)` window in seconds for playback-loop and
    /// export: the markers when active, else the whole timeline `(0, length_secs)`.
    pub fn range(&self) -> (f64, f64) {
        match (self.loop_start, self.loop_end) {
            (Some(s), Some(e)) if e > s => (s.max(0.0), e),
            _ => (0.0, self.length_secs),
        }
    }
}

/// One horizontal lane of audio clips.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArrTrack {
    /// Display name (e.g. "Bass", "Drums").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// Linear track gain (1.0 = unity).
    pub gain: f32,
    /// Muted tracks are skipped at play time.
    #[serde(default, skip_serializing_if = "is_false")]
    pub mute: bool,
    /// Soloed: if any track is soloed, only soloed tracks play.
    #[serde(default, skip_serializing_if = "is_false")]
    pub solo: bool,
    /// The clips placed on this track's timeline.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub clips: Vec<Clip>,
}

impl Default for ArrTrack {
    fn default() -> Self {
        Self {
            name: String::new(),
            gain: 1.0,
            mute: false,
            solo: false,
            clips: Vec::new(),
        }
    }
}

/// An audio clip placed on a track: an instance of a Sound's bounced buffer. The
/// waveform and audio come from `source`'s [`Bounce`](crate::Bounce); an un-bounced
/// source can't play. Blade/trim produce clips that share a source with different
/// `offset` / `length`.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Clip {
    /// Start position on the timeline, in seconds.
    pub start: f64,
    /// How long the clip occupies the timeline, in seconds.
    pub length: f64,
    /// The Sound whose bounce this clip plays (for audio, waveform, re-bounce).
    pub source: SampleId,
    /// Start offset into the bounced buffer, in seconds (trim head / scrub).
    #[serde(default)]
    pub offset: f64,
    /// Linear clip gain.
    pub gain: f32,
    /// Repeat the buffer to fill `length` instead of stopping when it ends.
    #[serde(default, skip_serializing_if = "is_false")]
    pub looping: bool,
    /// Playback speed (1.0 = normal). Above 1 plays faster and higher-pitched,
    /// below 1 slower and lower (time-stretch via playback rate; pitch is not
    /// preserved). The clip consumes `length * speed` buffer seconds over
    /// `length` seconds on the grid.
    #[serde(default = "one_f32", skip_serializing_if = "is_one")]
    pub speed: f32,
    /// Display name shown on the clip block.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
}

impl Default for Clip {
    fn default() -> Self {
        Self {
            start: 0.0,
            length: 0.0,
            source: SampleId::default(),
            offset: 0.0,
            gain: 1.0,
            looping: false,
            speed: 1.0,
            name: String::new(),
        }
    }
}

fn one_f32() -> f32 {
    1.0
}
fn is_one(v: &f32) -> bool {
    (*v - 1.0).abs() < 1e-6
}

/// A sample's rendered audio: the buffer asset it bounced to, plus a hash of the
/// source graph at bounce time so the editor can flag the bounce as **dirty** when
/// the Sound is edited afterward.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Bounce {
    /// The rendered [`BufferAsset`](crate::BufferAsset) id.
    pub asset: AssetId,
    /// Hash of the source graph when bounced (see editor dirty-check).
    pub source_hash: u64,
}

fn is_false(b: &bool) -> bool {
    !*b
}
