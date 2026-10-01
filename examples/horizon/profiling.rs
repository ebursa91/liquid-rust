//! Bounded, request-scoped active-wall profiling. This module never writes files.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::marker::PhantomData;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::ThreadId;
use std::time::Instant;

use serde_json::{json, Value};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Dispatch, Metadata, Subscriber};
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

const SITE_LIMIT: usize = 65_536;
const AUXILIARY_SITE_LIMIT: usize = 512;
const DEPTH_LIMIT: usize = 64;
const EVENT_LIMIT: usize = 32000;
const THREAD_LIMIT: usize = 16;
const LABEL_LIMIT: usize = 96;

thread_local! {
    static PHASE: Cell<&'static str> = const { Cell::new("render") };
}

/// Restores the previous phase on this thread, including during unwinding.
pub(super) struct PhaseGuard {
    previous: &'static str,
    // A thread-local guard must never migrate to another thread.
    _thread_bound: PhantomData<Rc<()>>,
}

pub(super) fn phase(name: &'static str) -> PhaseGuard {
    let name = match name {
        "initialize" | "warmup" | "measured" | "serve" | "single" | "render" => name,
        _ => "other",
    };
    let previous = PHASE.with(|value| value.replace(name));
    PhaseGuard {
        previous,
        _thread_bound: PhantomData,
    }
}

pub(super) fn current_phase() -> &'static str {
    PHASE.with(Cell::get)
}

impl Drop for PhaseGuard {
    fn drop(&mut self) {
        PHASE.with(|value| value.set(self.previous));
    }
}

trait Clock: Send + Sync {
    fn now_ns(&self) -> u64;
}

struct WallClock(Instant);

impl Clock for WallClock {
    fn now_ns(&self) -> u64 {
        u64::try_from(self.0.elapsed().as_nanos()).unwrap_or(u64::MAX)
    }
}

#[derive(Clone, Default, Eq, Ord, PartialEq, PartialOrd)]
struct Label {
    span: String,
    target: String,
    strings: BTreeMap<&'static str, String>,
    numbers: BTreeMap<&'static str, u64>,
}

#[derive(Clone, Copy, Default)]
struct Outcomes {
    ok: Option<bool>,
    found: Option<bool>,
    owned: Option<bool>,
    delegated: Option<bool>,
}

#[derive(Default)]
struct Fields {
    label: Label,
    outcomes: Outcomes,
    rejected: u64,
}

fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= LABEL_LIMIT
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
}

fn safe_template(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= LABEL_LIMIT
        && !value.starts_with('/')
        && value.split('/').all(safe_identifier)
        && !value.split('/').any(|part| part == "." || part == "..")
}

impl Visit for Fields {
    fn record_str(&mut self, field: &Field, value: &str) {
        let name = match field.name() {
            "kind" => "kind",
            "name" => "name",
            "template" => "template",
            "mode" => "mode",
            "frame" => "frame",
            "outcome" => {
                match value {
                    "ok" | "success" => self.outcomes.ok = Some(true),
                    "error" | "failure" => self.outcomes.ok = Some(false),
                    _ => self.rejected += 1,
                }
                return;
            }
            _ => return,
        };
        let valid = if name == "template" {
            safe_template(value)
        } else {
            safe_identifier(value) || (name == "name" && value == "#")
        };
        if valid {
            self.label.strings.insert(name, value.to_owned());
        } else {
            self.rejected += 1;
        }
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        let name = match field.name() {
            "buffer_id" => "buffer_id",
            "line" => "line",
            "column" => "column",
            "byte" => "byte",
            "index" => "index",
            "node_count" => "node_count",
            "filter_count" => "filter_count",
            "selectors" => "selectors",
            "depth" => "depth",
            _ => return,
        };
        self.label.numbers.insert(name, value);
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        if let Ok(value) = u64::try_from(value) {
            self.record_u64(field, value);
        }
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        match field.name() {
            "ok" => self.outcomes.ok = Some(value),
            "found" => self.outcomes.found = Some(value),
            "owned" => self.outcomes.owned = Some(value),
            "delegated" => self.outcomes.delegated = Some(value),
            _ => {}
        }
    }

    // In particular, never format a Liquid Value, expression, error or argument.
    fn record_debug(&mut self, _field: &Field, _value: &dyn fmt::Debug) {}
}

#[derive(Clone, Eq, Ord, PartialEq, PartialOrd)]
struct SiteKey {
    parent: Option<usize>,
    phase: &'static str,
    label: Label,
}

struct Site {
    key: SiteKey,
    calls: u64,
    enters: u64,
    exits: u64,
    closed: u64,
    ok: u64,
    error: u64,
    unknown: u64,
    found: u64,
    missing: u64,
    owned: u64,
    borrowed: u64,
    delegated: u64,
    inclusive_ns: u64,
    exclusive_ns: u64,
}

impl Site {
    fn new(key: SiteKey) -> Self {
        Self {
            key,
            calls: 0,
            enters: 0,
            exits: 0,
            closed: 0,
            ok: 0,
            error: 0,
            unknown: 0,
            found: 0,
            missing: 0,
            owned: 0,
            borrowed: 0,
            delegated: 0,
            inclusive_ns: 0,
            exclusive_ns: 0,
        }
    }
}

// Registry owns this data only while a span is live. Completed IDs are not saved.
struct SpanInfo {
    site: Option<usize>,
    resolved: bool,
    key: SiteKey,
    outcomes: Outcomes,
}

struct Activation {
    span: u64,
    site: usize,
    start_ns: u64,
    child_ns: u64,
}

#[derive(Default)]
struct ThreadState {
    stack: Vec<Activation>,
    suppressed_depth: usize,
}

struct TraceEvent {
    site: usize,
    thread: usize,
    start_ns: u64,
    duration_ns: u64,
}

#[derive(Default)]
struct State {
    sites: Vec<Site>,
    keys: BTreeMap<SiteKey, usize>,
    thread_ids: HashMap<ThreadId, usize>,
    threads: Vec<ThreadState>,
    // This map contains entered spans only, bounded by threads * depth.
    active_spans: HashMap<u64, HashMap<usize, usize>>,
    trace: Vec<TraceEvent>,
    new_spans: u64,
    closed_spans: u64,
    created_by_phase: BTreeMap<&'static str, u64>,
    closed_by_phase: BTreeMap<&'static str, u64>,
    dropped_sites: u64,
    dropped_sites_by_phase: BTreeMap<&'static str, u64>,
    auxiliary_sites: usize,
    dropped_depth: u64,
    dropped_threads: u64,
    dropped_trace_events: u64,
    rejected_fields: u64,
    imbalances: u64,
    reentries: u64,
    concurrent_span_enters: u64,
    non_nested_parents: u64,
    phase_mismatches: u64,
    clock_errors: u64,
    concurrency_observed: bool,
    lock_poisoned: bool,
}

impl State {
    fn thread(&mut self, id: ThreadId) -> Option<usize> {
        if let Some(index) = self.thread_ids.get(&id) {
            return Some(*index);
        }
        if self.threads.len() == THREAD_LIMIT {
            self.dropped_threads += 1;
            return None;
        }
        let index = self.threads.len();
        self.thread_ids.insert(id, index);
        self.threads.push(ThreadState::default());
        Some(index)
    }

    fn new_site(&mut self, key: SiteKey) -> Option<usize> {
        if let Some(index) = self.keys.get(&key).copied() {
            self.sites[index].calls += 1;
            return Some(index);
        }
        let auxiliary = matches!(key.phase, "initialize" | "warmup");
        if self.sites.len() == SITE_LIMIT
            || (auxiliary && self.auxiliary_sites == AUXILIARY_SITE_LIMIT)
        {
            self.dropped_sites += 1;
            *self.dropped_sites_by_phase.entry(key.phase).or_default() += 1;
            return None;
        }
        if auxiliary {
            self.auxiliary_sites += 1;
        }
        let index = self.sites.len();
        let mut site = Site::new(key.clone());
        site.calls = 1;
        self.sites.push(site);
        self.keys.insert(key, index);
        Some(index)
    }

    fn enter(&mut self, span: u64, site: usize, thread: ThreadId, now: u64) {
        self.sites[site].enters += 1;
        let Some(thread) = self.thread(thread) else {
            return;
        };
        if self.threads[thread].suppressed_depth > 0
            || self.threads[thread].stack.len() == DEPTH_LIMIT
        {
            self.threads[thread].suppressed_depth =
                self.threads[thread].suppressed_depth.saturating_add(1);
            self.dropped_depth += 1;
            return;
        }
        if self
            .threads
            .iter()
            .enumerate()
            .any(|(index, state)| index != thread && !state.stack.is_empty())
        {
            self.concurrency_observed = true;
        }
        let entered_threads = self.active_spans.entry(span).or_default();
        if entered_threads.get(&thread).is_some_and(|count| *count > 0) {
            self.reentries += 1;
        }
        if entered_threads.keys().any(|index| *index != thread) {
            self.concurrent_span_enters += 1;
        }
        *entered_threads.entry(thread).or_default() += 1;
        let actual_parent = self.threads[thread].stack.last().map(|frame| frame.site);
        if self.sites[site].key.parent != actual_parent {
            self.non_nested_parents += 1;
        }
        if self.sites[site].key.phase != current_phase() {
            self.phase_mismatches += 1;
        }
        self.threads[thread].stack.push(Activation {
            span,
            site,
            start_ns: now,
            child_ns: 0,
        });
    }

    fn exit(&mut self, span: u64, site: usize, thread: ThreadId, now: u64, trace: bool) {
        self.sites[site].exits += 1;
        let Some(thread) = self.thread_ids.get(&thread).copied() else {
            return;
        };
        if self.threads[thread].suppressed_depth > 0 {
            self.threads[thread].suppressed_depth -= 1;
            return;
        }
        let Some(position) = self.threads[thread]
            .stack
            .iter()
            .rposition(|frame| frame.span == span)
        else {
            self.imbalances += 1;
            return;
        };
        if position + 1 != self.threads[thread].stack.len() {
            self.imbalances += 1;
        }
        let frame = self.threads[thread].stack.remove(position);
        if let Some(entered_threads) = self.active_spans.get_mut(&span) {
            if let Some(count) = entered_threads.get_mut(&thread) {
                *count -= 1;
                if *count == 0 {
                    entered_threads.remove(&thread);
                }
            }
            if entered_threads.is_empty() {
                self.active_spans.remove(&span);
            }
        }
        if now < frame.start_ns {
            self.clock_errors += 1;
        }
        let duration = now.saturating_sub(frame.start_ns);
        if frame.child_ns > duration {
            self.clock_errors += 1;
        }
        let exclusive = duration.saturating_sub(frame.child_ns);
        self.sites[frame.site].inclusive_ns =
            self.sites[frame.site].inclusive_ns.saturating_add(duration);
        self.sites[frame.site].exclusive_ns = self.sites[frame.site]
            .exclusive_ns
            .saturating_add(exclusive);
        if let Some(parent) = self.threads[thread].stack.last_mut() {
            parent.child_ns = parent.child_ns.saturating_add(duration);
        }
        if trace && !matches!(self.sites[frame.site].key.phase, "initialize" | "warmup") {
            if self.trace.len() < EVENT_LIMIT {
                self.trace.push(TraceEvent {
                    site: frame.site,
                    thread,
                    start_ns: frame.start_ns,
                    duration_ns: duration,
                });
            } else {
                self.dropped_trace_events += 1;
            }
        }
    }

    fn close(&mut self, site: Option<usize>, phase: &'static str, outcomes: Outcomes) {
        self.closed_spans += 1;
        *self.closed_by_phase.entry(phase).or_default() += 1;
        let Some(site) = site else { return };
        let row = &mut self.sites[site];
        row.closed += 1;
        match outcomes.ok {
            Some(true) => row.ok += 1,
            Some(false) => row.error += 1,
            None => row.unknown += 1,
        }
        match outcomes.found {
            Some(true) => row.found += 1,
            Some(false) => row.missing += 1,
            None => {}
        }
        match outcomes.owned {
            Some(true) => row.owned += 1,
            Some(false) => row.borrowed += 1,
            None => {}
        }
        if outcomes.delegated == Some(true) {
            row.delegated += 1;
        }
    }

    fn stack_label(&self, index: usize) -> String {
        let site = &self.sites[index];
        let mut label = site.key.label.span.clone();
        if let Some(name) = site.key.label.strings.get("name") {
            label.push(':');
            label.push_str(name);
        }
        if let Some(template) = site.key.label.strings.get("template") {
            label.push('@');
            label.push_str(template);
        }
        for name in ["buffer_id", "line", "column"] {
            if let Some(value) = site.key.label.numbers.get(name) {
                use std::fmt::Write as _;
                let _ = write!(label, " {name}={value}");
            }
        }
        label
    }

    fn report(&self, detailed: bool, trace_events: bool, elapsed_ns: u64) -> Value {
        let active = self
            .threads
            .iter()
            .map(|thread| thread.stack.len())
            .sum::<usize>();
        let suppressed = self
            .threads
            .iter()
            .map(|thread| thread.suppressed_depth)
            .sum::<usize>();
        let open = self.new_spans.saturating_sub(self.closed_spans);
        let exclusive_valid = self.imbalances == 0
            && self.concurrent_span_enters == 0
            && self.non_nested_parents == 0
            && self.clock_errors == 0
            && self.dropped_depth == 0
            && self.dropped_threads == 0
            && self.dropped_sites == 0
            && active == 0
            && suppressed == 0
            && !self.lock_poisoned;
        let selected_site_drops: u64 = self
            .dropped_sites_by_phase
            .iter()
            .filter(|(phase, _)| !matches!(**phase, "initialize" | "warmup"))
            .map(|(_, drops)| drops)
            .sum();
        let selected_complete = self.imbalances == 0
            && self.concurrent_span_enters == 0
            && self.non_nested_parents == 0
            && self.clock_errors == 0
            && self.dropped_depth == 0
            && self.dropped_threads == 0
            && selected_site_drops == 0
            && active == 0
            && suppressed == 0
            && !self.lock_poisoned
            && open == 0
            && self.phase_mismatches == 0
            && self.dropped_trace_events == 0;
        let complete = exclusive_valid
            && open == 0
            && self.phase_mismatches == 0
            && self.dropped_trace_events == 0;
        let rows: Vec<_> = self
            .sites
            .iter()
            .enumerate()
            .map(|(index, site)| {
                json!({
                    "id": index,
                    "parent_id": site.key.parent,
                    "phase": site.key.phase,
                    "span": site.key.label.span,
                    "target": site.key.label.target,
                    "fields": site.key.label.strings,
                    "coordinates": site.key.label.numbers,
                    "calls": site.calls,
                    "enters": site.enters,
                    "exits": site.exits,
                    "closed": site.closed,
                    "ok": site.ok,
                    "error": site.error,
                    "unknown_outcome": site.unknown,
                    "found": site.found,
                    "missing": site.missing,
                    "owned": site.owned,
                    "borrowed": site.borrowed,
                    "delegated": site.delegated,
                    "inclusive_active_wall_ns": site.inclusive_ns,
                    "exclusive_active_wall_ns": site.exclusive_ns,
                })
            })
            .collect();
        let mut phases = BTreeMap::<&'static str, Value>::new();
        for (phase, created) in &self.created_by_phase {
            let phase_rows: Vec<_> = self
                .sites
                .iter()
                .filter(|site| site.key.phase == *phase)
                .collect();
            let sum = |get: fn(&Site) -> u64| phase_rows.iter().map(|site| get(site)).sum::<u64>();
            let calls = sum(|site| site.calls);
            let enters = sum(|site| site.enters);
            let exits = sum(|site| site.exits);
            let closed = self.closed_by_phase.get(phase).copied().unwrap_or(0);
            let drops = self.dropped_sites_by_phase.get(phase).copied().unwrap_or(0);
            let selected = !matches!(*phase, "initialize" | "warmup");
            phases.insert(phase, json!({
                "selected": selected, "new_spans": created, "closed_spans": closed,
                "recorded_sites": phase_rows.len(), "recorded_calls": calls,
                "recorded_enters": enters, "recorded_exits": exits, "entered_balanced": enters == exits,
                "ok": sum(|site| site.ok), "error": sum(|site| site.error),
                "unknown_outcome": sum(|site| site.unknown), "dropped_sites": drops,
                "complete": if selected { selected_complete } else { complete },
            }));
        }
        let mut folded = BTreeMap::<String, u64>::new();
        for (index, site) in self.sites.iter().enumerate() {
            if site.exclusive_ns == 0 {
                continue;
            }
            let mut parts = vec![self.stack_label(index)];
            let mut parent = site.key.parent;
            for _ in 0..DEPTH_LIMIT {
                let Some(index) = parent else { break };
                parts.push(self.stack_label(index));
                parent = self.sites[index].key.parent;
            }
            parts.push(site.key.phase.to_owned());
            parts.reverse();
            let weight = folded.entry(parts.join(";")).or_default();
            *weight = weight.saturating_add(site.exclusive_ns);
        }
        let events: Vec<_> = self
            .trace
            .iter()
            .map(|event| {
                json!({
                    "name": self.stack_label(event.site),
                    "cat": self.sites[event.site].key.phase,
                    "ph": "X",
                    "pid": 1,
                    "tid": event.thread + 1,
                    "ts": event.start_ns as f64 / 1000.0,
                    "dur": event.duration_ns as f64 / 1000.0,
                    "args": {"site_id": event.site},
                })
            })
            .collect();
        json!({
            "schema_version": 1,
            "clock": "monotonic_instrumented_active_wall",
            "time_unit": "nanoseconds",
            "detailed": detailed,
            "trace_events_enabled": trace_events,
            "complete": complete,
            "exclusive_valid": exclusive_valid,
            "selected_phases_complete": selected_complete,
            "selected_phases": ["measured", "single", "serve", "render", "other"],
            "auxiliary_policy": "initialize/warmup: DEBUG-or-higher aggregates, combined 512-site cap, no timeline events",
            "exclusive_scope": "well_nested_same_thread_activations_only",
            "collection_elapsed_ns": elapsed_ns,
            "calls_definition": "unique entered included span instances; enters/exits count activations separately",
            "outcomes_definition": "typed final ok/outcome fields on span close; absent outcomes stay unknown",
            "limits": {"sites": SITE_LIMIT, "auxiliary_sites": AUXILIARY_SITE_LIMIT, "depth": DEPTH_LIMIT, "trace_events": EVENT_LIMIT,
                "threads": THREAD_LIMIT, "label_bytes": LABEL_LIMIT},
            "counts": {"new_spans": self.new_spans, "closed_spans": self.closed_spans,
                "open_spans": open, "active_activations": active, "suppressed_depth": suppressed,
                "threads": self.threads.len(), "reentries": self.reentries,
                "concurrent_span_enters": self.concurrent_span_enters,
                "non_nested_parents": self.non_nested_parents, "imbalances": self.imbalances,
                "phase_mismatches": self.phase_mismatches, "clock_errors": self.clock_errors,
                "dropped_sites": self.dropped_sites, "dropped_sites_by_phase": self.dropped_sites_by_phase, "dropped_depth": self.dropped_depth,
                "dropped_threads": self.dropped_threads, "dropped_trace_events": self.dropped_trace_events,
                "rejected_fields": self.rejected_fields},
            "concurrency_observed": self.concurrency_observed,
            "lock_poisoned": self.lock_poisoned,
            "phases": phases,
            "aggregates": rows,
            "folded_stacks": folded.into_iter().map(|(stack, weight)| format!("{stack} {weight}")).collect::<Vec<_>>(),
            "folded_weight": "exclusive instrumented active wall nanoseconds; not CPU samples",
            "traceEvents": events,
            "displayTimeUnit": "ns",
            "caveats": [
                "Instrumentation adds overhead; active wall includes scheduling and is not CPU time.",
                "Exclusive time subtracts only recorded child activations on the same thread; filtered work remains in self time.",
                "Inclusive aggregates overlap and must not be summed as elapsed request time.",
                "Unwind guards exit spans; panic=abort, process::exit and forced termination cannot guarantee a report.",
                "Parse buffer coordinates are opaque unless the host explicitly provides a safe template label.",
                "Incomplete exports retain partial evidence; omitted events or unsupported nesting must not be treated as exact attribution."
            ]
        })
    }
}

struct Inner {
    detailed: bool,
    trace_events: bool,
    clock: Arc<dyn Clock>,
    state: Mutex<State>,
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        match self.state.lock() {
            Ok(state) => state,
            Err(error) => {
                let mut state = error.into_inner();
                state.lock_poisoned = true;
                state
            }
        }
    }
}

#[derive(Clone)]
struct Collector(Arc<Inner>);

impl<S> Layer<S> for Collector
where
    S: Subscriber + for<'span> LookupSpan<'span>,
{
    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(if self.0.detailed {
            tracing::level_filters::LevelFilter::TRACE
        } else {
            tracing::level_filters::LevelFilter::DEBUG
        })
    }

    fn register_callsite(
        &self,
        _metadata: &'static Metadata<'static>,
    ) -> tracing::subscriber::Interest {
        // Phase-sensitive TRACE filtering must not be cached as Always/Never.
        tracing::subscriber::Interest::sometimes()
    }

    fn enabled(&self, metadata: &Metadata<'_>, _ctx: Context<'_, S>) -> bool {
        metadata.is_span()
            && (metadata.target() == "liquid::profile"
                || metadata.target().starts_with("liquid::profile::")
                || metadata.target() == "horizon::profile"
                || metadata.target().starts_with("horizon::profile::"))
            && (*metadata.level() <= tracing::Level::DEBUG
                || (self.0.detailed && !matches!(current_phase(), "initialize" | "warmup")))
    }

    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let parent = span.scope().skip(1).find_map(|parent| {
            parent
                .extensions()
                .get::<SpanInfo>()
                .and_then(|info| info.site)
        });
        let mut fields = Fields::default();
        attributes.record(&mut fields);
        let metadata = attributes.metadata();
        fields.label.span = if safe_identifier(metadata.name()) {
            metadata.name().to_owned()
        } else {
            "redacted".to_owned()
        };
        // Targets are accepted above but still copied with a fixed byte bound.
        fields.label.target = metadata.target().chars().take(LABEL_LIMIT).collect();
        let mut state = self.0.state();
        state.new_spans += 1;
        *state.created_by_phase.entry(current_phase()).or_default() += 1;
        state.rejected_fields += fields.rejected;
        drop(state);
        // Resolve the site at first entry, after parser wrappers record names.
        span.extensions_mut().insert(SpanInfo {
            site: None,
            resolved: false,
            key: SiteKey {
                parent,
                phase: current_phase(),
                label: fields.label,
            },
            outcomes: fields.outcomes,
        });
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut fields = Fields::default();
        values.record(&mut fields);
        self.0.state().rejected_fields += fields.rejected;
        let mut extensions = span.extensions_mut();
        let Some(info) = extensions.get_mut::<SpanInfo>() else {
            return;
        };
        if !info.resolved {
            info.key.label.strings.extend(fields.label.strings);
            info.key.label.numbers.extend(fields.label.numbers);
        }
        for (target, source) in [
            (&mut info.outcomes.ok, fields.outcomes.ok),
            (&mut info.outcomes.found, fields.outcomes.found),
            (&mut info.outcomes.owned, fields.outcomes.owned),
            (&mut info.outcomes.delegated, fields.outcomes.delegated),
        ] {
            if source.is_some() {
                *target = source;
            }
        }
    }

    fn on_enter(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let mut extensions = span.extensions_mut();
        let Some(info) = extensions.get_mut::<SpanInfo>() else {
            return;
        };
        let now = self.0.clock.now_ns();
        let mut state = self.0.state();
        if !info.resolved {
            info.site = state.new_site(info.key.clone());
            info.resolved = true;
        }
        if let Some(site) = info.site {
            state.enter(id.into_u64(), site, std::thread::current().id(), now);
        }
    }

    fn on_exit(&self, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let site = span
            .extensions()
            .get::<SpanInfo>()
            .and_then(|info| info.site);
        if let Some(site) = site {
            // Match entry: timestamp before waiting for the collector mutex.
            let now = self.0.clock.now_ns();
            self.0.state().exit(
                id.into_u64(),
                site,
                std::thread::current().id(),
                now,
                self.0.trace_events,
            );
        }
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let extensions = span.extensions();
        let Some(info) = extensions.get::<SpanInfo>() else {
            return;
        };
        self.0
            .state()
            .close(info.site, info.key.phase, info.outcomes);
    }
}

pub(super) struct Controller {
    inner: Arc<Inner>,
    dispatch: Dispatch,
}

impl Controller {
    pub(super) fn new(detailed: bool, trace_events: bool) -> Self {
        Self::with_clock(detailed, trace_events, Arc::new(WallClock(Instant::now())))
    }

    fn with_clock(detailed: bool, trace_events: bool, clock: Arc<dyn Clock>) -> Self {
        let inner = Arc::new(Inner {
            detailed,
            trace_events,
            clock,
            state: Mutex::new(State::default()),
        });
        let subscriber = tracing_subscriber::registry().with(Collector(Arc::clone(&inner)));
        Self {
            inner,
            dispatch: Dispatch::new(subscriber),
        }
    }

    pub(super) fn with_default<T>(&self, action: impl FnOnce() -> T) -> T {
        tracing::dispatcher::with_default(&self.dispatch, action)
    }

    pub(super) fn report(&self) -> Value {
        self.inner.state().report(
            self.inner.detailed,
            self.inner.trace_events,
            self.inner.clock.now_ns(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Barrier;

    #[derive(Default)]
    struct ManualClock(AtomicU64);

    impl Clock for ManualClock {
        fn now_ns(&self) -> u64 {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn row<'a>(report: &'a Value, name: &str) -> &'a Value {
        report["aggregates"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["span"] == name)
            .unwrap()
    }

    #[test]
    fn active_wall_excludes_child_and_idle_lifetime_with_exact_clock() {
        let clock = Arc::new(ManualClock::default());
        let controller = Controller::with_clock(true, true, clock.clone());
        controller.with_default(|| {
            let _phase = phase("measured");
            let parent = tracing::debug_span!(target: "horizon::profile", "render", ok = tracing::field::Empty);
            let guard = parent.enter();
            clock.0.store(10, Ordering::SeqCst);
            {
                let child = tracing::trace_span!(target: "liquid::profile::filter", "liquid.filter", name = "times", ok = true);
                let _child = child.enter();
                clock.0.store(30, Ordering::SeqCst);
            }
            clock.0.store(50, Ordering::SeqCst);
            drop(guard);
            clock.0.store(1000, Ordering::SeqCst);
            parent.record("ok", true);
        });
        let report = controller.report();
        assert_eq!(report["complete"], true);
        assert_eq!(row(&report, "render")["inclusive_active_wall_ns"], 50);
        assert_eq!(row(&report, "render")["exclusive_active_wall_ns"], 30);
        assert_eq!(
            row(&report, "liquid.filter")["exclusive_active_wall_ns"],
            20
        );
        assert_eq!(row(&report, "render")["ok"], 1);
        assert_eq!(report["traceEvents"].as_array().unwrap().len(), 2);
        assert!(report["folded_stacks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|value| value.as_str().unwrap().ends_with("times 20")));
    }

    #[test]
    fn sequential_controllers_and_default_dispatch_do_not_leak() {
        let first = Controller::new(false, false);
        let second = Controller::new(false, false);
        first.with_default(|| {
            let _guard = tracing::debug_span!(target: "horizon::profile", "first").entered();
        });
        second.with_default(|| {
            let _guard = tracing::debug_span!(target: "horizon::profile", "second").entered();
            let _excluded = tracing::trace_span!(target: "liquid::profile", "detail").entered();
            let _unrelated = tracing::debug_span!(target: "unrelated", "unrelated").entered();
        });
        let _outside = tracing::debug_span!(target: "horizon::profile", "outside").entered();
        assert_eq!(first.report()["counts"]["new_spans"], 1);
        assert_eq!(second.report()["counts"]["new_spans"], 1);
        assert_eq!(first.report()["complete"], true);
        assert_eq!(second.report()["complete"], true);
    }

    #[test]
    fn level_hints_match_coarse_and_phase_sensitive_detailed_filtering() {
        fn emit() {
            let _parent =
                tracing::debug_span!(target: "horizon::profile", "template", ok = true).entered();
            let _child =
                tracing::trace_span!(target: "liquid::profile", "detail", ok = true).entered();
        }
        let coarse = Controller::new(false, false);
        let coarse_subscriber =
            tracing_subscriber::registry().with(Collector(Arc::clone(&coarse.inner)));
        assert_eq!(
            coarse_subscriber.max_level_hint(),
            Some(tracing::level_filters::LevelFilter::DEBUG)
        );
        coarse.with_default(|| {
            {
                let _phase = phase("warmup");
                emit();
            }
            {
                let _phase = phase("measured");
                emit();
            }
        });
        assert_eq!(coarse.report()["counts"]["new_spans"], 2);
        assert!(!coarse.report()["aggregates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|site| site["span"] == "detail"));

        let detailed = Controller::new(true, false);
        let detailed_subscriber =
            tracing_subscriber::registry().with(Collector(Arc::clone(&detailed.inner)));
        assert_eq!(
            detailed_subscriber.max_level_hint(),
            Some(tracing::level_filters::LevelFilter::TRACE)
        );
        detailed.with_default(|| {
            {
                let _phase = phase("warmup");
                emit();
            }
            {
                let _phase = phase("measured");
                emit();
            }
        });
        let report = detailed.report();
        assert_eq!(report["counts"]["new_spans"], 3);
        assert_eq!(row(&report, "detail")["phase"], "measured");
        assert_eq!(row(&report, "detail")["calls"], 1);
        assert_eq!(report["complete"], true);
    }

    #[test]
    fn concurrent_requests_have_independent_stacks() {
        let controller = Arc::new(Controller::new(true, false));
        let barrier = Arc::new(Barrier::new(2));
        let handles: Vec<_> = (0..2).map(|_| {
            let controller = Arc::clone(&controller);
            let barrier = Arc::clone(&barrier);
            std::thread::spawn(move || controller.with_default(|| {
                let _phase = phase("serve");
                let _root = tracing::debug_span!(target: "horizon::profile", "request", ok = true).entered();
                barrier.wait();
                let _child = tracing::trace_span!(target: "liquid::profile", "liquid.lookup", found = true).entered();
                barrier.wait();
            }))
        }).collect();
        for handle in handles {
            handle.join().unwrap();
        }
        let report = controller.report();
        assert_eq!(report["complete"], true);
        assert_eq!(report["concurrency_observed"], true);
        assert_eq!(report["counts"]["threads"], 2);
        assert_eq!(row(&report, "request")["calls"], 2);
        assert_eq!(row(&report, "liquid.lookup")["found"], 2);
        assert_eq!(report["counts"]["non_nested_parents"], 0);
    }

    #[test]
    fn sharing_one_span_across_threads_is_explicitly_not_an_exact_call_tree() {
        let controller = Arc::new(Controller::new(true, false));
        let barrier = Arc::new(Barrier::new(2));
        let span = controller
            .with_default(|| tracing::debug_span!(target: "horizon::profile", "shared", ok = true));
        let handles: Vec<_> = (0..2)
            .map(|_| {
                let controller = Arc::clone(&controller);
                let barrier = Arc::clone(&barrier);
                let span = span.clone();
                std::thread::spawn(move || {
                    controller.with_default(|| {
                        let _guard = span.enter();
                        barrier.wait();
                        barrier.wait();
                    });
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }
        drop(span);
        let report = controller.report();
        assert_eq!(report["counts"]["concurrent_span_enters"], 1);
        assert_eq!(row(&report, "shared")["calls"], 1);
        assert_eq!(row(&report, "shared")["enters"], 2);
        assert_eq!(report["exclusive_valid"], false);
        assert_eq!(report["counts"]["active_activations"], 0);
        assert!(controller.inner.state().active_spans.is_empty());
    }

    #[test]
    fn errors_and_unwind_restore_phase_and_activation_stack() {
        let controller = Controller::new(true, false);
        let result: Result<(), ()> = controller.with_default(|| {
            let _phase = phase("single");
            let span = tracing::debug_span!(target: "horizon::profile", "error", ok = false);
            let _entered = span.enter();
            Err(())
        });
        assert!(result.is_err());
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            controller.with_default(|| {
                let _phase = phase("warmup");
                let _guard = tracing::debug_span!(target: "horizon::profile", "unwind").entered();
                panic!("synthetic unwind");
            });
        }));
        assert!(unwind.is_err());
        assert_eq!(current_phase(), "render");
        let report = controller.report();
        assert_eq!(report["complete"], true);
        assert_eq!(row(&report, "error")["error"], 1);
        assert_eq!(row(&report, "unwind")["unknown_outcome"], 1);
        assert_eq!(report["counts"]["active_activations"], 0);
    }

    #[test]
    fn reentry_counts_activations_without_claiming_nested_call_tree() {
        let clock = Arc::new(ManualClock::default());
        let controller = Controller::with_clock(true, false, clock.clone());
        controller.with_default(|| {
            let span = tracing::debug_span!(target: "horizon::profile", "reentry", ok = true);
            let outer = span.enter();
            clock.0.store(10, Ordering::SeqCst);
            let inner = span.enter();
            clock.0.store(20, Ordering::SeqCst);
            drop(inner);
            clock.0.store(30, Ordering::SeqCst);
            drop(outer);
        });
        let report = controller.report();
        assert_eq!(report["counts"]["reentries"], 1);
        assert_eq!(row(&report, "reentry")["calls"], 1);
        assert_eq!(row(&report, "reentry")["enters"], 2);
        assert_eq!(row(&report, "reentry")["exits"], 2);
        assert_eq!(report["exclusive_valid"], false);
    }

    #[test]
    fn labels_are_bounded_and_debug_values_errors_and_paths_are_not_recorded() {
        struct MustNotFormat;
        impl fmt::Debug for MustNotFormat {
            fn fmt(&self, _: &mut fmt::Formatter<'_>) -> fmt::Result {
                panic!("collector formatted an argument")
            }
        }
        let controller = Controller::new(true, true);
        controller.with_default(|| {
            let span = tracing::trace_span!(target: "liquid::profile", "liquid.node",
                name = "tag with SECRET argument", template = "/private/SECRET.liquid",
                input = "SECRET", error = "SECRET", args = ?MustNotFormat,
                line = 12_u64, mode = "optional", ok = tracing::field::Empty);
            let _entered = span.enter();
            span.record("ok", false);
        });
        let report = controller.report();
        assert!(!report.to_string().contains("SECRET"));
        assert_eq!(report["counts"]["rejected_fields"], 2);
        assert_eq!(row(&report, "liquid.node")["coordinates"]["line"], 12);
        assert_eq!(row(&report, "liquid.node")["error"], 1);
    }

    #[test]
    fn names_recorded_before_entry_and_phase_filtering_use_the_final_key() {
        fn emit() {
            let span = tracing::trace_span!(target: "liquid::profile::node", "liquid.node", name = tracing::field::Empty, ok = true);
            span.record("name", "assign");
            let _guard = span.enter();
        }
        let controller = Controller::new(true, true);
        controller.with_default(|| {
            {
                let _phase = phase("warmup");
                emit();
            }
            {
                let _phase = phase("measured");
                emit();
            }
        });
        let report = controller.report();
        assert_eq!(report["counts"]["new_spans"], 1);
        assert_eq!(row(&report, "liquid.node")["fields"]["name"], "assign");
        assert_eq!(report["phases"]["measured"]["ok"], 1);
        assert_eq!(report["phases"]["measured"]["complete"], true);
        assert_eq!(report["traceEvents"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn inline_comment_label_accepts_only_the_exact_registered_name() {
        let controller = Controller::new(true, true);
        controller.with_default(|| {
            let span = tracing::trace_span!(target: "liquid::profile::node", "liquid.node",
                kind = "tag", name = tracing::field::Empty,
                comment = "SECRET comment text", ok = true);
            span.record("name", "#");
            let _entered = span.enter();
        });
        controller.with_default(|| {
            let span = tracing::trace_span!(target: "liquid::profile::node", "rejected.comment",
                name = "# SECRET comment text", kind = "#", template = "#");
            let _entered = span.enter();
        });
        let report = controller.report();
        assert_eq!(row(&report, "liquid.node")["fields"]["name"], "#");
        assert_eq!(row(&report, "liquid.node")["ok"], 1);
        assert_eq!(row(&report, "rejected.comment")["fields"], json!({}));
        assert_eq!(report["counts"]["rejected_fields"], 3);
        assert!(!report.to_string().contains("SECRET"));
    }

    #[test]
    fn auxiliary_site_overflow_preserves_selected_capacity_and_is_visible() {
        let mut state = State::default();
        for index in 0..AUXILIARY_SITE_LIMIT {
            let mut auxiliary = key(index as u64);
            auxiliary.phase = "warmup";
            assert!(state.new_site(auxiliary).is_some());
        }
        let mut overflow = key(AUXILIARY_SITE_LIMIT as u64);
        overflow.phase = "warmup";
        assert_eq!(state.new_site(overflow), None);
        assert!(state.new_site(key(99999)).is_some());
        let report = state.report(true, false, 0);
        assert_eq!(report["complete"], false);
        assert_eq!(report["selected_phases_complete"], true);
        assert_eq!(report["counts"]["dropped_sites_by_phase"]["warmup"], 1);
    }

    #[test]
    fn out_of_order_exits_are_flagged_and_do_not_panic_or_leave_active_ids() {
        let controller = Controller::new(true, false);
        controller.with_default(|| {
            let outer = tracing::debug_span!(target: "horizon::profile", "outer").entered();
            let inner = tracing::debug_span!(target: "horizon::profile", "inner").entered();
            drop(outer);
            drop(inner);
        });
        let report = controller.report();
        assert_eq!(report["complete"], false);
        assert_eq!(report["counts"]["imbalances"], 1);
        assert_eq!(report["counts"]["active_activations"], 0);
        assert!(controller.inner.state().active_spans.is_empty());
    }

    fn key(index: u64) -> SiteKey {
        let mut label = Label {
            span: "node".to_owned(),
            ..Label::default()
        };
        label.numbers.insert("line", index);
        SiteKey {
            parent: None,
            phase: "render",
            label,
        }
    }

    #[test]
    fn site_depth_thread_and_trace_caps_mark_partial_evidence() {
        let mut state = State::default();
        for index in 0..SITE_LIMIT {
            assert_eq!(state.new_site(key(index as u64)), Some(index));
        }
        assert_eq!(state.new_site(key(SITE_LIMIT as u64)), None);
        assert_eq!(state.sites.len(), SITE_LIMIT);
        let thread = std::thread::current().id();
        for index in 0..(DEPTH_LIMIT + 3) {
            state.enter(index as u64, 0, thread, index as u64);
        }
        assert_eq!(state.threads[0].stack.len(), DEPTH_LIMIT);
        assert_eq!(state.threads[0].suppressed_depth, 3);
        for index in (0..(DEPTH_LIMIT + 3)).rev() {
            state.exit(index as u64, 0, thread, 1000, false);
        }
        assert!(state.threads[0].stack.is_empty());
        assert_eq!(state.threads[0].suppressed_depth, 0);
        state.trace = (0..EVENT_LIMIT)
            .map(|_| TraceEvent {
                site: 0,
                thread: 0,
                start_ns: 0,
                duration_ns: 1,
            })
            .collect();
        state.enter(9999, 0, thread, 1001);
        state.exit(9999, 0, thread, 1002, true);
        assert_eq!(state.trace.len(), EVENT_LIMIT);
        assert_eq!(state.dropped_trace_events, 1);
        // Distinct real thread IDs exercise the fixed thread registry without render work.
        for _ in 0..THREAD_LIMIT {
            let id = std::thread::spawn(|| std::thread::current().id())
                .join()
                .unwrap();
            let _ = state.thread(id);
        }
        assert_eq!(state.threads.len(), THREAD_LIMIT);
        assert_eq!(state.dropped_threads, 1);
        assert_eq!(state.report(true, true, 2000)["complete"], false);
        assert!(state.active_spans.is_empty());
    }
}
