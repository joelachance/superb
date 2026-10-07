//! The state file: the switches a session keeps without undo, in their
//! file form. State (a track's mute, solo, arm, monitor and input, a clip's
//! mute, a send's on/off, whether playback loops, the click and its
//! count-in, where the playhead is) is never written into `memory.json`.
//! An application that wants it kept says where (`session::state_file`),
//! and this is what lands there, as JSON.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::primitives::session::{Input, Memory};
use crate::primitives::{ClipId, RawId, Result, SendId, SuperbError, TimePosition, TrackId};

/// Version written into every state file; loading refuses newer ones.
pub const STATE_FORMAT_VERSION: u32 = 1;

/// A track's switches, by raw id. The master is a track here too.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct TrackStateSnapshot {
    /// The track's raw id.
    pub id: RawId,
    /// Muted.
    #[serde(default)]
    pub mute: bool,
    /// Soloed.
    #[serde(default)]
    pub solo: bool,
    /// Armed for recording.
    #[serde(default)]
    pub arm: bool,
    /// Monitoring its input.
    #[serde(default)]
    pub monitor: bool,
    /// Which input channels feed it when it records.
    #[serde(default)]
    pub input: Option<Input>,
}

/// A clip's switches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ClipStateSnapshot {
    /// The clip's raw id.
    pub id: RawId,
    /// Muted.
    #[serde(default)]
    pub mute: bool,
}

/// A send's switches.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SendStateSnapshot {
    /// The send's raw id.
    pub id: RawId,
    /// On (a send is on unless the file says otherwise).
    #[serde(default = "on")]
    pub enabled: bool,
}

fn on() -> bool {
    true
}

/// Everything a session keeps as state, in file form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// [`STATE_FORMAT_VERSION`] of the writer.
    pub format_version: u32,
    /// Where the playhead was, in samples.
    #[serde(default)]
    pub playhead: i64,
    /// Whether playback loops over the loop range.
    #[serde(default)]
    pub looping: bool,
    /// Whether the click sounds while the transport rolls.
    #[serde(default)]
    pub metronome: bool,
    /// Bars of count-in before a recording.
    #[serde(default)]
    pub count_in: u32,
    /// Every track's switches, the master first.
    #[serde(default)]
    pub tracks: Vec<TrackStateSnapshot>,
    /// Every clip's switches.
    #[serde(default)]
    pub clips: Vec<ClipStateSnapshot>,
    /// Every send's switches.
    #[serde(default)]
    pub sends: Vec<SendStateSnapshot>,
}

impl Memory {
    /// The state as it stands, in file form.
    pub fn state_snapshot(&self) -> StateSnapshot {
        let mut tracks = Vec::with_capacity(self.track_count() + 1);
        let mut clips = Vec::new();
        let mut sends = Vec::new();
        for t in core::iter::once(self.master()).chain(self.tracks()) {
            tracks.push(TrackStateSnapshot {
                id: t.id().raw(),
                mute: t.state.mute,
                solo: t.state.solo,
                arm: t.state.arm,
                monitor: t.state.monitor,
                input: t.state.input,
            });
            for c in t.clips() {
                clips.push(ClipStateSnapshot {
                    id: c.id().raw(),
                    mute: c.state.mute,
                });
            }
            for s in t.sends() {
                sends.push(SendStateSnapshot {
                    id: s.id().raw(),
                    enabled: s.state.enabled,
                });
            }
        }
        StateSnapshot {
            format_version: STATE_FORMAT_VERSION,
            playhead: self.playhead().samples(),
            looping: self.looping(),
            metronome: self.metronome().enabled,
            count_in: self.metronome().count_in,
            tracks,
            clips,
            sends,
        }
    }

    /// Puts saved state back onto the memory. Ids the memory no longer has
    /// are skipped; a file from a newer version is refused.
    pub fn apply_state(&mut self, state: &StateSnapshot) -> Result<()> {
        if state.format_version > STATE_FORMAT_VERSION {
            return Err(SuperbError::InvalidArgument {
                what: "state file written by a newer version",
            });
        }
        for t in &state.tracks {
            if let Ok(track) = self.track_mut(TrackId::from_raw(t.id)) {
                track.state.mute = t.mute;
                track.state.solo = t.solo;
                track.state.arm = t.arm;
                track.state.monitor = t.monitor;
                track.state.input = t.input;
            }
        }
        for c in &state.clips {
            if let Ok(clip) = self.clip_mut(ClipId::from_raw(c.id)) {
                clip.state.mute = c.mute;
            }
        }
        for s in &state.sends {
            if let Ok(send) = self.send_mut(SendId::from_raw(s.id)) {
                send.state.enabled = s.enabled;
            }
        }
        self.set_looping(state.looping);
        {
            let metronome = self.metronome_mut();
            metronome.enabled = state.metronome;
            metronome.count_in = state.count_in;
        }
        self.set_playhead(TimePosition::from_samples(state.playhead));
        Ok(())
    }

    /// Writes the state to `path` as JSON, whole, through a temporary file
    /// so a crash mid-write leaves the last good one.
    pub fn save_state(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        let json = serde_json::to_string_pretty(&self.state_snapshot())
            .map_err(|e| SuperbError::Message(format!("serialise state: {e}")))?;
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)
            .map_err(|e| SuperbError::Message(format!("write {}: {e}", tmp.display())))?;
        std::fs::rename(&tmp, path)
            .map_err(|e| SuperbError::Message(format!("write {}: {e}", path.display())))
    }

    /// Reads a state file.
    pub fn load_state(path: impl AsRef<Path>) -> Result<StateSnapshot> {
        let path = path.as_ref();
        let text = std::fs::read_to_string(path)
            .map_err(|e| SuperbError::Message(format!("read {}: {e}", path.display())))?;
        serde_json::from_str(&text)
            .map_err(|e| SuperbError::Message(format!("parse {}: {e}", path.display())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::primitives::session::{Clip, ClipSource, TrackKind, TrackSend};
    use crate::primitives::{SampleRate, TimeDuration};

    fn rig() -> (Memory, TrackId, ClipId, SendId) {
        let mut m = Memory::new("s", SampleRate::HZ_48K);
        let vox = m.add_track("vox", TrackKind::Audio);
        let bus = m.add_track("fx", TrackKind::Bus);
        let clip = m
            .add_clip(
                vox,
                Clip::new(
                    "c",
                    TimePosition::ZERO,
                    TimeDuration::from_samples(10),
                    ClipSource::Empty,
                )
                .unwrap(),
            )
            .unwrap();
        let send = m.add_send(vox, TrackSend::new(bus)).unwrap();
        (m, vox, clip, send)
    }

    #[test]
    fn a_state_file_carries_the_switches_and_nothing_else() {
        let (mut m, vox, clip, send) = rig();
        let master = m.master().id();
        m.track_mut(vox).unwrap().state.mute = true;
        m.track_mut(vox).unwrap().state.arm = true;
        m.track_mut(vox).unwrap().state.input = Some(Input::Mono(3));
        m.track_mut(master).unwrap().state.mute = true;
        m.clip_mut(clip).unwrap().state.mute = true;
        m.send_mut(send).unwrap().state.enabled = false;
        m.set_looping(true);
        m.metronome_mut().enabled = true;
        m.metronome_mut().count_in = 2;
        m.set_playhead(TimePosition::from_samples(4_800));

        let snap = m.state_snapshot();
        let json = serde_json::to_string(&snap).unwrap();
        assert!(
            !json.contains("volume") && !json.contains("\"name\""),
            "switches only: {json}"
        );
        let back: StateSnapshot = serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap);

        // The same session fresh, without its state, takes it back.
        let (mut fresh, ..) = rig();
        fresh.apply_state(&back).unwrap();
        assert!(fresh.track(vox).unwrap().state.mute);
        assert!(fresh.track(vox).unwrap().state.arm);
        assert_eq!(fresh.track(vox).unwrap().state.input, Some(Input::Mono(3)));
        assert!(fresh.master().state.mute);
        assert!(fresh.clip(clip).unwrap().state.mute);
        assert!(!fresh.send(send).unwrap().state.enabled);
        assert!(fresh.looping());
        assert_eq!(fresh.metronome().count_in, 2);
        assert!(fresh.metronome().enabled);
        assert_eq!(fresh.playhead(), TimePosition::from_samples(4_800));

        // Ids that are gone are skipped; a newer file is refused.
        let mut stale = back.clone();
        stale.tracks.push(TrackStateSnapshot {
            id: 999,
            mute: true,
            solo: false,
            arm: false,
            monitor: false,
            input: None,
        });
        fresh.apply_state(&stale).unwrap();
        stale.format_version = STATE_FORMAT_VERSION + 1;
        assert!(fresh.apply_state(&stale).is_err());

        // A file without a send entry leaves the send on.
        let bare: StateSnapshot =
            serde_json::from_str("{\"format_version\":1,\"sends\":[{\"id\":1}]}").unwrap();
        assert!(bare.sends[0].enabled);
    }
}
