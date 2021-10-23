use atomic_refcell::AtomicRefCell;
use audio_graph::NodeRef;
use basedrop::{Handle, Shared};
use fnv::FnvHashMap;
use rusty_daw_core::block_buffer::{MonoBlockBuffer, StereoBlockBuffer};

use super::graph_state::PortIdent;
use super::node::AudioGraphNode;

// Total bytes = 16
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct DelayCompNodeKey {
    pub src_node_id: u32,
    pub src_node_port: PortIdent,
    pub dst_node_id: u32,
    pub dst_node_port: PortIdent,
}

// Total bytes = 8
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct SumNodeKey {
    pub node_id: u32,
    pub port: PortIdent,
}

/// Used for debugging purposes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DebugNodeID {
    User((NodeRef, &'static str)),
    DelayComp(u64),
    Sum(u64),
    Root,
}

/// Used for debugging purposes.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DebugBufferID {
    Block(usize),
    Temp(usize),
}

pub(crate) struct GraphResourcePool<GlobalData: Send + Sync + 'static, const MAX_BLOCKSIZE: usize> {
    // Using AtomicRefCell because these resources are only ever borrowed by
    // the rt thread. We keep these pointers in a non-rt thread so we can
    // cheaply clone and reconstruct a new schedule to send to the rt thread whenever the
    // graph is recompiled (only need to copy pointers instead of whole Vecs).
    pub(crate) nodes: Vec<
        Option<(
            Shared<(
                AtomicRefCell<Box<dyn AudioGraphNode<GlobalData, MAX_BLOCKSIZE>>>,
                DebugNodeID,
            )>,
            (u32, u32),
        )>,
    >,
    pub(crate) mono_block_buffers: Vec<
        Shared<(
            AtomicRefCell<MonoBlockBuffer<f32, MAX_BLOCKSIZE>>,
            DebugBufferID,
        )>,
    >,
    pub(crate) stereo_block_buffers: Vec<
        Shared<(
            AtomicRefCell<StereoBlockBuffer<f32, MAX_BLOCKSIZE>>,
            DebugBufferID,
        )>,
    >,

    // These buffers are used as temporary input/output buffers when inserting sum and
    // delay nodes into the schedule.
    //
    // TODO: We will need to ensure that none of these buffers overlap when we start using
    // a multi-threaded schedule.
    pub(crate) temp_mono_block_buffers: Vec<
        Shared<(
            AtomicRefCell<MonoBlockBuffer<f32, MAX_BLOCKSIZE>>,
            DebugBufferID,
        )>,
    >,
    pub(crate) temp_stereo_block_buffers: Vec<
        Shared<(
            AtomicRefCell<StereoBlockBuffer<f32, MAX_BLOCKSIZE>>,
            DebugBufferID,
        )>,
    >,

    pub(crate) delay_comp_nodes: FnvHashMap<
        DelayCompNodeKey,
        (
            Shared<(
                AtomicRefCell<Box<dyn AudioGraphNode<GlobalData, MAX_BLOCKSIZE>>>,
                DebugNodeID,
            )>,
            u32,
            bool,
        ),
    >,
    pub(crate) sum_nodes: FnvHashMap<
        SumNodeKey,
        (
            Shared<(
                AtomicRefCell<Box<dyn AudioGraphNode<GlobalData, MAX_BLOCKSIZE>>>,
                DebugNodeID,
            )>,
            u32,
            bool,
        ),
    >,

    coll_handle: Handle,
}

impl<GlobalData: Send + Sync + 'static, const MAX_BLOCKSIZE: usize> Clone
    for GraphResourcePool<GlobalData, MAX_BLOCKSIZE>
{
    fn clone(&self) -> Self {
        Self {
            nodes: self.nodes.clone(),
            mono_block_buffers: self.mono_block_buffers.clone(),
            stereo_block_buffers: self.stereo_block_buffers.clone(),
            temp_mono_block_buffers: self.temp_mono_block_buffers.clone(),
            temp_stereo_block_buffers: self.temp_stereo_block_buffers.clone(),
            delay_comp_nodes: self.delay_comp_nodes.clone(),
            sum_nodes: self.sum_nodes.clone(),
            coll_handle: self.coll_handle.clone(),
        }
    }
}

impl<GlobalData: Send + Sync + 'static, const MAX_BLOCKSIZE: usize>
    GraphResourcePool<GlobalData, MAX_BLOCKSIZE>
{
    /// Create a new resource pool. Only to be used by the non-rt thread.
    pub(crate) fn new(coll_handle: Handle) -> Self {
        Self {
            nodes: Vec::new(),
            mono_block_buffers: Vec::new(),
            stereo_block_buffers: Vec::new(),
            temp_mono_block_buffers: Vec::new(),
            temp_stereo_block_buffers: Vec::new(),
            delay_comp_nodes: FnvHashMap::default(),
            sum_nodes: FnvHashMap::default(),
            coll_handle,
        }
    }

    pub(crate) fn add_node(
        &mut self,
        node_ref: NodeRef,
        new_node: Box<dyn AudioGraphNode<GlobalData, MAX_BLOCKSIZE>>,
        node_id: DebugNodeID,
        mono_through_ports: u32,
        stereo_through_ports: u32,
    ) {
        let index: usize = node_ref.into();
        while index >= self.nodes.len() {
            self.nodes.push(None);
        }

        self.nodes[index] = Some((
            Shared::new(&self.coll_handle, (AtomicRefCell::new(new_node), node_id)),
            (mono_through_ports, stereo_through_ports),
        ));
    }

    pub(crate) fn remove_node(&mut self, node_ref: NodeRef) {
        let index: usize = node_ref.into();
        self.nodes[index] = None;
    }

    pub(crate) fn get_mono_audio_block_buffer(
        &mut self,
        buffer_id: usize,
    ) -> Shared<(
        AtomicRefCell<MonoBlockBuffer<f32, MAX_BLOCKSIZE>>,
        DebugBufferID,
    )> {
        // Resize if buffer does not exist
        if self.mono_block_buffers.len() <= buffer_id {
            let n_new_block_buffers = (buffer_id + 1) - self.mono_block_buffers.len();
            for _ in 0..n_new_block_buffers {
                self.mono_block_buffers.push(Shared::new(
                    &self.coll_handle,
                    (
                        AtomicRefCell::new(MonoBlockBuffer::new()),
                        DebugBufferID::Block(buffer_id),
                    ),
                ));
            }
        }

        Shared::clone(&self.mono_block_buffers[buffer_id])
    }

    pub(crate) fn get_stereo_audio_block_buffer(
        &mut self,
        buffer_id: usize,
    ) -> Shared<(
        AtomicRefCell<StereoBlockBuffer<f32, MAX_BLOCKSIZE>>,
        DebugBufferID,
    )> {
        // Resize if buffer does not exist
        if self.stereo_block_buffers.len() <= buffer_id {
            let n_new_block_buffers = (buffer_id + 1) - self.stereo_block_buffers.len();
            for _ in 0..n_new_block_buffers {
                self.stereo_block_buffers.push(Shared::new(
                    &self.coll_handle,
                    (
                        AtomicRefCell::new(StereoBlockBuffer::new()),
                        DebugBufferID::Block(buffer_id),
                    ),
                ));
            }
        }

        Shared::clone(&self.stereo_block_buffers[buffer_id])
    }

    pub(crate) fn remove_mono_block_buffers(&mut self, n_to_remove: usize) {
        let n = n_to_remove.min(self.mono_block_buffers.len());
        for _ in 0..n {
            let _ = self.mono_block_buffers.pop();
        }
    }

    pub(crate) fn remove_stereo_block_buffers(&mut self, n_to_remove: usize) {
        let n = n_to_remove.min(self.stereo_block_buffers.len());
        for _ in 0..n {
            let _ = self.stereo_block_buffers.pop();
        }
    }

    pub(crate) fn get_temp_mono_audio_block_buffer(
        &mut self,
        buffer_id: usize,
    ) -> Shared<(
        AtomicRefCell<MonoBlockBuffer<f32, MAX_BLOCKSIZE>>,
        DebugBufferID,
    )> {
        // Resize if buffer does not exist
        if self.temp_mono_block_buffers.len() <= buffer_id {
            let n_new_block_buffers = (buffer_id + 1) - self.temp_mono_block_buffers.len();
            for _ in 0..n_new_block_buffers {
                self.temp_mono_block_buffers.push(Shared::new(
                    &self.coll_handle,
                    (
                        AtomicRefCell::new(MonoBlockBuffer::new()),
                        DebugBufferID::Temp(buffer_id),
                    ),
                ));
            }
        }

        Shared::clone(&self.temp_mono_block_buffers[buffer_id])
    }

    pub(crate) fn get_temp_stereo_audio_block_buffer(
        &mut self,
        buffer_id: usize,
    ) -> Shared<(
        AtomicRefCell<StereoBlockBuffer<f32, MAX_BLOCKSIZE>>,
        DebugBufferID,
    )> {
        // Resize if buffer does not exist
        if self.temp_stereo_block_buffers.len() <= buffer_id {
            let n_new_block_buffers = (buffer_id + 1) - self.temp_stereo_block_buffers.len();
            for _ in 0..n_new_block_buffers {
                self.temp_stereo_block_buffers.push(Shared::new(
                    &self.coll_handle,
                    (
                        AtomicRefCell::new(StereoBlockBuffer::new()),
                        DebugBufferID::Temp(buffer_id),
                    ),
                ));
            }
        }

        Shared::clone(&self.temp_stereo_block_buffers[buffer_id])
    }

    pub(crate) fn remove_temp_mono_block_buffers(&mut self, n_to_remove: usize) {
        let n = n_to_remove.min(self.temp_mono_block_buffers.len());
        for _ in 0..n {
            let _ = self.temp_mono_block_buffers.pop();
        }
    }

    pub(crate) fn remove_temp_stereo_block_buffers(&mut self, n_to_remove: usize) {
        let n = n_to_remove.min(self.temp_stereo_block_buffers.len());
        for _ in 0..n {
            let _ = self.temp_stereo_block_buffers.pop();
        }
    }

    /// Only to be used by the rt thread.
    pub fn clear_all_buffers(&mut self, frames: usize) {
        let frames = frames.min(MAX_BLOCKSIZE);

        for b in self.mono_block_buffers.iter() {
            // Should not panic because the rt thread is the only thread that ever borrows resources.
            let b = &mut *AtomicRefCell::borrow_mut(&b.0);

            b.clear_frames(frames);
        }
        for b in self.stereo_block_buffers.iter() {
            // Should not panic because the rt thread is the only thread that ever borrows resources.
            let b = &mut *AtomicRefCell::borrow_mut(&b.0);

            b.clear_frames(frames);
        }

        // The temporary buffers do not need to be cleared since they will always be filled with data
        // by the scheduler before being sent to a node.
    }

    /// Flag all delay comp and sum nodes as unused.
    pub(crate) fn flag_unused(&mut self) {
        for (_, node) in self.delay_comp_nodes.iter_mut() {
            node.2 = false;
        }
        for (_, node) in self.sum_nodes.iter_mut() {
            node.2 = false;
        }
    }

    /// Drop all delay comp and sum nodes that are no longer being used.
    pub(crate) fn drop_unused(&mut self) {
        // `retain` can be quite expensive, so only use it when it is actually needed.

        let mut retain_delay_comp_nodes = false;
        for (_, node) in self.delay_comp_nodes.iter() {
            if node.2 == false {
                retain_delay_comp_nodes = true;
                break;
            }
        }

        let mut retain_sum_nodes = false;
        for (_, node) in self.sum_nodes.iter() {
            if node.2 == false {
                retain_sum_nodes = true;
                break;
            }
        }

        if retain_delay_comp_nodes {
            self.delay_comp_nodes.retain(|_, n| n.2);
        }
        if retain_sum_nodes {
            self.sum_nodes.retain(|_, n| n.2);
        }
    }
}
