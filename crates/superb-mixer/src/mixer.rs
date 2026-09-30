//! The mixer: tracks, buses, sends and the master, wired into an `AudioGraph`.

use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use superb_buffers::PlanarBuffer;
use superb_core::{BusId, IdGenerator, NodeId, Result, SampleRate, SendId, SuperbError, TrackId};
use superb_graph::nodes::PassThroughNode;
use superb_graph::{AudioGraph, BlockProcessor, BlockTime};
use superb_midi::MidiEventBuffer;
use superb_rt::thread::debug_assert_not_rt;

use crate::{PanLaw, SendNode, SendParams, StripNode, StripParams};

/// Where a send taps the track signal.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SendTap {
    /// Before the track's fader/pan/mute (from the input summing node).
    PreFader,
    /// After the track strip.
    PostFader,
}

/// Description of a track.
#[derive(Clone, Debug)]
pub struct TrackInfo {
    /// The track's ID.
    pub id: TrackId,
    /// Display name.
    pub name: String,
    /// Graph node sources connect to.
    pub input_node: NodeId,
    /// Graph node carrying the processed track signal.
    pub strip_node: NodeId,
    /// Bus the track's output feeds.
    pub output: BusId,
    /// Control surface.
    pub params: Arc<StripParams>,
}

/// Description of a bus.
#[derive(Clone, Debug)]
pub struct BusInfo {
    /// The bus's ID.
    pub id: BusId,
    /// Display name.
    pub name: String,
    /// Graph node that sums everything routed to the bus.
    pub input_node: NodeId,
    /// Graph node carrying the processed bus signal.
    pub strip_node: NodeId,
    /// Bus this bus feeds (`None` for the master).
    pub output: Option<BusId>,
    /// Control surface.
    pub params: Arc<StripParams>,
}

/// Description of a send.
#[derive(Clone, Debug)]
pub struct SendInfo {
    /// The send's ID.
    pub id: SendId,
    /// Source track.
    pub track: TrackId,
    /// Destination bus.
    pub bus: BusId,
    /// Pre or post fader.
    pub tap: SendTap,
    /// Graph node implementing the send gain.
    pub node: NodeId,
    /// Control surface.
    pub params: Arc<SendParams>,
}

/// A mixing console on top of an [`AudioGraph`].
///
/// Pan-law defaults: tracks use [`PanLaw::ConstantPower`] (−3 dB centre,
/// right for mono sources), buses and the master use [`PanLaw::Balance`]
/// (unity at centre, right for stereo material). Change per strip with
/// [`StripParams::set_pan_law`].
///
/// Structural edits (`add_track`, `add_send`, routing changes) happen off
/// the audio thread and require [`BlockProcessor::prepare`] afterwards.
/// Parameter changes go through the shared [`StripParams`] /
/// [`SendParams`] handles and need no re-prepare.
pub struct Mixer {
    graph: AudioGraph,
    ids: IdGenerator,
    channels: usize,
    tracks: Vec<TrackInfo>,
    buses: Vec<BusInfo>,
    sends: Vec<SendInfo>,
    master: BusId,
    solo_count: Arc<AtomicUsize>,
}

impl Mixer {
    /// Creates a mixer whose strips carry `channels` channels (2 = stereo),
    /// with a master bus already present and set as the graph output.
    pub fn new(channels: usize) -> Self {
        let mut graph = AudioGraph::new();
        let ids = IdGenerator::new();
        let master_params = Arc::new(StripParams::new(None));
        master_params.set_pan_law(PanLaw::Balance);
        let input_node = graph.add_node(Box::new(PassThroughNode::new(channels)));
        let strip_node = graph.add_node(Box::new(StripNode::new(
            Arc::clone(&master_params),
            channels,
        )));
        graph.connect(input_node, strip_node).expect("fresh nodes");
        graph.set_output(strip_node).expect("fresh node");
        let master: BusId = ids.next();
        let buses = vec![BusInfo {
            id: master,
            name: "master".into(),
            input_node,
            strip_node,
            output: None,
            params: master_params,
        }];
        Self {
            graph,
            ids,
            channels,
            tracks: Vec::new(),
            buses,
            sends: Vec::new(),
            master,
            solo_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Channels per strip.
    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The master bus.
    pub fn master(&self) -> BusId {
        self.master
    }

    /// The underlying graph (for adding source/effect nodes).
    pub fn graph(&self) -> &AudioGraph {
        &self.graph
    }

    /// Mutable graph access. Re-`prepare` after edits.
    pub fn graph_mut(&mut self) -> &mut AudioGraph {
        &mut self.graph
    }

    /// Consumes the mixer, returning the graph.
    pub fn into_graph(self) -> AudioGraph {
        self.graph
    }

    // ------------------------------------------------------------------
    // Tracks
    // ------------------------------------------------------------------

    /// Adds a track routed to the master bus.
    pub fn add_track(&mut self, name: impl Into<String>) -> TrackId {
        debug_assert_not_rt("Mixer::add_track");
        let params = Arc::new(StripParams::new(Some(Arc::clone(&self.solo_count))));
        let input_node = self
            .graph
            .add_node(Box::new(PassThroughNode::new(self.channels)));
        let strip_node = self
            .graph
            .add_node(Box::new(StripNode::new(Arc::clone(&params), self.channels)));
        self.graph
            .connect(input_node, strip_node)
            .expect("fresh nodes");
        let master_in = self.bus(self.master).expect("master exists").input_node;
        self.graph
            .connect(strip_node, master_in)
            .expect("fresh nodes");
        let id: TrackId = self.ids.next();
        self.tracks.push(TrackInfo {
            id,
            name: name.into(),
            input_node,
            strip_node,
            output: self.master,
            params,
        });
        id
    }

    /// Removes a track, its sends and its graph nodes.
    pub fn remove_track(&mut self, id: TrackId) -> Result<()> {
        debug_assert_not_rt("Mixer::remove_track");
        let idx = self.track_index(id)?;
        let sends: Vec<SendId> = self
            .sends
            .iter()
            .filter(|s| s.track == id)
            .map(|s| s.id)
            .collect();
        for s in sends {
            self.remove_send(s)?;
        }
        let info = self.tracks.remove(idx);
        if info.params.is_solo() {
            info.params.set_solo(false);
        }
        self.graph.remove_node(info.strip_node)?;
        self.graph.remove_node(info.input_node)?;
        Ok(())
    }

    /// Track description.
    pub fn track(&self, id: TrackId) -> Result<&TrackInfo> {
        self.tracks
            .get(self.track_index(id)?)
            .ok_or(SuperbError::NotFound {
                kind: "track",
                id: id.raw(),
            })
    }

    /// All tracks in creation order.
    pub fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Control handle for a track.
    pub fn track_params(&self, id: TrackId) -> Result<Arc<StripParams>> {
        Ok(Arc::clone(&self.track(id)?.params))
    }

    /// Connects an existing graph node's output to a track's input.
    pub fn connect_source(&mut self, source: NodeId, track: TrackId) -> Result<()> {
        let input = self.track(track)?.input_node;
        self.graph.connect(source, input)
    }

    /// Routes a track's output to a bus (the master by default).
    pub fn set_track_output(&mut self, track: TrackId, bus: BusId) -> Result<()> {
        debug_assert_not_rt("Mixer::set_track_output");
        let idx = self.track_index(track)?;
        let new_in = self.bus(bus)?.input_node;
        let old_in = self.bus(self.tracks[idx].output)?.input_node;
        if self.tracks[idx].output == bus {
            return Ok(());
        }
        let strip = self.tracks[idx].strip_node;
        self.graph.disconnect(strip, old_in)?;
        self.graph.connect(strip, new_in)?;
        self.tracks[idx].output = bus;
        Ok(())
    }

    // ------------------------------------------------------------------
    // Buses
    // ------------------------------------------------------------------

    /// Adds a bus routed to the master.
    pub fn add_bus(&mut self, name: impl Into<String>) -> BusId {
        debug_assert_not_rt("Mixer::add_bus");
        let params = Arc::new(StripParams::new(None));
        params.set_pan_law(PanLaw::Balance);
        let input_node = self
            .graph
            .add_node(Box::new(PassThroughNode::new(self.channels)));
        let strip_node = self
            .graph
            .add_node(Box::new(StripNode::new(Arc::clone(&params), self.channels)));
        self.graph
            .connect(input_node, strip_node)
            .expect("fresh nodes");
        let master_in = self.bus(self.master).expect("master exists").input_node;
        self.graph
            .connect(strip_node, master_in)
            .expect("fresh nodes");
        let id: BusId = self.ids.next();
        self.buses.push(BusInfo {
            id,
            name: name.into(),
            input_node,
            strip_node,
            output: Some(self.master),
            params,
        });
        id
    }

    /// Removes a bus (not the master). Tracks and buses routed to it are
    /// re-routed to the master; sends to it are removed.
    pub fn remove_bus(&mut self, id: BusId) -> Result<()> {
        debug_assert_not_rt("Mixer::remove_bus");
        if id == self.master {
            return Err(SuperbError::InvalidArgument {
                what: "cannot remove master bus",
            });
        }
        let idx = self.bus_index(id)?;
        let sends: Vec<SendId> = self
            .sends
            .iter()
            .filter(|s| s.bus == id)
            .map(|s| s.id)
            .collect();
        for s in sends {
            self.remove_send(s)?;
        }
        let tracks: Vec<TrackId> = self
            .tracks
            .iter()
            .filter(|t| t.output == id)
            .map(|t| t.id)
            .collect();
        for t in tracks {
            self.set_track_output(t, self.master)?;
        }
        let buses: Vec<BusId> = self
            .buses
            .iter()
            .filter(|b| b.output == Some(id))
            .map(|b| b.id)
            .collect();
        for b in buses {
            self.set_bus_output(b, self.master)?;
        }
        let info = self.buses.remove(idx);
        self.graph.remove_node(info.strip_node)?;
        self.graph.remove_node(info.input_node)?;
        Ok(())
    }

    /// Bus description.
    pub fn bus(&self, id: BusId) -> Result<&BusInfo> {
        self.buses
            .get(self.bus_index(id)?)
            .ok_or(SuperbError::NotFound {
                kind: "bus",
                id: id.raw(),
            })
    }

    /// All buses (master first).
    pub fn buses(&self) -> &[BusInfo] {
        &self.buses
    }

    /// Control handle for a bus.
    pub fn bus_params(&self, id: BusId) -> Result<Arc<StripParams>> {
        Ok(Arc::clone(&self.bus(id)?.params))
    }

    /// Routes a bus's output into another bus. Rejects the master and cycles.
    pub fn set_bus_output(&mut self, bus: BusId, into: BusId) -> Result<()> {
        debug_assert_not_rt("Mixer::set_bus_output");
        if bus == self.master {
            return Err(SuperbError::InvalidArgument {
                what: "master has no output bus",
            });
        }
        let idx = self.bus_index(bus)?;
        let new_in = self.bus(into)?.input_node;
        if self.buses[idx].output == Some(into) {
            return Ok(());
        }
        let strip = self.buses[idx].strip_node;
        let old = self.buses[idx]
            .output
            .expect("non-master bus has an output");
        let old_in = self.bus(old)?.input_node;
        // `connect` rejects cycles; try it before tearing down the old edge.
        self.graph.disconnect(strip, old_in)?;
        if let Err(e) = self.graph.connect(strip, new_in) {
            self.graph.connect(strip, old_in)?;
            return Err(e);
        }
        self.buses[idx].output = Some(into);
        Ok(())
    }

    // ------------------------------------------------------------------
    // Sends
    // ------------------------------------------------------------------

    /// Adds a send from `track` to `bus` at linear `level`.
    pub fn add_send(
        &mut self,
        track: TrackId,
        bus: BusId,
        level: f32,
        tap: SendTap,
    ) -> Result<SendId> {
        debug_assert_not_rt("Mixer::add_send");
        let t = self.track(track)?.clone();
        let bus_in = self.bus(bus)?.input_node;
        let params = Arc::new(SendParams::new(level));
        let node = self
            .graph
            .add_node(Box::new(SendNode::new(Arc::clone(&params), self.channels)));
        let tap_node = match tap {
            SendTap::PreFader => t.input_node,
            SendTap::PostFader => t.strip_node,
        };
        self.graph.connect(tap_node, node)?;
        if let Err(e) = self.graph.connect(node, bus_in) {
            self.graph.remove_node(node)?;
            return Err(e);
        }
        let id: SendId = self.ids.next();
        self.sends.push(SendInfo {
            id,
            track,
            bus,
            tap,
            node,
            params,
        });
        Ok(id)
    }

    /// Removes a send.
    pub fn remove_send(&mut self, id: SendId) -> Result<()> {
        debug_assert_not_rt("Mixer::remove_send");
        let idx = self
            .sends
            .iter()
            .position(|s| s.id == id)
            .ok_or(SuperbError::NotFound {
                kind: "send",
                id: id.raw(),
            })?;
        let info = self.sends.remove(idx);
        self.graph.remove_node(info.node)?;
        Ok(())
    }

    /// Send description.
    pub fn send(&self, id: SendId) -> Result<&SendInfo> {
        self.sends
            .iter()
            .find(|s| s.id == id)
            .ok_or(SuperbError::NotFound {
                kind: "send",
                id: id.raw(),
            })
    }

    /// All sends.
    pub fn sends(&self) -> &[SendInfo] {
        &self.sends
    }

    /// Control handle for a send.
    pub fn send_params(&self, id: SendId) -> Result<Arc<SendParams>> {
        Ok(Arc::clone(&self.send(id)?.params))
    }

    /// Number of currently soloed tracks.
    pub fn solo_count(&self) -> usize {
        self.solo_count.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Clears solo on every track.
    pub fn clear_solo(&self) {
        for t in &self.tracks {
            t.params.set_solo(false);
        }
    }

    fn track_index(&self, id: TrackId) -> Result<usize> {
        self.tracks
            .iter()
            .position(|t| t.id == id)
            .ok_or(SuperbError::NotFound {
                kind: "track",
                id: id.raw(),
            })
    }

    fn bus_index(&self, id: BusId) -> Result<usize> {
        self.buses
            .iter()
            .position(|b| b.id == id)
            .ok_or(SuperbError::NotFound {
                kind: "bus",
                id: id.raw(),
            })
    }
}

impl BlockProcessor for Mixer {
    fn prepare(&mut self, sample_rate: SampleRate, max_frames: usize) -> Result<()> {
        self.graph.prepare(sample_rate, max_frames)
    }

    fn process(&mut self, time: BlockTime, frames: usize, midi: &MidiEventBuffer) -> Result<()> {
        self.graph.process(time, frames, midi)
    }

    fn output(&self) -> &PlanarBuffer {
        self.graph.output()
    }

    fn output_channels(&self) -> usize {
        self.channels
    }

    fn reset(&mut self) {
        self.graph.reset()
    }

    fn latency_frames(&self) -> usize {
        self.graph.latency_frames()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use superb_graph::nodes::ConstantNode;

    const R: SampleRate = SampleRate::HZ_48K;
    const N: usize = 16;

    fn no_midi() -> MidiEventBuffer {
        MidiEventBuffer::with_capacity(0)
    }

    /// Runs two blocks so ramps settle, returns the last sample per channel.
    fn settle(m: &mut Mixer) -> Vec<f32> {
        let midi = no_midi();
        for _ in 0..2 {
            m.process(BlockTime::free_running(), N, &midi).unwrap();
        }
        (0..m.channels())
            .map(|c| m.output().channel(c)[N - 1])
            .collect()
    }

    fn add_source(m: &mut Mixer, track: TrackId, value: f32) -> NodeId {
        let n = m
            .graph_mut()
            .add_node(Box::new(ConstantNode::new(value, 2)));
        m.connect_source(n, track).unwrap();
        n
    }

    fn balanced(m: &mut Mixer, id: TrackId) {
        m.track_params(id).unwrap().set_pan_law(PanLaw::Balance);
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-5
    }

    #[test]
    fn master_exists_and_empty_mixer_is_silent() {
        let mut m = Mixer::new(2);
        assert_eq!(m.buses().len(), 1);
        assert_eq!(m.bus_params(m.master()).unwrap().pan_law(), PanLaw::Balance);
        let t = m.add_track("t");
        assert_eq!(m.track_params(t).unwrap().pan_law(), PanLaw::ConstantPower);
        m.remove_track(t).unwrap();
        assert_eq!(m.bus(m.master()).unwrap().name, "master");
        assert!(m.bus(m.master()).unwrap().output.is_none());
        m.prepare(R, N).unwrap();
        m.process(BlockTime::free_running(), N, &no_midi()).unwrap();
        assert!(m.output().is_silent());
        assert_eq!(m.output_channels(), 2);
        assert_eq!(m.channels(), 2);
    }

    #[test]
    fn tracks_sum_into_master_and_gain_applies() {
        let mut m = Mixer::new(2);
        let a = m.add_track("a");
        let b = m.add_track("b");
        balanced(&mut m, a);
        balanced(&mut m, b);
        add_source(&mut m, a, 0.25);
        add_source(&mut m, b, 0.5);
        m.prepare(R, N).unwrap();
        assert_eq!(settle(&mut m), vec![0.75, 0.75]);
        m.track_params(b).unwrap().set_gain(0.5);
        assert_eq!(settle(&mut m), vec![0.5, 0.5]);
        m.bus_params(m.master()).unwrap().set_gain_db(-6.0206);
        let out = settle(&mut m);
        assert!(close(out[0], 0.25));
        assert_eq!(m.tracks().len(), 2);
        assert_eq!(m.track(a).unwrap().name, "a");
        assert_eq!(m.track(a).unwrap().output, m.master());
    }

    #[test]
    fn mute_and_solo_semantics() {
        let mut m = Mixer::new(2);
        let a = m.add_track("a");
        let b = m.add_track("b");
        balanced(&mut m, a);
        balanced(&mut m, b);
        add_source(&mut m, a, 0.25);
        add_source(&mut m, b, 0.5);
        m.prepare(R, N).unwrap();
        let pa = m.track_params(a).unwrap();
        let pb = m.track_params(b).unwrap();

        pa.set_mute(true);
        assert_eq!(settle(&mut m), vec![0.5, 0.5]);
        pa.set_mute(false);

        pb.set_solo(true);
        assert_eq!(m.solo_count(), 1);
        assert_eq!(
            settle(&mut m),
            vec![0.5, 0.5],
            "solo silences the other track"
        );
        pa.set_solo(true);
        assert_eq!(settle(&mut m), vec![0.75, 0.75], "two solos: both play");
        pa.set_mute(true);
        assert_eq!(settle(&mut m), vec![0.5, 0.5], "mute wins over solo");
        pa.set_mute(false);
        m.clear_solo();
        assert_eq!(m.solo_count(), 0);
        assert_eq!(settle(&mut m), vec![0.75, 0.75]);

        // Solo does not silence buses: route b to a bus, solo a.
        let fx = m.add_bus("fx");
        m.set_track_output(b, fx).unwrap();
        m.prepare(R, N).unwrap();
        pa.set_solo(true);
        assert_eq!(
            settle(&mut m),
            vec![0.25, 0.25],
            "b is silenced at the track, bus passes a's signal"
        );
        pb.set_solo(true);
        assert_eq!(settle(&mut m), vec![0.75, 0.75]);
    }

    #[test]
    fn pan_laws_on_master_output() {
        let mut m = Mixer::new(2);
        let t = m.add_track("t");
        add_source(&mut m, t, 1.0);
        m.prepare(R, N).unwrap();
        let p = m.track_params(t).unwrap();
        let centre = settle(&mut m);
        assert!(close(centre[0], core::f32::consts::FRAC_1_SQRT_2));
        assert!(close(centre[1], core::f32::consts::FRAC_1_SQRT_2));
        p.set_pan(1.0);
        let right = settle(&mut m);
        assert!(close(right[0], 0.0) && close(right[1], 1.0));
        p.set_pan_law(PanLaw::Balance);
        p.set_pan(-0.5);
        let left = settle(&mut m);
        assert!(close(left[0], 1.0) && close(left[1], 0.5));
    }

    #[test]
    fn sends_pre_and_post_fader() {
        let mut m = Mixer::new(2);
        let t = m.add_track("t");
        balanced(&mut m, t);
        add_source(&mut m, t, 1.0);
        let fx = m.add_bus("fx");
        // Mute the fx return so we can observe the send in isolation via the bus meter.
        let post = m.add_send(t, fx, 0.5, SendTap::PostFader).unwrap();
        m.prepare(R, N).unwrap();
        let tp = m.track_params(t).unwrap();
        // master = track (1.0) + fx (0.5)
        assert_eq!(settle(&mut m), vec![1.5, 1.5]);
        tp.set_gain(0.5);
        // post-fader: track 0.5, send 0.25
        assert_eq!(settle(&mut m), vec![0.75, 0.75]);
        m.send_params(post).unwrap().set_enabled(false);
        assert_eq!(settle(&mut m), vec![0.5, 0.5]);
        m.remove_send(post).unwrap();
        assert!(m.send(post).is_err());
        assert!(m.remove_send(post).is_err());

        let pre = m.add_send(t, fx, 0.5, SendTap::PreFader).unwrap();
        m.prepare(R, N).unwrap();
        // pre-fader: send unaffected by the 0.5 track gain
        assert_eq!(settle(&mut m), vec![1.0, 1.0]);
        tp.set_mute(true);
        assert_eq!(
            settle(&mut m),
            vec![0.5, 0.5],
            "pre-fader send survives mute"
        );
        assert_eq!(m.sends().len(), 1);
        assert_eq!(m.send(pre).unwrap().tap, SendTap::PreFader);
        let bus_peak = m.bus_params(fx).unwrap().peak(0);
        assert!(close(bus_peak, 0.5));
    }

    #[test]
    fn routing_to_buses_and_removal_reroutes_to_master() {
        let mut m = Mixer::new(2);
        let t = m.add_track("t");
        balanced(&mut m, t);
        add_source(&mut m, t, 0.5);
        let sub = m.add_bus("sub");
        let sub2 = m.add_bus("sub2");
        m.set_track_output(t, sub).unwrap();
        m.set_track_output(t, sub).unwrap(); // idempotent
        m.set_bus_output(sub, sub2).unwrap();
        m.bus_params(sub).unwrap().set_gain(0.5);
        m.bus_params(sub2).unwrap().set_gain(0.5);
        m.prepare(R, N).unwrap();
        assert_eq!(settle(&mut m), vec![0.125, 0.125]);
        assert_eq!(m.bus(sub).unwrap().output, Some(sub2));

        // cycles are rejected and the old route restored
        assert_eq!(m.set_bus_output(sub2, sub), Err(SuperbError::Cycle));
        assert_eq!(m.bus(sub2).unwrap().output, Some(m.master()));
        assert!(m.set_bus_output(m.master(), sub).is_err());

        let _ = m.add_send(t, sub2, 1.0, SendTap::PostFader).unwrap();
        m.remove_bus(sub2).unwrap();
        assert!(m.sends().is_empty());
        assert_eq!(m.bus(sub).unwrap().output, Some(m.master()));
        assert!(m.remove_bus(m.master()).is_err());
        assert!(m.remove_bus(sub2).is_err());
        m.prepare(R, N).unwrap();
        assert_eq!(settle(&mut m), vec![0.25, 0.25]);

        m.remove_bus(sub).unwrap();
        assert_eq!(m.track(t).unwrap().output, m.master());
        m.prepare(R, N).unwrap();
        assert_eq!(settle(&mut m), vec![0.5, 0.5]);
    }

    #[test]
    fn remove_track_cleans_up_solo_and_sends() {
        let mut m = Mixer::new(2);
        let a = m.add_track("a");
        let b = m.add_track("b");
        balanced(&mut m, b);
        add_source(&mut m, b, 0.5);
        let fx = m.add_bus("fx");
        m.add_send(a, fx, 1.0, SendTap::PostFader).unwrap();
        m.track_params(a).unwrap().set_solo(true);
        let nodes_before = m.graph().node_count();
        m.remove_track(a).unwrap();
        assert_eq!(
            m.solo_count(),
            0,
            "removing a soloed track releases the solo"
        );
        assert!(m.sends().is_empty());
        assert_eq!(m.graph().node_count(), nodes_before - 3);
        assert!(m.track(a).is_err());
        assert!(m.remove_track(a).is_err());
        m.prepare(R, N).unwrap();
        assert_eq!(settle(&mut m), vec![0.5, 0.5]);
    }

    #[test]
    fn meters_follow_signal() {
        let mut m = Mixer::new(2);
        let t = m.add_track("t");
        balanced(&mut m, t);
        add_source(&mut m, t, 0.8);
        m.prepare(R, N).unwrap();
        settle(&mut m);
        let p = m.track_params(t).unwrap();
        assert!(close(p.peak(0), 0.8) && close(p.peak(1), 0.8));
        assert!(close(m.bus_params(m.master()).unwrap().take_peak(0), 0.8));
        p.set_mute(true);
        settle(&mut m);
        p.take_peak(0);
        settle(&mut m);
        assert_eq!(p.peak(0), 0.0);
    }

    #[test]
    fn errors_for_unknown_ids() {
        let mut m = Mixer::new(2);
        let ghost_t = TrackId::from_raw(99);
        let ghost_b = BusId::from_raw(99);
        assert!(m.track(ghost_t).is_err());
        assert!(m.track_params(ghost_t).is_err());
        assert!(m.bus(ghost_b).is_err());
        assert!(m.set_track_output(ghost_t, m.master()).is_err());
        let t = m.add_track("t");
        assert!(m.set_track_output(t, ghost_b).is_err());
        assert!(m.add_send(t, ghost_b, 1.0, SendTap::PostFader).is_err());
        assert!(m
            .add_send(ghost_t, m.master(), 1.0, SendTap::PostFader)
            .is_err());
        assert!(m.connect_source(NodeId::from_raw(1234), t).is_err());
        let g = m.into_graph();
        assert_eq!(g.node_count(), 4);
    }
}
