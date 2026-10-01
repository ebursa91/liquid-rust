#![cfg(feature = "profiling")]

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::{self, Write};
use std::sync::{Arc, Mutex};

use liquid_core::parser::{
    self, BlockReflection, FilterArguments, FilterReflection, Language, ParameterReflection,
    ParseBlock, ParseFilter, ParseTag, TagBlock, TagReflection, TagTokenIter,
};
use liquid_core::runtime::{RuntimeBuilder, SandboxedStackFrame, StackFrame};
use liquid_core::{
    Error, Expression, Filter, Renderable, Result, Runtime, Template, Value, ValueCow, ValueView,
};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::Subscriber;
use tracing_subscriber::layer::{Context, Layer};
use tracing_subscriber::prelude::*;
use tracing_subscriber::registry::LookupSpan;

#[derive(Clone, Debug, PartialEq)]
enum FieldValue {
    Number(u64),
    Bool(bool),
    Text(String),
}
#[derive(Default, Debug)]
struct SpanRecord {
    name: String,
    parent: Option<u64>,
    fields: BTreeMap<String, FieldValue>,
    entries: usize,
    exits: usize,
    closed: bool,
}
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<State>>);
#[derive(Default)]
struct State {
    records: HashMap<u64, SpanRecord>,
    active: HashMap<u64, u64>,
    next: u64,
}
struct Fields<'a>(&'a mut BTreeMap<String, FieldValue>);
impl Visit for Fields<'_> {
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0
            .insert(field.name().into(), FieldValue::Number(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_u64(field, u64::try_from(value).unwrap());
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.insert(field.name().into(), FieldValue::Bool(value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0
            .insert(field.name().into(), FieldValue::Text(value.into()));
    }
    fn record_debug(&mut self, field: &Field, _value: &dyn fmt::Debug) {
        panic!("profiling must not record Debug values: {}", field.name());
    }
}
impl<S: Subscriber + for<'a> LookupSpan<'a>> Layer<S> for Capture {
    fn on_new_span(&self, attributes: &Attributes<'_>, id: &Id, context: Context<'_, S>) {
        let mut state = self.0.lock().unwrap();
        let parent = attributes
            .parent()
            .map(Id::into_u64)
            .or_else(|| context.current_span().id().map(Id::into_u64))
            .map(|id| state.active[&id]);
        let mut record = SpanRecord {
            name: attributes.metadata().name().into(),
            parent,
            ..SpanRecord::default()
        };
        attributes.record(&mut Fields(&mut record.fields));
        state.next += 1;
        let key = state.next;
        assert!(state.active.insert(id.into_u64(), key).is_none());
        state.records.insert(key, record);
    }
    fn on_record(&self, id: &Id, values: &Record<'_>, _context: Context<'_, S>) {
        let mut state = self.0.lock().unwrap();
        let key = state.active[&id.into_u64()];
        values.record(&mut Fields(
            &mut state.records.get_mut(&key).unwrap().fields,
        ));
    }
    fn on_enter(&self, id: &Id, _context: Context<'_, S>) {
        let mut state = self.0.lock().unwrap();
        let key = state.active[&id.into_u64()];
        state.records.get_mut(&key).unwrap().entries += 1;
    }
    fn on_exit(&self, id: &Id, _context: Context<'_, S>) {
        let mut state = self.0.lock().unwrap();
        let key = state.active[&id.into_u64()];
        state.records.get_mut(&key).unwrap().exits += 1;
    }
    fn on_close(&self, id: Id, _context: Context<'_, S>) {
        let mut state = self.0.lock().unwrap();
        let key = state.active.remove(&id.into_u64()).unwrap();
        state.records.get_mut(&key).unwrap().closed = true;
    }
}
impl Capture {
    fn run<T>(&self, operation: impl FnOnce() -> T) -> T {
        tracing::subscriber::with_default(
            tracing_subscriber::registry().with(self.clone()),
            operation,
        )
    }
    fn assert_balanced(&self) {
        let state = self.0.lock().unwrap();
        assert!(state.active.is_empty());
        let records = &state.records;
        assert!(!records.is_empty());
        assert!(
            records
                .values()
                .all(|r| r.entries == 1 && r.exits == 1 && r.closed),
            "{records:?}"
        );
    }
}

#[derive(Clone)]
struct Probe;
impl FilterReflection for Probe {
    fn name(&self) -> &str {
        "probe"
    }
    fn description(&self) -> &str {
        "Original profiling regression filter"
    }
    fn positional_parameters(&self) -> &'static [ParameterReflection] {
        &[]
    }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] {
        &[]
    }
}
impl ParseFilter for Probe {
    fn parse(&self, mut arguments: FilterArguments<'_>) -> Result<Box<dyn Filter>> {
        Ok(Box::new(ProbeFilter {
            selector: arguments.positional.next(),
        }))
    }
    fn reflection(&self) -> &dyn FilterReflection {
        self
    }
}
#[derive(Debug)]
struct ProbeFilter {
    selector: Option<Expression>,
}
impl fmt::Display for ProbeFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("probe: SECRET-DISPLAY-ARG")
    }
}
impl Filter for ProbeFilter {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        if let Some(selector) = &self.selector {
            let value = selector.evaluate(runtime)?;
            if value.to_kstr() == "reject" {
                return Err(Error::with_msg("SECRET-FILTER-ERROR"));
            }
        }
        Ok(input.to_value())
    }
}

#[derive(Clone)]
struct Nest;
impl BlockReflection for Nest {
    fn start_tag(&self) -> &str {
        "nest"
    }
    fn end_tag(&self) -> &str {
        "endnest"
    }
    fn description(&self) -> &str {
        "Original nested test body"
    }
}
impl ParseBlock for Nest {
    fn parse(
        &self,
        mut arguments: TagTokenIter<'_>,
        mut block: TagBlock<'_, '_>,
        options: &Language,
    ) -> Result<Box<dyn Renderable>> {
        arguments.expect_nothing()?;
        let body = Template::new(block.parse_all(options)?);
        block.assert_empty();
        Ok(Box::new(body))
    }
    fn reflection(&self) -> &dyn BlockReflection {
        self
    }
}

#[derive(Clone)]
struct Side;
impl TagReflection for Side {
    fn tag(&self) -> &str {
        "side"
    }
    fn description(&self) -> &str {
        "Original silent test side effect"
    }
}
impl ParseTag for Side {
    fn parse(
        &self,
        mut arguments: TagTokenIter<'_>,
        _options: &Language,
    ) -> Result<Box<dyn Renderable>> {
        arguments.expect_nothing()?;
        Ok(Box::new(SideNode))
    }
    fn reflection(&self) -> &dyn TagReflection {
        self
    }
}
#[derive(Debug)]
struct SideNode;
impl Renderable for SideNode {
    fn is_blank(&self) -> bool {
        true
    }
    fn render_to(&self, _writer: &mut dyn Write, runtime: &dyn Runtime) -> Result<()> {
        runtime.set_global("changed".into(), Value::scalar(true));
        Ok(())
    }
}

#[derive(Clone)]
struct Synth;
impl TagReflection for Synth {
    fn tag(&self) -> &str {
        "synth"
    }
    fn description(&self) -> &str {
        "Original synthesized parse-buffer test"
    }
}
impl ParseTag for Synth {
    fn parse(
        &self,
        mut arguments: TagTokenIter<'_>,
        options: &Language,
    ) -> Result<Box<dyn Renderable>> {
        arguments.expect_nothing()?;
        Ok(Box::new(Template::new(parser::parse(
            "{{ secret_value | probe: selector }}",
            options,
        )?)))
    }
    fn reflection(&self) -> &dyn TagReflection {
        self
    }
}
fn language() -> Language {
    let mut options = Language::empty();
    options.filters.register("probe".into(), Box::new(Probe));
    options.blocks.register("nest".into(), Box::new(Nest));
    options.tags.register("side".into(), Box::new(Side));
    options.tags.register("synth".into(), Box::new(Synth));
    options
}
fn parse(source: &str) -> Template {
    Template::new(parser::parse(source, &language()).unwrap())
}

#[test]
fn nested_nodes_filters_lookups_and_projections_have_real_parentage_without_values() {
    let capture = Capture::default();
    let globals = liquid_core::object!({"secret_value":"SECRET-GLOBAL-VALUE", "private_selector_key":"SECRET-SELECTOR-VALUE"});
    let template =
        parse("SECRET-TEMPLATE-TEXT\n{% nest %}{{ secret_value | probe: private_selector_key }}{% endnest %}");
    let runtime = RuntimeBuilder::new().set_globals(&globals).build();
    let frame =
        StackFrame::new(&runtime, liquid_core::object!({})).with_name("SECRET-RUNTIME-NAME");
    assert_eq!(
        capture.run(|| template.render(&frame)).unwrap(),
        "SECRET-TEMPLATE-TEXT\nSECRET-GLOBAL-VALUE"
    );
    capture.assert_balanced();
    let state = capture.0.lock().unwrap();
    let records = &state.records;
    let rendered = format!("{records:?}");
    for secret in [
        "SECRET-TEMPLATE-TEXT",
        "SECRET-GLOBAL-VALUE",
        "SECRET-SELECTOR-VALUE",
        "SECRET-RUNTIME-NAME",
        "SECRET-DISPLAY-ARG",
        "secret_value",
        "private_selector_key",
    ] {
        assert!(!rendered.contains(secret), "leaked {secret}");
    }
    let filter = records
        .values()
        .find(|r| r.name == "liquid.filter")
        .unwrap();
    assert_eq!(filter.fields["name"], FieldValue::Text("probe".into()));
    let filter_id = records
        .iter()
        .find(|(_, r)| r.name == "liquid.filter")
        .unwrap()
        .0;
    assert!(records
        .values()
        .any(|r| r.name == "liquid.lookup" && r.parent == Some(*filter_id)));
    assert!(records
        .values()
        .any(|r| r.name == "liquid.project" && r.parent == Some(*filter_id)));
    let output_node = records[&filter.parent.unwrap()].parent.unwrap();
    assert_eq!(records[&output_node].name, "liquid.output");
    let buffer_ids: Vec<_> = records
        .values()
        .filter(|r| r.name == "liquid.node")
        .map(|r| r.fields["buffer_id"].clone())
        .collect();
    assert!(buffer_ids.iter().all(|id| *id == buffer_ids[0]));
    assert!(records.values().filter(|r| r.name == "liquid.node").all(
        |r| matches!(r.fields["line"], FieldValue::Number(n) if n > 0)
            && matches!(r.fields["column"], FieldValue::Number(n) if n > 0)
    ));
}

#[test]
fn blank_trim_and_silent_side_effects_survive_private_wrappers() {
    let mut template = parse("{% nest %}\n{% side %}\n{% endnest %}");
    assert!(template.is_blank());
    template.trim_blank();
    let runtime = RuntimeBuilder::new().build();
    let capture = Capture::default();
    assert_eq!(capture.run(|| template.render(&runtime)).unwrap(), "");
    assert_eq!(
        runtime.get(&["changed".into()]).unwrap().to_value(),
        Value::scalar(true)
    );
    capture.assert_balanced();
}

#[test]
fn generated_buffers_keep_distinct_opaque_ids_and_restore_outer_buffer() {
    let template = parse("{% synth %}{{ secret_value }}");
    let globals = liquid_core::object!({"secret_value":"ok", "selector":"ok"});
    let runtime = RuntimeBuilder::new().set_globals(&globals).build();
    let capture = Capture::default();
    assert_eq!(capture.run(|| template.render(&runtime)).unwrap(), "okok");
    capture.assert_balanced();
    let state = capture.0.lock().unwrap();
    let records = &state.records;
    let outer = records
        .values()
        .find(|r| {
            r.name == "liquid.node"
                && r.fields.get("name") == Some(&FieldValue::Text("synth".into()))
        })
        .unwrap();
    let outer_id = &outer.fields["buffer_id"];
    let filters = records
        .values()
        .find(|r| r.name == "liquid.filter")
        .unwrap();
    assert_ne!(filters.fields["buffer_id"], *outer_id);
    assert!(records.values().any(|r| r.name == "liquid.node"
        && r.fields["buffer_id"] == *outer_id
        && r.fields.get("kind") == Some(&FieldValue::Text("output".into()))));
}

struct FailingWriter;
impl Write for FailingWriter {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("SECRET-WRITER-ERROR"))
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
#[test]
fn filter_and_writer_errors_are_preserved_and_close_all_spans() {
    let template = parse("{{ secret_value | probe: selector }}");
    for selector in ["reject", "ok"] {
        let globals = liquid_core::object!({"secret_value":"value", "selector":selector});
        let runtime = RuntimeBuilder::new().set_globals(&globals).build();
        let capture = Capture::default();
        let result = if selector == "reject" {
            capture.run(|| template.render(&runtime)).map(|_| ())
        } else {
            capture.run(|| template.render_to(&mut FailingWriter, &runtime))
        };
        let error = result.unwrap_err().to_string();
        if selector == "reject" {
            assert!(error.contains("SECRET-FILTER-ERROR"));
        } else {
            assert!(error.contains("Failed to render"));
        }
        capture.assert_balanced();
        let state = capture.0.lock().unwrap();
        let records = &state.records;
        assert!(records
            .values()
            .any(|r| r.fields.get("ok") == Some(&FieldValue::Bool(false))));
        assert!(!format!("{records:?}").contains("SECRET-"));
    }
    let globals = liquid_core::object!({"secret_value":"next", "selector":"ok"});
    let runtime = RuntimeBuilder::new().set_globals(&globals).build();
    let capture = Capture::default();
    assert_eq!(capture.run(|| template.render(&runtime)).unwrap(), "next");
    capture.assert_balanced();
    assert_eq!(
        capture
            .0
            .lock()
            .unwrap()
            .records
            .values()
            .filter(|r| r.parent.is_none())
            .count(),
        1
    );
}

#[test]
fn lookup_ownership_missing_selectors_and_sandbox_boundaries_are_unchanged() {
    let globals =
        liquid_core::object!({"secret_value":"ok","key":"secret_value","missing_selector":nil});
    let runtime = RuntimeBuilder::new()
        .set_globals(&globals)
        .set_strict_variables(false)
        .build();
    let expression = Expression::Variable(parser::parse_variable("secret_value").unwrap());
    let capture = Capture::default();
    let borrowed = capture.run(|| expression.evaluate(&runtime)).unwrap();
    assert!(matches!(borrowed, ValueCow::Borrowed(_)));
    let expected = runtime.try_get(&["secret_value".into()]).unwrap();
    assert!(std::ptr::eq(borrowed.as_view(), expected.as_view()));
    runtime.set_global("secret_value".into(), Value::scalar("assigned"));
    let assigned = capture.run(|| expression.evaluate(&runtime)).unwrap();
    assert!(matches!(assigned, ValueCow::Owned(_)));
    assert_eq!(assigned.to_value(), Value::scalar("assigned"));
    let sandbox = SandboxedStackFrame::new(&runtime, liquid_core::object!({"visible":"local"}));
    assert!(capture
        .run(|| expression.evaluate(&sandbox))
        .unwrap()
        .is_nil());
    let missing =
        Expression::Variable(parser::parse_variable("secret_value[missing_selector]").unwrap());
    assert!(capture.run(|| missing.evaluate(&runtime)).unwrap().is_nil());
    let strict = RuntimeBuilder::new().set_globals(&globals).build();
    assert!(capture.run(|| missing.evaluate(&strict)).is_err());
    capture.assert_balanced();
    let state = capture.0.lock().unwrap();
    let records = &state.records;
    assert!(records
        .values()
        .any(|r| r.name == "liquid.lookup"
            && r.fields.get("owned") == Some(&FieldValue::Bool(false))));
    assert!(records.values().any(
        |r| r.name == "liquid.lookup" && r.fields.get("owned") == Some(&FieldValue::Bool(true))
    ));
    assert!(records.values().any(|r| r.name == "liquid.frame"
        && r.fields.get("frame") == Some(&FieldValue::Text("sandbox".into()))
        && r.fields.get("found") == Some(&FieldValue::Bool(false))));
}

#[test]
fn shared_ast_creates_independent_invocation_parents_across_threads() {
    let template = Arc::new(parse("{{ secret_value | probe }}"));
    let capture = Capture::default();
    let dispatch = tracing::Dispatch::new(tracing_subscriber::registry().with(capture.clone()));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let template = template.clone();
            let dispatch = dispatch.clone();
            std::thread::spawn(move || {
                tracing::dispatcher::with_default(&dispatch, || {
                    let globals = liquid_core::object!({"secret_value":"ok"});
                    let runtime = RuntimeBuilder::new().set_globals(&globals).build();
                    template.render(&runtime).unwrap()
                })
            })
        })
        .collect();
    for handle in handles {
        assert_eq!(handle.join().unwrap(), "ok");
    }
    drop(dispatch);
    capture.assert_balanced();
    let state = capture.0.lock().unwrap();
    let records = &state.records;
    let roots: Vec<_> = records.iter().filter(|(_, r)| r.parent.is_none()).collect();
    assert_eq!(roots.len(), 2);
    assert!(roots.iter().all(|(_, r)| r.name == "liquid.template"));
    let filter_parents: Vec<_> = records
        .values()
        .filter(|r| r.name == "liquid.filter")
        .map(|r| r.parent)
        .collect();
    assert_eq!(filter_parents.len(), 2);
    assert_ne!(filter_parents[0], filter_parents[1]);
}
