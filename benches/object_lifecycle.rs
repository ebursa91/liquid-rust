//! Generic ownership lifecycle controls for identical baseline/candidate builds.
//!
//! Fixture construction, runtime construction and assignment are outside
//! lookup-only samples. Construction and mutation samples include owning
//! consumption and drop. In particular, clone and its subsequent mutation are
//! timed together, so eager copying and deferred detachment do equivalent work.
//! Assertions run before timing; no result or source validation is timed.

use std::hint::black_box;

use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion};
use liquid_core::model::{KString, Object, ScalarCow, Value, ValueCow};
use liquid_core::runtime::{Runtime, RuntimeBuilder};

const FIELD_KEYS: [&str; 8] = [
    "field0", "field1", "field2", "field3", "field4", "field5", "field6", "field7",
];
const ITEM_COUNT: i64 = 24;
const CHANGED_SCALAR: &str = "changed scalar";
const CHANGED_NOTE: &str = "changed note";

type Consumed = Vec<(KString, Value)>;

fn consume(object: Object) -> Consumed {
    let entries = black_box(object).into_iter().collect();
    black_box(entries)
}

fn construct_mutate_consume(keys: &[&'static str]) -> Consumed {
    let mut object = Object::new();
    for (key, index) in keys.iter().zip(0_i64..) {
        object.insert(black_box(*key).into(), Value::scalar(black_box(index)));
    }
    if let Some(value) = object.get_mut(black_box(FIELD_KEYS[0])) {
        *value = Value::scalar(black_box(100_i64));
    }
    consume(object)
}

fn nested_fixture() -> Object {
    let note =
        "A deterministic generic note with enough text to own its backing storage. ".repeat(4);
    let mut items = Vec::new();
    for index in 0_i64..ITEM_COUNT {
        items.push(Value::Object(liquid_core::object!({
            "id": Value::scalar(index),
            "title": format!("Item {index:02}"),
            "score": index + 10,
            "enabled": true,
            "tags": ["first", "second", "third"],
            "details": {
                "owner": "generic reader",
                "note": note.clone(),
                "coordinates": {"row": Value::scalar(index), "column": index + 1},
            },
            "attachments": [
                {"name": "first attachment", "size": 512, "metadata": {"caption": note.clone()}},
                {"name": "second attachment", "size": 1024, "metadata": {"caption": note.clone()}},
            ],
        })));
    }
    let note = Value::scalar(note);
    let items = Value::Array(items);
    liquid_core::object!({
        "id": 101,
        "title": "Record with nested generic values",
        "description": note,
        "category": "sample",
        "active": true,
        "score": 17,
        "count": Value::scalar(ITEM_COUNT),
        "version": 2,
        "status": "ready",
        "tags": ["alpha", "beta", "gamma"],
        "metadata": {"owner": "reader", "revision": 3, "coordinates": {"x": 1, "y": 2}},
        "items": items,
        "empty": {},
        "optional": nil,
    })
}

fn change_one_deep_note(object: &mut Object) {
    let Value::Array(items) = object.get_mut(black_box("items")).unwrap() else {
        panic!("Expected items array");
    };
    let Value::Object(item) = &mut items[black_box(7_usize)] else {
        panic!("Expected item object");
    };
    let Value::Object(details) = item.get_mut(black_box("details")).unwrap() else {
        panic!("Expected details object");
    };
    details.insert(
        black_box("note").into(),
        Value::scalar(black_box(CHANGED_NOTE)),
    );
}

fn replace_scalar_leaves(value: &mut Value) {
    match value {
        Value::Scalar(_) => *value = Value::scalar(black_box(CHANGED_SCALAR)),
        Value::Array(values) => {
            for value in values {
                replace_scalar_leaves(value);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                replace_scalar_leaves(value);
            }
        }
        Value::Nil | Value::State(_) => {}
    }
}

fn clone_one_mutation_consume(source: &Object) -> Consumed {
    let mut object = black_box(source).clone();
    change_one_deep_note(&mut object);
    consume(object)
}

fn clone_most_mutations_consume(source: &Object) -> Consumed {
    let mut object = black_box(source).clone();
    for value in object.values_mut() {
        replace_scalar_leaves(value);
    }
    consume(object)
}

fn validate_changed_shape(actual: &Value, original: &Value) -> usize {
    match original {
        Value::Scalar(_) => {
            assert_eq!(actual, &Value::scalar(CHANGED_SCALAR));
            1
        }
        Value::Array(original) => {
            let Value::Array(actual) = actual else {
                panic!("Expected unchanged array shape");
            };
            assert_eq!(actual.len(), original.len());
            actual
                .iter()
                .zip(original)
                .map(|(actual, original)| validate_changed_shape(actual, original))
                .sum()
        }
        Value::Object(original) => {
            let Value::Object(actual) = actual else {
                panic!("Expected unchanged object shape");
            };
            assert_eq!(actual.len(), original.len());
            original
                .iter()
                .map(|(key, original)| validate_changed_shape(actual.get(key).unwrap(), original))
                .sum()
        }
        Value::Nil => {
            assert_eq!(actual, &Value::Nil);
            0
        }
        Value::State(original) => {
            let Value::State(actual) = actual else {
                panic!("Expected unchanged state value");
            };
            assert_eq!(actual, original);
            0
        }
    }
}

fn validate_one_deep_mutation(actual: &Object, original: &Object) {
    assert_eq!(actual.len(), original.len());
    for (key, original_value) in original {
        let actual_value = actual.get(key).unwrap();
        if key != "items" {
            assert_eq!(actual_value, original_value);
            continue;
        }
        let (Value::Array(actual_items), Value::Array(original_items)) =
            (actual_value, original_value)
        else {
            panic!("Expected items arrays");
        };
        assert_eq!(actual_items.len(), original_items.len());
        for (index, (actual_item, original_item)) in
            actual_items.iter().zip(original_items).enumerate()
        {
            if index != 7 {
                assert_eq!(actual_item, original_item);
                continue;
            }
            let (Value::Object(actual_item), Value::Object(original_item)) =
                (actual_item, original_item)
            else {
                panic!("Expected item objects");
            };
            assert_eq!(actual_item.len(), original_item.len());
            for (key, original_field) in original_item {
                let actual_field = actual_item.get(key).unwrap();
                if key != "details" {
                    assert_eq!(actual_field, original_field);
                    continue;
                }
                let (Value::Object(actual_details), Value::Object(original_details)) =
                    (actual_field, original_field)
                else {
                    panic!("Expected details objects");
                };
                assert_eq!(actual_details.len(), original_details.len());
                for (key, original_detail) in original_details {
                    let expected = if key == "note" {
                        Value::scalar(CHANGED_NOTE)
                    } else {
                        original_detail.clone()
                    };
                    assert_eq!(actual_details.get(key), Some(&expected));
                }
            }
        }
    }
}

fn bench_construction(c: &mut Criterion) {
    let mut group = c.benchmark_group("object_lifecycle/fresh_construct_mutate_consume");
    for count in [0_usize, 1, 2, 8] {
        let keys = &FIELD_KEYS[..count];
        let checked: Object = construct_mutate_consume(keys).into_iter().collect();
        assert_eq!(checked.len(), count);
        for (key, index) in keys.iter().zip(0_i64..) {
            let expected = if index == 0 { 100 } else { index };
            assert_eq!(checked.get(*key), Some(&Value::scalar(expected)));
        }
        group.bench_with_input(BenchmarkId::from_parameter(count), &count, |b, &count| {
            b.iter(|| {
                let consumed = construct_mutate_consume(black_box(&FIELD_KEYS[..count]));
                drop(black_box(consumed));
            });
        });
    }
    group.finish();
}

fn bench_assigned_lookups(c: &mut Criterion) {
    let source = nested_fixture();
    let independently_built = nested_fixture();
    assert_eq!(source, independently_built);
    let globals = Object::new();
    let runtime = RuntimeBuilder::new().set_globals(&globals).build();
    runtime.set_global("saved".into(), Value::Object(source.clone()));
    runtime.set_global("saved_scalar".into(), Value::scalar(101));
    let full_path: [ScalarCow<'static>; 1] = ["saved".into()];
    let leaf_path: [ScalarCow<'static>; 5] = [
        "saved".into(),
        "items".into(),
        7_i64.into(),
        "details".into(),
        "note".into(),
    ];
    let scalar_path: [ScalarCow<'static>; 1] = ["saved_scalar".into()];

    let full = runtime.try_get(&full_path).unwrap();
    assert!(matches!(full, ValueCow::Owned(_)));
    assert_eq!(full.into_owned(), Value::Object(independently_built));
    let Value::Array(items) = &source["items"] else {
        panic!("Expected items array");
    };
    let Value::Object(item) = &items[7] else {
        panic!("Expected item object");
    };
    let Value::Object(details) = &item["details"] else {
        panic!("Expected details object");
    };
    assert_eq!(
        runtime.try_get(&leaf_path).unwrap().into_owned(),
        details["note"]
    );
    assert_eq!(
        runtime.try_get(&scalar_path).unwrap().into_owned(),
        Value::scalar(101)
    );

    let mut group = c.benchmark_group("object_lifecycle/retained_assigned_lookup");
    for (name, path) in [
        ("full_object", full_path.as_slice()),
        ("nested_scalar_leaf", leaf_path.as_slice()),
        ("scalar_control", scalar_path.as_slice()),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let value = black_box(&runtime)
                    .try_get(black_box(path))
                    .expect("Validated assigned path must remain available")
                    .into_owned();
                drop(black_box(value));
            });
        });
    }
    group.finish();
    assert_eq!(source, nested_fixture());
    assert_eq!(
        runtime.try_get(&full_path).unwrap().into_owned(),
        Value::Object(nested_fixture())
    );
}

fn bench_shared_mutations(c: &mut Criterion) {
    let source = nested_fixture();
    let independently_built = nested_fixture();
    let one: Object = clone_one_mutation_consume(&source).into_iter().collect();
    validate_one_deep_mutation(&one, &independently_built);
    assert_eq!(source, independently_built);

    let most: Object = clone_most_mutations_consume(&source).into_iter().collect();
    let changed = validate_changed_shape(&Value::Object(most), &Value::Object(nested_fixture()));
    assert!(changed > 200, "Fixture must exercise many scalar writes");
    assert_eq!(source, nested_fixture());

    let mut group = c.benchmark_group("object_lifecycle/clone_mutate_consume");
    group.bench_function("one_deep_mutation", |b| {
        b.iter(|| {
            let consumed = clone_one_mutation_consume(black_box(&source));
            drop(black_box(consumed));
        });
    });
    group.bench_function("most_fields_mutated", |b| {
        b.iter(|| {
            let consumed = clone_most_mutations_consume(black_box(&source));
            drop(black_box(consumed));
        });
    });
    group.finish();
    assert_eq!(source, nested_fixture());
}

criterion_group!(
    benches,
    bench_construction,
    bench_assigned_lookups,
    bench_shared_mutations
);
criterion_main!(benches);
