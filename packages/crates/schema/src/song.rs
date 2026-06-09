//! A **song**: a tempo plus a set of note [`Track`]s, the data a
//! [`MidiSongNode`](crate::MidiSongNode) sequences. Times are *musical* (beats /
//! quarter notes), not seconds, so the tempo can change without rewriting every
//! event, and a piano-roll editor can think in bars. The player converts beats
//! to seconds at play time from [`Song::bpm`].
//!
//! A song carries no audio. It triggers instruments (other samples) wired to the
//! sequencer node — see [`MidiSongNode`](crate::MidiSongNode).

use serde::{Deserialize, Serialize};

/// One note: a pitch + velocity placed on the timeline, with a duration.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteEvent {
    /// Start time, in beats (quarter notes) from the song's beginning.
    pub start: f64,
    /// Duration, in beats. Note-off fires `start + length` beats in.
    pub length: f64,
    /// MIDI note number (0..=127). 60 = middle C = the instrument's unison
    /// pitch; the sequencer transposes the instrument by `note - 60` semitones.
    /// In a drum-mode sequencer this picks a percussion *sound* instead of a
    /// pitch (see [`SequencerMode`](crate::SequencerMode)).
    pub note: u8,
    /// MIDI velocity (1..=127), scaling the note's amplitude.
    pub velocity: u8,
}

/// A stream of notes — usually one MIDI track / channel. Played polyphonically
/// by whichever instrument a [`Part`](crate::Part) binds it to.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Track {
    /// Display name (e.g. the SMF track name, or "Piano").
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    /// The source MIDI channel (0..=15), informational. (Whether notes are
    /// pitches or drum sounds is a whole-node choice — see
    /// [`SequencerMode`](crate::SequencerMode) — not a per-track flag.)
    #[serde(default)]
    pub channel: u8,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<NoteEvent>,
}

impl Track {
    /// End of the last note, in beats (0 if empty).
    pub fn duration_beats(&self) -> f64 {
        self.events
            .iter()
            .map(|e| e.start + e.length)
            .fold(0.0, f64::max)
    }
}

/// A tempo change at a beat position (an entry in a [`Song`]'s tempo map).
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TempoChange {
    /// Beat position the new tempo takes effect (0 = song start).
    pub beat: f64,
    /// Tempo in beats per minute from this beat onward.
    pub bpm: f64,
}

/// A tempo and a set of note tracks.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Song {
    /// Base tempo in beats per minute (the tempo before any [`tempo_map`] change,
    /// and the only tempo when the map is empty).
    ///
    /// [`tempo_map`]: Song::tempo_map
    pub bpm: f64,
    /// Optional mid-song tempo changes (from a `.mid` tempo map), sorted by beat.
    /// Empty means a constant [`bpm`](Song::bpm).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tempo_map: Vec<TempoChange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracks: Vec<Track>,
}

impl Default for Song {
    fn default() -> Self {
        Self {
            bpm: 120.0,
            tempo_map: Vec::new(),
            tracks: Vec::new(),
        }
    }
}

/// The General-MIDI percussion name for a drum-channel note number, or `None`
/// outside the GM drum range. Used to label notes on a drum track.
pub fn gm_drum_name(note: u8) -> Option<&'static str> {
    Some(match note {
        35 => "Acoustic Bass Drum",
        36 => "Bass Drum 1",
        37 => "Side Stick",
        38 => "Acoustic Snare",
        39 => "Hand Clap",
        40 => "Electric Snare",
        41 => "Low Floor Tom",
        42 => "Closed Hi-Hat",
        43 => "High Floor Tom",
        44 => "Pedal Hi-Hat",
        45 => "Low Tom",
        46 => "Open Hi-Hat",
        47 => "Low-Mid Tom",
        48 => "Hi-Mid Tom",
        49 => "Crash Cymbal 1",
        50 => "High Tom",
        51 => "Ride Cymbal 1",
        52 => "Chinese Cymbal",
        53 => "Ride Bell",
        54 => "Tambourine",
        55 => "Splash Cymbal",
        56 => "Cowbell",
        57 => "Crash Cymbal 2",
        59 => "Ride Cymbal 2",
        60 => "Hi Bongo",
        61 => "Low Bongo",
        62 => "Mute Hi Conga",
        63 => "Open Hi Conga",
        64 => "Low Conga",
        65 => "High Timbale",
        66 => "Low Timbale",
        67 => "High Agogo",
        68 => "Low Agogo",
        69 => "Cabasa",
        70 => "Maracas",
        75 => "Claves",
        _ => return None,
    })
}

impl Song {
    /// Length of the longest track, in beats.
    pub fn duration_beats(&self) -> f64 {
        self.tracks
            .iter()
            .map(Track::duration_beats)
            .fold(0.0, f64::max)
    }

    /// Convert a beat position to seconds, honoring the tempo map (piecewise
    /// across tempo changes). Constant-tempo when the map is empty.
    pub fn beats_to_secs(&self, beats: f64) -> f64 {
        if self.tempo_map.is_empty() {
            return beats * 60.0 / self.bpm.max(1.0);
        }
        let mut secs = 0.0;
        let mut prev_beat = 0.0;
        let mut cur_bpm = self.bpm.max(1.0);
        for ch in &self.tempo_map {
            if ch.beat <= 0.0 {
                cur_bpm = ch.bpm.max(1.0); // a change at/before 0 sets the base
                continue;
            }
            if ch.beat >= beats {
                break;
            }
            secs += (ch.beat - prev_beat) * 60.0 / cur_bpm;
            prev_beat = ch.beat;
            cur_bpm = ch.bpm.max(1.0);
        }
        secs + (beats - prev_beat) * 60.0 / cur_bpm
    }

    /// Convert seconds to a beat position — the inverse of [`beats_to_secs`],
    /// honoring the tempo map. Used for the playback playhead.
    ///
    /// [`beats_to_secs`]: Song::beats_to_secs
    pub fn secs_to_beats(&self, secs: f64) -> f64 {
        if self.tempo_map.is_empty() {
            return secs * self.bpm.max(1.0) / 60.0;
        }
        let mut elapsed = 0.0;
        let mut prev_beat = 0.0;
        let mut cur_bpm = self.bpm.max(1.0);
        for ch in &self.tempo_map {
            if ch.beat <= 0.0 {
                cur_bpm = ch.bpm.max(1.0);
                continue;
            }
            let seg = (ch.beat - prev_beat) * 60.0 / cur_bpm;
            if elapsed + seg >= secs {
                break;
            }
            elapsed += seg;
            prev_beat = ch.beat;
            cur_bpm = ch.bpm.max(1.0);
        }
        prev_beat + (secs - elapsed) * cur_bpm / 60.0
    }
}

// ======================================================================
// Standard MIDI File (.mid) import
// ======================================================================

use std::collections::HashMap;

/// Notes accumulated for one MIDI channel within one SMF track.
#[derive(Default)]
struct ChannelNotes {
    /// Currently-sounding notes: key → (start tick, velocity).
    open: HashMap<u8, (u64, u8)>,
    events: Vec<NoteEvent>,
}

impl ChannelNotes {
    /// Close an open note `key` at `end_tick`, emitting a [`NoteEvent`].
    fn close(&mut self, key: u8, end_tick: u64, ppq: f64) {
        if let Some((start_tick, vel)) = self.open.remove(&key) {
            let start = start_tick as f64 / ppq;
            let length = (end_tick.saturating_sub(start_tick)) as f64 / ppq;
            self.events.push(NoteEvent {
                start,
                length: length.max(1.0 / 256.0),
                note: key,
                velocity: vel.max(1),
            });
        }
    }
}

/// Parse a Standard MIDI File into a [`Song`]. Each (SMF track × channel) with
/// notes becomes a [`Track`]; channel 10 (index 9) is flagged as drums. Tempo is
/// taken from the file's first tempo event (default 120 BPM); mid-song tempo
/// changes are not yet applied. SMPTE-timed files are rejected.
pub fn parse_smf(bytes: &[u8]) -> Result<Song, String> {
    use midly::{MetaMessage, MidiMessage, Smf, Timing, TrackEventKind};

    let smf = Smf::parse(bytes).map_err(|e| format!("invalid MIDI file: {e}"))?;

    let ppq = match smf.header.timing {
        Timing::Metrical(t) => t.as_int() as f64,
        Timing::Timecode(..) => {
            return Err("SMPTE-timed MIDI files aren't supported yet".to_string());
        }
    };
    if ppq <= 0.0 {
        return Err("MIDI file has an invalid ticks-per-quarter".to_string());
    }

    // Collect every tempo change (across all tracks) into a beat-sorted map.
    let mut tempo_changes: Vec<TempoChange> = Vec::new();
    for track in &smf.tracks {
        let mut abs: u64 = 0;
        for ev in track {
            abs += ev.delta.as_int() as u64;
            if let TrackEventKind::Meta(MetaMessage::Tempo(us)) = ev.kind {
                let us = us.as_int() as f64;
                if us > 0.0 {
                    tempo_changes.push(TempoChange {
                        beat: abs as f64 / ppq,
                        bpm: 60_000_000.0 / us,
                    });
                }
            }
        }
    }
    tempo_changes.sort_by(|a, b| {
        a.beat
            .partial_cmp(&b.beat)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    // Base tempo = the earliest change (default 120 if none).
    let bpm = tempo_changes.first().map(|c| c.bpm).unwrap_or(120.0);
    // Only keep a map if the tempo actually varies.
    let tempo_map = if tempo_changes.iter().any(|c| (c.bpm - bpm).abs() > 1e-6) {
        tempo_changes
    } else {
        Vec::new()
    };

    let mut out_tracks: Vec<Track> = Vec::new();
    for track in &smf.tracks {
        let mut abs: u64 = 0;
        let mut name = String::new();
        let mut chans: HashMap<u8, ChannelNotes> = HashMap::new();

        for ev in track {
            abs += ev.delta.as_int() as u64;
            match ev.kind {
                TrackEventKind::Meta(MetaMessage::TrackName(raw)) => {
                    name = String::from_utf8_lossy(raw).trim().to_string();
                }
                TrackEventKind::Midi { channel, message } => {
                    let ch = channel.as_int();
                    let st = chans.entry(ch).or_default();
                    match message {
                        MidiMessage::NoteOn { key, vel } => {
                            let (k, v) = (key.as_int(), vel.as_int());
                            // A note-on at velocity 0 is a note-off. Always close
                            // any same-key note first (re-articulation).
                            st.close(k, abs, ppq);
                            if v > 0 {
                                st.open.insert(k, (abs, v));
                            }
                        }
                        MidiMessage::NoteOff { key, .. } => st.close(key.as_int(), abs, ppq),
                        _ => {}
                    }
                }
                _ => {}
            }
        }

        // Emit one Song track per channel (sorted for determinism), closing any
        // notes left hanging at the track's end.
        let multi = chans.len() > 1;
        let mut by_chan: Vec<(u8, ChannelNotes)> = chans.into_iter().collect();
        by_chan.sort_by_key(|(ch, _)| *ch);
        for (ch, mut st) in by_chan {
            let open: Vec<u8> = st.open.keys().copied().collect();
            for k in open {
                st.close(k, abs, ppq);
            }
            if st.events.is_empty() {
                continue;
            }
            st.events.sort_by(|a, b| {
                a.start
                    .partial_cmp(&b.start)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let label = match (name.is_empty(), multi) {
                (true, _) => format!("ch {}", ch + 1),
                (false, true) => format!("{name} (ch {})", ch + 1),
                (false, false) => name.clone(),
            };
            out_tracks.push(Track {
                name: label,
                channel: ch,
                events: st.events,
            });
        }
    }

    if out_tracks.is_empty() {
        return Err("MIDI file contains no notes".to_string());
    }
    Ok(Song {
        bpm,
        tempo_map,
        tracks: out_tracks,
    })
}

/// Parse controller-change (CC) automation from a `.mid` into control lanes —
/// one lane per `(channel, controller number)` that carries any CC events. Each
/// returned tuple is `(label, points)` where points are `(beat, value)` with the
/// value normalized to `0.0..=1.0` (CC 0..127). The editor turns each into a
/// `ControlLane` to wire at a parameter. Returns an empty vec if the file has no
/// CC data (not an error).
#[allow(clippy::type_complexity)]
pub fn parse_smf_control(bytes: &[u8]) -> Result<Vec<(String, Vec<(f64, f32)>)>, String> {
    use midly::{MidiMessage, Smf, Timing, TrackEventKind};

    let smf = Smf::parse(bytes).map_err(|e| format!("invalid MIDI file: {e}"))?;
    let ppq = match smf.header.timing {
        Timing::Metrical(t) => t.as_int() as f64,
        Timing::Timecode(..) => return Err("SMPTE-timed MIDI files aren't supported yet".into()),
    };
    if ppq <= 0.0 {
        return Err("MIDI file has an invalid ticks-per-quarter".to_string());
    }

    // (channel, controller) → points, preserving first-seen order for stable lanes.
    let mut lanes: Vec<((u8, u8), Vec<(f64, f32)>)> = Vec::new();
    for track in &smf.tracks {
        let mut abs: u64 = 0;
        for ev in track {
            abs += ev.delta.as_int() as u64;
            if let TrackEventKind::Midi {
                channel,
                message: MidiMessage::Controller { controller, value },
            } = ev.kind
            {
                let key = (channel.as_int(), controller.as_int());
                let beat = abs as f64 / ppq;
                let v = value.as_int() as f32 / 127.0;
                match lanes.iter_mut().find(|(k, _)| *k == key) {
                    Some((_, pts)) => pts.push((beat, v)),
                    None => lanes.push((key, vec![(beat, v)])),
                }
            }
        }
    }

    Ok(lanes
        .into_iter()
        .map(|((ch, cc), points)| (format!("CC {cc} (ch {})", ch + 1), points))
        .collect())
}
