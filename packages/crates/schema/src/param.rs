//! [`AudioParam`] modeled at full WebAudio fidelity: an intrinsic value plus an
//! ordered list of scheduled [`AutomationEvent`]s. *Modulation* — driving a
//! param by connecting another node's output into it — is not stored here; it
//! lives in the graph's [`Connection`](crate::Connection) list (a
//! [`ConnectionSink::NodeParam`](crate::ConnectionSink) target), so the full
//! signal topology stays in one place.

use serde::{Deserialize, Serialize};

use crate::enums::AutomationRate;

/// An automatable scalar parameter (e.g. oscillator frequency, gain).
///
/// Field order matters for TOML: the scalar `value`/`automation_rate` are
/// declared before the `automation` array-of-tables, since TOML requires a
/// table's scalar keys to be emitted ahead of any sub-tables.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AudioParam {
    /// The intrinsic base value (`AudioParam.value` / `setValueAtTime` floor).
    /// The effective value at render time is this plus any connected modulation
    /// and the result of applying `automation`.
    pub value: f32,

    /// `a-rate` vs `k-rate`. `None` leaves the node's per-param default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation_rate: Option<AutomationRate>,

    /// Scheduled automation, in the order it should be applied. Times are `f64`
    /// seconds relative to the owning sample's trigger (note-on = `0.0`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub automation: Vec<AutomationEvent>,
}

impl AudioParam {
    /// A param pinned to `value` with no automation.
    pub fn new(value: f32) -> Self {
        Self {
            value,
            automation_rate: None,
            automation: Vec::new(),
        }
    }
}

impl Default for AudioParam {
    fn default() -> Self {
        Self::new(0.0)
    }
}

/// One scheduled change on an [`AudioParam`] timeline. Mirrors the
/// `AudioParam` scheduling methods one-to-one. Adjacently tagged (`event` +
/// `args`) so it round-trips cleanly through TOML.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "event", content = "args")]
pub enum AutomationEvent {
    /// `setValueAtTime(value, time)`.
    SetValue { value: f32, time: f64 },
    /// `linearRampToValueAtTime(value, time)`.
    LinearRamp { value: f32, time: f64 },
    /// `exponentialRampToValueAtTime(value, time)`. `value` must be > 0.
    ExponentialRamp { value: f32, time: f64 },
    /// `setTargetAtTime(target, startTime, timeConstant)`.
    SetTarget {
        target: f32,
        start_time: f64,
        time_constant: f64,
    },
    /// `setValueCurveAtTime(values, startTime, duration)`.
    SetValueCurve {
        values: Vec<f32>,
        start_time: f64,
        duration: f64,
    },
    /// `cancelScheduledValues(time)`.
    CancelScheduled { time: f64 },
    /// `cancelAndHoldAtTime(time)`.
    CancelAndHold { time: f64 },
}

/// Names an automatable parameter on a node. For built-in nodes these are the
/// WebAudio param names (`"frequency"`, `"gain"`, `"Q"`, …); for
/// [`AudioWorkletNode`](crate::AudioWorkletNode)s and referenced-sample macros
/// it's the author-defined name. Validated against the node it targets.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ParamId(pub String);

impl ParamId {
    pub const FREQUENCY: &'static str = "frequency";
    pub const DETUNE: &'static str = "detune";
    pub const GAIN: &'static str = "gain";
    pub const Q: &'static str = "Q";
    pub const PLAYBACK_RATE: &'static str = "playbackRate";
    pub const OFFSET: &'static str = "offset";
    pub const DELAY_TIME: &'static str = "delayTime";
    pub const THRESHOLD: &'static str = "threshold";
    pub const KNEE: &'static str = "knee";
    pub const RATIO: &'static str = "ratio";
    pub const ATTACK: &'static str = "attack";
    pub const RELEASE: &'static str = "release";
    pub const PAN: &'static str = "pan";
    pub const POSITION_X: &'static str = "positionX";
    pub const POSITION_Y: &'static str = "positionY";
    pub const POSITION_Z: &'static str = "positionZ";
    pub const ORIENTATION_X: &'static str = "orientationX";
    pub const ORIENTATION_Y: &'static str = "orientationY";
    pub const ORIENTATION_Z: &'static str = "orientationZ";
}

impl<T: Into<String>> From<T> for ParamId {
    fn from(s: T) -> Self {
        Self(s.into())
    }
}
