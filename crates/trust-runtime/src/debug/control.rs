//! Debug control and state.

#![allow(missing_docs)]

use std::collections::HashMap;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Condvar, Mutex};

use smol_str::SmolStr;

use crate::eval::expr::{Expr, LValue};
use crate::eval::{eval_expr, EvalContext};
use crate::io::{IoAddress, IoSnapshot};
use crate::memory::{FrameId, InstanceId};
use crate::value::Value;

use super::breakpoints::matches_breakpoint;
use super::hook::DebugHook;
use super::trace::trace_debug;
use super::{
    DebugBreakpoint, DebugLog, DebugSnapshot, DebugStop, DebugStopReason, RuntimeEvent,
    SourceLocation,
};

/// Debugger execution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DebugMode {
    /// Execute statements without pausing.
    Running,
    /// Pause at the next statement boundary.
    Paused,
}

/// Control actions requested by the debug adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlAction {
    /// Pause execution at the next statement boundary.
    Pause(Option<u32>),
    /// Continue running.
    Continue,
    /// Execute a single statement, then pause.
    StepIn(Option<u32>),
    /// Step over the current statement.
    StepOver(Option<u32>),
    /// Step out to the caller.
    StepOut(Option<u32>),
}

/// Outcome of applying a control action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlOutcome {
    /// The action changed the debug state.
    Applied,
    /// The action was ignored because it had no effect.
    Ignored,
}

/// Step behavior while running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepKind {
    /// Pause after the next statement, regardless of call depth.
    Into,
    /// Pause after completing the current statement at the same call depth.
    Over,
    /// Pause after returning to the caller (lower call depth).
    Out,
}
#[derive(Debug, Clone, Copy)]
struct StepState {
    kind: StepKind,
    target_depth: u32,
    started: bool,
}

#[derive(Debug)]
struct DebugState {
    mode: DebugMode,
    last_location: Option<SourceLocation>,
    last_call_depth: u32,
    last_call_depths: HashMap<u32, u32>,
    current_thread: Option<u32>,
    target_thread: Option<u32>,
    breakpoints: Vec<DebugBreakpoint>,
    breakpoint_generation: HashMap<u32, u64>,
    frame_locations: HashMap<FrameId, SourceLocation>,
    logs: Vec<DebugLog>,
    snapshot: Option<DebugSnapshot>,
    watches: Vec<WatchEntry>,
    watch_changed: bool,
    log_tx: Option<Sender<DebugLog>>,
    io_tx: Option<Sender<IoSnapshot>>,
    stop_tx: Option<Sender<DebugStop>>,
    runtime_tx: Option<Sender<RuntimeEvent>>,
    runtime_events: Vec<RuntimeEvent>,
    pending_stop: Option<DebugStopReason>,
    stops: Vec<DebugStop>,
    last_stop: Option<DebugStop>,
    steps: HashMap<u32, StepState>,
    io_writes: Vec<(IoAddress, Value)>,
    pending_var_writes: Vec<PendingVarWrite>,
    pending_lvalue_writes: Vec<PendingLValueWrite>,
    forced_vars: Vec<ForcedVar>,
    forced_io: Vec<(IoAddress, Value)>,
}

#[derive(Debug, Clone)]
struct WatchEntry {
    expr: Expr,
    last: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ForcedVarTarget {
    Global(SmolStr),
    Retain(SmolStr),
    Instance(InstanceId, SmolStr),
}

#[derive(Debug, Clone)]
pub(crate) struct ForcedVar {
    pub target: ForcedVarTarget,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct ForcedSnapshot {
    pub vars: Vec<ForcedVar>,
    pub io: Vec<(IoAddress, Value)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingVarTarget {
    Global(SmolStr),
    Retain(SmolStr),
    Instance(InstanceId, SmolStr),
    Local(FrameId, SmolStr),
}

#[derive(Debug, Clone)]
pub(crate) struct PendingVarWrite {
    pub target: PendingVarTarget,
    pub value: Value,
}

#[derive(Debug, Clone)]
pub(crate) struct PendingLValueWrite {
    pub frame_id: Option<FrameId>,
    pub using: Vec<SmolStr>,
    pub target: LValue,
    pub value: Value,
}

/// Shared debug control and hook implementation.
#[derive(Debug, Clone)]
pub struct DebugControl {
    state: Arc<(Mutex<DebugState>, Condvar)>,
}

impl DebugControl {
    /// Create a new debug control handle in running mode.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new((
                Mutex::new(DebugState {
                    mode: DebugMode::Running,
                    last_location: None,
                    last_call_depth: 0,
                    last_call_depths: HashMap::new(),
                    current_thread: Some(1),
                    target_thread: None,
                    breakpoints: Vec::new(),
                    breakpoint_generation: HashMap::new(),
                    frame_locations: HashMap::new(),
                    logs: Vec::new(),
                    snapshot: None,
                    watches: Vec::new(),
                    watch_changed: false,
                    log_tx: None,
                    io_tx: None,
                    stop_tx: None,
                    runtime_tx: None,
                    runtime_events: Vec::new(),
                    pending_stop: None,
                    stops: Vec::new(),
                    last_stop: None,
                    steps: HashMap::new(),
                    io_writes: Vec::new(),
                    pending_var_writes: Vec::new(),
                    pending_lvalue_writes: Vec::new(),
                    forced_vars: Vec::new(),
                    forced_io: Vec::new(),
                }),
                Condvar::new(),
            )),
        }
    }

    /// Apply a requested control action.
    pub fn apply_action(&self, action: ControlAction) -> ControlOutcome {
        let (lock, cvar) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        let mut notify = false;
        let mut outcome = ControlOutcome::Applied;
        let previous_mode = state.mode;
        let step_started = matches!(previous_mode, DebugMode::Paused);

        match action {
            ControlAction::Pause(thread_id) => {
                if matches!(state.mode, DebugMode::Paused) {
                    outcome = ControlOutcome::Ignored;
                } else {
                    state.mode = DebugMode::Paused;
                    state.steps.clear();
                    state.pending_stop = Some(DebugStopReason::Pause);
                    state.snapshot = None;
                    state.target_thread = thread_id;
                }
            }
            ControlAction::Continue => {
                state.mode = DebugMode::Running;
                state.steps.clear();
                state.pending_stop = None;
                state.snapshot = None;
                state.target_thread = None;
                notify = true;
            }
            ControlAction::StepIn(thread_id) => {
                state.steps.clear();
                let target_thread = thread_id.or(state.current_thread);
                let step_key = target_thread.unwrap_or(0);
                let target_depth = state.last_call_depth;
                state.steps.insert(
                    step_key,
                    StepState {
                        kind: StepKind::Into,
                        target_depth,
                        started: step_started,
                    },
                );
                state.mode = DebugMode::Running;
                state.pending_stop = None;
                state.snapshot = None;
                state.target_thread = target_thread;
                notify = true;
            }
            ControlAction::StepOver(thread_id) => {
                state.steps.clear();
                let target_thread = thread_id.or(state.current_thread);
                let target_depth = target_thread
                    .and_then(|id| state.last_call_depths.get(&id).copied())
                    .unwrap_or(state.last_call_depth);
                let step_key = target_thread.unwrap_or(0);
                state.steps.insert(
                    step_key,
                    StepState {
                        kind: StepKind::Over,
                        target_depth,
                        started: step_started,
                    },
                );
                state.mode = DebugMode::Running;
                state.pending_stop = None;
                state.snapshot = None;
                state.target_thread = target_thread;
                notify = true;
            }
            ControlAction::StepOut(thread_id) => {
                state.steps.clear();
                let target_thread = thread_id.or(state.current_thread);
                let target_depth = target_thread
                    .and_then(|id| state.last_call_depths.get(&id).copied())
                    .unwrap_or(state.last_call_depth);
                let step_key = target_thread.unwrap_or(0);
                state.steps.insert(
                    step_key,
                    StepState {
                        kind: StepKind::Out,
                        target_depth: target_depth.saturating_sub(1),
                        started: step_started,
                    },
                );
                state.mode = DebugMode::Running;
                state.pending_stop = None;
                state.snapshot = None;
                state.target_thread = target_thread;
                notify = true;
            }
        }

        if notify {
            cvar.notify_all();
        }

        trace_debug(&format!(
            "action={action:?} outcome={outcome:?} mode={previous_mode:?}->{:?}",
            state.mode
        ));

        outcome
    }

    /// Replace all breakpoints for a given file id.
    pub fn set_breakpoints_for_file(&self, file_id: u32, breakpoints: Vec<DebugBreakpoint>) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        let requested_count = breakpoints.len();
        let generation = {
            let entry = state.breakpoint_generation.entry(file_id).or_insert(0);
            *entry = entry.saturating_add(1);
            *entry
        };
        state
            .breakpoints
            .retain(|bp| bp.location.file_id != file_id);
        state
            .breakpoints
            .extend(breakpoints.into_iter().map(|mut bp| {
                bp.generation = generation;
                bp
            }));
        trace_debug(&format!(
            "breakpoints.set file_id={} generation={} requested={} total={}",
            file_id,
            generation,
            requested_count,
            state.breakpoints.len()
        ));
    }

    /// Clear all breakpoints.
    pub fn clear_breakpoints(&self) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        let prev_total = state.breakpoints.len();
        state.breakpoints.clear();
        state.breakpoint_generation.clear();
        trace_debug(&format!("breakpoints.clear prev_total={prev_total}"));
    }

    /// Returns the number of active breakpoints (primarily for tests).
    #[doc(hidden)]
    pub fn breakpoint_count(&self) -> usize {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.breakpoints.len()
    }

    /// Snapshot current breakpoints.
    #[must_use]
    pub fn breakpoints(&self) -> Vec<DebugBreakpoint> {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.breakpoints.clone()
    }

    /// Pause execution at the next statement boundary.
    pub fn pause(&self) {
        let _ = self.apply_action(ControlAction::Pause(None));
    }

    /// Pause execution at the next statement boundary for a specific thread.
    pub fn pause_thread(&self, thread_id: u32) {
        let _ = self.apply_action(ControlAction::Pause(Some(thread_id)));
    }

    /// Pause execution at the next statement boundary with an entry reason.
    pub fn pause_entry(&self) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        if matches!(state.mode, DebugMode::Paused) {
            return;
        }
        state.mode = DebugMode::Paused;
        state.steps.clear();
        state.pending_stop = Some(DebugStopReason::Entry);
        state.snapshot = None;
        state.target_thread = None;
    }

    /// Continue running until the next pause request.
    pub fn continue_run(&self) {
        let _ = self.apply_action(ControlAction::Continue);
    }

    /// Execute a single statement and pause again.
    pub fn step(&self) {
        let _ = self.apply_action(ControlAction::StepIn(None));
    }

    /// Execute a single statement and pause again (thread-scoped).
    pub fn step_thread(&self, thread_id: u32) {
        let _ = self.apply_action(ControlAction::StepIn(Some(thread_id)));
    }

    /// Step over the current statement at the last observed call depth.
    pub fn step_over(&self) {
        let _ = self.apply_action(ControlAction::StepOver(None));
    }

    /// Step over the current statement at the last observed call depth (thread-scoped).
    pub fn step_over_thread(&self, thread_id: u32) {
        let _ = self.apply_action(ControlAction::StepOver(Some(thread_id)));
    }

    /// Step out of the current call frame.
    pub fn step_out(&self) {
        let _ = self.apply_action(ControlAction::StepOut(None));
    }

    /// Step out of the current call frame (thread-scoped).
    pub fn step_out_thread(&self, thread_id: u32) {
        let _ = self.apply_action(ControlAction::StepOut(Some(thread_id)));
    }

    /// Get the current execution mode.
    #[must_use]
    pub fn mode(&self) -> DebugMode {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.mode
    }

    /// Get the last observed statement location.
    #[must_use]
    pub fn last_location(&self) -> Option<SourceLocation> {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.last_location
    }

    /// Get the current breakpoint generation for a file id.
    #[must_use]
    pub fn breakpoint_generation(&self, file_id: u32) -> Option<u64> {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.breakpoint_generation.get(&file_id).copied()
    }

    /// Get the last observed location for a frame.
    #[must_use]
    pub fn frame_location(&self, frame_id: FrameId) -> Option<SourceLocation> {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.frame_locations.get(&frame_id).copied()
    }

    /// Snapshot all recorded frame locations.
    #[must_use]
    pub fn frame_locations(&self) -> HashMap<FrameId, SourceLocation> {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.frame_locations.clone()
    }

    /// Get the last observed call depth.
    #[must_use]
    pub fn last_call_depth(&self) -> u32 {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.last_call_depth
    }

    /// Set the current thread id for the active statement.
    pub fn set_current_thread(&self, thread_id: Option<u32>) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.current_thread = thread_id;
    }

    /// Get the current thread id, if any.
    #[must_use]
    pub fn current_thread(&self) -> Option<u32> {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.current_thread
    }

    /// Get the currently targeted thread for stepping/pausing, if any.
    #[must_use]
    pub fn target_thread(&self) -> Option<u32> {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.target_thread
    }

    /// Drain buffered log output.
    #[must_use]
    pub fn drain_logs(&self) -> Vec<DebugLog> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        std::mem::take(&mut state.logs)
    }

    /// Drain buffered runtime events.
    #[must_use]
    pub fn drain_runtime_events(&self) -> Vec<RuntimeEvent> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        std::mem::take(&mut state.runtime_events)
    }

    /// Get the last captured debug snapshot, if any.
    #[must_use]
    pub fn snapshot(&self) -> Option<DebugSnapshot> {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.snapshot.clone()
    }

    /// Return whether execution is currently paused.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        matches!(state.mode, DebugMode::Paused)
    }

    /// Return the most recent stop, if any.
    #[must_use]
    pub fn last_stop(&self) -> Option<DebugStop> {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        state.last_stop.clone()
    }

    /// Drain buffered stop events.
    #[must_use]
    pub fn drain_stops(&self) -> Vec<DebugStop> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        std::mem::take(&mut state.stops)
    }

    /// Mutate the stored snapshot, if one exists.
    pub fn with_snapshot<T>(&self, f: impl FnOnce(&mut DebugSnapshot) -> T) -> Option<T> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.snapshot.as_mut().map(f)
    }

    /// Queue an input write to be applied at the next cycle boundary.
    pub fn enqueue_io_write(&self, address: IoAddress, value: Value) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.io_writes.push((address, value));
    }

    /// Drain queued input writes.
    #[must_use]
    pub fn drain_io_writes(&self) -> Vec<(IoAddress, Value)> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        std::mem::take(&mut state.io_writes)
    }

    /// Queue a pending variable write to apply at the next cycle boundary.
    pub fn enqueue_global_write(&self, name: impl Into<SmolStr>, value: Value) {
        self.enqueue_var_write(PendingVarTarget::Global(name.into()), value);
    }

    /// Queue a pending retained variable write to apply at the next cycle boundary.
    pub fn enqueue_retain_write(&self, name: impl Into<SmolStr>, value: Value) {
        self.enqueue_var_write(PendingVarTarget::Retain(name.into()), value);
    }

    /// Queue a pending instance variable write to apply at the next cycle boundary.
    pub fn enqueue_instance_write(
        &self,
        instance_id: InstanceId,
        name: impl Into<SmolStr>,
        value: Value,
    ) {
        self.enqueue_var_write(PendingVarTarget::Instance(instance_id, name.into()), value);
    }

    /// Queue a pending local variable write to apply at the next cycle boundary.
    pub fn enqueue_local_write(&self, frame_id: FrameId, name: impl Into<SmolStr>, value: Value) {
        self.enqueue_var_write(PendingVarTarget::Local(frame_id, name.into()), value);
    }

    /// Queue a pending lvalue write to apply at the next cycle boundary.
    pub fn enqueue_lvalue_write(
        &self,
        frame_id: Option<FrameId>,
        using: Vec<SmolStr>,
        target: LValue,
        value: Value,
    ) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.pending_lvalue_writes.push(PendingLValueWrite {
            frame_id,
            using,
            target,
            value,
        });
    }

    /// Drain pending variable writes.
    #[must_use]
    pub(crate) fn drain_var_writes(&self) -> Vec<PendingVarWrite> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        std::mem::take(&mut state.pending_var_writes)
    }

    /// Drain pending lvalue writes.
    #[must_use]
    pub(crate) fn drain_lvalue_writes(&self) -> Vec<PendingLValueWrite> {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        std::mem::take(&mut state.pending_lvalue_writes)
    }

    fn enqueue_var_write(&self, target: PendingVarTarget, value: Value) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        if let Some(entry) = state
            .pending_var_writes
            .iter_mut()
            .find(|entry| entry.target == target)
        {
            entry.value = value;
        } else {
            state
                .pending_var_writes
                .push(PendingVarWrite { target, value });
        }
    }

    /// Force a global variable to the given value.
    pub fn force_global(&self, name: impl Into<SmolStr>, value: Value) {
        self.set_forced_var(ForcedVarTarget::Global(name.into()), value);
    }

    /// Force a retained global variable to the given value.
    pub fn force_retain(&self, name: impl Into<SmolStr>, value: Value) {
        self.set_forced_var(ForcedVarTarget::Retain(name.into()), value);
    }

    /// Force an instance variable to the given value.
    pub fn force_instance(&self, instance_id: InstanceId, name: impl Into<SmolStr>, value: Value) {
        self.set_forced_var(ForcedVarTarget::Instance(instance_id, name.into()), value);
    }

    /// Release a forced global variable.
    pub fn release_global(&self, name: &str) {
        self.clear_forced_var(|target| match target {
            ForcedVarTarget::Global(current) => current.as_str() == name,
            _ => false,
        });
    }

    /// Release a forced retained variable.
    pub fn release_retain(&self, name: &str) {
        self.clear_forced_var(|target| match target {
            ForcedVarTarget::Retain(current) => current.as_str() == name,
            _ => false,
        });
    }

    /// Release a forced instance variable.
    pub fn release_instance(&self, instance_id: InstanceId, name: &str) {
        self.clear_forced_var(|target| match target {
            ForcedVarTarget::Instance(current_id, current_name) => {
                *current_id == instance_id && current_name.as_str() == name
            }
            _ => false,
        });
    }

    /// Force an I/O address to the given value.
    pub fn force_io(&self, address: IoAddress, value: Value) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        if let Some(entry) = state
            .forced_io
            .iter_mut()
            .find(|(current, _)| *current == address)
        {
            entry.1 = value;
        } else {
            state.forced_io.push((address, value));
        }
    }

    /// Release a forced I/O address.
    pub fn release_io(&self, address: &IoAddress) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.forced_io.retain(|(current, _)| current != address);
    }

    pub(crate) fn forced_snapshot(&self) -> ForcedSnapshot {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        ForcedSnapshot {
            vars: state.forced_vars.clone(),
            io: state.forced_io.clone(),
        }
    }

    fn set_forced_var(&self, target: ForcedVarTarget, value: Value) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        if let Some(entry) = state
            .forced_vars
            .iter_mut()
            .find(|entry| entry.target == target)
        {
            entry.value = value;
        } else {
            state.forced_vars.push(ForcedVar { target, value });
        }
    }

    fn clear_forced_var(&self, predicate: impl Fn(&ForcedVarTarget) -> bool) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.forced_vars.retain(|entry| !predicate(&entry.target));
    }

    /// Register a watch expression for change detection.
    pub fn register_watch_expression(&self, expr: Expr) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.watches.push(WatchEntry { expr, last: None });
    }

    /// Clear watch expressions.
    pub fn clear_watch_expressions(&self) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.watches.clear();
        state.watch_changed = false;
    }

    /// Returns whether watch values changed since the last stop, and resets the flag.
    #[must_use]
    pub fn take_watch_changed(&self) -> bool {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        let changed = state.watch_changed;
        state.watch_changed = false;
        changed
    }

    /// Stream log output to a sender instead of buffering.
    pub fn set_log_sender(&self, sender: Sender<DebugLog>) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.log_tx = Some(sender);
    }

    /// Stop streaming log output; new logs will buffer.
    pub fn clear_log_sender(&self) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.log_tx = None;
    }

    /// Stream I/O snapshots to a sender.
    pub fn set_io_sender(&self, sender: Sender<IoSnapshot>) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.io_tx = Some(sender);
    }

    /// Stop streaming I/O snapshots.
    pub fn clear_io_sender(&self) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.io_tx = None;
    }

    /// Stream runtime events to a sender.
    pub fn set_runtime_sender(&self, sender: Sender<RuntimeEvent>) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.runtime_tx = Some(sender);
    }

    /// Stop streaming runtime events.
    pub fn clear_runtime_sender(&self) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.runtime_tx = None;
    }

    /// Stream stop events to a sender.
    pub fn set_stop_sender(&self, sender: Sender<DebugStop>) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.stop_tx = Some(sender);
    }

    /// Stop streaming stop events.
    pub fn clear_stop_sender(&self) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        state.stop_tx = None;
    }

    /// Emit an I/O snapshot to listeners, if configured.
    pub fn push_io_snapshot(&self, snapshot: IoSnapshot) {
        let (lock, _) = &*self.state;
        let state = lock.lock().expect("debug state poisoned");
        if let Some(sender) = &state.io_tx {
            let _ = sender.send(snapshot);
        }
    }

    /// Emit a runtime event to listeners, if configured.
    pub fn push_runtime_event(&self, event: RuntimeEvent) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        if let Some(sender) = &state.runtime_tx {
            let _ = sender.send(event.clone());
        } else {
            state.runtime_events.push(event);
        }
    }

    /// Refresh the stored snapshot using the provided evaluation context.
    pub fn refresh_snapshot(&self, ctx: &mut EvalContext<'_>) {
        let (lock, _) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        update_watch_snapshot(&mut state, ctx);
        update_snapshot(&mut state, ctx);
    }
}

impl Default for DebugControl {
    fn default() -> Self {
        Self::new()
    }
}

impl DebugHook for DebugControl {
    fn on_statement(&mut self, location: Option<&SourceLocation>, call_depth: u32) {
        self.on_statement_inner(location, call_depth, None);
    }

    fn on_statement_with_context(
        &mut self,
        ctx: &mut EvalContext<'_>,
        location: Option<&SourceLocation>,
        call_depth: u32,
    ) {
        self.on_statement_inner(location, call_depth, Some(ctx));
    }
}

impl DebugControl {
    fn on_statement_inner(
        &mut self,
        location: Option<&SourceLocation>,
        call_depth: u32,
        mut ctx: Option<&mut EvalContext<'_>>,
    ) {
        let (lock, cvar) = &*self.state;
        let mut state = lock.lock().expect("debug state poisoned");
        trace_debug(&format!(
            "hook.entry location={} depth={} mode={:?} current_thread={:?} target_thread={:?} pending_stop={:?} steps={} breakpoints={}",
            format_location_ref(location),
            call_depth,
            state.mode,
            state.current_thread,
            state.target_thread,
            state.pending_stop,
            state.steps.len(),
            state.breakpoints.len()
        ));
        state.last_location = location.copied();
        state.last_call_depth = call_depth;
        if let Some(thread_id) = state.current_thread {
            state.last_call_depths.insert(thread_id, call_depth);
        }
        if let (Some(location), Some(eval_ctx)) = (location, ctx.as_deref()) {
            let frames = eval_ctx.storage.frames();
            state
                .frame_locations
                .retain(|id, _| frames.iter().any(|frame| frame.id == *id));
            if let Some(frame) = eval_ctx.storage.current_frame() {
                state.frame_locations.insert(frame.id, *location);
            }
        }
        let is_target_thread =
            state.target_thread.is_none() || state.target_thread == state.current_thread;
        if matches!(state.mode, DebugMode::Paused) && is_target_thread {
            if let Some(reason) = state.pending_stop.take() {
                trace_debug(&format!(
                    "hook.pending_stop.consume reason={reason:?} location={} thread={:?}",
                    format_location_ref(location),
                    state.current_thread
                ));
                if let Some(eval_ctx) = ctx.as_mut() {
                    update_watch_snapshot(&mut state, eval_ctx);
                    update_snapshot(&mut state, eval_ctx);
                }
                emit_stop(&mut state, reason, location.copied(), None);
            }
        }
        let effective_mode = if is_target_thread {
            state.mode
        } else {
            DebugMode::Running
        };
        trace_debug(&format!(
            "hook.decision effective_mode={effective_mode:?} is_target_thread={} location={}",
            is_target_thread,
            format_location_ref(location)
        ));
        if let (DebugMode::Running, Some(location)) = (effective_mode, location) {
            let mut should_pause = false;
            let mut stop_reason = None;
            let mut stop_generation = None;
            if is_target_thread {
                let step_key = state
                    .current_thread
                    .filter(|id| state.steps.contains_key(id))
                    .or_else(|| state.steps.contains_key(&0).then_some(0));
                if let Some(step_key) = step_key {
                    if let Some(step) = state.steps.get_mut(&step_key) {
                        if !step.started {
                            step.started = true;
                            trace_debug(&format!(
                                "hook.step.arm key={} kind={:?} target_depth={} current_depth={}",
                                step_key, step.kind, step.target_depth, call_depth
                            ));
                        } else {
                            should_pause = match step.kind {
                                StepKind::Into => true,
                                StepKind::Over => call_depth <= step.target_depth,
                                StepKind::Out => call_depth <= step.target_depth,
                            };
                            trace_debug(&format!(
                                "hook.step.check key={} kind={:?} target_depth={} current_depth={} should_pause={}",
                                step_key, step.kind, step.target_depth, call_depth, should_pause
                            ));
                            if should_pause {
                                state.steps.remove(&step_key);
                                stop_reason = Some(DebugStopReason::Step);
                            }
                        }
                    }
                }
            }
            if !should_pause {
                let breakpoint_generation = {
                    let DebugState {
                        breakpoints,
                        logs,
                        log_tx,
                        ..
                    } = &mut *state;
                    matches_breakpoint(breakpoints, logs, log_tx.as_ref(), location, &mut ctx)
                };
                trace_debug(&format!(
                    "hook.breakpoint.check location={} matched_generation={:?}",
                    format_location_ref(Some(location)),
                    breakpoint_generation
                ));
                if let Some(generation) = breakpoint_generation {
                    should_pause = true;
                    state.steps.clear();
                    stop_reason = Some(DebugStopReason::Breakpoint);
                    stop_generation = Some(generation);
                    state.target_thread = None;
                }
            }
            if should_pause {
                state.mode = DebugMode::Paused;
                if let Some(reason) = stop_reason {
                    state.pending_stop = None;
                    trace_debug(&format!(
                        "hook.pause.enter reason={reason:?} generation={:?} location={} thread={:?}",
                        stop_generation,
                        format_location_ref(Some(location)),
                        state.current_thread
                    ));
                    if let Some(eval_ctx) = ctx.as_mut() {
                        update_watch_snapshot(&mut state, eval_ctx);
                        update_snapshot(&mut state, eval_ctx);
                    }
                    emit_stop(&mut state, reason, Some(*location), stop_generation);
                }
            }
        }
        loop {
            let is_target_thread =
                state.target_thread.is_none() || state.target_thread == state.current_thread;
            if matches!(state.mode, DebugMode::Paused) && is_target_thread {
                if let Some(reason) = state.pending_stop.take() {
                    trace_debug(&format!(
                        "hook.pending_stop.consume reason={reason:?} location={} thread={:?}",
                        format_location_ref(location),
                        state.current_thread
                    ));
                    if let Some(eval_ctx) = ctx.as_mut() {
                        update_watch_snapshot(&mut state, eval_ctx);
                        update_snapshot(&mut state, eval_ctx);
                    }
                    emit_stop(&mut state, reason, location.copied(), None);
                }
            }
            match state.mode {
                DebugMode::Running => {
                    trace_debug(&format!(
                        "hook.exit reason=running location={} thread={:?}",
                        format_location_ref(location),
                        state.current_thread
                    ));
                    return;
                }
                DebugMode::Paused => {
                    if !is_target_thread {
                        trace_debug(&format!(
                            "hook.exit reason=paused_non_target location={} current_thread={:?} target_thread={:?}",
                            format_location_ref(location),
                            state.current_thread,
                            state.target_thread
                        ));
                        return;
                    }
                    trace_debug(&format!(
                        "hook.wait location={} current_thread={:?} target_thread={:?}",
                        format_location_ref(location),
                        state.current_thread,
                        state.target_thread
                    ));
                    state = cvar.wait(state).expect("debug state poisoned");
                    trace_debug(&format!(
                        "hook.wake mode={:?} location={} current_thread={:?} target_thread={:?}",
                        state.mode,
                        format_location_ref(location),
                        state.current_thread,
                        state.target_thread
                    ));
                }
            }
        }
    }
}

fn format_location_ref(location: Option<&SourceLocation>) -> String {
    location
        .map(|loc| format!("{}:{}..{}", loc.file_id, loc.start, loc.end))
        .unwrap_or_else(|| "<none>".to_string())
}

fn emit_stop(
    state: &mut DebugState,
    reason: DebugStopReason,
    location: Option<SourceLocation>,
    breakpoint_generation: Option<u64>,
) {
    trace_debug(&format!(
        "stop reason={reason:?} location={:?} thread={:?}",
        location, state.current_thread
    ));
    let stop = DebugStop {
        reason,
        location,
        thread_id: state.current_thread,
        breakpoint_generation,
    };
    if let Some(sender) = &state.stop_tx {
        let _ = sender.send(stop.clone());
    }
    state.last_stop = Some(stop.clone());
    state.stops.push(stop);
}

fn update_watch_snapshot(state: &mut DebugState, ctx: &mut EvalContext<'_>) {
    let mut changed = false;
    for watch in &mut state.watches {
        let next = eval_expr(ctx, &watch.expr).ok();
        if watch.last != next {
            watch.last = next;
            changed = true;
        }
    }
    if changed {
        state.watch_changed = true;
    }
}

fn update_snapshot(state: &mut DebugState, ctx: &mut EvalContext<'_>) {
    state.snapshot = Some(DebugSnapshot {
        storage: ctx.storage.clone(),
        now: ctx.now,
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn breakpoint_clears_pending_pause() {
        let control = DebugControl::new();
        let location = SourceLocation::new(0, 0, 5);
        control.set_breakpoints_for_file(0, vec![DebugBreakpoint::new(location)]);

        let (stop_tx, stop_rx) = channel();
        control.set_stop_sender(stop_tx);

        {
            let (lock, _) = &*control.state;
            let mut state = lock.lock().expect("debug state poisoned");
            state.pending_stop = Some(DebugStopReason::Pause);
            state.mode = DebugMode::Running;
        }

        let mut hook = control.clone();
        let handle = thread::spawn(move || {
            hook.on_statement(Some(&location), 0);
        });

        let stop = stop_rx.recv_timeout(Duration::from_millis(250)).unwrap();
        assert_eq!(stop.reason, DebugStopReason::Breakpoint);

        control.continue_run();
        handle.join().unwrap();

        let (lock, _) = &*control.state;
        let state = lock.lock().expect("debug state poisoned");
        assert!(state.pending_stop.is_none());
    }

    #[test]
    fn pause_after_continue_while_waiting_emits_pause_stop() {
        let control = DebugControl::new();
        let location = SourceLocation::new(0, 0, 5);
        control.set_breakpoints_for_file(0, vec![DebugBreakpoint::new(location)]);

        let (stop_tx, stop_rx) = channel();
        control.set_stop_sender(stop_tx);

        let mut hook = control.clone();
        let handle = thread::spawn(move || {
            hook.on_statement(Some(&location), 0);
        });

        let first = stop_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("breakpoint stop");
        assert_eq!(first.reason, DebugStopReason::Breakpoint);

        // Race window: continue and immediately pause again while the hook may still be waking.
        control.continue_run();
        control.pause();

        let second = stop_rx
            .recv_timeout(Duration::from_millis(250))
            .expect("pause stop");
        assert_eq!(second.reason, DebugStopReason::Pause);

        control.continue_run();
        handle.join().expect("hook thread joins");
    }
}
