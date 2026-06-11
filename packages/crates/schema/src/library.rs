//! [`SampleLibrary`] — the top-level document the editor saves and the player
//! loads. It holds every [`Sample`], the shared [`AssetTable`], the context
//! [`Listener`], and which sample is the `root` (the one the player wires to
//! the hardware `ctx.destination`).

use serde::{Deserialize, Serialize};

use crate::asset::AssetTable;
use crate::ids::SampleId;
use crate::param::AudioParam;
use crate::sample::Sample;

/// On-disk schema version. Bumped on breaking layout changes; lets a loader
/// migrate or reject old documents.
pub const SCHEMA_VERSION: u32 = 1;

/// A complete authored audio document.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SampleLibrary {
    #[serde(default = "default_version")]
    pub version: u32,
    /// Every sample, including both leaf patches and composite ones that
    /// reference others via [`SampleRef`](crate::SampleRef).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<Sample>,
    #[serde(default)]
    pub assets: AssetTable,
    /// The entry sample whose outlets the player connects to `ctx.destination`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root: Option<SampleId>,
    /// The context-level spatial listener that [`PannerNode`](crate::PannerNode)s
    /// are positioned against. `None` leaves it at the platform default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listener: Option<Listener>,
}

impl Default for SampleLibrary {
    fn default() -> Self {
        Self {
            version: SCHEMA_VERSION,
            samples: Vec::new(),
            assets: AssetTable::default(),
            root: None,
            listener: None,
        }
    }
}

impl SampleLibrary {
    /// Look up a sample by id.
    pub fn sample(&self, id: SampleId) -> Option<&Sample> {
        self.samples.iter().find(|s| s.id == id)
    }

    /// Mutable lookup of a sample by id.
    pub fn sample_mut(&mut self, id: SampleId) -> Option<&mut Sample> {
        self.samples.iter_mut().find(|s| s.id == id)
    }
}

fn default_version() -> u32 {
    SCHEMA_VERSION
}

/// The single per-context `AudioListener`, modeled as automatable params so it
/// can move over time alongside the [`PannerNode`](crate::PannerNode)s.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Listener {
    pub position_x: AudioParam,
    pub position_y: AudioParam,
    pub position_z: AudioParam,
    pub forward_x: AudioParam,
    pub forward_y: AudioParam,
    pub forward_z: AudioParam,
    pub up_x: AudioParam,
    pub up_y: AudioParam,
    pub up_z: AudioParam,
}

impl Default for Listener {
    fn default() -> Self {
        Self {
            position_x: AudioParam::new(0.0),
            position_y: AudioParam::new(0.0),
            position_z: AudioParam::new(0.0),
            forward_x: AudioParam::new(0.0),
            forward_y: AudioParam::new(0.0),
            forward_z: AudioParam::new(-1.0),
            up_x: AudioParam::new(0.0),
            up_y: AudioParam::new(1.0),
            up_z: AudioParam::new(0.0),
        }
    }
}
