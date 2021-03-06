mod alloc;
mod indices;

use crate::ValueAsBytes;
use alloc::{AllocBuffer, BufferView};
use gl_typed as gl;
use ProfilingConfiguration as Configuration;
use ProfilingContext as Context;

use indices::ProfilerIndex;
pub use indices::{FrameIndex, RunIndex, SampleIndex};

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq)]
pub struct ProfilingConfiguration {
    pub name: Option<std::path::PathBuf>,
    pub record_events: bool,
    pub record_frames: bool,
    pub display: bool,
    pub hide: Vec<String>,
}

pub enum FrameEvent {
    BeginTimeSpan(SampleIndex),
    EndTimeSpan,
    RecordClusterBuffer { byte_offset: usize },
    RecordBasicBuffer { byte_offset: usize },
}

struct FrameContext {
    events: Vec<FrameEvent>,
    profilers_used: usize,
    profilers: Vec<TimeSpanProfiler>,
    buffer: AllocBuffer,
}

impl FrameContext {
    pub fn new(gl: &gl::Gl) -> Self {
        Self {
            events: Vec::new(),
            profilers_used: 0,
            profilers: Vec::new(),
            // TODO: Maybe someday this should be changed.
            buffer: AllocBuffer::with_capacity(gl, std::mem::size_of::<[ClusterBuffer; 2]>()),
        }
    }

    pub fn reset(&mut self) {
        self.events.clear();
        self.profilers_used = 0;
        self.buffer.reset();
    }
}

struct FrameContextRing([FrameContext; FrameContextRing::CAPACITY]);

impl FrameContextRing {
    const CAPACITY: usize = 3;

    pub fn new(gl: &gl::Gl) -> Self {
        Self([FrameContext::new(gl), FrameContext::new(gl), FrameContext::new(gl)])
    }

    pub fn reset(&mut self) {
        for context in self.0.iter_mut() {
            context.reset();
        }
    }
}

impl std::ops::Index<FrameIndex> for FrameContextRing {
    type Output = FrameContext;

    fn index(&self, index: FrameIndex) -> &Self::Output {
        &self.0[index.to_usize() % Self::CAPACITY]
    }
}

impl std::ops::IndexMut<FrameIndex> for FrameContextRing {
    fn index_mut(&mut self, index: FrameIndex) -> &mut Self::Output {
        &mut self.0[index.to_usize() % Self::CAPACITY]
    }
}

#[derive(Default)]
struct SamplesRing {
    sample_count: usize,
    ring: [Vec<Option<GpuCpuTimeSpan>>; SamplesRing::CAPACITY],
}

impl SamplesRing {
    const CAPACITY: usize = 9;

    pub fn add_sample(&mut self) -> SampleIndex {
        let index = SampleIndex::from_usize(self.sample_count);
        self.sample_count += 1;

        for samples in self.ring.iter_mut() {
            samples.push(None);
            debug_assert_eq!(samples.len(), self.sample_count);
        }

        index
    }

    pub fn clear(&mut self) {
        for samples in self.ring.iter_mut() {
            for sample in samples.iter_mut() {
                *sample = None;
            }
        }
    }
}

impl std::ops::Index<FrameIndex> for SamplesRing {
    type Output = Vec<Option<GpuCpuTimeSpan>>;

    fn index(&self, index: FrameIndex) -> &Self::Output {
        &self.ring[index.to_usize() % Self::CAPACITY]
    }
}

impl std::ops::IndexMut<FrameIndex> for SamplesRing {
    fn index_mut(&mut self, index: FrameIndex) -> &mut Self::Output {
        &mut self.ring[index.to_usize() % Self::CAPACITY]
    }
}

pub struct ProfilingContext {
    epoch: std::time::Instant,
    run_index: RunIndex,
    run_started: bool,
    frame_index: FrameIndex,
    frame_started: bool,
    frame_context_ring: FrameContextRing,
    samples_ring: SamplesRing,
    pub sample_names: Vec<&'static str>,
    thread: ProfilingThread,
}

struct ProfilingThreadInner {
    handle: std::thread::JoinHandle<()>,
    tx: std::sync::mpsc::Sender<Option<MeasurementEvent>>,
}

pub struct ProfilingThread(Option<ProfilingThreadInner>);

impl ProfilingThread {
    fn emit(&mut self, event: MeasurementEvent) {
        if let Some(thread) = self.0.as_mut() {
            thread.tx.send(Some(event)).unwrap();
        }
    }
}

impl Drop for ProfilingThread {
    fn drop(&mut self) {
        if let Some(thread) = self.0.take() {
            thread.tx.send(None).unwrap();
            thread.handle.join().unwrap();
        }
    }
}

impl Context {
    pub fn new(gl: &gl::Gl, profiling_dir: &std::path::Path, configuration: &Configuration) -> Self {
        let thread = ProfilingThread(if configuration.record_events {
            let mut file = std::io::BufWriter::new(std::fs::File::create(profiling_dir.join("events.bin")).unwrap());
            let (tx, rx) = std::sync::mpsc::channel();
            let handle = std::thread::Builder::new()
                .name("profiling".to_string())
                .spawn(move || {
                    while let Some(event) = rx.recv().unwrap() {
                        bincode::serialize_into(&mut file, &event).unwrap();
                    }
                })
                .unwrap();
            Some(ProfilingThreadInner { handle, tx })
        } else {
            None
        });

        Self {
            epoch: std::time::Instant::now(),
            frame_context_ring: FrameContextRing::new(gl),
            run_index: RunIndex::from_usize(0),
            run_started: false,
            frame_index: FrameIndex::from_usize(0),
            frame_started: false,
            samples_ring: Default::default(),
            sample_names: Default::default(),
            thread,
        }
    }

    #[inline]
    pub fn run_index(&self) -> RunIndex {
        assert_eq!(true, self.run_started);
        self.run_index
    }

    #[inline]
    pub fn add_sample(&mut self, sample: &'static str) -> SampleIndex {
        let sample_index = self.samples_ring.add_sample();
        self.sample_names.push(sample);
        self.thread
            .emit(MeasurementEvent::SampleName(sample_index, sample.to_string()));
        sample_index
    }

    #[inline]
    pub fn begin_run(&mut self, run_index: RunIndex) {
        assert_eq!(false, self.run_started);
        assert_eq!(self.run_index, run_index);
        self.run_started = true;
        self.thread.emit(MeasurementEvent::BeginRun(run_index));
    }

    #[inline]
    pub fn end_run(&mut self, run_index: RunIndex) {
        assert_eq!(true, self.run_started);
        assert_eq!(self.run_index, run_index);
        self.run_started = false;
        self.run_index.increment();

        self.thread.emit(MeasurementEvent::EndRun);

        // Reset frame-related data.
        self.frame_index = FrameIndex::from_usize(0);
        self.frame_context_ring.reset();
        self.samples_ring.clear();
    }

    #[inline]
    pub fn begin_frame(&mut self, gl: &gl::Gl, frame_index: FrameIndex) {
        assert_eq!(true, self.run_started);
        assert_eq!(false, self.frame_started);
        self.frame_started = true;
        assert_eq!(self.frame_index, frame_index);

        let context = &mut self.frame_context_ring[frame_index];

        if frame_index.to_usize() >= FrameContextRing::CAPACITY {
            // Compute the frame index when these events were recorded.
            let frame_index = FrameIndex::from_usize(frame_index.to_usize() - FrameContextRing::CAPACITY);

            self.thread.emit(MeasurementEvent::BeginFrame(frame_index));

            let samples = &mut self.samples_ring[frame_index];

            // Clear all samples because we're not sure we will write to every one.
            for sample in samples.iter_mut() {
                *sample = None;
            }

            // Read back data from the GPU.
            let mut profilers_used = 0;
            for event in context.events.iter() {
                match *event {
                    FrameEvent::BeginTimeSpan(sample_index) => {
                        let profiler_index = profilers_used;
                        profilers_used += 1;
                        debug_assert!(
                            samples[sample_index.to_usize()].is_none(),
                            "{} ({:?}) is written to more than once",
                            self.sample_names[sample_index.to_usize()],
                            sample_index
                        );
                        let sample = context.profilers[profiler_index].read(gl).unwrap();
                        self.thread.emit(MeasurementEvent::BeginTimeSpan(sample_index, sample));
                        samples[sample_index.to_usize()] = Some(sample);
                    }
                    FrameEvent::EndTimeSpan => self.thread.emit(MeasurementEvent::EndTimeSpan),
                    FrameEvent::RecordClusterBuffer { byte_offset } => unsafe {
                        let mut buffer = ClusterBuffer::default();
                        context.buffer.read(gl, byte_offset, buffer.value_as_bytes_mut());
                        self.thread.emit(MeasurementEvent::RecordClusterBuffer(buffer));
                    },
                    FrameEvent::RecordBasicBuffer { byte_offset } => unsafe {
                        let mut buffer = BasicBuffer::default();
                        context.buffer.read(gl, byte_offset, buffer.value_as_bytes_mut());
                        self.thread.emit(MeasurementEvent::RecordBasicBuffer(buffer));
                    },
                }
            }

            self.thread.emit(MeasurementEvent::EndFrame);

            debug_assert_eq!(profilers_used, context.profilers_used);

            context.reset();
        }
    }

    #[inline]
    pub fn end_frame(&mut self, frame_index: FrameIndex) {
        assert!(true, self.run_started);
        assert!(true, self.frame_started);
        assert_eq!(self.frame_index, frame_index);
        self.frame_started = false;
        self.frame_index.increment();
    }

    #[inline]
    pub fn events(&self, frame_index: FrameIndex) -> Option<&[FrameEvent]> {
        if frame_index.to_usize() >= 1 {
            // Most recent complete frame.
            let frame_index = FrameIndex::from_usize(frame_index.to_usize() - 1);
            let context = &self.frame_context_ring[frame_index];
            Some(&context.events)
        } else {
            None
        }
    }

    #[inline]
    pub fn start(&mut self, gl: &gl::Gl, sample_index: SampleIndex) -> ProfilerIndex {
        assert!(true, self.run_started);
        assert!(true, self.frame_started);
        let context = &mut self.frame_context_ring[self.frame_index];
        context.events.push(FrameEvent::BeginTimeSpan(sample_index));
        let profiler_index = ProfilerIndex(context.profilers_used);
        context.profilers_used += 1;
        while context.profilers.len() < profiler_index.0 + 1 {
            context.profilers.push(TimeSpanProfiler::new(gl));
        }
        context.profilers[profiler_index.0].start(gl, self.epoch);
        profiler_index
    }

    #[inline]
    pub fn stop(&mut self, gl: &gl::Gl, profiler_index: ProfilerIndex) {
        assert!(true, self.run_started);
        assert!(true, self.frame_started);
        let context = &mut self.frame_context_ring[self.frame_index];
        context.events.push(FrameEvent::EndTimeSpan);
        context.profilers[profiler_index.0].stop(gl, self.epoch);
    }

    #[inline]
    pub unsafe fn record_cluster_buffer(&mut self, gl: &gl::Gl, name: &gl::BufferName, byte_offset: usize) {
        assert!(true, self.run_started);
        assert!(true, self.frame_started);
        let context = &mut self.frame_context_ring[self.frame_index];
        let view = context.buffer.alloc::<ClusterBuffer>(gl, 1);
        gl.copy_named_buffer_sub_data(name, view.name, byte_offset, view.byte_offset, view.byte_count);
        context.events.push(FrameEvent::RecordClusterBuffer {
            byte_offset: view.byte_offset,
        });
    }

    #[inline]
    pub unsafe fn begin_basic_buffer(&mut self, gl: &gl::Gl) -> BufferView {
        assert!(true, self.run_started);
        assert!(true, self.frame_started);
        let context = &mut self.frame_context_ring[self.frame_index];

        let mut view = context.buffer.alloc::<BasicBuffer>(gl, 1);
        view.clear_0u32(gl);
        view
    }

    #[inline]
    pub unsafe fn end_basic_buffer(&mut self, _gl: &gl::Gl, view: BufferView) {
        assert!(true, self.run_started);
        assert!(true, self.frame_started);
        let context = &mut self.frame_context_ring[self.frame_index];

        context.events.push(FrameEvent::RecordBasicBuffer {
            byte_offset: view.byte_offset,
        });
    }

    #[inline]
    pub fn sample(&self, sample_index: SampleIndex) -> Option<GpuCpuTimeSpan> {
        assert!(true, self.run_started);
        assert!(true, self.frame_started);
        if self.frame_index.to_usize() >= FrameContextRing::CAPACITY {
            let frame_index = FrameIndex::from_usize(self.frame_index.to_usize() - FrameContextRing::CAPACITY);
            self.samples_ring[frame_index][sample_index.to_usize()]
        } else {
            None
        }
    }

    #[inline]
    pub fn stats(&self, sample_index: SampleIndex) -> Option<GpuCpuStats> {
        let mut cpu_elapsed = [0u64; SamplesRing::CAPACITY];
        let mut gpu_elapsed = [0u64; SamplesRing::CAPACITY];
        for index in 0..SamplesRing::CAPACITY {
            let span = self.samples_ring[FrameIndex::from_usize(index)][sample_index.to_usize()]?;
            cpu_elapsed[index] = span.cpu.delta();
            gpu_elapsed[index] = span.gpu.delta();
        }
        Some(GpuCpuStats {
            cpu_elapsed_avg: cpu_elapsed.iter().copied().sum::<u64>() / SamplesRing::CAPACITY as u64,
            cpu_elapsed_min: cpu_elapsed.iter().copied().min().unwrap(),
            cpu_elapsed_max: cpu_elapsed.iter().copied().max().unwrap(),
            gpu_elapsed_avg: gpu_elapsed.iter().copied().sum::<u64>() / SamplesRing::CAPACITY as u64,
            gpu_elapsed_min: gpu_elapsed.iter().copied().min().unwrap(),
            gpu_elapsed_max: gpu_elapsed.iter().copied().max().unwrap(),
        })
    }

    pub fn time_sensitive(&self) -> bool {
        assert!(true, self.run_started);
        self.run_index.to_usize() != 0
    }
}

pub type Epoch = std::time::Instant;

#[derive(serde::Serialize, serde::Deserialize, Debug, Copy, Clone, Default)]
pub struct TimeSpan {
    pub begin: u64,
    pub end: u64,
}

impl TimeSpan {
    pub fn delta(&self) -> u64 {
        // I'd rather see a 0 somewhere than crash when profiling timers overflow.
        self.end.saturating_sub(self.begin)
    }
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Copy, Clone, Default)]
pub struct GpuCpuTimeSpan {
    pub gpu: TimeSpan,
    pub cpu: TimeSpan,
}

#[derive(Debug)]
pub struct TimeSpanProfiler {
    begin_query_name: gl::QueryName,
    end_query_name: gl::QueryName,
    state: State,
}

#[derive(Debug)]
enum State {
    Empty,
    Started { cpu_begin: u64 },
    Stopped { cpu_begin: u64, cpu_end: u64 },
}

impl TimeSpanProfiler {
    #[inline]
    pub fn new(gl: &gl::Gl) -> Self {
        Self {
            begin_query_name: unsafe { gl.create_query(gl::TIMESTAMP) },
            end_query_name: unsafe { gl.create_query(gl::TIMESTAMP) },
            state: State::Empty,
        }
    }

    #[inline]
    pub fn start(&mut self, gl: &gl::Gl, epoch: Epoch) {
        self.state = match self.state {
            State::Empty | State::Stopped { .. } => {
                unsafe {
                    gl.query_counter(self.begin_query_name);
                }
                State::Started {
                    cpu_begin: epoch.elapsed().as_nanos() as u64,
                }
            }
            State::Started { .. } => {
                panic!("Tried to start a profiler that had already been started!");
            }
        };
    }

    #[inline]
    pub fn stop(&mut self, gl: &gl::Gl, epoch: Epoch) {
        self.state = match self.state {
            State::Empty => {
                panic!("Tried to stop a profiler that was never started!");
            }
            State::Started { cpu_begin } => {
                unsafe {
                    gl.query_counter(self.end_query_name);
                }
                State::Stopped {
                    cpu_begin,
                    cpu_end: epoch.elapsed().as_nanos() as u64,
                }
            }
            State::Stopped { .. } => {
                panic!("Tried to stop a profiler that had already been stopped!");
            }
        }
    }

    #[inline]
    pub fn read(&mut self, gl: &gl::Gl) -> Option<GpuCpuTimeSpan> {
        match self.state {
            State::Empty => None,
            State::Started { .. } => {
                panic!("Tried to read a profiler that was started but never stopped!");
            }
            State::Stopped { cpu_begin, cpu_end } => {
                // Not really necessary but I wan't to catch double reads.
                self.state = State::Empty;

                let (gpu_begin, gpu_end) = unsafe {
                    (
                        gl.try_query_result_u64(self.begin_query_name)
                            .expect("Query result was not ready!"),
                        gl.try_query_result_u64(self.end_query_name)
                            .expect("Query result was not ready!"),
                    )
                };

                Some(GpuCpuTimeSpan {
                    gpu: TimeSpan {
                        begin: gpu_begin.get(),
                        end: gpu_end.get(),
                    },
                    cpu: TimeSpan {
                        begin: cpu_begin,
                        end: cpu_end,
                    },
                })
            }
        }
    }
}

pub struct GpuCpuStats {
    pub gpu_elapsed_avg: u64,
    pub gpu_elapsed_min: u64,
    pub gpu_elapsed_max: u64,
    pub cpu_elapsed_avg: u64,
    pub cpu_elapsed_min: u64,
    pub cpu_elapsed_max: u64,
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub enum MeasurementEvent {
    SampleName(SampleIndex, String),
    BeginRun(RunIndex),
    EndRun,
    BeginFrame(FrameIndex),
    EndFrame,
    BeginTimeSpan(SampleIndex, GpuCpuTimeSpan),
    EndTimeSpan,
    RecordClusterBuffer(ClusterBuffer),
    RecordBasicBuffer(BasicBuffer),
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
#[repr(C)]
pub struct ClusterBuffer {
    data: [[u32; 32]; 32],
}

#[derive(serde::Serialize, serde::Deserialize, Debug, Clone, Default)]
#[repr(C)]
pub struct BasicBuffer {
    shading_ops: u32,
    lighting_ops: u32,
}
