//! Bounded Horizon fixture renderer. Theme source is parsed without rewriting it.
//! The platform adapter is deliberately separate from core Liquid compatibility.

use std::borrow::Cow;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs;
use std::io::{self, BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use std::time::Instant;

use liquid::reflection::ParserReflection;
use liquid_core::model::{
    DisplayCow, KString, KStringCow, KStringRef, ObjectView, ScalarCow, State,
};
use liquid_core::parser::{
    BlockReflection, FilterArguments, FilterReflection, ParameterReflection,
};
use liquid_core::partials::{LazyCompiler, OnDemandCompiler, PartialCompiler, PartialSource};
use liquid_core::runtime::{PartialStore, Registers, RuntimeBuilder, StackFrame, Template};
use liquid_core::{Error, Expression, Filter, Language, Object, ParseBlock, ParseFilter, ParseTag};
use liquid_core::{
    Renderable, Result, Runtime, TagBlock, TagReflection, TagTokenIter, Value, ValueCow, ValueView,
};
use serde_json::{json, Value as Json};
use sha2::{Digest, Sha256};

fn failure(error: impl fmt::Display) -> Error {
    Error::with_msg(error.to_string())
}

fn read_json(path: &Path) -> Result<Json> {
    let text = fs::read_to_string(path).map_err(failure)?;
    serde_json::from_str(&json_comments(&text)?).map_err(failure)
}

// Theme configuration accepts JSON comments. Liquid source is never changed.
fn json_comments(text: &str) -> Result<String> {
    let mut output = String::new();
    let mut chars = text.chars().peekable();
    let mut quoted = false;
    let mut escaped = false;
    while let Some(character) = chars.next() {
        if quoted {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
        } else if character == '"' {
            quoted = true;
            output.push(character);
        } else if character == '/' && chars.peek() == Some(&'/') {
            chars.next();
            output.push_str("  ");
            for character in chars.by_ref() {
                if character == '\n' {
                    output.push('\n');
                    break;
                }
                output.push(' ');
            }
        } else if character == '/' && chars.peek() == Some(&'*') {
            chars.next();
            output.push_str("  ");
            let mut closed = false;
            while let Some(character) = chars.next() {
                if character == '*' && chars.peek() == Some(&'/') {
                    chars.next();
                    output.push_str("  ");
                    closed = true;
                    break;
                }
                output.push(if character == '\n' { '\n' } else { ' ' });
            }
            if !closed {
                return Err(failure("Unclosed JSON block comment"));
            }
        } else {
            output.push(character);
        }
    }
    Ok(output)
}

#[derive(Clone, Debug, Default)]
struct Sources(
    Arc<BTreeMap<String, String>>,
    Arc<Mutex<BTreeMap<String, Json>>>,
);

impl Sources {
    fn new(files: BTreeMap<String, String>) -> Self {
        Self(Arc::new(files), Arc::new(Mutex::new(BTreeMap::new())))
    }

    fn read(theme: &Path) -> Result<Self> {
        let mut files = BTreeMap::new();
        for directory in ["snippets", "blocks", "sections", "layout"] {
            for entry in fs::read_dir(theme.join(directory)).map_err(failure)? {
                let path = entry.map_err(failure)?.path();
                if path.extension().and_then(|extension| extension.to_str()) != Some("liquid") {
                    continue;
                }
                let name = path
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .ok_or_else(|| failure("Invalid theme filename"))?;
                let key = if directory == "snippets" {
                    name.to_owned()
                } else {
                    format!("{directory}/{name}")
                };
                files.insert(key, fs::read_to_string(path).map_err(failure)?);
            }
        }
        Ok(Self::new(files))
    }

    fn schema(&self, name: &str) -> Result<Json> {
        if let Some(schema) = self.1.lock().map_err(failure)?.get(name) {
            return Ok(schema.clone());
        }
        let source = self
            .0
            .get(name)
            .ok_or_else(|| failure(format!("Missing source: {name}")))?;
        let pattern =
            regex::Regex::new(r"(?s)\{%-?\s*schema\s*-?%\}(.*?)\{%-?\s*endschema\s*-?%\}")
                .map_err(failure)?;
        let Some(captures) = pattern.captures(source) else {
            return Ok(json!({}));
        };
        let body = captures
            .get(1)
            .ok_or_else(|| failure("Missing schema body"))?
            .as_str();
        let schema: Json = serde_json::from_str(&json_comments(body)?).map_err(failure)?;
        self.1
            .lock()
            .map_err(failure)?
            .insert(name.to_owned(), schema.clone());
        Ok(schema)
    }
}

impl PartialSource for Sources {
    fn contains(&self, name: &str) -> bool {
        self.0.contains_key(name.trim_end_matches(".liquid"))
    }
    fn names(&self) -> Vec<&str> {
        self.0.keys().map(String::as_str).collect()
    }
    fn try_get<'a>(&'a self, name: &str) -> Option<Cow<'a, str>> {
        self.0
            .get(name.trim_end_matches(".liquid"))
            .map(|source| Cow::Borrowed(source.as_str()))
    }
}

#[derive(Debug)]
struct Context {
    theme: PathBuf,
    locales: Json,
    sources: Sources,
    fixture: Json,
    page: String,
    locale: String,
    globals: Object,
    styles: Mutex<Vec<(String, String)>>,
    scripts: Mutex<BTreeSet<String>>,
    calls: Mutex<BTreeMap<String, usize>>,
    rendered_sources: Mutex<Vec<String>>,
    palette: Vec<FixtureColor>,
    focal_points: BTreeMap<FocalPointKey, FixtureFocalPoint>,
    json_cache: Mutex<BTreeMap<String, Json>>,
    asset_cache: Mutex<BTreeMap<String, String>>,
}

impl Context {
    // Keep only immutable source/configuration caches between requests.
    fn reset_request(&self) -> Result<()> {
        self.styles.lock().map_err(failure)?.clear();
        self.scripts.lock().map_err(failure)?.clear();
        self.calls.lock().map_err(failure)?.clear();
        self.rendered_sources.lock().map_err(failure)?.clear();
        Ok(())
    }

    fn json_source(&self, name: &str) -> Result<Json> {
        if let Some(value) = self.json_cache.lock().map_err(failure)?.get(name) {
            return Ok(value.clone());
        }
        let value = read_json(&self.theme.join(name))?;
        self.json_cache
            .lock()
            .map_err(failure)?
            .insert(name.to_owned(), value.clone());
        Ok(value)
    }

    fn asset_content(&self, name: &str) -> Result<String> {
        if let Some(body) = self.asset_cache.lock().map_err(failure)?.get(name) {
            return Ok(body.clone());
        }
        let root = self.theme.canonicalize().map_err(failure)?;
        let path = root
            .join("assets")
            .join(name)
            .canonicalize()
            .map_err(failure)?;
        if !path.starts_with(&root) {
            return Err(failure("Asset path escapes theme root"));
        }
        let body = fs::read_to_string(path).map_err(failure)?;
        self.asset_cache
            .lock()
            .map_err(failure)?
            .insert(name.to_owned(), body.clone());
        Ok(body)
    }

    fn note(&self, name: &str) -> Result<()> {
        *self
            .calls
            .lock()
            .map_err(failure)?
            .entry(name.to_owned())
            .or_default() += 1;
        Ok(())
    }

    fn record_source(&self, name: &str) -> Result<()> {
        let path = if name.contains('/') {
            format!("{name}.liquid")
        } else {
            format!("snippets/{name}.liquid")
        };
        let mut sources = self.rendered_sources.lock().map_err(failure)?;
        if !sources.contains(&path) {
            sources.push(path);
        }
        Ok(())
    }

    fn materialize_settings(
        &self,
        values: &mut Object,
        schema: &Json,
        globals: &dyn ObjectView,
    ) -> Result<()> {
        bind_settings(values, globals)?;
        if let Some(definitions) = schema["settings"].as_array() {
            for definition in definitions {
                let Some(id) = definition["id"].as_str() else {
                    continue;
                };
                let Some(value) = values.get_mut(id) else {
                    continue;
                };
                if value.is_nil() || value.to_kstr().is_empty() {
                    continue;
                }
                let kind = definition["type"].as_str().unwrap_or("");
                match kind {
                    "collection" | "product" | "link_list" if value.is_scalar() => {
                        let registry = match kind {
                            "collection" => "collections",
                            "product" => "all_products",
                            _ => "linklists",
                        };
                        let handle = value.to_kstr();
                        *value = globals
                            .get(registry)
                            .and_then(ValueView::as_object)
                            .and_then(|items| items.get(&handle))
                            .map(ValueView::to_value)
                            .unwrap_or(Value::Nil);
                    }
                    "url" => *value = Value::scalar(value.to_kstr().replacen("shopify://", "/", 1)),
                    "color" => {
                        Color::parse(value.to_kstr().as_str())?;
                    }
                    "font_picker" => {
                        fixture_font(value)?;
                    }
                    "color_palette" => {
                        let palette = value
                            .as_object()
                            .ok_or_else(|| failure("Palette requires named colors"))?;
                        for (_, color) in palette.iter() {
                            Color::parse(color.to_kstr().as_str())?;
                        }
                    }
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn render_section(
        &self,
        writer: &mut dyn Write,
        partials: &dyn PartialStore,
        id: &str,
        raw: &Json,
        index: usize,
    ) -> Result<()> {
        let prepared = self.prepare(id, raw, "sections", index)?;
        let kind = prepared["type"]
            .as_str()
            .ok_or_else(|| failure("Section requires type"))?;
        let name = format!("sections/{kind}");
        let schema = self.sources.schema(&name)?;
        let mut globals = Object::new();
        let mut section = liquid_core::model::to_value(&prepared)?;
        if let Value::Object(object) = &mut section {
            if let Some(Value::Object(settings)) = object.get_mut("settings") {
                self.materialize_settings(
                    settings,
                    &schema,
                    &globals_view(&self.globals, &globals),
                )?;
                let mut closest = self
                    .globals
                    .get("closest")
                    .and_then(ValueView::as_object)
                    .map(|closest| {
                        closest
                            .iter()
                            .map(|(key, value)| (key.into_owned(), value.to_value()))
                            .collect::<Object>()
                    })
                    .unwrap_or_default();
                if let Some(collection) = settings.get("collection").filter(|value| !value.is_nil())
                {
                    closest.insert("collection".into(), collection.clone());
                }
                globals.insert("closest".into(), Value::Object(closest));
            }
        }
        globals.insert("section".into(), section);
        globals.insert("block".into(), Value::Nil);
        let globals = Arc::new(ScopeOverlay::root(globals));
        let values = globals_view(&self.globals, globals.as_ref());
        let inner = RuntimeBuilder::new()
            .set_globals(&values)
            .set_partials(partials)
            .build();
        set_platform_bindings(&inner, &globals);
        let frame = FixtureRuntime {
            inner: &inner,
            name: &name,
            palette: &self.palette,
            focal_points: &self.focal_points,
        };
        let tag = schema
            .get("tag")
            .map(|tag| tag.as_str())
            .unwrap_or(Some("div"));
        if let Some(tag) = tag {
            write_wrapper(
                writer,
                tag,
                &format!("shopify-section-{id}"),
                &wrapper_class("shopify-section", &schema),
                false,
            )?;
        }
        self.record_source(&name)?;
        partials.get(&name)?.render_to(writer, &frame)?;
        if let Some(tag) = tag {
            write_wrapper(writer, tag, "", "", true)?;
        }
        Ok(())
    }

    fn render_group(
        &self,
        writer: &mut dyn Write,
        partials: &dyn PartialStore,
        name: &str,
    ) -> Result<()> {
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
        {
            return Err(failure("Invalid section group name"));
        }
        let group = self.json_source(&format!("sections/{name}.json"))?;
        for (index, id) in group["order"]
            .as_array()
            .ok_or_else(|| failure("Group requires section order"))?
            .iter()
            .enumerate()
        {
            let id = id
                .as_str()
                .ok_or_else(|| failure("Invalid group section ID"))?;
            self.render_section(writer, partials, id, &group["sections"][id], index + 1)?;
        }
        Ok(())
    }

    fn setting_defaults(&self, schema: &Json) -> Json {
        let mut values = serde_json::Map::new();
        if let Some(settings) = schema["settings"].as_array() {
            for setting in settings {
                if let Some(id) = setting["id"].as_str() {
                    values.insert(
                        id.to_owned(),
                        setting.get("default").cloned().unwrap_or_else(|| {
                            match setting["type"].as_str() {
                                Some("checkbox") => Json::Bool(false),
                                Some("select" | "radio") => setting["options"][0]["value"].clone(),
                                _ => Json::Null,
                            }
                        }),
                    );
                }
            }
        }
        Json::Object(values)
    }

    fn prepare(&self, id: &str, input: &Json, directory: &str, index: usize) -> Result<Json> {
        let kind = input["type"]
            .as_str()
            .ok_or_else(|| failure("Block/section requires type"))?;
        let schema = self.sources.schema(&format!("{directory}/{kind}"))?;
        let mut settings = self.setting_defaults(&schema);
        merge(&mut settings, &input["settings"]);
        let overrides = if directory == "sections" {
            "section_overrides"
        } else {
            "block_overrides"
        };
        merge(
            &mut settings,
            &self.fixture["theme"][overrides][id]["settings"],
        );
        merge(
            &mut settings,
            &self.fixture["theme"]["page_overrides"][&self.page][overrides][id]["settings"],
        );
        let mut blocks = Vec::new();
        let mut order = input["block_order"].as_array().cloned().unwrap_or_else(|| {
            input["blocks"]
                .as_object()
                .map(|items| items.keys().map(|key| json!(key)).collect())
                .unwrap_or_default()
        });
        if let Some(children) = input["blocks"].as_object() {
            for (child_id, child) in children {
                if child["static"] == true && !order.iter().any(|id| id.as_str() == Some(child_id))
                {
                    order.push(json!(child_id));
                }
            }
        }
        for child in order {
            let child_id = child
                .as_str()
                .ok_or_else(|| failure("Invalid block order"))?;
            blocks.push(self.prepare(child_id, &input["blocks"][child_id], "blocks", 0)?);
        }
        Ok(
            json!({"id":id,"type":kind,"settings":settings,"blocks":blocks,"index":index,"static":input["static"].as_bool().unwrap_or(false),"shopify_attributes":""}),
        )
    }

    fn scope(&self, runtime: &dyn Runtime) -> Arc<ScopeOverlay> {
        runtime.registers().get_mut::<PlatformBindings>().0.clone()
    }

    fn render_block(
        &self,
        writer: &mut dyn Write,
        runtime: &dyn Runtime,
        mut block: Value,
        closest: Option<Value>,
        locals: &Object,
    ) -> Result<()> {
        let object = block
            .as_object()
            .ok_or_else(|| failure("Expected block object"))?;
        let kind = object
            .get("type")
            .ok_or_else(|| failure("Missing block type"))?
            .to_kstr()
            .into_owned();
        let id = object
            .get("id")
            .ok_or_else(|| failure("Missing block ID"))?
            .to_kstr()
            .into_owned();
        let name = format!("blocks/{kind}");
        let schema = self.sources.schema(&name)?;
        let mut globals = ScopeOverlay::child(self.scope(runtime), Object::new());
        if let Some(closest) = closest {
            globals.local.insert("closest".into(), closest);
        }
        if let Value::Object(object) = &mut block {
            if let Some(Value::Object(settings)) = object.get_mut("settings") {
                self.materialize_settings(
                    settings,
                    &schema,
                    &globals_view(&self.globals, &globals),
                )?;
            }
        }
        globals.local.insert("block".into(), block);
        // Borrow immutable fixture values; runtime assignments stay in the request GlobalFrame.
        let globals = Arc::new(globals);
        let platform_values = globals_view(&self.globals, globals.as_ref());
        let values = globals_view(&platform_values, locals);
        let inner = RuntimeBuilder::new()
            .set_globals(&values)
            .set_partials(runtime.partials())
            .build();
        set_platform_bindings(&inner, &globals);
        let frame = FixtureRuntime {
            inner: &inner,
            name: &name,
            palette: &self.palette,
            focal_points: &self.focal_points,
        };
        let tag = schema
            .get("tag")
            .map(|tag| tag.as_str())
            .unwrap_or(Some("div"));
        if let Some(tag) = tag {
            write_wrapper(
                writer,
                tag,
                &format!("shopify-block-{id}"),
                &wrapper_class("shopify-block", &schema),
                false,
            )?;
        }
        self.record_source(&name)?;
        runtime.partials().get(&name)?.render_to(writer, &frame)?;
        if let Some(tag) = tag {
            write_wrapper(writer, tag, "", "", true)?;
        }
        Ok(())
    }
}

// A request-local immutable parent chain retains platform bindings without copying
// enclosing section/block/closest values. Keywords and assignments never enter it.
#[derive(Debug, Default)]
struct ScopeOverlay {
    parent: Option<Arc<ScopeOverlay>>,
    local: Object,
}

impl ScopeOverlay {
    fn root(local: Object) -> Self {
        Self {
            parent: None,
            local,
        }
    }
    fn child(parent: Arc<Self>, local: Object) -> Self {
        Self {
            parent: Some(parent),
            local,
        }
    }
    fn view(&self) -> BTreeMap<KStringCow<'_>, &dyn ValueView> {
        let mut values = BTreeMap::new();
        let mut current = Some(self);
        // Enumerate the chain once. Local keys, including nil, win by presence.
        while let Some(scope) = current {
            for (key, value) in scope.local.iter() {
                values
                    .entry(KStringCow::from_ref(key.as_str()))
                    .or_insert_with(|| value.as_view());
            }
            current = scope.parent.as_deref();
        }
        values
    }
}

impl ValueView for ScopeOverlay {
    fn as_debug(&self) -> &dyn fmt::Debug {
        self
    }
    fn render(&self) -> DisplayCow<'_> {
        DisplayCow::Owned(Box::new(self.view().render().to_string()))
    }
    fn source(&self) -> DisplayCow<'_> {
        DisplayCow::Owned(Box::new(self.view().source().to_string()))
    }
    fn type_name(&self) -> &'static str {
        "object"
    }
    fn query_state(&self, state: State) -> bool {
        match state {
            State::Truthy => true,
            State::DefaultValue | State::Empty | State::Blank => self.size() == 0,
        }
    }
    fn to_kstr(&self) -> KStringCow<'_> {
        KStringCow::from_string(self.view().to_kstr().into_owned().to_string())
    }
    fn to_value(&self) -> Value {
        self.view().to_value()
    }
    fn as_object(&self) -> Option<&dyn ObjectView> {
        Some(self)
    }
}

impl ObjectView for ScopeOverlay {
    fn as_value(&self) -> &dyn ValueView {
        self
    }
    fn size(&self) -> i64 {
        self.view().len() as i64
    }
    fn keys<'k>(&'k self) -> Box<dyn Iterator<Item = KStringCow<'k>> + 'k> {
        Box::new(self.view().into_keys())
    }
    fn values<'k>(&'k self) -> Box<dyn Iterator<Item = &'k dyn ValueView> + 'k> {
        Box::new(self.view().into_values())
    }
    fn iter<'k>(&'k self) -> Box<dyn Iterator<Item = (KStringCow<'k>, &'k dyn ValueView)> + 'k> {
        Box::new(self.view().into_iter())
    }
    fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }
    fn get<'s>(&'s self, key: &str) -> Option<&'s dyn ValueView> {
        self.local
            .get(key)
            .map(Value::as_view)
            .or_else(|| self.parent.as_ref().and_then(|parent| parent.get(key)))
    }
}

// Request bindings contain only overrides of the immutable fixture globals. Sharing this
// snapshot preserves the platform environment across isolated snippets without copying the store.
#[derive(Default)]
struct PlatformBindings(Arc<ScopeOverlay>);

fn set_platform_bindings(runtime: &dyn Runtime, globals: &Arc<ScopeOverlay>) {
    runtime.registers().get_mut::<PlatformBindings>().0 = globals.clone();
}

fn globals_view<'a>(
    base: &'a dyn ObjectView,
    overrides: &'a dyn ObjectView,
) -> BTreeMap<KStringCow<'a>, &'a dyn ValueView> {
    base.iter().chain(overrides.iter()).collect()
}

fn merge(target: &mut Json, overrides: &Json) {
    if let (Some(target), Some(overrides)) = (target.as_object_mut(), overrides.as_object()) {
        target.extend(
            overrides
                .iter()
                .map(|(key, value)| (key.clone(), value.clone())),
        );
    }
}

fn write_wrapper(
    writer: &mut dyn Write,
    tag: &str,
    id: &str,
    class: &str,
    closing: bool,
) -> Result<()> {
    if tag.is_empty()
        || !tag.as_bytes()[0].is_ascii_lowercase()
        || !tag
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(failure("Invalid wrapper tag"));
    }
    if closing {
        write!(writer, "</{tag}>").map_err(failure)
    } else {
        write!(
            writer,
            "<{tag} id=\"{}\" class=\"{}\">",
            html_escape(id),
            html_escape(class)
        )
        .map_err(failure)
    }
}

fn wrapper_class(prefix: &str, schema: &Json) -> String {
    schema["class"]
        .as_str()
        .map(|class| format!("{prefix} {class}"))
        .unwrap_or_else(|| prefix.to_owned())
}

fn bind_settings(settings: &mut Object, globals: &dyn ObjectView) -> Result<()> {
    static PATTERN: LazyLock<std::result::Result<regex::Regex, regex::Error>> =
        LazyLock::new(|| regex::Regex::new(r"\{\{\s*([\w.]+)\s*\}\}"));
    let pattern = PATTERN.as_ref().map_err(failure)?;
    let fallback = Value::Object(settings.clone());
    let mut root = globals.iter().collect::<BTreeMap<_, _>>();
    root.entry("settings".into()).or_insert(&fallback);
    for (_, value) in settings.iter_mut() {
        let Some(text) = value.as_scalar().and_then(|value| {
            value
                .to_kstr()
                .as_str()
                .contains("{{")
                .then(|| value.to_kstr().into_owned())
        }) else {
            continue;
        };
        let lookup = |path: &str| {
            let path = path
                .split('.')
                .map(|part| ScalarCow::from(part.to_owned()))
                .collect::<Vec<_>>();
            liquid_core::model::try_find(&root, &path)
                .map(ValueCow::into_owned)
                .unwrap_or(Value::Nil)
        };
        if let Some(captures) = pattern.captures(&text) {
            if captures
                .get(0)
                .is_some_and(|matched| matched.as_str() == text.as_str())
            {
                *value = lookup(
                    captures
                        .get(1)
                        .ok_or_else(|| failure("Missing binding path"))?
                        .as_str(),
                );
                continue;
            }
        }
        let rendered = pattern
            .replace_all(&text, |captures: &regex::Captures<'_>| {
                lookup(captures.get(1).map(|path| path.as_str()).unwrap_or(""))
                    .to_kstr()
                    .into_owned()
            })
            .into_owned();
        if rendered.contains("{{") {
            return Err(failure(format!(
                "Unsupported fixture setting binding: {text}"
            )));
        }
        *value = Value::scalar(rendered);
    }
    Ok(())
}

// Ruby oracle configuration is strict parsing with strict_variables=false.
// Missing paths therefore become nil at the environment boundary.
struct FixtureRuntime<'a> {
    inner: &'a dyn Runtime,
    name: &'a str,
    palette: &'a Vec<FixtureColor>,
    focal_points: &'a BTreeMap<FocalPointKey, FixtureFocalPoint>,
}

impl Runtime for FixtureRuntime<'_> {
    fn strict_variables(&self) -> bool {
        false
    }
    fn project_value<'a>(
        &'a self,
        value: ValueCow<'a>,
        scope: &'a dyn Runtime,
        path: Option<&[ScalarCow<'_>]>,
    ) -> ValueCow<'a> {
        if let Some((property, prefix)) = path.and_then(|path| path.split_last()) {
            if property.to_kstr() == "size" {
                if let Some(name) = scope.try_get(prefix).and_then(|value| {
                    if value.as_object()?.contains_key("size") {
                        return None;
                    }
                    option_value_name(value.as_view()).map(|name| name.into_owned())
                }) {
                    return ValueCow::Owned(Value::scalar(name.chars().count() as i64));
                }
            }
        }
        focal_point_key(value.as_view())
            .and_then(|key| self.focal_points.get(&key))
            .map(|point| ValueCow::Borrowed(point as &dyn ValueView))
            .unwrap_or(value)
    }
    fn partials(&self) -> &dyn PartialStore {
        self.inner.partials()
    }
    fn name(&self) -> Option<KStringRef<'_>> {
        Some(self.name.into())
    }
    fn roots(&self) -> BTreeSet<KStringCow<'_>> {
        self.inner.roots()
    }
    fn try_get(&self, path: &[ScalarCow<'_>]) -> Option<ValueCow<'_>> {
        if let Some((property, prefix)) = path.split_last() {
            if property.to_kstr() == "size" {
                if let Some(name) = self.inner.try_get(prefix).and_then(|value| {
                    if value.as_object()?.contains_key("size") {
                        return None;
                    }
                    option_value_name(value.as_view()).map(|name| name.into_owned())
                }) {
                    return Some(ValueCow::Owned(Value::scalar(name.chars().count() as i64)));
                }
            }
        }
        if path.len() == 2
            && path[0].to_kstr() == "settings"
            && path[1].to_kstr() == "color_palette"
            && !self.palette.is_empty()
        {
            return Some(ValueCow::Borrowed(self.palette));
        }
        if path.len() == 1 && path[0].to_kstr() == "template" {
            let value = self.inner.try_get(path)?;
            if let Some(name) = value.as_object().and_then(|object| object.get("name")) {
                return Some(ValueCow::Owned(Value::scalar(name.to_kstr().into_owned())));
            }
        }
        self.inner.try_get(path).or_else(|| {
            let (property, prefix) = path.split_last()?;
            let value = self.inner.try_get(prefix)?;
            if let Ok(font) = fixture_font(value.as_view()) {
                if let Some(field) = font.get(property.to_kstr().as_str()) {
                    return Some(ValueCow::Owned(field.clone()));
                }
            }
            let scalar = value.as_scalar()?;
            let color = Color::parse(scalar.to_kstr().as_str()).ok()?;
            let value = match property.to_kstr().as_str() {
                "rgb" => Value::scalar(color.rgb()),
                "rgba" => Value::scalar(format!("{} / {}", color.rgb(), ruby_float(color.alpha))),
                "red" => Value::scalar(i64::from(color.red)),
                "green" => Value::scalar(i64::from(color.green)),
                "blue" => Value::scalar(i64::from(color.blue)),
                "alpha" => Value::scalar(color.alpha),
                _ => return None,
            };
            Some(ValueCow::Owned(value))
        })
    }
    fn get(&self, path: &[ScalarCow<'_>]) -> Result<ValueCow<'_>> {
        Ok(self.try_get(path).unwrap_or(ValueCow::Owned(Value::Nil)))
    }
    fn set_global(&self, name: KString, value: Value) -> Option<Value> {
        self.inner.set_global(name, value)
    }
    fn set_index(&self, name: KString, value: Value) -> Option<Value> {
        self.inner.set_index(name, value)
    }
    fn get_index(&self, name: &str) -> Option<ValueCow<'_>> {
        self.inner.get_index(name)
    }
    fn registers(&self) -> &Registers {
        self.inner.registers()
    }
}

#[derive(Clone, Debug)]
struct PlatformTag {
    name: &'static str,
    context: Arc<Context>,
}

impl TagReflection for PlatformTag {
    fn tag(&self) -> &str {
        self.name
    }
    fn description(&self) -> &str {
        "Bounded fixture platform tag"
    }
}

impl ParseTag for PlatformTag {
    fn reflection(&self) -> &dyn TagReflection {
        self
    }
    fn parse(
        &self,
        mut arguments: TagTokenIter<'_>,
        _options: &Language,
    ) -> Result<Box<dyn Renderable>> {
        let primary = arguments
            .expect_next("Expected tag argument")?
            .expect_value()
            .into_result()?;
        let mut keywords = Vec::new();
        while let Some(token) = arguments.next() {
            let key = if token.as_str() == "," {
                let Some(key) = arguments.next() else {
                    break;
                };
                key
            } else if keywords.is_empty() {
                token
            } else {
                return Err(failure("Expected comma between platform tag keywords"));
            };
            let key = key.as_str().to_owned();
            arguments
                .expect_next("Expected colon")?
                .expect_str(":")
                .into_result()?;
            let value = arguments
                .expect_next("Expected value")?
                .expect_value()
                .into_result()?;
            keywords.push((key, value));
        }
        Ok(Box::new(TagNode {
            name: self.name,
            context: self.context.clone(),
            primary,
            keywords,
        }))
    }
}

#[derive(Debug)]
struct TagNode {
    name: &'static str,
    context: Arc<Context>,
    primary: Expression,
    keywords: Vec<(String, Expression)>,
}

impl Renderable for TagNode {
    fn render_to(&self, writer: &mut dyn Write, runtime: &dyn Runtime) -> Result<()> {
        self.context.note(self.name)?;
        let primary = self.primary.evaluate(runtime)?.to_kstr().into_owned();
        let mut arguments = Object::new();
        for (key, expression) in &self.keywords {
            arguments.insert(
                key.clone().into(),
                expression
                    .try_evaluate(runtime)
                    .map(ValueCow::into_owned)
                    .unwrap_or(Value::Nil),
            );
        }
        if self.name == "render" {
            let globals = self.context.scope(runtime);
            let platform_values = globals_view(&self.context.globals, globals.as_ref());
            let values = globals_view(&platform_values, &arguments);
            let inner = RuntimeBuilder::new()
                .set_globals(&values)
                .set_partials(runtime.partials())
                .build();
            set_platform_bindings(&inner, &globals);
            let frame = FixtureRuntime {
                inner: &inner,
                name: &primary,
                palette: &self.context.palette,
                focal_points: &self.context.focal_points,
            };
            self.context.record_source(&primary)?;
            return runtime.partials().get(&primary)?.render_to(writer, &frame);
        }
        if self.name == "sections" {
            return self
                .context
                .render_group(writer, runtime.partials(), &primary);
        }
        if self.name != "content_for" {
            return Err(failure(format!("Unsupported platform tag: {}", self.name)));
        }
        let parent = runtime
            .try_get(&["block".into()])
            .filter(|value| !value.is_nil())
            .or_else(|| runtime.try_get(&["section".into()]))
            .ok_or_else(|| failure("content_for requires a section or block"))?;
        let children = parent
            .as_object()
            .and_then(|parent| parent.get("blocks"))
            .and_then(ValueView::as_array);
        let platform_globals = self.context.scope(runtime);
        let platform_values = globals_view(&self.context.globals, platform_globals.as_ref());
        let closest = arguments
            .iter()
            .filter(|(key, _)| key.starts_with("closest."))
            .fold(
                platform_values
                    .get("closest")
                    .and_then(|value| value.as_object().map(|object| object.to_value()))
                    .unwrap_or_else(|| Value::Object(Object::new())),
                |mut value, (key, item)| {
                    if let Value::Object(object) = &mut value {
                        object.insert(
                            key.trim_start_matches("closest.").to_owned().into(),
                            item.clone(),
                        );
                    }
                    value
                },
            );
        let locals = arguments
            .iter()
            .filter(|(key, _)| {
                !matches!(key.as_str(), "id" | "type") && !key.starts_with("closest.")
            })
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect::<Object>();
        if primary == "blocks" {
            if let Some(children) = children {
                for block in children.values().filter(|block| {
                    !block
                        .as_object()
                        .and_then(|object| object.get("static"))
                        .is_some_and(|value| {
                            value.as_scalar().and_then(|value| value.to_bool()) == Some(true)
                        })
                }) {
                    self.context.render_block(
                        writer,
                        runtime,
                        block.to_value(),
                        Some(closest.clone()),
                        &locals,
                    )?;
                }
            }
            return Ok(());
        }
        if primary == "block" {
            let id = arguments
                .get("id")
                .ok_or_else(|| failure("content_for block requires id"))?
                .to_kstr();
            let existing = children
                .and_then(|children| {
                    children.values().find(|block| {
                        block
                            .as_object()
                            .and_then(|object| object.get("id"))
                            .is_some_and(|value| value.to_kstr() == id)
                    })
                })
                .map(ValueView::to_value);
            let block = match existing {
                Some(block) => {
                    let expected = arguments
                        .get("type")
                        .ok_or_else(|| failure("content_for block requires type"))?
                        .to_kstr();
                    let actual = block
                        .as_object()
                        .and_then(|object| object.get("type"))
                        .ok_or_else(|| failure("Block requires type"))?
                        .to_kstr();
                    if actual != expected {
                        return Err(failure(format!("Block {id} type mismatch")));
                    }
                    block
                }
                None => {
                    let kind = arguments
                        .get("type")
                        .ok_or_else(|| failure("content_for block requires type"))?
                        .to_kstr();
                    liquid_core::model::to_value(&self.context.prepare(
                        &id,
                        &json!({"type":kind.as_str()}),
                        "blocks",
                        0,
                    )?)?
                }
            };
            return self
                .context
                .render_block(writer, runtime, block, Some(closest), &locals);
        }
        Err(failure(format!("Unsupported content_for kind: {primary}")))
    }
}

#[derive(Clone, Debug)]
struct PlatformBlock {
    name: &'static str,
    end: &'static str,
    context: Arc<Context>,
}

impl BlockReflection for PlatformBlock {
    fn start_tag(&self) -> &str {
        self.name
    }
    fn end_tag(&self) -> &str {
        self.end
    }
    fn description(&self) -> &str {
        "Bounded fixture platform block"
    }
}

impl ParseBlock for PlatformBlock {
    fn reflection(&self) -> &dyn BlockReflection {
        self
    }
    fn parse(
        &self,
        mut arguments: TagTokenIter<'_>,
        mut block: TagBlock<'_, '_>,
        options: &Language,
    ) -> Result<Box<dyn Renderable>> {
        let mut parameters = Vec::new();
        let mut keywords = Vec::new();
        if self.name == "paginate" {
            parameters.push(
                arguments
                    .expect_next("Expected collection")?
                    .expect_value()
                    .into_result()?,
            );
            arguments
                .expect_next("Expected by")?
                .expect_str("by")
                .into_result()?;
            parameters.push(
                arguments
                    .expect_next("Expected page size")?
                    .expect_value()
                    .into_result()?,
            );
        }
        if self.name == "form" {
            parameters.push(
                arguments
                    .expect_next("Expected form type")?
                    .expect_value()
                    .into_result()?,
            );
            let mut tokens = arguments
                .by_ref()
                .collect::<Vec<_>>()
                .into_iter()
                .peekable();
            while let Some(token) = tokens.next() {
                token.expect_str(",").into_result()?;
                let value = tokens
                    .next()
                    .ok_or_else(|| failure("Expected form argument"))?;
                if tokens.peek().is_some_and(|next| next.as_str() == ":") {
                    let key = value.as_str().to_owned();
                    tokens.next();
                    let expression = tokens
                        .next()
                        .ok_or_else(|| failure("Expected form keyword value"))?
                        .expect_value()
                        .into_result()?;
                    keywords.push((key, expression));
                } else {
                    parameters.push(value.expect_value().into_result()?);
                }
            }
        }
        arguments.expect_nothing()?;
        let (raw, body) = if matches!(self.name, "style" | "paginate" | "form") {
            (
                String::new(),
                Some(Template::new(block.parse_all(options)?)),
            )
        } else {
            (block.escape_liquid(false)?.to_owned(), None)
        };
        Ok(Box::new(BlockNode {
            name: self.name,
            context: self.context.clone(),
            raw,
            body,
            parameters,
            keywords,
        }))
    }
}

#[derive(Debug)]
struct BlockNode {
    name: &'static str,
    context: Arc<Context>,
    raw: String,
    body: Option<Template>,
    parameters: Vec<Expression>,
    keywords: Vec<(String, Expression)>,
}

impl Renderable for BlockNode {
    fn render_to(&self, writer: &mut dyn Write, runtime: &dyn Runtime) -> Result<()> {
        self.context.note(self.name)?;
        match self.name {
            "schema" | "doc" => Ok(()),
            "stylesheet" => {
                let key = runtime
                    .name()
                    .map(|name| name.to_string())
                    .unwrap_or_default();
                let key = if key.contains('/') {
                    format!("{key}.liquid")
                } else {
                    format!("snippets/{key}.liquid")
                };
                let mut styles = self.context.styles.lock().map_err(failure)?;
                if !styles.iter().any(|(name, _)| name == &key) {
                    styles.push((key, self.raw.clone()));
                }
                Ok(())
            }
            "javascript" => {
                let name = runtime
                    .name()
                    .map(|name| name.to_string())
                    .unwrap_or_default();
                if self.context.scripts.lock().map_err(failure)?.insert(name) {
                    writer
                        .write_all(b"<script data-shopify>")
                        .map_err(failure)?;
                    writer.write_all(self.raw.as_bytes()).map_err(failure)?;
                    writer.write_all(b"</script>").map_err(failure)?;
                }
                Ok(())
            }
            "style" => {
                writer.write_all(b"<style data-shopify>").map_err(failure)?;
                if let Some(body) = &self.body {
                    body.render_to(writer, runtime)?;
                }
                writer.write_all(b"</style>").map_err(failure)
            }
            "form" => {
                let kind = self
                    .parameters
                    .first()
                    .ok_or_else(|| failure("Form requires type"))?
                    .evaluate(runtime)?
                    .to_kstr()
                    .into_owned();
                let action = match kind.as_str() {
                    "product" => "/cart/add",
                    "customer" => "/contact",
                    "localization" => "/localization",
                    "cart" => "/cart",
                    _ => return Err(failure(format!("Unsupported fixture form {kind}"))),
                };
                let arguments = self
                    .keywords
                    .iter()
                    .map(|(key, expression)| {
                        expression
                            .evaluate(runtime)
                            .map(|value| (key.clone(), value.into_owned()))
                    })
                    .collect::<Result<BTreeMap<_, _>>>()?;
                write!(writer, "<form method=\"post\" action=\"{action}\"").map_err(failure)?;
                for (key, value) in &arguments {
                    if !value.is_nil() {
                        write!(writer, " {key}=\"{}\"", html_escape(&value.to_kstr()))
                            .map_err(failure)?;
                    }
                }
                write!(
                    writer,
                    "><input type=\"hidden\" name=\"form_type\" value=\"{}\">",
                    html_escape(&kind)
                )
                .map_err(failure)?;
                let globals = liquid_core::object!({"form":{"type":kind,"errors":Value::Nil,"posted_successfully?":false,"id":arguments.get("id").cloned().unwrap_or(Value::Nil)}});
                let frame = StackFrame::new(runtime, &globals);
                let name = runtime.name();
                let frame = FixtureRuntime {
                    inner: &frame,
                    name: name.as_ref().map(|name| name.as_str()).unwrap_or(""),
                    palette: &self.context.palette,
                    focal_points: &self.context.focal_points,
                };
                if let Some(body) = &self.body {
                    body.render_to(writer, &frame)?;
                }
                writer.write_all(b"</form>").map_err(failure)
            }
            "paginate" => {
                let collection = self.parameters[0].evaluate(runtime)?;
                let size = self.parameters[1]
                    .evaluate(runtime)?
                    .as_scalar()
                    .and_then(|value| value.to_integer())
                    .ok_or_else(|| failure("paginate requires integer size"))?;
                if size <= 0 {
                    return Err(failure("Fixture pagination requires positive page size"));
                }
                let items = collection
                    .as_array()
                    .map(|array| array.size())
                    .ok_or_else(|| {
                        failure(format!(
                            "paginate requires collection array: {} ({})",
                            collection.source(),
                            collection.type_name()
                        ))
                    })?;
                let pages = (items / size + i64::from(items % size != 0)).max(1);
                let current = runtime.get(&["current_page".into()])?;
                let current_page = if current.is_nil() {
                    1
                } else {
                    current
                        .as_scalar()
                        .and_then(|value| value.to_integer())
                        .ok_or_else(|| failure("Fixture current_page requires integer"))?
                };
                if !(1..=pages).contains(&current_page) {
                    return Err(failure(
                        "Fixture current_page is outside the paginated collection",
                    ));
                }
                let offset = (current_page - 1)
                    .checked_mul(size)
                    .ok_or_else(|| failure("Pagination offset overflow"))?;
                let path = match &self.parameters[0] {
                    Expression::Variable(variable) => variable.evaluate(runtime)?,
                    Expression::Literal(_) => {
                        return Err(failure("Fixture pagination requires a variable path"))
                    }
                };
                let window = Value::Array(
                    collection
                        .as_array()
                        .ok_or_else(|| failure("Paginated value requires array"))?
                        .values()
                        .skip(usize::try_from(offset).map_err(failure)?)
                        .take(usize::try_from(size).map_err(failure)?)
                        .map(ValueView::to_value)
                        .collect(),
                );
                let mut globals = Object::new();
                let (name, properties) = path
                    .as_slice()
                    .split_first()
                    .ok_or_else(|| failure("Missing pagination path"))?;
                let root = runtime.get(std::slice::from_ref(name))?;
                globals.insert(
                    name.to_kstr().into_owned(),
                    pagination_window(root.as_view(), properties, window)?,
                );
                let request_path = runtime
                    .get(&["request".into(), "path".into()])?
                    .to_kstr()
                    .into_owned();
                let page_url = |page: i64| format!("{request_path}?page={page}");
                let parts = (1..=pages).map(|page| json!({"title":page.to_string(),"is_link":page != current_page,"url":if page == current_page { Json::Null } else { json!(page_url(page)) }})).collect::<Vec<_>>();
                globals.insert("paginate".into(), liquid_core::model::to_value(&json!({"current_page":current_page,"pages":pages,"items":items,"page_size":size,"current_offset":offset,"previous":if current_page > 1 { json!({"title":"Previous","url":page_url(current_page - 1)}) } else { Json::Null },"next":if current_page < pages { json!({"title":"Next","url":page_url(current_page + 1)}) } else { Json::Null },"parts":parts}))?);
                let frame = StackFrame::new(runtime, &globals);
                let name = runtime.name();
                let frame = FixtureRuntime {
                    inner: &frame,
                    name: name.as_ref().map(|name| name.as_str()).unwrap_or(""),
                    palette: &self.context.palette,
                    focal_points: &self.context.focal_points,
                };
                if let Some(body) = &self.body {
                    body.render_to(writer, &frame)?;
                }
                Ok(())
            }
            _ => Err(failure("Unsupported platform block")),
        }
    }
}

#[derive(Clone, Debug)]
struct PlatformFilter {
    name: String,
    context: Arc<Context>,
}

impl FilterReflection for PlatformFilter {
    fn name(&self) -> &str {
        &self.name
    }
    fn description(&self) -> &str {
        "Bounded fixture filter; unsupported executed calls fail"
    }
    fn positional_parameters(&self) -> &'static [ParameterReflection] {
        &[]
    }
    fn keyword_parameters(&self) -> &'static [ParameterReflection] {
        &[]
    }
}

impl ParseFilter for PlatformFilter {
    fn reflection(&self) -> &dyn FilterReflection {
        self
    }
    fn parse(&self, arguments: FilterArguments<'_>) -> Result<Box<dyn Filter>> {
        Ok(Box::new(FilterNode {
            name: self.name.clone(),
            context: self.context.clone(),
            positional: arguments.positional.collect(),
            keyword: arguments
                .keyword
                .map(|(name, value)| (name.to_owned(), value))
                .collect(),
        }))
    }
}

#[derive(Debug)]
struct FilterNode {
    name: String,
    context: Arc<Context>,
    positional: Vec<Expression>,
    keyword: Vec<(String, Expression)>,
}

// Palette elements expose color properties while preserving their scalar rendering.
// A borrowed palette keeps those properties available in standard for-loop scopes.
#[derive(Debug)]
struct FixtureColor {
    text: Value,
    fields: Object,
}
impl FixtureColor {
    fn new(text: &str) -> Result<Self> {
        let color = Color::parse(text)?;
        Ok(Self {
            text: Value::scalar(text.to_owned()),
            fields: liquid_core::object!({"red":i64::from(color.red),"green":i64::from(color.green),"blue":i64::from(color.blue),"alpha":color.alpha,"rgb":color.rgb(),"rgba":format!("{} / {}",color.rgb(),ruby_float(color.alpha))}),
        })
    }
}
impl ValueView for FixtureColor {
    fn as_debug(&self) -> &dyn fmt::Debug {
        self
    }
    fn render(&self) -> DisplayCow<'_> {
        self.text.render()
    }
    fn source(&self) -> DisplayCow<'_> {
        self.text.source()
    }
    fn type_name(&self) -> &'static str {
        "fixture color"
    }
    fn query_state(&self, state: State) -> bool {
        self.text.query_state(state)
    }
    fn to_kstr(&self) -> KStringCow<'_> {
        self.text.to_kstr()
    }
    fn to_value(&self) -> Value {
        self.text.clone()
    }
    fn as_scalar(&self) -> Option<ScalarCow<'_>> {
        self.text.as_scalar()
    }
    fn as_object(&self) -> Option<&dyn ObjectView> {
        Some(&self.fields)
    }
}

// The finite fixture models focal points as exact two-number {x,y} objects.
// Views preserve that object when copied into assigns/loops; interpolation and
// string filters receive the percentage string used by the Ruby adapter.
// Recognition is structural: another exact x/y object with the same registered
// coordinates and numeric types also receives this view. Presentation is restored
// at expression evaluation, not every filter's internal array-element access.
type FocalPointKey = (u64, u64, &'static str, &'static str);

fn focal_point_key(value: &dyn ValueView) -> Option<FocalPointKey> {
    let object = value.as_object()?;
    if object.size() != 2 {
        return None;
    }
    let x = object.get("x")?.as_scalar()?;
    let y = object.get("y")?.as_scalar()?;
    if !["whole number", "fractional number"].contains(&x.type_name())
        || !["whole number", "fractional number"].contains(&y.type_name())
    {
        return None;
    }
    let (x_number, y_number) = (x.to_float()?, y.to_float()?);
    if !x_number.is_finite()
        || !y_number.is_finite()
        || !(0.0..=100.0).contains(&x_number)
        || !(0.0..=100.0).contains(&y_number)
    {
        return None;
    }
    Some((
        x_number.to_bits(),
        y_number.to_bits(),
        x.type_name(),
        y.type_name(),
    ))
}

#[derive(Debug)]
struct FixtureFocalPoint {
    fields: Object,
    text: String,
}

impl ValueView for FixtureFocalPoint {
    fn as_debug(&self) -> &dyn fmt::Debug {
        self
    }
    fn render(&self) -> DisplayCow<'_> {
        DisplayCow::Borrowed(&self.text)
    }
    fn source(&self) -> DisplayCow<'_> {
        self.fields.source()
    }
    fn type_name(&self) -> &'static str {
        "fixture focal point"
    }
    fn query_state(&self, state: State) -> bool {
        self.fields.query_state(state)
    }
    fn to_kstr(&self) -> KStringCow<'_> {
        self.text.as_str().into()
    }
    fn to_value(&self) -> Value {
        Value::Object(self.fields.clone())
    }
    fn as_object(&self) -> Option<&dyn ObjectView> {
        Some(&self.fields)
    }
}

fn fixture_focal_points(
    values: &dyn ValueView,
) -> Result<BTreeMap<FocalPointKey, FixtureFocalPoint>> {
    fn collect(
        value: &dyn ValueView,
        points: &mut BTreeMap<FocalPointKey, FixtureFocalPoint>,
    ) -> Result<()> {
        if let Some(object) = value.as_object() {
            for (name, value) in object.iter() {
                if name == "focal_point" && !value.is_nil() {
                    let key = focal_point_key(value).ok_or_else(|| failure("Fixture focal_point requires exactly numeric x/y percentages in 0..=100"))?;
                    if let std::collections::btree_map::Entry::Vacant(entry) = points.entry(key) {
                        let object = value
                            .as_object()
                            .ok_or_else(|| failure("Expected fixture focal point object"))?;
                        let fields = object
                            .iter()
                            .map(|(name, value)| (name.into_owned(), value.to_value()))
                            .collect();
                        entry.insert(FixtureFocalPoint {
                            fields,
                            text: format!(
                                "{}% {}%",
                                object
                                    .get("x")
                                    .ok_or_else(|| failure("Missing focal point x"))?
                                    .render(),
                                object
                                    .get("y")
                                    .ok_or_else(|| failure("Missing focal point y"))?
                                    .render()
                            ),
                        });
                    }
                }
                collect(value, points)?;
            }
        } else if let Some(array) = value.as_array() {
            for value in array.values() {
                collect(value, points)?;
            }
        }
        Ok(())
    }
    let mut points = BTreeMap::new();
    collect(values, &mut points)?;
    Ok(points)
}

fn ruby_float(value: f64) -> String {
    let text = value.to_string();
    if text.contains('.') || text.contains('e') || text.contains('E') {
        text
    } else {
        format!("{text}.0")
    }
}

fn fixture_font(input: &dyn ValueView) -> Result<Object> {
    if let Some(object) = input.as_object() {
        if object.contains_key("__fixture_font") {
            return Ok(object
                .iter()
                .map(|(key, value)| (key.into_owned(), value.to_value()))
                .collect());
        }
    }
    let scalar = input.as_scalar().ok_or_else(|| {
        failure(format!(
            "Unsupported fixture font type: {}",
            input.type_name()
        ))
    })?;
    let handle = scalar.to_kstr();
    let weight = match handle.as_str() {
        "inter_n4" => 400,
        "inter_n5" => 500,
        "inter_n7" => 700,
        _ => return Err(failure(format!("Unsupported fixture font: {handle}"))),
    };
    Ok(
        liquid_core::object!({"__fixture_font":handle.as_str(),"weight":weight,"style":"normal","family":"Arial","fallback_families":"sans-serif","system?":true}),
    )
}

struct Color {
    red: u8,
    green: u8,
    blue: u8,
    alpha: f64,
}

impl Color {
    fn parse(value: &str) -> Result<Self> {
        let error = || failure(format!("Unsupported fixture color: {value}"));
        if let Some(hex) = value.strip_prefix('#') {
            let nibble = |byte: u8| -> Option<u8> {
                match byte {
                    b'0'..=b'9' => Some(byte - b'0'),
                    b'a'..=b'f' => Some(byte - b'a' + 10),
                    b'A'..=b'F' => Some(byte - b'A' + 10),
                    _ => None,
                }
            };
            let bytes = hex.as_bytes();
            if ![6, 8].contains(&bytes.len()) {
                return Err(error());
            }
            let values = bytes
                .chunks_exact(2)
                .map(|pair| Some(nibble(pair[0])? * 16 + nibble(pair[1])?))
                .collect::<Option<Vec<_>>>()
                .ok_or_else(error)?;
            return Ok(Self {
                red: values[0],
                green: values[1],
                blue: values[2],
                alpha: values
                    .get(3)
                    .map(|alpha| f64::from(*alpha) / 255.0)
                    .unwrap_or(1.0),
            });
        }
        let rgba = value
            .strip_prefix("rgba(")
            .or_else(|| value.strip_prefix("rgb("))
            .and_then(|value| value.strip_suffix(')'))
            .ok_or_else(error)?;
        let parts = rgba.split(',').map(str::trim).collect::<Vec<_>>();
        if ![3, 4].contains(&parts.len()) {
            return Err(error());
        }
        let alpha = parts
            .get(3)
            .map(|value| value.parse::<f64>())
            .transpose()
            .map_err(failure)?
            .unwrap_or(1.0);
        if !(0.0..=1.0).contains(&alpha) {
            return Err(error());
        }
        Ok(Self {
            red: parts[0].parse().map_err(failure)?,
            green: parts[1].parse().map_err(failure)?,
            blue: parts[2].parse().map_err(failure)?,
            alpha,
        })
    }
    fn luminance(&self) -> f64 {
        [self.red, self.green, self.blue]
            .into_iter()
            .zip([0.2126, 0.7152, 0.0722])
            .map(|(channel, weight)| {
                let channel = f64::from(channel) / 255.0;
                let channel = if channel <= 0.04045 {
                    channel / 12.92
                } else {
                    ((channel + 0.055) / 1.055).powf(2.4)
                };
                channel * weight
            })
            .sum()
    }
    fn shifted_lightness(&self, amount: f64) -> String {
        let [red, green, blue] =
            [self.red, self.green, self.blue].map(|channel| f64::from(channel) / 255.0);
        let min = red.min(green).min(blue);
        let max = red.max(green).max(blue);
        let lightness = (min + max) / 2.0;
        let delta = max - min;
        let saturation = if delta == 0.0 {
            0.0
        } else {
            delta / (1.0 - (2.0 * lightness - 1.0).abs())
        };
        let hue = if delta == 0.0 {
            0.0
        } else if max == red {
            ((green - blue) / delta).rem_euclid(6.0)
        } else if max == green {
            (blue - red) / delta + 2.0
        } else {
            (red - green) / delta + 4.0
        };
        let lightness = (lightness + amount / 100.0).clamp(0.0, 1.0);
        let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
        let intermediate = chroma * (1.0 - (hue % 2.0 - 1.0).abs());
        let channels = if hue < 1.0 {
            [chroma, intermediate, 0.0]
        } else if hue < 2.0 {
            [intermediate, chroma, 0.0]
        } else if hue < 3.0 {
            [0.0, chroma, intermediate]
        } else if hue < 4.0 {
            [0.0, intermediate, chroma]
        } else if hue < 5.0 {
            [intermediate, 0.0, chroma]
        } else {
            [chroma, 0.0, intermediate]
        };
        let [red, green, blue] =
            channels.map(|channel| ((channel + lightness - chroma / 2.0) * 255.0).round() as u8);
        if self.alpha == 1.0 {
            format!("#{red:02x}{green:02x}{blue:02x}")
        } else {
            format!("rgba({red}, {green}, {blue}, {})", ruby_float(self.alpha))
        }
    }
    fn rgb(&self) -> String {
        format!("{} {} {}", self.red, self.green, self.blue)
    }
}

fn pagination_window(root: &dyn ValueView, path: &[ScalarCow<'_>], window: Value) -> Result<Value> {
    let Some((property, rest)) = path.split_first() else {
        return Ok(window);
    };
    if property.type_name() != "string" {
        return Err(failure(
            "Fixture pagination supports object property paths only",
        ));
    }
    let object = root
        .as_object()
        .ok_or_else(|| failure("Paginated parent requires object"))?;
    let key = property.to_kstr();
    let child = object
        .get(key.as_str())
        .ok_or_else(|| failure("Missing pagination parent property"))?;
    let child = pagination_window(child, rest, window)?;
    let mut result = object
        .iter()
        .map(|(key, value)| (key.into_owned(), value.to_value()))
        .collect::<Object>();
    result.insert(key.into_owned(), child);
    Ok(Value::Object(result))
}

fn option_value_name(input: &dyn ValueView) -> Option<KStringCow<'_>> {
    let option = input.as_object()?;
    if [
        "id",
        "name",
        "available",
        "selected",
        "variant",
        "product_url",
    ]
    .iter()
    .all(|key| option.contains_key(key))
    {
        return option
            .get("name")?
            .as_scalar()
            .map(|value| value.to_kstr().into_owned().into());
    }
    None
}

fn product_structured_data(input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
    let product = input
        .as_object()
        .ok_or_else(|| failure("structured_data requires fixture product"))?;
    let field = |name: &str| {
        product
            .get(name)
            .ok_or_else(|| failure(format!("Structured product requires {name}")))
    };
    let quote = |text: &str| serde_json::to_string(text).map_err(failure);
    let canonical = runtime
        .get(&[ScalarCow::from("canonical_url")])?
        .to_kstr()
        .into_owned();
    let origin = canonical.split('/').take(3).collect::<Vec<_>>().join("/");
    if !canonical.starts_with("https://") || origin == "https://" {
        return Err(failure(
            "Structured product requires absolute fixture HTTPS canonical_url",
        ));
    }
    let title = field("title")?.to_kstr();
    let description = field("description")?.to_kstr();
    let description = regex::Regex::new(r"<[^>]*>")
        .map_err(failure)?
        .replace_all(&description, "");
    let images = field("images")?
        .as_array()
        .ok_or_else(|| failure("Structured product requires fixture images"))?;
    let images = images
        .values()
        .map(|image| {
            let source = image
                .as_object()
                .and_then(|image| image.get("src"))
                .ok_or_else(|| failure("Structured product image requires src"))?
                .to_kstr();
            if !source.starts_with('/') {
                return Err(failure("Structured image requires local URL"));
            }
            quote(&format!("{origin}{source}"))
        })
        .collect::<Result<Vec<_>>>()?
        .join(",");
    let variants = field("variants")?
        .as_array()
        .ok_or_else(|| failure("Structured product requires variants"))?;
    let mut variant_json = Vec::new();
    for variant in variants.values() {
        let variant = variant
            .as_object()
            .ok_or_else(|| failure("Structured variant requires object"))?;
        let field = |name: &str| {
            variant
                .get(name)
                .ok_or_else(|| failure(format!("Structured variant requires {name}")))
        };
        let cents = field("price")?
            .as_scalar()
            .and_then(|value| value.to_integer())
            .ok_or_else(|| failure("Structured price requires integer USD cents"))?;
        if cents < 0 {
            return Err(failure("Structured price must be nonnegative"));
        }
        let path = field("url")?.to_kstr();
        if !path.starts_with('/') {
            return Err(failure("Structured variant requires local URL"));
        }
        let availability = if field("available")?
            .as_scalar()
            .and_then(|value| value.to_bool())
            .ok_or_else(|| failure("Structured availability requires boolean"))?
        {
            "InStock"
        } else {
            "OutOfStock"
        };
        variant_json.push(format!("{{\"@type\":\"Product\",\"name\":{},\"sku\":{},\"url\":{},\"offers\":{{\"@type\":\"Offer\",\"priceCurrency\":\"USD\",\"price\":{},\"availability\":{}}}}}",
            quote(&format!("{title} - {}", field("title")?.to_kstr()))?, quote(&field("sku")?.to_kstr())?, quote(&format!("{origin}{path}"))?, quote(&format!("{}.{:02}", cents / 100, cents % 100))?, quote(&format!("https://schema.org/{availability}"))?));
    }
    Ok(Value::scalar(format!("{{\"@context\":\"https://schema.org\",\"@type\":\"ProductGroup\",\"@id\":{},\"name\":{},\"description\":{},\"url\":{},\"image\":[{}],\"brand\":{{\"@type\":\"Brand\",\"name\":{}}},\"productGroupID\":{},\"hasVariant\":[{}]}}",
        quote(&format!("{canonical}#product"))?, quote(&title)?, quote(&description)?, quote(&canonical)?, images, quote(&field("vendor")?.to_kstr())?, quote(&field("id")?.to_kstr())?, variant_json.join(","))))
}

impl fmt::Display for FilterNode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)
    }
}

impl Filter for FilterNode {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        self.context.note(&self.name)?;
        let positional = self
            .positional
            .iter()
            .map(|value| value.evaluate(runtime).map(ValueCow::into_owned))
            .collect::<Result<Vec<_>>>()?;
        let keyword = self
            .keyword
            .iter()
            .map(|(name, value)| {
                value
                    .evaluate(runtime)
                    .map(|value| (name.as_str(), value.into_owned()))
            })
            .collect::<Result<BTreeMap<_, _>>>()?;
        let positional_bounds = match self.name.as_str() {
            "placeholder_svg_tag" => (0, 1),
            "standard_event_data"
            | "link_to"
            | "date"
            | "item_count_for_variant"
            | "color_contrast"
            | "color_lighten"
            | "color_darken" => (1, 1),
            "font_modify" | "color_modify" => (2, 2),
            _ => (0, 0),
        };
        if positional.len() < positional_bounds.0 || positional.len() > positional_bounds.1 {
            return Err(failure(format!(
                "Unsupported positional arguments for {}",
                self.name
            )));
        }
        let accepts_keywords = matches!(
            self.name.as_str(),
            "t" | "image_url"
                | "image_tag"
                | "preload_tag"
                | "stylesheet_tag"
                | "font_face"
                | "standard_event_data"
        );
        if !accepts_keywords && !keyword.is_empty() {
            return Err(failure(format!(
                "Unsupported keyword arguments for {}",
                self.name
            )));
        }
        match self.name.as_str() {
            "escape" => {
                // Shopify option values are Drops with a name string projection.
                // Preserve ordinary stdlib escaping for all other values.
                let text = option_value_name(input).unwrap_or_else(|| input.to_kstr());
                Ok(Value::scalar(html_escape(&text)))
            }
            "structured_data" => {
                if self.context.fixture["manifest"]["mock_contract"]["structured_data"]
                    != "synthetic Schema.org ProductGroup"
                {
                    return Err(failure(
                        "structured_data requires the declared synthetic product contract",
                    ));
                }
                if self.context.fixture["globals"]["shop"]["currency"] != "USD" {
                    return Err(failure("Fixture structured_data supports USD only"));
                }
                product_structured_data(input, runtime)
            }
            "placeholder_svg_tag" => {
                if input.to_kstr() != "hero-apparel-1" {
                    return Err(failure("Unsupported fixture placeholder"));
                }
                let class = positional
                    .first()
                    .map(|value| value.to_kstr().into_owned())
                    .unwrap_or_default();
                let class = class
                    .replace('&', "&amp;")
                    .replace('"', "&quot;")
                    .replace('<', "&lt;")
                    .replace('>', "&gt;");
                Ok(Value::scalar(format!("<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 1200 800\" class=\"{class}\" role=\"img\" aria-label=\"Fixture placeholder\"><rect width=\"1200\" height=\"800\" fill=\"#e8e8e8\"/></svg>")))
            }
            "color_brightness" => {
                if !positional.is_empty() || !keyword.is_empty() {
                    return Err(failure("color_brightness accepts no arguments"));
                }
                let color = Color::parse(input.to_kstr().as_str())?;
                Ok(Value::scalar(
                    (f64::from(color.red) * 299.0
                        + f64::from(color.green) * 587.0
                        + f64::from(color.blue) * 114.0)
                        / 1000.0,
                ))
            }
            "t" => {
                let key = input.to_kstr();
                let mut text = &self.context.locales;
                for part in key.split('.') {
                    text = text
                        .get(part)
                        .ok_or_else(|| failure(format!("Missing fixture translation: {key}")))?;
                }
                if let Some(count) = keyword.get("count").filter(|_| text.is_object()) {
                    let count = count.as_scalar().and_then(|value| value.to_integer());
                    let category = match count {
                        Some(1) => "one",
                        Some(count) if self.context.locale == "pl" => {
                            let count = count.unsigned_abs();
                            if (2..=4).contains(&(count % 10))
                                && !(12..=14).contains(&(count % 100))
                            {
                                "few"
                            } else {
                                "many"
                            }
                        }
                        _ => "other",
                    };
                    text = text.get(category).ok_or_else(|| {
                        failure(format!(
                            "Missing fixture plural translation: {key}.{category}"
                        ))
                    })?;
                }
                let text = text
                    .as_str()
                    .ok_or_else(|| failure("Fixture translation must be scalar"))?;
                static MATCHER: LazyLock<std::result::Result<regex::Regex, regex::Error>> =
                    LazyLock::new(|| regex::Regex::new(r"\{\{\s*(\w+)\s*\}\}"));
                let matcher = MATCHER.as_ref().map_err(failure)?;
                Ok(Value::scalar(
                    matcher
                        .replace_all(text, |captures: &regex::Captures<'_>| {
                            captures
                                .get(1)
                                .and_then(|name| keyword.get(name.as_str()))
                                .map(|value| value.to_kstr().into_string())
                                .unwrap_or_else(|| {
                                    captures
                                        .get(0)
                                        .map(|matched| matched.as_str())
                                        .unwrap_or("")
                                        .to_owned()
                                })
                        })
                        .into_owned(),
                ))
            }
            "image_url" => {
                let image = input
                    .as_object()
                    .ok_or_else(|| failure("Fixture image_url requires image object"))?;
                let source = image
                    .get("src")
                    .ok_or_else(|| failure("Image requires src"))?
                    .to_kstr();
                let width = keyword
                    .get("width")
                    .and_then(|value| value.as_scalar())
                    .and_then(|value| value.to_integer())
                    .ok_or_else(|| failure("image_url requires width"))?;
                if keyword.len() != 1 || width <= 0 {
                    return Err(failure("Unsupported image_url options"));
                }
                Ok(Value::scalar(format!("{source}?width={width}")))
            }
            "image_tag" => {
                let url = input.to_kstr();
                if !url.starts_with("/cdn/shop/") {
                    return Err(failure("image_tag requires fixture URL"));
                }
                let mut attributes = format!("src=\"{}\"", html_escape(&url));
                if let Some(widths) = keyword.get("widths") {
                    let base = url
                        .rsplit_once("?width=")
                        .map(|(base, _)| base)
                        .unwrap_or(&url);
                    let widths = widths
                        .to_kstr()
                        .split(',')
                        .map(|width| width.trim().parse::<i64>().map_err(failure))
                        .collect::<Result<Vec<_>>>()?;
                    let srcset = widths
                        .iter()
                        .map(|width| format!("{base}?width={width} {width}w"))
                        .collect::<Vec<_>>()
                        .join(", ");
                    attributes.push_str(&format!(" srcset=\"{}\"", html_escape(&srcset)));
                }
                for (name, value) in keyword {
                    if name == "widths" || value.is_nil() {
                        continue;
                    }
                    if !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_-".contains(&byte))
                    {
                        return Err(failure("Invalid image attribute"));
                    }
                    attributes.push_str(&format!(" {name}=\"{}\"", html_escape(&value.to_kstr())));
                }
                Ok(Value::scalar(format!("<img {attributes}>")))
            }
            "handleize" => {
                let text = input.to_kstr();
                if !text
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b" _-".contains(&byte))
                {
                    return Err(failure("Fixture handleize supports ASCII identifiers only"));
                }
                let text = text.to_ascii_lowercase().replace('_', "-");
                let text = regex::Regex::new(r"[ -]+")
                    .map_err(failure)?
                    .replace_all(&text, "-");
                Ok(Value::scalar(text.trim_matches('-').to_owned()))
            }
            "money" | "money_with_currency" | "money_without_currency" => {
                if self.context.fixture["globals"]["shop"]["currency"] != "USD" {
                    return Err(failure("Fixture money supports USD only"));
                }
                let cents = if input.is_nil() {
                    0
                } else {
                    input
                        .as_scalar()
                        .and_then(|value| value.to_integer())
                        .ok_or_else(|| failure("Money requires integer cents"))?
                };
                let absolute = cents.unsigned_abs();
                let digits = (absolute / 100).to_string();
                let mut grouped = String::new();
                for (index, digit) in digits.chars().enumerate() {
                    if index != 0 && (digits.len() - index) % 3 == 0 {
                        grouped.push(',');
                    }
                    grouped.push(digit);
                }
                let sign = if cents < 0 { "-" } else { "" };
                let currency = if self.name == "money_with_currency" {
                    " USD"
                } else {
                    ""
                };
                let symbol = if self.name == "money_without_currency" {
                    ""
                } else {
                    "$"
                };
                Ok(Value::scalar(format!(
                    "{sign}{symbol}{grouped}.{:02}{currency}",
                    absolute % 100
                )))
            }
            "inline_asset_content" | "asset_url" => {
                let name = input.to_kstr();
                if name.is_empty()
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
                {
                    return Err(failure("Invalid fixture asset name"));
                }
                let body = self.context.asset_content(name.as_str())?;
                Ok(Value::scalar(if self.name == "asset_url" {
                    format!("/assets/{name}")
                } else {
                    body
                }))
            }
            "standard_event_data" => {
                let resource = input
                    .as_object()
                    .ok_or_else(|| failure("Event requires product/cart object"))?;
                if positional.first().map(|value| value.to_kstr()) != Some("view".into()) {
                    return Err(failure("Fixture supports product/cart view events only"));
                }
                let context = keyword.get("context").cloned().unwrap_or(Value::Nil);
                let serialize = |value: &dyn ValueView| {
                    serde_json::to_string(&value.to_value()).map_err(failure)
                };
                let payload = if resource.contains_key("products") && resource.contains_key("id") {
                    let id = resource
                        .get("id")
                        .ok_or_else(|| failure("Collection requires id"))?;
                    format!(
                        "{{\"event\":\"view\",\"collection_id\":{},\"context\":{}}}",
                        serialize(id)?,
                        serialize(&context)?
                    )
                } else if let Some(id) = resource.get("id") {
                    format!(
                        "{{\"event\":\"view\",\"product_id\":{},\"context\":{}}}",
                        serialize(id)?,
                        serialize(&context)?
                    )
                } else if resource.contains_key("items") && resource.contains_key("total_price") {
                    let count = resource
                        .get("item_count")
                        .ok_or_else(|| failure("Cart event requires item_count"))?;
                    let price = resource
                        .get("total_price")
                        .ok_or_else(|| failure("Cart event requires total_price"))?;
                    format!("{{\"event\":\"view\",\"cart_item_count\":{},\"cart_total_price\":{},\"context\":{}}}",serialize(count)?,serialize(price)?,serialize(&context)?)
                } else {
                    return Err(failure("Event requires product or cart"));
                };
                Ok(Value::scalar(payload))
            }
            "json" => {
                let text = if self.context.fixture["manifest"]["mock_contract"]["json_object_order"]
                    == "sorted"
                {
                    // serde_json maps sort keys recursively for the declared mock protocol.
                    let value = serde_json::to_value(input.to_value()).map_err(failure)?;
                    serde_json::to_string(&value).map_err(failure)?
                } else {
                    serde_json::to_string(&input.to_value()).map_err(failure)?
                };
                Ok(Value::scalar(text))
            }
            "preload_tag" => {
                let mut output = format!(
                    "<link rel=\"preload\" href=\"{}\"",
                    html_escape(&input.to_kstr())
                );
                for (name, value) in keyword {
                    output.push_str(&format!(" {name}=\"{}\"", html_escape(&value.to_kstr())));
                }
                output.push('>');
                Ok(Value::scalar(output))
            }
            "stylesheet_tag" => {
                if keyword.keys().any(|key| *key != "preload") {
                    return Err(failure("Unsupported stylesheet_tag options"));
                }
                let url = html_escape(&input.to_kstr());
                let preload = if keyword
                    .get("preload")
                    .is_some_and(|value| value.query_state(State::Truthy))
                {
                    format!("<link rel=\"preload\" href=\"{url}\" as=\"style\">")
                } else {
                    String::new()
                };
                Ok(Value::scalar(format!(
                    "{preload}<link rel=\"stylesheet\" href=\"{url}\">"
                )))
            }
            "link_to" => {
                let url = positional
                    .first()
                    .ok_or_else(|| failure("link_to requires URL"))?
                    .to_kstr();
                Ok(Value::scalar(format!(
                    "<a href=\"{}\">{}</a>",
                    html_escape(&url),
                    html_escape(&input.to_kstr())
                )))
            }
            "font_modify" => {
                let mut font = fixture_font(input)?;
                let property = positional
                    .first()
                    .ok_or_else(|| failure("font_modify requires property"))?
                    .to_kstr();
                let value = positional
                    .get(1)
                    .ok_or_else(|| failure("font_modify requires value"))?;
                match property.as_str() {
                    "weight" => {
                        let weight = if value.to_kstr() == "bold" {
                            700
                        } else {
                            value
                                .as_scalar()
                                .and_then(|value| value.to_integer())
                                .ok_or_else(|| failure("Font weight requires integer"))?
                        };
                        font.insert("weight".into(), Value::scalar(weight));
                    }
                    "style" => {
                        if !["normal", "italic"].contains(&value.to_kstr().as_str()) {
                            return Err(failure("Unsupported font style"));
                        }
                        font.insert("style".into(), value.clone());
                    }
                    _ => return Err(failure("Unsupported font property")),
                }
                Ok(Value::Object(font))
            }
            "font_face" => {
                fixture_font(input)?;
                if keyword.keys().any(|key| *key != "font_display") {
                    return Err(failure("Unsupported font_face options"));
                }
                Ok(Value::scalar(""))
            }
            "date" => {
                let format = positional
                    .first()
                    .ok_or_else(|| failure("date requires format"))?
                    .to_kstr();
                let value = if ["now", "today"].contains(&input.to_kstr().as_str()) {
                    self.context.note("date:fixture_clock")?;
                    liquid_core::model::to_value(&self.context.fixture["manifest"]["created_at"])?
                } else {
                    input.to_value()
                };
                let date = value.as_scalar().and_then(|value| value.to_date_time());
                if let Some(date) = date {
                    if !format.is_empty() {
                        return Ok(Value::scalar(date.format(&format).map_err(failure)?));
                    }
                }
                Ok(value)
            }
            "payment_terms" => {
                let form = input
                    .as_object()
                    .ok_or_else(|| failure("payment_terms requires form"))?;
                if !form
                    .get("type")
                    .is_some_and(|value| ["cart", "product"].contains(&value.to_kstr().as_str()))
                    || self.context.fixture["manifest"]["platform_capabilities"]["payment_terms"]
                        != false
                {
                    return Err(failure(
                        "payment_terms requires explicitly disabled product/cart financing",
                    ));
                }
                Ok(Value::scalar(""))
            }
            "payment_button" => {
                let form = input
                    .as_object()
                    .ok_or_else(|| failure("payment_button requires form"))?;
                if form.get("type").map(|value| value.to_kstr()) != Some("product".into())
                    || self.context.fixture["manifest"]["platform_capabilities"]["payment_button"]
                        != false
                {
                    return Err(failure(
                        "payment_button requires explicitly disabled product accelerated checkout",
                    ));
                }
                Ok(Value::scalar(""))
            }
            "item_count_for_variant" => {
                let cart = input
                    .as_object()
                    .ok_or_else(|| failure("Variant count requires cart"))?;
                let items = cart
                    .get("items")
                    .and_then(ValueView::as_array)
                    .ok_or_else(|| failure("Cart requires items"))?;
                let variant_id = positional
                    .first()
                    .ok_or_else(|| failure("Variant count requires variant_id"))?;
                let mut quantity = 0i64;
                for item in items.values() {
                    let item = item
                        .as_object()
                        .ok_or_else(|| failure("Cart item requires object"))?;
                    let id = item.get("variant_id").or_else(|| {
                        item.get("variant")
                            .and_then(ValueView::as_object)
                            .and_then(|variant| variant.get("id"))
                    });
                    if id.is_some_and(|id| id.to_value() == *variant_id) {
                        quantity += item
                            .get("quantity")
                            .and_then(ValueView::as_scalar)
                            .and_then(|quantity| quantity.to_integer())
                            .ok_or_else(|| failure("Cart item requires quantity"))?;
                    }
                }
                Ok(Value::scalar(quantity))
            }
            "color_contrast" => {
                let first = Color::parse(input.to_kstr().as_str())?.luminance();
                let second = Color::parse(
                    positional
                        .first()
                        .ok_or_else(|| failure("color_contrast requires second color"))?
                        .to_kstr()
                        .as_str(),
                )?
                .luminance();
                let contrast = (first.max(second) + 0.05) / (first.min(second) + 0.05);
                Ok(Value::scalar((contrast * 10.0).round() / 10.0))
            }
            "color_lighten" | "color_darken" => {
                let amount = positional
                    .first()
                    .and_then(ValueView::as_scalar)
                    .and_then(|value| value.to_float())
                    .ok_or_else(|| failure("Color shift requires amount"))?;
                if !(0.0..=100.0).contains(&amount) {
                    return Err(failure("Color shift must be 0..100"));
                }
                Ok(Value::scalar(
                    Color::parse(input.to_kstr().as_str())?.shifted_lightness(
                        if self.name == "color_darken" {
                            -amount
                        } else {
                            amount
                        },
                    ),
                ))
            }
            "color_modify" => {
                let property = positional
                    .first()
                    .ok_or_else(|| failure("color_modify requires property"))?
                    .to_kstr();
                let amount = positional
                    .get(1)
                    .and_then(ValueView::as_scalar)
                    .and_then(|value| value.to_float())
                    .ok_or_else(|| failure("color_modify requires alpha"))?;
                if property != "alpha" || !(0.0..=1.0).contains(&amount) {
                    return Err(failure("color_modify supports alpha0..1 only"));
                }
                let color = Color::parse(input.to_kstr().as_str())?;
                Ok(Value::scalar(format!(
                    "rgba({}, {}, {}, {})",
                    color.red,
                    color.green,
                    color.blue,
                    positional[1].to_kstr()
                )))
            }
            "md5" => {
                if !positional.is_empty() || !keyword.is_empty() {
                    return Err(failure("md5 accepts no arguments"));
                }
                Ok(Value::scalar(format!(
                    "{:x}",
                    md5::compute(input.to_kstr().as_bytes())
                )))
            }
            _ => Err(failure(format!(
                "Executed unsupported fixture filter: {}",
                self.name
            ))),
        }
    }
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

struct CaptureCompiler {
    source: Sources,
    language: Arc<OnceLock<Arc<Language>>>,
}

impl PartialCompiler for CaptureCompiler {
    fn source(&self) -> &dyn PartialSource {
        &self.source
    }
    fn compile(self, language: Arc<Language>) -> Result<Box<dyn PartialStore + Send + Sync>> {
        self.language
            .set(language.clone())
            .map_err(|_| failure("Language already initialized"))?;
        OnDemandCompiler::new(self.source).compile(language)
    }
}

fn language(context: Arc<Context>) -> Result<Arc<Language>> {
    let source = context.sources.clone();
    let mut builder = liquid::ParserBuilder::with_stdlib();
    let known: BTreeSet<String> = builder
        .filters()
        .map(|filter| filter.name().to_owned())
        .collect();
    let matcher = regex::Regex::new(r"\|\s*([A-Za-z_][A-Za-z0-9_]*)").map_err(failure)?;
    let mut names = BTreeSet::new();
    for body in source.0.values() {
        for captures in matcher.captures_iter(body) {
            if let Some(name) = captures.get(1) {
                names.insert(name.as_str().to_owned());
            }
        }
    }
    for name in names
        .iter()
        .filter(|name| !known.contains(*name) || ["date", "escape"].contains(&name.as_str()))
    {
        builder = builder.filter(PlatformFilter {
            name: name.clone(),
            context: context.clone(),
        });
    }
    for name in ["render", "content_for", "sections"] {
        builder = builder.tag(PlatformTag {
            name,
            context: context.clone(),
        });
    }
    for (name, end) in [
        ("schema", "endschema"),
        ("doc", "enddoc"),
        ("stylesheet", "endstylesheet"),
        ("javascript", "endjavascript"),
        ("style", "endstyle"),
        ("paginate", "endpaginate"),
        ("form", "endform"),
    ] {
        builder = builder.block(PlatformBlock {
            name,
            end,
            context: context.clone(),
        });
    }
    let captured = Arc::new(OnceLock::new());
    builder
        .partials(CaptureCompiler {
            source,
            language: captured.clone(),
        })
        .build()?;
    captured
        .get()
        .cloned()
        .ok_or_else(|| failure("Missing compiled language"))
}

#[derive(Debug, PartialEq, Eq)]
struct RenderOutput {
    html: Vec<u8>,
    css: String,
    sections: Vec<String>,
    error: Option<String>,
}

impl RenderOutput {
    fn ensure_success(&self) -> std::result::Result<(), Box<dyn std::error::Error>> {
        match &self.error {
            Some(error) => Err(error.clone().into()),
            None => Ok(()),
        }
    }
}

fn select_product_variant(product: &mut Json, variant_id: Option<&Json>) -> Result<()> {
    let original = product["variants"]
        .as_array()
        .ok_or_else(|| failure("Canonical product requires variants"))?;
    let mut variants = original.clone();
    let selected_index = variant_id
        .map(|id| {
            variants
                .iter()
                .position(|variant| variant["id"] == *id)
                .ok_or_else(|| failure("Selected variant is missing from canonical product"))
        })
        .transpose()?;
    for (index, variant) in variants.iter_mut().enumerate() {
        variant["selected"] = json!(Some(index) == selected_index);
    }
    let selected = selected_index
        .map(|index| variants[index].clone())
        .unwrap_or(Json::Null);
    let chosen = selected_index
        .map(|index| variants[index].clone())
        .or_else(|| {
            product
                .get("first_available_variant")
                .filter(|value| value.is_object())
                .cloned()
        })
        .or_else(|| variants.first().cloned())
        .ok_or_else(|| failure("Product requires at least one variant"))?;
    let selected_options = chosen["options"].as_array().cloned().unwrap_or_default();
    let root = product
        .as_object_mut()
        .ok_or_else(|| failure("Expected product object"))?;
    root.insert("selected_variant".to_owned(), selected);
    root.insert("selected_or_first_available_variant".to_owned(), chosen);
    if let Some(options) = root
        .get_mut("options_with_values")
        .and_then(Json::as_array_mut)
    {
        for (ordinal, option) in options.iter_mut().enumerate() {
            let index = option["position"]
                .as_u64()
                .and_then(|position| position.checked_sub(1))
                .and_then(|position| usize::try_from(position).ok())
                .unwrap_or(ordinal);
            let Some(chosen) = selected_options.get(index) else {
                continue;
            };
            option["selected_value"] = chosen.clone();
            if let Some(values) = option["values"].as_array_mut() {
                for value in values {
                    value["selected"] = json!(value["name"] == *chosen);
                    let matching = variants
                        .iter()
                        .filter(|variant| {
                            variant["options"]
                                .as_array()
                                .and_then(|options| options.get(index))
                                == Some(&value["name"])
                        })
                        .collect::<Vec<_>>();
                    let same_other = matching.iter().find(|variant| {
                        variant["options"].as_array().is_some_and(|options| {
                            options.len() == selected_options.len()
                                && options.iter().enumerate().all(|(slot, name)| {
                                    slot == index || *name == selected_options[slot]
                                })
                        })
                    });
                    value["variant"] = same_other
                        .or_else(|| matching.iter().find(|variant| variant["available"] == true))
                        .or_else(|| matching.first())
                        .map(|variant| (**variant).clone())
                        .unwrap_or(Json::Null);
                }
            }
        }
        let named = options
            .iter()
            .filter_map(|option| {
                option["name"]
                    .as_str()
                    .map(|name| (name.to_lowercase(), option.clone()))
            })
            .collect::<serde_json::Map<_, _>>();
        root.insert("options_by_name".to_owned(), Json::Object(named));
    }
    root.insert("variants".to_owned(), json!(variants));
    Ok(())
}

// Page resources come from the same canonical fixture registries as picker settings.
// These are bounded platform projections; they do not alter the Liquid source.
fn page_globals(fixture: &Json, page: &str, diagnostic_only: bool) -> Result<(Object, String)> {
    let page_data = &fixture["pages"][page];
    if !page_data.is_object() && !diagnostic_only {
        return Err(failure("Missing page metadata"));
    }
    let mut globals = fixture["globals"].clone();
    let root = globals
        .as_object_mut()
        .ok_or_else(|| failure("Fixture globals must be an object"))?;
    for (target, source) in [("page_title", "title"), ("page_description", "description")] {
        root.insert(target.to_owned(), page_data[source].clone());
    }
    if let Some(current_page) = page_data.get("current_page") {
        if current_page.as_u64().filter(|page| *page > 0).is_none() {
            return Err(failure("Page current_page must be a positive integer"));
        }
        root.insert("current_page".to_owned(), current_page.clone());
    }
    root.entry("current_page").or_insert(json!(1));
    root.entry("current_tags").or_insert(json!([]));
    if let Some(request_id) = page_data.get("request_id") {
        let request = root
            .get_mut("request")
            .and_then(Json::as_object_mut)
            .ok_or_else(|| failure("Page request identity requires request object"))?;
        request.insert(
            "id".to_owned(),
            json!(request_id
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| request_id.to_string())),
        );
    }
    let kind = page_data["type"].as_str().unwrap_or(page);
    root.insert(
        "template".to_owned(),
        json!({"name":kind,"type":kind,"suffix":null}),
    );
    if let Some(canonical) = page_data["canonical_url"].as_str() {
        root.insert("canonical_url".to_owned(), json!(canonical));
    }
    if let Some(resource) = page_data.get("resource") {
        let resource_kind = resource["type"]
            .as_str()
            .ok_or_else(|| failure("Page resource requires type"))?;
        if resource_kind != kind || !["product", "collection"].contains(&kind) {
            return Err(failure("Unsupported or mismatched page resource type"));
        }
        let handle = resource["handle"]
            .as_str()
            .ok_or_else(|| failure("Page resource requires handle"))?;
        let registry = if kind == "product" {
            "all_products"
        } else {
            "collections"
        };
        let mut value = fixture["globals"][registry]
            .get(handle)
            .filter(|value| value.is_object())
            .cloned()
            .ok_or_else(|| failure("Page resource is missing from canonical registry"))?;
        let variant_id = resource.get("variant_id");
        if variant_id.is_some_and(|id| kind != "product" || !id.is_u64()) {
            return Err(failure("Product variant_id must be a nonnegative integer"));
        }
        if kind == "product" {
            select_product_variant(&mut value, variant_id)?;
        }
        if let Some(registry) = root.get_mut(registry).and_then(Json::as_object_mut) {
            registry.insert(handle.to_owned(), value.clone());
        }
        root.insert(kind.to_owned(), value.clone());
        let mut closest = root
            .get("closest")
            .and_then(Json::as_object)
            .cloned()
            .unwrap_or_default();
        closest.insert(kind.to_owned(), value);
        root.insert("closest".to_owned(), Json::Object(closest));
        let request = root
            .get_mut("request")
            .and_then(Json::as_object_mut)
            .ok_or_else(|| failure("Resource page requires request object"))?;
        request.insert("page_type".to_owned(), json!(kind));
        request.insert("path".to_owned(), page_data["url"].clone());
    }
    let locale = root
        .get("request")
        .and_then(|request| request["locale"]["iso_code"].as_str())
        .unwrap_or("en")
        .to_owned();
    if !["en", "de", "pl"].contains(&locale.as_str()) {
        return Err(failure("Fixture locale supports en, de, and pl only"));
    }
    if let Some(language) = root
        .get("localization")
        .and_then(|value| value["language"]["iso_code"].as_str())
    {
        if language != locale {
            return Err(failure("Request locale and localization language disagree"));
        }
    }
    Ok((liquid_core::model::to_object(&globals)?, locale))
}

struct Renderer {
    context: Arc<Context>,
    language: Arc<Language>,
    partials: Box<dyn PartialStore + Send + Sync>,
    page: String,
    template: Json,
    fixture_sha256: String,
}

impl Renderer {
    fn new(
        theme: PathBuf,
        store: &Path,
        page: &str,
        diagnostic_only: bool,
    ) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        Self::from_fixture_bytes(theme, fs::read(store)?, page, diagnostic_only)
    }

    fn from_fixture_bytes(
        theme: PathBuf,
        fixture_bytes: Vec<u8>,
        page: &str,
        diagnostic_only: bool,
    ) -> std::result::Result<Self, Box<dyn std::error::Error>> {
        if fixture_bytes.len() > MAX_FIXTURE_BYTES {
            return Err("Fixture exceeds the 64 MiB context limit".into());
        }
        let fixture_sha256 = format!("{:x}", Sha256::digest(&fixture_bytes));
        let fixture: Json =
            serde_json::from_str(&json_comments(std::str::from_utf8(&fixture_bytes)?)?)?;
        let sources = Sources::read(&theme)?;
        if fixture["synthetic"] != true || fixture["schema_version"] != 1 {
            return Err("Expected synthetic fixture schema version 1".into());
        }
        let definitions = read_json(&theme.join("config/settings_schema.json"))?
            .as_array()
            .ok_or("Settings schema must be an array")?
            .iter()
            .flat_map(|group| group["settings"].as_array().cloned().unwrap_or_default())
            .collect::<Vec<_>>();
        let schema = json!({"settings":definitions});
        let (mut globals, locale) = page_globals(&fixture, page, diagnostic_only)?;
        if let Some(Value::Object(cart)) = globals.get_mut("cart") {
            if let Some(Value::Array(items)) = cart.get_mut("items") {
                for (index, item) in items.iter_mut().enumerate() {
                    if let Value::Object(item) = item {
                        item.insert("index".into(), Value::scalar(index as i64));
                    }
                }
            }
        }
        let focal_points = fixture_focal_points(&globals)?;
        let mut context = Context {
            theme: theme.clone(),
            locales: read_json(&theme.join(format!(
                "locales/{}.json",
                if locale == "en" {
                    "en.default"
                } else {
                    &locale
                }
            )))?,
            sources,
            fixture,
            page: page.to_owned(),
            locale,
            globals,
            styles: Mutex::new(Vec::new()),
            scripts: Mutex::new(BTreeSet::new()),
            calls: Mutex::new(BTreeMap::new()),
            rendered_sources: Mutex::new(Vec::new()),
            palette: Vec::new(),
            focal_points,
            json_cache: Mutex::new(BTreeMap::new()),
            asset_cache: Mutex::new(BTreeMap::new()),
        };
        let mut settings = context.setting_defaults(&schema);
        if context.fixture["theme"]["configuration_source"] != "service" {
            let data = read_json(&theme.join("config/settings_data.json"))?;
            merge(&mut settings, &data["current"]);
        }
        merge(&mut settings, &context.fixture["theme"]["settings"]);
        let mut settings = liquid_core::model::to_object(&settings)?;
        context.materialize_settings(&mut settings, &schema, &context.globals)?;
        if let Some(colors) = settings.get("color_palette").and_then(ValueView::as_object) {
            context.palette = colors
                .iter()
                .map(|(name, value)| {
                    FixtureColor::new(value.to_kstr().as_str())
                        .map(|color| (name.into_owned(), color))
                })
                .collect::<Result<BTreeMap<_, _>>>()?
                .into_values()
                .collect();
        }
        context
            .globals
            .insert("settings".into(), Value::Object(settings));
        let context = Arc::new(context);
        let template = if diagnostic_only {
            Json::Null
        } else {
            let page_template = context.fixture["pages"][page]["template"]
                .as_str()
                .ok_or("Missing page template")?;
            context.json_source(page_template)?
        };
        Ok(Self::from_context(
            context,
            page.to_owned(),
            template,
            fixture_sha256,
        )?)
    }

    fn from_context(
        context: Arc<Context>,
        page: String,
        template: Json,
        fixture_sha256: String,
    ) -> Result<Self> {
        let language = language(context.clone())?;
        // The existing lazy compiler retains ASTs, never rendered response bytes.
        let partials = LazyCompiler::new(context.sources.clone()).compile(language.clone())?;
        Ok(Self {
            context,
            language,
            partials,
            page,
            template,
            fixture_sha256,
        })
    }

    fn parse_source(&self, name: &str) -> Result<()> {
        let source = self
            .context
            .sources
            .try_get(name)
            .ok_or_else(|| failure("Missing diagnostic source"))?;
        liquid_core::parser::parse(&source, &self.language)?;
        Ok(())
    }

    fn render(&mut self, scope: &str, only: Option<&str>) -> Result<RenderOutput> {
        let context = &self.context;
        let partials = &self.partials;
        context.reset_request()?;
        if !["hero", "template", "page"].contains(&scope) {
            return Err(failure("Unsupported scope"));
        }
        let mut html = Vec::new();
        let mut rendered = Vec::new();
        let mut error = None;
        for (position, id) in self.template["order"]
            .as_array()
            .ok_or_else(|| failure("Missing section order"))?
            .iter()
            .enumerate()
        {
            let id = id.as_str().ok_or_else(|| failure("Invalid section ID"))?;
            let section = &self.template["sections"][id];
            let kind = section["type"]
                .as_str()
                .ok_or_else(|| failure("Missing section type"))?;
            if scope == "hero" && position != 0 {
                continue;
            }
            if only.is_some_and(|only| only != kind && only != id) {
                continue;
            }
            let render = (|| -> Result<Vec<u8>> {
                let mut rendered = Vec::new();
                context.render_section(
                    &mut rendered,
                    partials.as_ref(),
                    id,
                    section,
                    position + 1,
                )?;
                Ok(rendered)
            })();
            match render {
                Ok(bytes) => {
                    html.extend(bytes);
                    rendered.push(id.to_owned());
                }
                Err(failure) => {
                    error = Some(failure.to_string());
                    break;
                }
            }
        }
        if error.is_none() && scope == "page" {
            let render = (|| -> Result<Vec<u8>> {
                let mut globals = Object::new();
                let body = String::from_utf8(html.clone()).map_err(failure)?;
                globals.insert("content_for_layout".into(), Value::scalar(body));
                globals.insert(
                    "content_for_header".into(),
                    Value::scalar("<!-- horizon-fixture-stylesheets -->"),
                );
                let globals = Arc::new(ScopeOverlay::root(globals));
                let values = globals_view(&context.globals, globals.as_ref());
                let runtime = RuntimeBuilder::new()
                    .set_globals(&values)
                    .set_partials(partials.as_ref())
                    .build();
                set_platform_bindings(&runtime, &globals);
                let runtime = FixtureRuntime {
                    inner: &runtime,
                    name: "layout/theme",
                    palette: &context.palette,
                    focal_points: &context.focal_points,
                };
                let mut output = Vec::new();
                context.record_source("layout/theme")?;
                partials
                    .get("layout/theme")?
                    .render_to(&mut output, &runtime)?;
                let body = String::from_utf8(output).map_err(failure)?;
                let marker = "<!-- horizon-fixture-stylesheets -->";
                if body.matches(marker).count() != 1 {
                    return Err(failure("Page layout must expose content_for_header once"));
                }
                let css = context
                    .styles
                    .lock()
                    .map_err(failure)?
                    .iter()
                    .map(|(_, body)| body.as_str())
                    .collect::<String>();
                Ok(body
                    .replacen(
                        marker,
                        &format!("<style data-horizon-fixture>{css}</style>"),
                        1,
                    )
                    .into_bytes())
            })();
            match render {
                Ok(bytes) => html = bytes,
                Err(failure) => error = Some(failure.to_string()),
            }
        }
        let css = context
            .styles
            .lock()
            .map_err(failure)?
            .iter()
            .map(|(_, body)| body.as_str())
            .collect::<String>();
        Ok(RenderOutput {
            html,
            css,
            sections: rendered,
            error,
        })
    }

    fn report(&self, output: &RenderOutput) -> Result<Json> {
        let context = &self.context;
        let report = json!({"engine":"liquid-rust","page":self.page,"page_type":context.fixture["pages"][&self.page]["type"].as_str().unwrap_or(&self.page),"locale":context.locale,"request_id":context.fixture["pages"][&self.page]["request_id"],"scenario_id":context.fixture["manifest"]["scenario_id"],"fixture_sha256":self.fixture_sha256,"theme_sha":context.fixture["theme"]["sha"],"theme":context.fixture["theme"],"store_schema_version":context.fixture["schema_version"],"configuration":{"parsing":"strict","strict_variables":false,"strict_filters":true},"sections_rendered":output.sections,"platform_calls":*context.calls.lock().map_err(failure)?,"sources":*context.rendered_sources.lock().map_err(failure)?,"platform_contract":{"focal_point":"registered exact numeric xy pairs project at expression evaluation; identical xy objects also match; not arbitrary Drop/filter-element coercion","option_value":"declared option shape uses display-name size; explicit size keys retain their values","pagination":"request current_page with scoped product window","font":"configured Inter uses local system Arial","events":"synthetic product/collection/cart view JSON","javascript":"raw inline script once per source; bundling unsupported","structured_data":"synthetic Schema.org ProductGroup; not hosted Shopify byte output","payment_button":"empty only when fixture explicitly disables service","form_submission":"unsupported","cart_item_index":"derived zero-based index","payment_terms":"empty only when fixture explicitly disables service","optional_variables":"missing optional properties resolve to nil","clock":context.fixture["manifest"]["created_at"]},"error":output.error});
        Ok(report)
    }

    fn write_output(
        &self,
        directory: &Path,
        output: &RenderOutput,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        fs::create_dir_all(directory)?;
        fs::write(directory.join("index.html"), &output.html)?;
        fs::write(directory.join("styles.css"), &output.css)?;
        fs::write(
            directory.join("report.json"),
            serde_json::to_string_pretty(&self.report(output)?)?,
        )?;
        Ok(())
    }
}

fn digest(bytes: &[u8]) -> Json {
    json!({"bytes":bytes.len(), "sha256":format!("{:x}", Sha256::digest(bytes))})
}

fn benchmark(
    renderer: &mut Renderer,
    options: &BTreeMap<String, String>,
    initialization_ms: f64,
) -> std::result::Result<Json, Box<dyn std::error::Error>> {
    let iterations = options
        .get("--iterations")
        .map(String::as_str)
        .unwrap_or("10")
        .parse::<usize>()?;
    let warmup = options
        .get("--warmup")
        .map(String::as_str)
        .unwrap_or("3")
        .parse::<usize>()?;
    if iterations == 0 {
        return Err("--iterations must be positive".into());
    }
    if warmup == 0 {
        return Err("--warmup must be positive to prime lazy AST caches".into());
    }
    if options
        .get("--benchmark-mode")
        .is_some_and(|mode| mode != "direct")
    {
        return Err("Rust benchmark supports --benchmark-mode direct only".into());
    }
    let scope = options.get("--scope").map(String::as_str).unwrap_or("hero");
    let only = options.get("--only").map(String::as_str);
    let output_dir = options.get("--output-dir").map(PathBuf::from);
    let mut expected = None;
    let mut samples = Vec::with_capacity(iterations);
    let mut samples_ms = Vec::with_capacity(iterations);
    let mut final_output = None;
    for index in 0..warmup
        .checked_add(iterations)
        .ok_or("Iteration count overflow")?
    {
        let start = Instant::now();
        let output = renderer.render(scope, only)?;
        let elapsed_ms = start.elapsed().as_secs_f64() * 1000.0;
        output.ensure_success()?;
        // Digests and correctness checks are deliberately outside the render timer.
        let fingerprints = (digest(&output.html), digest(output.css.as_bytes()));
        if expected
            .as_ref()
            .is_some_and(|value| value != &fingerprints)
        {
            return Err("Benchmark output changed between requests".into());
        }
        expected.get_or_insert_with(|| fingerprints.clone());
        if index >= warmup {
            samples_ms.push(elapsed_ms);
            samples.push(
                json!({"elapsed_ms":elapsed_ms, "html":fingerprints.0, "css":fingerprints.1}),
            );
        }
        final_output = Some(output);
    }
    let output = final_output.ok_or("Missing benchmark output")?;
    let (html, css) = expected.ok_or("Missing benchmark fingerprint")?;
    let diagnostics = renderer.report(&output)?;
    if let Some(directory) = output_dir {
        renderer.write_output(&directory, &output)?;
    }
    Ok(json!({
        "schema_version":1, "engine":"liquid-rust", "benchmark_mode":"direct",
        "initialization_ms":initialization_ms, "warmup":warmup, "iterations":iterations,
        "samples_ms":samples_ms, "samples":samples, "html":html, "css":css,
        "fixture_sha256":renderer.fixture_sha256,
        "theme_sha":renderer.context.fixture["theme"]["sha"],
        "scope":scope, "page":renderer.page,
        "correctness_verified":true, "response_cache":false,
        "build_profile":if cfg!(debug_assertions) {"debug"} else {"release"},
        "release":!cfg!(debug_assertions),
        "runtime":{"package_version":env!("CARGO_PKG_VERSION"),"os":std::env::consts::OS,"architecture":std::env::consts::ARCH},
        "timer":"std::time::Instant monotonic wall clock",
        "peak_rss_bytes":null, "samples_cpu_ms":null,
        "initialization_contract":"fixture/source/configuration loading and parser setup; lazy AST compilation is exercised by warmup",
        "request_contract":"fresh Liquid runtimes; styles, platform calls and source diagnostics reset; immutable source/schema/AST caches retained",
        "diagnostics":diagnostics
    }))
}

// The serving worker is bounded to the independently verified synthetic homepage.
// Only parsed/source caches persist: every response evaluates a fresh request.
const MAX_REQUEST_LINE_BYTES: usize = 4096;
const MAX_PROTOCOL_HEADER_BYTES: usize = 4096;
const HORIZON_THEME_SHA: &str = "5acd1b6b66c02f61d3216e3adace5dd9e0404fc9";
type WorkerResult<T> = std::result::Result<T, Box<dyn std::error::Error>>;

struct RenderOracle {
    html: Json,
    css: Json,
}

impl RenderOracle {
    fn pinned_homepage() -> Self {
        Self {
            html: json!({"bytes":435304,"sha256":"d97c35b3ba08f026536cb4c469623acb9957af59fb9a9171db012958515fe990"}),
            css: json!({"bytes":279112,"sha256":"67a6538e0b763c32ced001728ebf68f375dec497d4fb91c0ee136658ff9f2034"}),
        }
    }

    fn verify(&self, output: &RenderOutput) -> WorkerResult<()> {
        output.ensure_success()?;
        if digest(&output.html) != self.html || digest(output.css.as_bytes()) != self.css {
            return Err("Serving output differs from the independent homepage oracle".into());
        }
        Ok(())
    }

    fn verify_sizes(&self, output: &RenderOutput) -> WorkerResult<()> {
        output.ensure_success()?;
        if self.html["bytes"].as_u64() != Some(output.html.len() as u64)
            || self.css["bytes"].as_u64() != Some(output.css.len() as u64)
        {
            return Err("Serving output sizes differ from the independent homepage oracle".into());
        }
        Ok(())
    }
}

fn write_protocol_header(writer: &mut impl Write, header: &Json) -> WorkerResult<()> {
    let serialized = serde_json::to_vec(header)?;
    if serialized.len() >= MAX_PROTOCOL_HEADER_BYTES {
        return Err("Worker protocol header exceeds 4096 bytes".into());
    }
    writer.write_all(&serialized)?;
    writer.write_all(b"\n")?;
    Ok(())
}

fn write_worker_error(
    writer: &mut impl Write,
    request_id: Option<u64>,
    error: &dyn fmt::Display,
) -> WorkerResult<()> {
    // Bound diagnostics too; JSON escapes newlines so the frame stays on one line.
    let message = error.to_string().chars().take(512).collect::<String>();
    write_protocol_header(
        writer,
        &json!({"request_id":request_id,"html_bytes":0,"css_bytes":0,"error":message}),
    )?;
    writer.flush()?;
    Ok(())
}

fn read_worker_request(reader: &mut impl BufRead) -> WorkerResult<Option<u64>> {
    let mut line = Vec::new();
    // Limit read_until itself, rather than allocating an unbounded malformed line.
    let bytes = reader
        .by_ref()
        .take((MAX_REQUEST_LINE_BYTES + 1) as u64)
        .read_until(b'\n', &mut line)?;
    if bytes == 0 {
        return Ok(None);
    }
    if bytes > MAX_REQUEST_LINE_BYTES {
        return Err("Worker request line exceeds 4096 bytes".into());
    }
    if !line.ends_with(b"\n") {
        return Err("Worker request must end with a newline".into());
    }
    let request: Json = serde_json::from_slice(&line)?;
    let object = request
        .as_object()
        .ok_or("Worker request must be an object")?;
    if object.len() != 2 || request["action"] != "render" {
        return Err("Worker request requires only action=render and request_id".into());
    }
    let id = request["request_id"]
        .as_u64()
        .ok_or("Worker request_id must be an integer between 0 and 18446744073709551615")?;
    Ok(Some(id))
}

fn write_render_response(
    writer: &mut impl Write,
    request_id: u64,
    rendered: WorkerResult<RenderOutput>,
    oracle: &RenderOracle,
) -> WorkerResult<()> {
    let checked = rendered.and_then(|output| {
        // The common HTTP load client hashes every response. Both worker engines
        // check sizes here; startup warmups verify the independent SHA256 oracle.
        oracle.verify_sizes(&output)?;
        Ok(output)
    });
    let output = match checked {
        Ok(output) => output,
        Err(error) => {
            write_worker_error(writer, Some(request_id), &error)?;
            return Err(error);
        }
    };
    write_protocol_header(
        writer,
        &json!({"request_id":request_id,"html_bytes":output.html.len(),"css_bytes":output.css.len(),"error":null}),
    )?;
    writer.write_all(&output.html)?;
    writer.write_all(output.css.as_bytes())?;
    writer.flush()?;
    Ok(())
}

fn serve_stdio(
    renderer: &mut Renderer,
    scope: &str,
    warmup: usize,
    oracle: &RenderOracle,
    reader: &mut impl BufRead,
    writer: &mut impl Write,
) -> WorkerResult<()> {
    if warmup == 0 {
        let error = "--warmup must be positive to prime lazy AST caches";
        write_worker_error(writer, None, &error)?;
        return Err(error.into());
    }
    for _ in 0..warmup {
        let warmed = renderer.render(scope, None).map_err(Into::into);
        let checked = warmed.and_then(|output| oracle.verify(&output));
        if let Err(error) = checked {
            write_worker_error(writer, None, &error)?;
            return Err(error);
        }
    }
    write_protocol_header(
        writer,
        &json!({"action":"ready","schema_version":1,"engine":"liquid-rust",
            "theme_sha":renderer.context.fixture["theme"]["sha"],
            "fixture_sha256":renderer.fixture_sha256,"warmup":warmup,
            "build_profile":if cfg!(debug_assertions) {"debug"} else {"release"},
            "release":!cfg!(debug_assertions),"response_cache":false,"correctness_verified":true,
            "scope":scope,"page":renderer.page,"html":oracle.html,"css":oracle.css}),
    )?;
    writer.flush()?;
    loop {
        let id = match read_worker_request(reader) {
            Ok(Some(id)) => id,
            Ok(None) => return Ok(()),
            Err(error) => {
                write_worker_error(writer, None, &error)?;
                return Err(error);
            }
        };
        let rendered = renderer.render(scope, None).map_err(Into::into);
        write_render_response(writer, id, rendered, oracle)?;
    }
}

const MAX_FIXTURE_BYTES: usize = 64 * 1024 * 1024;

fn read_fixture_stdin(reader: &mut impl Read) -> WorkerResult<Vec<u8>> {
    read_bounded_fixture(reader, MAX_FIXTURE_BYTES)
}

fn read_bounded_fixture(reader: &mut impl Read, limit: usize) -> WorkerResult<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take((limit + 1) as u64).read_to_end(&mut bytes)?;
    if bytes.is_empty() || bytes.len() > limit {
        return Err("Expected nonempty authoritative fixture JSON within 64 MiB".into());
    }
    Ok(bytes)
}

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    let mut options = BTreeMap::new();
    let mut arguments = std::env::args().skip(1);
    while let Some(key) = arguments.next() {
        let value = if ["--serve-stdio", "--fixture-stdin"].contains(&key.as_str()) {
            "true".to_owned()
        } else {
            arguments.next().ok_or("Each option requires a value")?
        };
        options.insert(key, value);
    }
    let theme = PathBuf::from(
        options
            .get("--theme-root")
            .or_else(|| options.get("--theme"))
            .ok_or("--theme-root is required")?,
    );
    let stdin_fixture = options.contains_key("--fixture-stdin");
    let store = options.get("--fixture").or_else(|| options.get("--store"));
    if stdin_fixture == store.is_some() {
        return Err("Use exactly one of --fixture/--store or --fixture-stdin".into());
    }
    if stdin_fixture && options.contains_key("--serve-stdio") {
        return Err("--fixture-stdin and --serve-stdio cannot share stdin".into());
    }
    let page = options.get("--page").map(String::as_str).unwrap_or("index");
    let serving = options.contains_key("--serve-stdio");
    let scope = options
        .get("--scope")
        .map(String::as_str)
        .unwrap_or(if serving { "page" } else { "hero" });
    if !["hero", "template", "page"].contains(&scope) {
        return Err("Unsupported scope".into());
    }
    if serving
        && (scope != "page"
            || page != "index"
            || [
                "--only",
                "--parse-source",
                "--benchmark-json",
                "--output-dir",
            ]
            .iter()
            .any(|key| options.contains_key(*key)))
    {
        return Err(
            "--serve-stdio requires the complete index page and no artifact/diagnostic options"
                .into(),
        );
    }
    if !serving
        && !options.contains_key("--output-dir")
        && !options.contains_key("--benchmark-json")
    {
        return Err("--output-dir is required".into());
    }
    if let Some(path) = options.get("--benchmark-json") {
        match fs::remove_file(path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    let start = Instant::now();
    let mut renderer = if stdin_fixture {
        let bytes = read_fixture_stdin(&mut io::stdin().lock())?;
        Renderer::from_fixture_bytes(theme, bytes, page, options.contains_key("--parse-source"))?
    } else {
        Renderer::new(
            theme,
            Path::new(store.ok_or("Missing fixture path")?),
            page,
            options.contains_key("--parse-source"),
        )?
    };
    let initialization_ms = start.elapsed().as_secs_f64() * 1000.0;
    if serving {
        if renderer.context.fixture["theme"]["sha"] != HORIZON_THEME_SHA {
            return Err("Serving fixture theme SHA does not match the pin".into());
        }
        let warmup = options
            .get("--warmup")
            .map(String::as_str)
            .unwrap_or("50")
            .parse::<usize>()?;
        return serve_stdio(
            &mut renderer,
            scope,
            warmup,
            &RenderOracle::pinned_homepage(),
            &mut io::stdin().lock(),
            &mut io::stdout().lock(),
        );
    }
    if let Some(name) = options.get("--parse-source") {
        renderer.parse_source(name)?;
        println!("Parsed {name}");
        return Ok(());
    }
    if let Some(path) = options.get("--benchmark-json") {
        let report = benchmark(&mut renderer, &options, initialization_ms)?;
        let text = serde_json::to_string_pretty(&report)?;
        if let Some(parent) = Path::new(path)
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        fs::write(path, &text)?;
        println!("{text}");
        return Ok(());
    }
    let output_dir = PathBuf::from(
        options
            .get("--output-dir")
            .ok_or("--output-dir is required")?,
    );
    let output = renderer.render(scope, options.get("--only").map(String::as_str))?;
    renderer.write_output(&output_dir, &output)?;
    output.ensure_success()?;
    println!("{}", output_dir.join("index.html").display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(sources: &[(&str, &str)]) -> Arc<Context> {
        Arc::new(Context {
            theme: PathBuf::new(),
            locales: json!({}),
            sources: Sources::new(
                sources
                    .iter()
                    .map(|(name, body)| ((*name).to_owned(), (*body).to_owned()))
                    .collect(),
            ),
            fixture: json!({"globals":{"shop":{"currency":"USD"}},"theme":{}}),
            page: "index".to_owned(),
            locale: "en".to_owned(),
            globals: liquid_core::object!({"shop":{"name":"Fixture"}}),
            styles: Mutex::new(Vec::new()),
            scripts: Mutex::new(BTreeSet::new()),
            calls: Mutex::new(BTreeMap::new()),
            rendered_sources: Mutex::new(Vec::new()),
            palette: Vec::new(),
            focal_points: BTreeMap::new(),
            json_cache: Mutex::new(BTreeMap::new()),
            asset_cache: Mutex::new(BTreeMap::new()),
        })
    }

    #[test]
    fn page_resources_use_canonical_registry_and_selected_variant() {
        let fixture = json!({"globals":{"shop":{"name":"Fixture"},"canonical_url":"https://fixture.example/","request":{"locale":{"iso_code":"de"}},"localization":{"language":{"iso_code":"de"}},"all_products":{"boot":{"id":7,"title":"Boot","variants":[{"id":71},{"id":72}]}}},"pages":{"product":{"type":"product","url":"/products/boot","canonical_url":"https://fixture.example/products/boot","title":"Boot page","description":"Original synthetic fixture","resource":{"type":"product","handle":"boot","variant_id":72}}}});
        let (globals, locale) = page_globals(&fixture, "product", false).unwrap();
        assert_eq!(locale, "de");
        assert_eq!(
            globals["product"]
                .as_object()
                .unwrap()
                .get("selected_variant")
                .unwrap()
                .as_object()
                .unwrap()
                .get("id")
                .unwrap()
                .to_kstr(),
            "72"
        );
        assert_eq!(
            globals["closest"]
                .as_object()
                .unwrap()
                .get("product")
                .unwrap()
                .to_value(),
            globals["product"]
        );
        assert_eq!(
            globals["canonical_url"].to_kstr(),
            "https://fixture.example/products/boot"
        );
        assert_eq!(
            globals["request"]
                .as_object()
                .unwrap()
                .get("path")
                .unwrap()
                .to_kstr(),
            "/products/boot"
        );
        assert_eq!(
            fixture["globals"]["all_products"]["boot"].get("selected_variant"),
            None
        );
        let mut invalid = fixture.clone();
        invalid["pages"]["product"]["resource"]["variant_id"] = json!(73);
        assert!(page_globals(&invalid, "product", false).is_err());
        invalid = fixture.clone();
        invalid["globals"]["localization"]["language"]["iso_code"] = json!("en");
        assert!(page_globals(&invalid, "product", false).is_err());
    }

    #[test]
    fn index_metadata_keeps_absolute_canonical_and_diagnostic_parse_needs_no_page() {
        let fixture = json!({"globals":{"canonical_url":"https://fixture.example/","current_tags":[],"request":{"locale":{"iso_code":"en"}}},"pages":{"index":{"title":"Home","description":"Home description","url":"/"}}});
        let (globals, _) = page_globals(&fixture, "index", false).unwrap();
        assert_eq!(
            globals["canonical_url"].to_kstr(),
            "https://fixture.example/"
        );
        assert_eq!(globals["current_tags"].as_array().unwrap().size(), 0);
        assert!(page_globals(&fixture, "absent", true).is_ok());
        assert!(page_globals(&fixture, "absent", false).is_err());
    }

    #[test]
    fn page_overrides_and_page_closest_survive_section_and_snippet_frames() {
        let mut host = Arc::try_unwrap(context(&[("sections/probe", "{{ section.settings.text }}:{% content_for 'blocks' %}{% schema %}{\"tag\":null,\"settings\":[{\"id\":\"text\",\"type\":\"text\",\"default\":\"default\"}]}{% endschema %}"), ("blocks/child", "{% render 'leaf' %}{% schema %}{\"tag\":null}{% endschema %}"), ("leaf", "{{ closest.product.title }}/{{ closest.collection.title }}")])).unwrap();
        host.page = "product".to_owned();
        host.globals.insert(
            "closest".into(),
            liquid_core::value!({"product":{"title":"Product"},"collection":{"title":"All"}}),
        );
        host.fixture["theme"] = json!({"section_overrides":{"main":{"settings":{"text":"base"}}},"page_overrides":{"product":{"section_overrides":{"main":{"settings":{"text":"product page"}}}}}});
        let mut renderer = Renderer::from_context(Arc::new(host), "product".to_owned(), json!({"order":["main"],"sections":{"main":{"type":"probe","blocks":{"child":{"type":"child"}},"block_order":["child"]}}}), "fixture".to_owned()).unwrap();
        assert_eq!(
            renderer.render("template", None).unwrap().html,
            b"product page:Product/All"
        );
    }

    #[test]
    fn pagination_slices_only_the_body_and_preserves_collection_counts() {
        let source = "{% paginate collection.products by 2 %}{% for product in collection.products %}{{ product.id }}{% endfor %}:{{ collection.products_count }}:{{ paginate.pages }}:{{ paginate.next.url }}{% endpaginate %}|{{ collection.products.size }}";
        let values = liquid_core::object!({"collection":{"products":[{"id":1},{"id":2},{"id":3},{"id":4}],"products_count":4},"request":{"path":"/collections/all"},"current_page":1});
        assert_eq!(
            render(context(&[]), source, values).unwrap(),
            "12:4:2:/collections/all?page=2|4"
        );
        let values = liquid_core::object!({"products":[1,2,3,4,5],"current_page":2,"request":{"path":"/collections/all"}});
        assert_eq!(render(context(&[]), "{% paginate products by 2 %}{{ products | join: ',' }}:{{ paginate.current_offset }}:{{ paginate.previous.url }}:{{ paginate.next.url }}{% endpaginate %}", values).unwrap(), "3,4:2:/collections/all?page=1:/collections/all?page=3");
        let values = liquid_core::object!({"products":[1,2,3,4,5],"current_page":3});
        assert_eq!(render(context(&[]), "{% paginate products by 2 %}{{ products | join: ',' }}:{{ paginate.current_offset }}:{{ paginate.next | default: 'nil' }}{% endpaginate %}", values).unwrap(), "5:4:nil");
        let values = liquid_core::object!({"products":[1,2,3],"current_page":3});
        assert!(render(
            context(&[]),
            "{% paginate products by 2 %}x{% endpaginate %}",
            values
        )
        .is_err());
    }

    #[test]
    fn javascript_preserves_raw_source_and_deduplicates_per_request() {
        let mut renderer = prepared(&[("sections/probe", "{% render 'script' %}{% render 'script' %}{% schema %}{\"tag\":null}{% endschema %}"), ("script", "{% javascript %}\nconst raw = '{{ ignored }}';\n{% endjavascript %}")], json!({"one":{"type":"probe"}}), json!(["one"]));
        let expected = b"<script data-shopify>\nconst raw = '{{ ignored }}';\n</script>";
        for _ in 0..2 {
            assert_eq!(renderer.render("template", None).unwrap().html, expected);
        }
        assert_eq!(renderer.context.scripts.lock().unwrap().len(), 1);
    }

    #[test]
    fn declared_json_order_option_value_escape_and_disabled_checkout_are_bounded() {
        let mut host = Arc::try_unwrap(context(&[(
            "probe",
            "{{ option | escape }}{{ object | json }}{{ form | payment_button }}",
        )]))
        .unwrap();
        host.fixture["manifest"] = json!({"mock_contract":{"json_object_order":"sorted"},"platform_capabilities":{"payment_button":false}});
        let values = liquid_core::object!({"option":{"id":1,"name":"A&B","available":true,"selected":true,"variant":Value::Nil,"product_url":Value::Nil},"object":{"z":{"y":2,"a":1},"a":0},"form":{"type":"product"}});
        assert_eq!(render(Arc::new(host), "{{ option | escape }}:{{ option.size }}|{{ object | json }}|{{ form | payment_button }}", values).unwrap(), "A&amp;B:3|{\"a\":0,\"z\":{\"a\":1,\"y\":2}}|");
        assert!(render(
            context(&[("probe", "{{ form | payment_button }}")]),
            "{{ form | payment_button }}",
            liquid_core::object!({"form":{"type":"product"}})
        )
        .is_err());
    }

    #[test]
    fn structured_product_group_uses_real_fixture_variants_and_locale_canonical() {
        let product = liquid_core::value!({"id":7,"title":"Canvas & daypack","description":"<p>Original mock</p>","vendor":"Harbor","images":[{"src":"/cdn/shop/files/mock.svg"}],"variants":[{"id":71,"title":"Blue","sku":"MOCK-71","url":"/products/pack?variant=71","price":9500,"available":true}]});
        let globals =
            liquid_core::object!({"canonical_url":"https://fixture.example/de/products/pack"});
        let runtime = RuntimeBuilder::new().set_globals(&globals).build();
        let output = product_structured_data(&product, &runtime)
            .unwrap()
            .to_kstr()
            .into_string();
        let parsed: Json = serde_json::from_str(&output).unwrap();
        assert!(output.starts_with(
            "{\"@context\":\"https://schema.org\",\"@type\":\"ProductGroup\",\"@id\":"
        ));
        assert_eq!(
            parsed["@id"],
            "https://fixture.example/de/products/pack#product"
        );
        assert_eq!(parsed["description"], "Original mock");
        assert_eq!(
            parsed["image"][0],
            "https://fixture.example/cdn/shop/files/mock.svg"
        );
        assert_eq!(
            parsed["hasVariant"][0]["url"],
            "https://fixture.example/products/pack?variant=71"
        );
        assert_eq!(parsed["hasVariant"][0]["offers"]["price"], "95.00");
        assert!(product_structured_data(&Value::Nil, &runtime).is_err());
    }

    #[test]
    fn polish_integer_plural_categories_use_the_actual_locale_keys() {
        let mut host =
            Arc::try_unwrap(context(&[("probe", "{{ 'items' | t: count: count }}")])).unwrap();
        host.locale = "pl".to_owned();
        host.locales = json!({"items":{"one":"one","few":"few","many":"many","other":"other"}});
        let host = Arc::new(host);
        for (count, expected) in [
            (1, "one"),
            (2, "few"),
            (4, "few"),
            (12, "many"),
            (14, "many"),
            (22, "few"),
            (25, "many"),
            (0, "many"),
        ] {
            assert_eq!(
                render(
                    host.clone(),
                    "{{ 'items' | t: count: count }}",
                    liquid_core::object!({"count":count})
                )
                .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn authoritative_stdin_context_preserves_exact_bytes_and_rejects_invalid_bounds() {
        let bytes = b"{\n  \"synthetic\": true\n}\n";
        assert_eq!(
            read_fixture_stdin(&mut io::Cursor::new(bytes)).unwrap(),
            bytes
        );
        assert!(read_fixture_stdin(&mut io::Cursor::new(b"")).is_err());
        let mut large = io::Cursor::new(b"0123456789");
        assert!(read_bounded_fixture(&mut large, 4).is_err());
        assert_eq!(large.position(), 5);
    }

    #[test]
    fn service_configuration_uses_authoritative_settings_without_local_current_data() {
        let directory = std::env::temp_dir().join(format!(
            "horizon-service-config-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        for name in [
            "snippets",
            "blocks",
            "sections",
            "layout",
            "config",
            "locales",
            "templates",
        ] {
            fs::create_dir_all(directory.join(name)).unwrap();
        }
        // Original minimal theme: persisted local settings are deliberately absent.
        fs::write(
            directory.join("config/settings_schema.json"),
            r#"[{"settings":[{"id":"title","type":"text","default":"schema default"}]}]"#,
        )
        .unwrap();
        fs::write(directory.join("locales/en.default.json"), "{}").unwrap();
        fs::write(
            directory.join("sections/probe.liquid"),
            "{{ settings.title }}{% schema %}{\"tag\":null}{% endschema %}",
        )
        .unwrap();
        fs::write(
            directory.join("templates/index.json"),
            r#"{"order":["main"],"sections":{"main":{"type":"probe"}}}"#,
        )
        .unwrap();
        let mut fixture = json!({"synthetic":true,"schema_version":1,"theme":{"configuration_source":"service","settings":{"title":"service setting"}},"globals":{"request":{"locale":{"iso_code":"en"}},"localization":{"language":{"iso_code":"en"}}},"pages":{"index":{"template":"templates/index.json"}}});
        let mut renderer = Renderer::from_fixture_bytes(
            directory.clone(),
            serde_json::to_vec(&fixture).unwrap(),
            "index",
            false,
        )
        .unwrap();
        assert_eq!(
            renderer.render("template", None).unwrap().html,
            b"service setting"
        );
        fixture["theme"]
            .as_object_mut()
            .unwrap()
            .remove("configuration_source");
        assert!(Renderer::from_fixture_bytes(
            directory.clone(),
            serde_json::to_vec(&fixture).unwrap(),
            "index",
            false
        )
        .is_err());
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn focal_point_views_survive_loop_assign_capture_render_and_keep_xy_properties() {
        let values = liquid_core::object!({"image":{"presentation":{"focal_point":{"x":25.0,"y":75.0}}},"media":[{"presentation":{"focal_point":{"x":25.0,"y":75.0}}}],"ordinary":{"x":1,"y":2}});
        let source = "{% assign saved = image.presentation.focal_point %}{{ saved }}:{{ saved.x }}/{{ saved.y }}|{{ 'point=' | append: saved }}|{{ saved | json }}|{% for item in media %}{% assign point = item.presentation.focal_point %}{% capture label %}{{ point }}{% endcapture %}{{ item.presentation.focal_point }}:{{ label }}:{% render 'leaf', point: point %}{% endfor %}|{{ ordinary | json }}";
        let mut host = Arc::try_unwrap(context(&[
            ("leaf", "{{ point }}:{{ point.x }}/{{ point.y }}"),
            ("probe", source),
        ]))
        .unwrap();
        host.focal_points = fixture_focal_points(&values).unwrap();
        host.fixture["manifest"] = json!({"mock_contract":{"json_object_order":"sorted"}});
        let host = Arc::new(host);
        assert_eq!(render(host.clone(), source, values.clone()).unwrap(), "25.0% 75.0%:25.0/75.0|point=25.0% 75.0%|{\"x\":25.0,\"y\":75.0}|25.0% 75.0%:25.0% 75.0%:25.0% 75.0%:25.0/75.0|{\"x\":1,\"y\":2}");
        assert_eq!(render(host, source, values.clone()).unwrap(), "25.0% 75.0%:25.0/75.0|point=25.0% 75.0%|{\"x\":25.0,\"y\":75.0}|25.0% 75.0%:25.0% 75.0%:25.0% 75.0%:25.0/75.0|{\"x\":1,\"y\":2}");
        assert_eq!(
            values["image"]
                .as_object()
                .unwrap()
                .get("presentation")
                .unwrap()
                .as_object()
                .unwrap()
                .get("focal_point")
                .unwrap()
                .to_value(),
            liquid_core::value!({"x":25.0,"y":75.0})
        );
    }

    #[test]
    fn focal_point_registry_rejects_invalid_coordinates_and_keeps_numeric_types() {
        for value in [
            liquid_core::value!({"focal_point":{"x":"25","y":75.0}}),
            liquid_core::value!({"focal_point":{"x":-1.0,"y":75.0}}),
            liquid_core::value!({"focal_point":{"x":25.0,"y":101.0}}),
            liquid_core::value!({"focal_point":{"x":25.0,"y":75.0,"extra":true}}),
        ] {
            assert!(fixture_focal_points(&value).is_err());
        }
        let points =
            fixture_focal_points(&liquid_core::value!({"focal_point":{"x":25,"y":75.0}})).unwrap();
        let point = points.values().next().unwrap();
        assert_eq!(point.to_kstr(), "25% 75.0%");
        assert_eq!(point.to_value(), liquid_core::value!({"x":25,"y":75.0}));
        assert!(
            fixture_focal_points(&liquid_core::value!({"focal_point":nil}))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn option_value_size_uses_names_inside_loop_assign_and_sandbox_frames() {
        let source = "{% for item in options %}{% assign saved = item %}{{ item.size }}/{{ saved.size }}/{{ saved | escape }}:{{ saved.name }}{% endfor %}|{{ ordinary.size }}";
        let host = context(&[("probe", source)]);
        let values = liquid_core::object!({"options":[{"id":1,"name":"A&B🌲","available":true,"selected":true,"variant":nil,"product_url":nil}],"ordinary":{"a":1,"b":2}});
        assert_eq!(
            render(host, source, values).unwrap(),
            "4/4/A&amp;B🌲:A&B🌲|2"
        );
        for (size, expected) in [(Value::Nil, "nil"), (Value::scalar(99), "99")] {
            let item = liquid_core::value!({"id":1,"name":"label","available":true,"selected":true,"variant":nil,"product_url":nil,"size":size});
            assert_eq!(
                render(
                    context(&[]),
                    "{{ item.size | default: 'nil' }}",
                    liquid_core::object!({"item":item})
                )
                .unwrap(),
                expected
            );
        }
        let globals = liquid_core::object!({"private":{"id":1,"name":"parent secret","available":true,"selected":true,"variant":nil,"product_url":nil}});
        let inner = RuntimeBuilder::new().set_globals(&globals).build();
        let palette = vec![];
        let points = BTreeMap::new();
        let fixture = FixtureRuntime {
            inner: &inner,
            name: "probe",
            palette: &palette,
            focal_points: &points,
        };
        let locals = liquid_core::object!({"item":{"id":2,"name":"local","available":true,"selected":true,"variant":nil,"product_url":nil}});
        let sandbox = liquid_core::runtime::SandboxedStackFrame::new(&fixture, &locals);
        for (path, expected) in [
            ("item.size", Value::scalar(5)),
            ("private.size", Value::Nil),
            ("private[missing].size", Value::Nil),
        ] {
            let expression =
                Expression::Variable(liquid_core::parser::parse_variable(path).unwrap());
            assert_eq!(expression.evaluate(&sandbox).unwrap().to_value(), expected);
        }
    }

    fn render(context: Arc<Context>, source: &str, values: Object) -> Result<String> {
        let language = language(context.clone())?;
        let partials = OnDemandCompiler::new(context.sources.clone()).compile(language.clone())?;
        let values = Arc::new(ScopeOverlay::root(values));
        let runtime = RuntimeBuilder::new()
            .set_globals(values.as_ref())
            .set_partials(partials.as_ref())
            .build();
        set_platform_bindings(&runtime, &values);
        let runtime = FixtureRuntime {
            inner: &runtime,
            name: "test",
            palette: &context.palette,
            focal_points: &context.focal_points,
        };
        Template::new(liquid_core::parser::parse(source, &language)?).render(&runtime)
    }

    #[test]
    fn immutable_scope_overlay_borrows_parents_and_nil_shadows_present_values() {
        let root = Arc::new(ScopeOverlay::root(
            liquid_core::object!({"section":{"id":"parent","blocks":[{"id":"child"}]},"block":{"id":"old"},"closest":{"product":{"title":"Original"}}}),
        ));
        let child = ScopeOverlay::child(
            root.clone(),
            liquid_core::object!({"block":{"id":"new"},"closest":Value::Nil,"extra":7}),
        );
        assert!(std::ptr::eq(
            child.get("section").unwrap(),
            root.get("section").unwrap()
        ));
        assert!(child.get("closest").unwrap().is_nil());
        assert!(child.contains_key("closest"));
        assert_eq!(child.size(), 4);
        assert_eq!(
            child
                .keys()
                .map(|key| key.into_owned().to_string())
                .collect::<Vec<_>>(),
            vec!["block", "closest", "extra", "section"]
        );
        assert_eq!(child.iter().count(), 4);
        assert_eq!(child.values().count(), 4);
        let expected = liquid_core::object!({"section":{"id":"parent","blocks":[{"id":"child"}]},"block":{"id":"new"},"closest":Value::Nil,"extra":7});
        assert_eq!(child.to_value(), expected.to_value());
        let sorted = ObjectView::iter(&root.local)
            .chain(ObjectView::iter(&child.local))
            .collect::<BTreeMap<_, _>>();
        assert_eq!(child.render().to_string(), sorted.render().to_string());
        assert_eq!(child.source().to_string(), sorted.source().to_string());
        assert_eq!(child.to_kstr(), sorted.to_kstr());
        assert!(child.query_state(State::Truthy));
        assert!(!child.query_state(State::Empty));
        assert_eq!(
            root.get("block")
                .unwrap()
                .as_object()
                .unwrap()
                .get("id")
                .unwrap()
                .to_kstr(),
            "old"
        );
    }

    #[test]
    fn render_preserves_platform_globals_and_isolates_caller_assigns() {
        let context = context(&[(
            "probe",
            "{{ block.id }}/{{ section.id }}/{{ shop.name }}/{{ closest.product.title }}/{{ private | default: 'nil' }}",
        )]);
        let values = liquid_core::object!({"block":{"id":"child"},"section":{"id":"parent"},"closest":{"product":{"title":"Original product"}}});
        assert_eq!(
            render(
                context,
                "{% assign private = 'caller' %}{% assign block = 'shadow' %}{% assign section = 'shadow' %}{% assign closest = 'shadow' %}{% render 'probe' %}",
                values
            )
            .unwrap(),
            "child/parent/Fixture/Original product/nil"
        );
    }

    #[test]
    fn sibling_block_mutations_do_not_leak_and_closest_is_preserved() {
        let context = context(&[("blocks/child", "{{ block.id }}:{{ closest.product.title }};{% assign block = 'corrupted' %}{% schema %}{\"tag\":null}{% endschema %}")]);
        let values = liquid_core::object!({"section":{"id":"section","blocks":[{"id":"first","type":"child"},{"id":"second","type":"child"}]},"product":{"title":"Test product"}});
        assert_eq!(
            render(
                context,
                "{% content_for 'blocks', closest.product: product %}",
                values
            )
            .unwrap(),
            "first:Test product;second:Test product;"
        );
    }

    #[test]
    fn unknown_filter_in_dead_branch_does_not_hide_executed_errors() {
        let context = context(&[("test", "{{ 'value' | unavailable_platform_filter }}")]);
        assert_eq!(
            render(
                context.clone(),
                "{% if false %}{{ 'value' | unavailable_platform_filter }}{% endif %}ok",
                Object::new()
            )
            .unwrap(),
            "ok"
        );
        let error = render(
            context,
            "{{ 'value' | unavailable_platform_filter }}",
            Object::new(),
        )
        .unwrap_err()
        .to_string();
        assert!(
            error.contains("Executed unsupported fixture filter: unavailable_platform_filter"),
            "{error}"
        );
    }

    #[test]
    fn stylesheet_is_deduplicated_by_template_without_changing_html() {
        let context = context(&[(
            "css",
            "{% stylesheet %}\n.example { color: red; }\n{% endstylesheet %}x",
        )]);
        assert_eq!(
            render(
                context.clone(),
                "{% render 'css' %}{% render 'css' %}",
                Object::new()
            )
            .unwrap(),
            "xx"
        );
        assert_eq!(
            context.styles.lock().unwrap().as_slice(),
            &[(
                "snippets/css.liquid".to_owned(),
                "\n.example { color: red; }\n".to_owned()
            )]
        );
    }

    #[test]
    fn fixture_color_properties_and_invalid_unicode_are_explicit() {
        let context = context(&[]);
        assert_eq!(
            render(
                context,
                "{{ color.rgb }}|{{ color.alpha }}",
                liquid_core::object!({"color":"#102030"})
            )
            .unwrap(),
            "16 32 48|1.0"
        );
        assert!(Color::parse("#aéaaa").is_err());
        assert!(Color::parse("not-a-color").is_err());
    }

    #[test]
    fn configuration_comments_preserve_quoted_urls() {
        let text = r#"/* lead */ {"url":"https://example.test/a/*b*/",// line
        "text":"//still text"} /* tail */"#;
        assert_eq!(
            serde_json::from_str::<Json>(&json_comments(text).unwrap()).unwrap(),
            json!({"url":"https://example.test/a/*b*/","text":"//still text"})
        );
    }

    #[test]
    fn md5_uses_the_algorithm_for_arbitrary_input() {
        let context = context(&[("test", "{{ 'value' | md5 }}")]);
        assert_eq!(
            render(
                context.clone(),
                "{{ 'product-card-link-static-product-card-1101' | md5 }}",
                Object::new()
            )
            .unwrap(),
            "2bab5c2fa5048d79521f5ff6bb8a9684"
        );
        assert_eq!(
            render(context, "{{ 'abc' | md5 }}", Object::new()).unwrap(),
            "900150983cd24fb0d6963f7d28e17f72"
        );
    }
    #[test]
    fn content_for_locals_are_block_outer_scope_and_static_types_are_checked() {
        let context=context(&[("blocks/child","{{ variant }}/{{ block.id }}{% render 'leaf' %}{% schema %}{\"tag\":null}{% endschema %}"),("leaf","/{{ variant | default: 'nil' }}")]);
        let values = liquid_core::object!({"section":{"blocks":[{"id":"menu","type":"child","settings":{}}]}});
        assert_eq!(
            render(
                context.clone(),
                "{% content_for 'block', type: 'child', id: 'menu', variant: 'mobile' %}",
                values.clone()
            )
            .unwrap(),
            "mobile/menu/nil"
        );
        assert!(render(
            context,
            "{% content_for 'block', type: 'wrong', id: 'menu' %}",
            values
        )
        .unwrap_err()
        .to_string()
        .contains("type mismatch"));
    }

    #[test]
    fn setting_bindings_use_global_settings_and_preserve_typed_values() {
        let mut settings = liquid_core::object!({"foreground":"{{ settings.palette.foreground }}","count":"{{ closest.product.count }}","text":"Count {{ closest.product.count }}"});
        let globals = liquid_core::object!({"settings":{"palette":{"foreground":"#123456"}},"closest":{"product":{"count":4}}});
        bind_settings(&mut settings, &globals).unwrap();
        assert_eq!(settings["foreground"], Value::scalar("#123456"));
        assert_eq!(settings["count"], Value::scalar(4));
        assert_eq!(settings["text"], Value::scalar("Count 4"));
    }

    #[test]
    fn setting_bindings_preserve_unicode_paths_whitespace_and_missing_values() {
        let globals = liquid_core::object!({"catalog":{"café":7}});
        for _ in 0..2 {
            let mut settings = liquid_core::object!({"count":"{{\u{a0}catalog.café\u{a0}}}","text":"Count {{ catalog.café }}; {{ missing }}"});
            bind_settings(&mut settings, &globals).unwrap();
            assert_eq!(settings["count"], Value::scalar(7));
            assert_eq!(settings["text"], Value::scalar("Count 7; "));
        }
    }

    #[test]
    fn translation_captures_preserve_unicode_whitespace_and_unknown_parameters() {
        let source = "{{ 'message' | t: name: name }}";
        let mut host = Arc::try_unwrap(context(&[("translation", source)])).unwrap();
        host.locales = json!({"message":"Hello, {{\u{a0}name\u{a0}}}! {{ missing }}"});
        let host = Arc::new(host);
        for (name, expected) in [
            ("Björk", "Hello, Björk! {{ missing }}"),
            ("Żaneta", "Hello, Żaneta! {{ missing }}"),
        ] {
            assert_eq!(
                render(host.clone(), source, liquid_core::object!({"name":name})).unwrap(),
                expected
            );
        }
    }

    #[test]
    fn fake_forms_have_sorted_attributes_and_no_submission_side_effects() {
        let context = context(&[]);
        assert_eq!(render(context,"{% form 'customer', id: 'signup', class: 'form' %}{{ form.type }}:{{ form.id }}{% endform %}",Object::new()).unwrap(),"<form method=\"post\" action=\"/contact\" class=\"form\" id=\"signup\"><input type=\"hidden\" name=\"form_type\" value=\"customer\">customer:signup</form>");
        assert!(json_comments("{\"value\":1} /* unterminated").is_err());
    }
    #[test]
    fn borrowed_palette_retains_color_properties_in_standard_for_scopes() {
        let mut context = Arc::try_unwrap(context(&[])).unwrap();
        context.palette = vec![
            FixtureColor::new("#102030").unwrap(),
            FixtureColor::new("#00000080").unwrap(),
        ];
        let context = Arc::new(context);
        assert_eq!(render(context,"{% for color in settings.color_palette %}{{ color }}:{{ color.alpha }}:{{ color.rgb }};{% endfor %}",Object::new()).unwrap(),"#102030:1.0:16 32 48;#00000080:0.5019607843137255:0 0 0;");
    }
    #[test]
    fn financing_requires_explicit_disabled_capability_and_clock_is_frozen() {
        let mut host = Arc::try_unwrap(context(&[(
            "filters",
            "{{ form | payment_terms }}{{ 'now' | date: '%Y' }}",
        )]))
        .unwrap();
        host.fixture["manifest"] = json!({"created_at":"2026-01-15 12:00:00 +0000","platform_capabilities":{"payment_terms":false}});
        let host = Arc::new(host);
        let values = liquid_core::object!({"form":{"type":"cart"}});
        assert_eq!(
            render(
                host.clone(),
                "{{ form | payment_terms }}{{ 'now' | date: '%Y' }}",
                values.clone()
            )
            .unwrap(),
            "2026"
        );
        let mut enabled = Arc::try_unwrap(host).unwrap();
        enabled.fixture["manifest"]["platform_capabilities"]["payment_terms"] = Json::Bool(true);
        assert!(
            render(Arc::new(enabled), "{{ form | payment_terms }}", values)
                .unwrap_err()
                .to_string()
                .contains("explicitly disabled product/cart financing")
        );
    }
    #[test]
    fn render_keyword_variables_can_be_reassigned_before_iteration() {
        let context = context(&[(
            "leaf",
            "{% assign order = order | split: ',' %}{% for item in order %}{{ item }};{% endfor %}",
        )]);
        assert_eq!(
            render(
                context,
                "{% render 'leaf', order: 'one,two' %}",
                Object::new()
            )
            .unwrap(),
            "one;two;"
        );
    }
    #[test]
    fn declared_optional_variable_policy_reaches_loop_and_capture_frames() {
        let context = context(&[]);
        let values = liquid_core::object!({"items":[{"parent_relationship":Value::Nil}]});
        assert_eq!(render(context,"{% for item in items %}{% capture relation %}{{ item.parent_relationship.parent | default: 'absent' }}{% endcapture %}{{ relation }}{% endfor %}",values).unwrap(),"absent");
    }
    fn prepared(sources: &[(&str, &str)], sections: Json, order: Json) -> Renderer {
        Renderer::from_context(
            context(sources),
            "index".to_owned(),
            json!({"sections":sections,"order":order}),
            "fixture-digest".to_owned(),
        )
        .unwrap()
    }

    #[test]
    fn prepared_renderer_reuses_asts_but_clears_all_request_state() {
        let mut renderer = prepared(&[("sections/sample", "{% stylesheet %}.fresh { color: red; }{% endstylesheet %}{{ section.id }}:{% increment counter %}{% schema %}{}{% endschema %}")],json!({"one":{"type":"sample"}}),json!(["one"]));
        let original_globals = renderer.context.globals.clone();
        let first = renderer.render("template", None).unwrap();
        assert_eq!(
            first.html,
            b"<div id=\"shopify-section-one\" class=\"shopify-section\">one:0</div>"
        );
        assert_eq!(first.css, ".fresh { color: red; }");
        let ast = renderer.partials.get("sections/sample").unwrap();
        renderer
            .context
            .styles
            .lock()
            .unwrap()
            .push(("stale".to_owned(), "stale css".to_owned()));
        renderer
            .context
            .calls
            .lock()
            .unwrap()
            .insert("stale".to_owned(), 99);
        renderer
            .context
            .rendered_sources
            .lock()
            .unwrap()
            .push("stale.liquid".to_owned());
        let second = renderer.render("template", None).unwrap();
        assert_eq!(first, second);
        assert!(Arc::ptr_eq(
            &ast,
            &renderer.partials.get("sections/sample").unwrap()
        ));
        assert!(renderer
            .context
            .sources
            .1
            .lock()
            .unwrap()
            .contains_key("sections/sample"));
        assert_eq!(renderer.context.globals, original_globals);
        let report = renderer.report(&second).unwrap();
        assert!(report["platform_calls"].get("stale").is_none());
        assert_eq!(report["sources"], json!(["sections/sample.liquid"]));
    }

    #[test]
    fn failed_request_does_not_pollute_later_successful_request() {
        let mut renderer = prepared(&[("sections/good", "{% stylesheet %}.good {}{% endstylesheet %}ok{% schema %}{}{% endschema %}"),("sections/bad", "{% stylesheet %}.bad {}{% endstylesheet %}{{ 'value' | unavailable_fixture_filter }}{% schema %}{}{% endschema %}")],json!({"good":{"type":"good"},"bad":{"type":"bad"}}),json!(["good","bad"]));
        let failed = renderer.render("template", None).unwrap();
        assert!(failed
            .error
            .as_ref()
            .unwrap()
            .contains("unavailable_fixture_filter"));
        assert!(failed.css.contains(".bad"));
        let succeeded = renderer.render("template", Some("good")).unwrap();
        assert_eq!(succeeded.error, None);
        assert_eq!(succeeded.css, ".good {}");
        assert_eq!(succeeded.sections, vec!["good"]);
        let report = renderer.report(&succeeded).unwrap();
        assert_eq!(report["sources"], json!(["sections/good.liquid"]));
        assert!(report["platform_calls"]
            .get("unavailable_fixture_filter")
            .is_none());
    }

    #[test]
    fn benchmark_reports_each_checked_warm_result_and_rejects_unprimed_workload() {
        let mut renderer = prepared(
            &[("sections/sample", "rendered{% schema %}{}{% endschema %}")],
            json!({"one":{"type":"sample"}}),
            json!(["one"]),
        );
        let mut options = BTreeMap::from([
            ("--scope".to_owned(), "template".to_owned()),
            ("--iterations".to_owned(), "2".to_owned()),
            ("--warmup".to_owned(), "1".to_owned()),
        ]);
        let report = benchmark(&mut renderer, &options, 1.0).unwrap();
        assert_eq!(report["samples"].as_array().unwrap().len(), 2);
        assert_eq!(report["samples_ms"].as_array().unwrap().len(), 2);
        for sample in report["samples"].as_array().unwrap() {
            assert_eq!(sample["html"], report["html"]);
            assert_eq!(sample["css"], report["css"]);
            assert!(sample["elapsed_ms"].as_f64().unwrap() >= 0.0);
        }
        assert_eq!(report["correctness_verified"], true);
        assert_eq!(report["response_cache"], false);
        options.insert("--warmup".to_owned(), "0".to_owned());
        assert!(benchmark(&mut renderer, &options, 1.0)
            .unwrap_err()
            .to_string()
            .contains("--warmup"));
    }
    fn served_fixture() -> (Renderer, RenderOracle, &'static [u8], &'static str) {
        let renderer = prepared(
            &[("sections/sample", "{% stylesheet %}.fresh {}{% endstylesheet %}{{ section.id }}:{% increment counter %}{% schema %}{}{% endschema %}"),
              ("layout/theme", "{{ content_for_header }}{{ content_for_layout }}")],
            json!({"one":{"type":"sample"}}), json!(["one"]),
        );
        let html = b"<style data-horizon-fixture>.fresh {}</style><div id=\"shopify-section-one\" class=\"shopify-section\">one:0</div>";
        let css = ".fresh {}";
        let oracle = RenderOracle {
            html: digest(html),
            css: digest(css.as_bytes()),
        };
        (renderer, oracle, html, css)
    }

    fn read_header(input: &mut io::Cursor<Vec<u8>>) -> Json {
        let mut line = String::new();
        input.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap()
    }

    #[test]
    fn stdio_worker_frames_fresh_responses_and_shuts_down_on_eof() {
        let (mut renderer, oracle, html, css) = served_fixture();
        let mut input = io::Cursor::new(
            b"{\"action\":\"render\",\"request_id\":7}\n{\"action\":\"render\",\"request_id\":8}\n"
                .to_vec(),
        );
        let mut output = Vec::new();
        serve_stdio(&mut renderer, "page", 2, &oracle, &mut input, &mut output).unwrap();
        let mut framed = io::Cursor::new(output);
        let ready = read_header(&mut framed);
        assert_eq!(ready["action"], "ready");
        assert_eq!(ready["schema_version"], 1);
        assert_eq!(ready["engine"], "liquid-rust");
        assert_eq!(ready["fixture_sha256"], "fixture-digest");
        assert_eq!(ready["warmup"], 2);
        assert_eq!(ready["response_cache"], false);
        assert_eq!(ready["correctness_verified"], true);
        assert_eq!(ready["html"], oracle.html);
        assert_eq!(ready["css"], oracle.css);
        for id in [7, 8] {
            let header = read_header(&mut framed);
            assert_eq!(
                header,
                json!({"request_id":id,"html_bytes":html.len(),"css_bytes":css.len(),"error":null})
            );
            let mut body = vec![0; html.len() + css.len()];
            framed.read_exact(&mut body).unwrap();
            assert_eq!(&body[..html.len()], html);
            assert_eq!(&body[html.len()..], css.as_bytes());
        }
        assert_eq!(framed.position(), framed.get_ref().len() as u64);
        assert_eq!(renderer.context.styles.lock().unwrap().len(), 1);
        assert_eq!(renderer.context.rendered_sources.lock().unwrap().len(), 2);
    }

    #[test]
    fn stdio_worker_bounds_requests_and_reports_invalid_frames_without_a_body() {
        let invalid = [
            b"{\"action\":\"render\",\"request_id\":-1}\n".to_vec(),
            b"{\"action\":\"render\",\"request_id\":18446744073709551616}\n".to_vec(),
            b"{\"action\":\"render\",\"request_id\":1.0}\n".to_vec(),
            b"{\"action\":\"render\",\"request_id\":\"1\"}\n".to_vec(),
            b"{\"action\":\"cached\",\"request_id\":1}\n".to_vec(),
            b"{\"action\":\"render\",\"request_id\":1,\"extra\":true}\n".to_vec(),
            b"{\"action\":\"render\",\"request_id\":1}".to_vec(),
            vec![b'x'; MAX_REQUEST_LINE_BYTES + 1],
        ];
        for bytes in invalid {
            let (mut renderer, oracle, _, _) = served_fixture();
            let mut input = io::Cursor::new(bytes);
            let mut output = Vec::new();
            assert!(
                serve_stdio(&mut renderer, "page", 1, &oracle, &mut input, &mut output).is_err()
            );
            let mut frames = io::Cursor::new(output);
            assert_eq!(read_header(&mut frames)["action"], "ready");
            let error = read_header(&mut frames);
            assert!(error["request_id"].is_null());
            assert_eq!(error["html_bytes"], 0);
            assert_eq!(error["css_bytes"], 0);
            assert!(error["error"]
                .as_str()
                .is_some_and(|message| !message.is_empty()));
            assert_eq!(frames.position(), frames.get_ref().len() as u64);
        }
        assert_eq!(
            read_worker_request(&mut io::Cursor::new(Vec::new())).unwrap(),
            None
        );
        assert_eq!(
            read_worker_request(&mut io::Cursor::new(
                b"{\"action\":\"render\",\"request_id\":0}\n"
            ))
            .unwrap(),
            Some(0)
        );
        assert_eq!(
            read_worker_request(&mut io::Cursor::new(
                b"{\"action\":\"render\",\"request_id\":18446744073709551615}\r\n"
            ))
            .unwrap(),
            Some(u64::MAX)
        );
    }

    #[test]
    fn stdio_worker_fails_before_ready_when_warmup_oracle_is_wrong() {
        let (mut renderer, mut oracle, _, _) = served_fixture();
        oracle.html = digest(b"independent output differs");
        for warmup in [0, 1] {
            let mut output = Vec::new();
            assert!(serve_stdio(
                &mut renderer,
                "page",
                warmup,
                &oracle,
                &mut io::Cursor::new(Vec::new()),
                &mut output
            )
            .is_err());
            let mut frames = io::Cursor::new(output);
            let error = read_header(&mut frames);
            assert!(error.get("action").is_none());
            assert_eq!(error["html_bytes"], 0);
            assert_eq!(error["css_bytes"], 0);
            assert!(error["error"].is_string());
            assert_eq!(frames.position(), frames.get_ref().len() as u64);
        }
    }

    #[test]
    fn stdio_worker_headers_bound_unicode_and_escaped_diagnostics() {
        let message = "🧪\0\n".repeat(1024);
        let mut output = Vec::new();
        write_worker_error(&mut output, Some(u64::MAX), &message).unwrap();
        assert!(output.len() <= MAX_PROTOCOL_HEADER_BYTES);
        assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
        let mut frames = io::Cursor::new(output);
        let header = read_header(&mut frames);
        assert_eq!(header["error"].as_str().unwrap().chars().count(), 512);
        assert_eq!(header["request_id"], u64::MAX);
        assert_eq!(frames.position(), frames.get_ref().len() as u64);
        let mut rejected = Vec::new();
        assert!(write_protocol_header(
            &mut rejected,
            &json!({"oversized":"\0".repeat(MAX_PROTOCOL_HEADER_BYTES)})
        )
        .is_err());
        assert!(rejected.is_empty());
    }

    #[test]
    fn stdio_worker_render_errors_return_zero_lengths_and_stop() {
        let mut renderer = prepared(&[("sections/bad", "partial HTML{% stylesheet %}partial CSS{% endstylesheet %}{{ 'value' | unsupported_fixture_filter }}{% schema %}{}{% endschema %}")], json!({"one":{"type":"bad"}}), json!(["one"]));
        let rendered = renderer.render("template", None).unwrap();
        assert!(rendered.error.is_some());
        let mut output = Vec::new();
        assert!(write_render_response(
            &mut output,
            9,
            Ok(rendered),
            &RenderOracle::pinned_homepage()
        )
        .is_err());
        let mut frames = io::Cursor::new(output);
        let header = read_header(&mut frames);
        assert_eq!(header["request_id"], 9);
        assert_eq!(header["html_bytes"], 0);
        assert_eq!(header["css_bytes"], 0);
        assert!(header["error"]
            .as_str()
            .unwrap()
            .contains("unsupported_fixture_filter"));
        assert_eq!(frames.position(), frames.get_ref().len() as u64);
    }

    #[test]
    fn borrowed_global_view_shares_store_values_and_keeps_explicit_nil_overrides() {
        let base = liquid_core::object!({"shop":{"name":"Original"},"optional":{"present":true}});
        let overrides = liquid_core::object!({"optional":Value::Nil,"new":"request"});
        let view = globals_view(&base, &overrides);
        assert!(std::ptr::eq(
            *view.get("shop").unwrap(),
            base.get("shop").unwrap().as_view()
        ));
        assert!(view.get("optional").unwrap().is_nil());
        assert_eq!(view.get("new").unwrap().to_kstr(), "request");
        let runtime = RuntimeBuilder::new().set_globals(&view).build();
        runtime.set_global("shop".into(), Value::scalar("assigned"));
        assert_eq!(runtime.get(&["shop".into()]).unwrap().to_kstr(), "assigned");
        assert_eq!(
            base["shop"]
                .as_object()
                .unwrap()
                .get("name")
                .unwrap()
                .to_kstr(),
            "Original"
        );
        assert!(runtime
            .try_get(&["optional".into(), "present".into()])
            .is_none());
    }

    // Detect accidental formatting of large optional objects without timing assertions.
    #[derive(Debug)]
    struct NoString(Value);

    impl ValueView for NoString {
        fn as_debug(&self) -> &dyn fmt::Debug {
            self
        }
        fn render(&self) -> DisplayCow<'_> {
            self.0.render()
        }
        fn source(&self) -> DisplayCow<'_> {
            self.0.source()
        }
        fn type_name(&self) -> &'static str {
            self.0.type_name()
        }
        fn query_state(&self, state: State) -> bool {
            self.0.query_state(state)
        }
        fn to_kstr(&self) -> KStringCow<'_> {
            panic!("optional compound values must not be stringified")
        }
        fn to_value(&self) -> Value {
            self.0.clone()
        }
        fn as_object(&self) -> Option<&dyn ObjectView> {
            self.0.as_object()
        }
        fn as_array(&self) -> Option<&dyn liquid_core::model::ArrayView> {
            self.0.as_array()
        }
        fn is_nil(&self) -> bool {
            self.0.is_nil()
        }
    }

    #[test]
    fn optional_compound_properties_do_not_stringify_store_values() {
        for value in [
            Value::Object(liquid_core::object!({"present":"kept"})),
            Value::Array(vec![Value::scalar("item")]),
            Value::Nil,
        ] {
            let probe = NoString(value);
            assert!(fixture_font(&probe).is_err());
            let globals = BTreeMap::from([("probe".to_owned(), &probe)]);
            let inner = RuntimeBuilder::new().set_globals(&globals).build();
            let palette = vec![];
            let runtime = FixtureRuntime {
                inner: &inner,
                name: "probe",
                palette: &palette,
                focal_points: &BTreeMap::new(),
            };
            assert!(runtime
                .get(&["probe".into(), "unavailable".into()])
                .unwrap()
                .is_nil());
        }
    }

    #[test]
    fn scalar_colors_and_font_objects_keep_platform_properties() {
        let font = fixture_font(&Value::scalar("inter_n4")).unwrap();
        assert_eq!(font["family"], Value::scalar("Arial"));
        assert_eq!(fixture_font(&Value::Object(font.clone())).unwrap(), font);
        let globals = liquid_core::object!({"color":"#102030","font":"inter_n4","modified":font,"ordinary":{"family":"data field"}});
        let inner = RuntimeBuilder::new().set_globals(&globals).build();
        let palette = vec![];
        let runtime = FixtureRuntime {
            inner: &inner,
            name: "probe",
            palette: &palette,
            focal_points: &BTreeMap::new(),
        };
        for (root, property, expected) in [
            ("color", "rgb", Value::scalar("16 32 48")),
            ("font", "system?", Value::scalar(true)),
            ("modified", "weight", Value::scalar(400)),
            ("ordinary", "family", Value::scalar("data field")),
        ] {
            assert_eq!(
                runtime
                    .get(&[root.into(), property.into()])
                    .unwrap()
                    .into_owned(),
                expected
            );
        }
    }
}
