//! Original cases checked against Shopify Liquid 5.14.0 (4e39ae4).
//! Cover scalar values and object properties in rendered filter chains.

use liquid_core::Value;
use liquid_lib::stdlib::FindIndex;

#[test]
fn find_index_matches_the_first_truthy_property() {
    for (input, property, expected) in [
        (
            liquid_core::value!(["12", "16", "20", "16"]),
            liquid_core::value!("16"),
            liquid_core::value!(1),
        ),
        (
            liquid_core::value!(["116", "16"]),
            liquid_core::value!("16"),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!("alphabet"),
            liquid_core::value!("bet"),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!(["", "x"]),
            liquid_core::value!(""),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!(["é雪", "雪"]),
            liquid_core::value!("雪"),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!(["", "é雪"]),
            liquid_core::value!(1),
            liquid_core::value!(1),
        ),
        (
            liquid_core::value!(["", "é雪"]),
            liquid_core::value!(-1),
            liquid_core::value!(1),
        ),
        (
            liquid_core::value!(["é雪"]),
            liquid_core::value!(1.9),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!([{"flag":false}, {"flag":true}, {"flag":true}]),
            liquid_core::value!("flag"),
            liquid_core::value!(1),
        ),
        (
            liquid_core::value!([{"flag":""}]),
            liquid_core::value!("flag"),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!([{"flag":[]}]),
            liquid_core::value!("flag"),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!([{"flag":{}}]),
            liquid_core::value!("flag"),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!([{"flag":0}]),
            liquid_core::value!("flag"),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!({"flag":true}),
            liquid_core::value!("flag"),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!([[], [{"flag":false}], [{"flag":true}]]),
            liquid_core::value!("flag"),
            liquid_core::value!(1),
        ),
        (
            liquid_core::value!([{"foo":{"bar":true}}, {"foo.bar":true}]),
            liquid_core::value!("foo.bar"),
            liquid_core::value!(1),
        ),
        (
            liquid_core::value!([{"flag":true}, 3]),
            liquid_core::value!("flag"),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!(6),
            liquid_core::value!(0),
            liquid_core::value!(0),
        ),
    ] {
        assert_eq!(
            liquid_core::call_filter!(FindIndex, input, property).unwrap(),
            expected
        );
    }
}

#[test]
fn find_index_preserves_target_types_and_nil_selects_truthiness() {
    for (input, property, target, expected) in [
        (
            liquid_core::value!([{"id":1}, {"id":2}, {"id":2}]),
            liquid_core::value!("id"),
            liquid_core::value!(2),
            liquid_core::value!(1),
        ),
        (
            liquid_core::value!([{"id":1}]),
            liquid_core::value!("id"),
            liquid_core::value!(1.0),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!([{}, {"flag":true}, {"flag":false}]),
            liquid_core::value!("flag"),
            liquid_core::value!(false),
            liquid_core::value!(2),
        ),
        (
            liquid_core::value!([{"flag":[]}, {"flag":"yes"}, {"flag":true}]),
            liquid_core::value!("flag"),
            liquid_core::value!(true),
            liquid_core::value!(2),
        ),
        (
            liquid_core::value!([{"flag":nil}, {"flag":false}, {"flag":true}]),
            liquid_core::value!("flag"),
            Value::Nil,
            liquid_core::value!(2),
        ),
        (
            liquid_core::value!([{"flag":[1]}, {"flag":[true]}]),
            liquid_core::value!("flag"),
            liquid_core::value!([true]),
            liquid_core::value!(1),
        ),
        (
            liquid_core::value!([{"flag":{"value":1}}, {"flag":{"value":true}}]),
            liquid_core::value!("flag"),
            liquid_core::value!({"value":true}),
            liquid_core::value!(1),
        ),
        (
            liquid_core::value!(["xxneedle", "needle"]),
            liquid_core::value!("needle"),
            liquid_core::value!("needle"),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!(["ABC"]),
            liquid_core::value!(1),
            liquid_core::value!("B"),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!(6),
            liquid_core::value!(0),
            liquid_core::value!(0),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!(-1),
            liquid_core::value!(70),
            liquid_core::value!(1),
            liquid_core::value!(0),
        ),
    ] {
        assert_eq!(
            liquid_core::call_filter!(FindIndex, input, property, target).unwrap(),
            expected
        );
    }
}

#[test]
fn find_index_compares_integer_and_float_targets_without_rounding() {
    for (input, target, expected) in [
        (
            liquid_core::value!([{"id":9007199254740993i64}]),
            liquid_core::value!(9007199254740992.0),
            Value::Nil,
        ),
        (
            liquid_core::value!([{"id":9007199254740992.0}]),
            liquid_core::value!(9007199254740993i64),
            Value::Nil,
        ),
        (
            liquid_core::value!([{"id":-9007199254740993i64}]),
            liquid_core::value!(-9007199254740992.0),
            Value::Nil,
        ),
        (
            liquid_core::value!([{"id":i64::MAX}]),
            liquid_core::value!(2.0f64.powi(63)),
            Value::Nil,
        ),
        (
            liquid_core::value!([{"id":9007199254740992i64}]),
            liquid_core::value!(9007199254740992.0),
            liquid_core::value!(0),
        ),
        (
            liquid_core::value!([{"id":i64::MIN}]),
            liquid_core::value!(-2.0f64.powi(63)),
            liquid_core::value!(0),
        ),
    ] {
        assert_eq!(
            liquid_core::call_filter!(FindIndex, input, "id", target).unwrap(),
            expected
        );
    }
}

#[test]
fn find_index_returns_nil_for_missing_properties_or_non_indexable_values() {
    for (input, property) in [
        (liquid_core::value!([]), liquid_core::value!("foo")),
        (Value::Nil, liquid_core::value!("foo")),
        (
            liquid_core::value!("alphabet"),
            liquid_core::value!("absent"),
        ),
        (liquid_core::value!(["é雪"]), liquid_core::value!(2)),
        (
            liquid_core::value!([{"other":true}]),
            liquid_core::value!("flag"),
        ),
        (liquid_core::value!({"1":true}), liquid_core::value!(1)),
        (liquid_core::value!(false), liquid_core::value!("flag")),
        (liquid_core::value!(3.4), liquid_core::value!("flag")),
        (
            liquid_core::value!([nil, {"flag":true}]),
            liquid_core::value!("flag"),
        ),
        (liquid_core::value!([]), liquid_core::value!(["invalid"])),
    ] {
        assert_eq!(
            liquid_core::call_filter!(FindIndex, input, property).unwrap(),
            Value::Nil
        );
    }
}

#[test]
fn find_index_rejects_invalid_properties_and_arity() {
    for (input, property) in [
        (liquid_core::value!(["text"]), liquid_core::value!(true)),
        (liquid_core::value!(["text"]), Value::Nil),
        (liquid_core::value!(["text"]), liquid_core::value!(["x"])),
        (liquid_core::value!(3), liquid_core::value!("flag")),
        (
            liquid_core::value!([3, {"flag":true}]),
            liquid_core::value!("flag"),
        ),
    ] {
        assert!(liquid_core::call_filter!(FindIndex, input, property).is_err());
    }
    assert!(liquid_core::call_filter!(FindIndex, liquid_core::value!([])).is_err());
    assert!(liquid_core::call_filter!(
        FindIndex,
        liquid_core::value!([]),
        liquid_core::value!("id"),
        liquid_core::value!(1),
        liquid_core::value!(2)
    )
    .is_err());
}

#[test]
fn find_index_renders_scalar_and_object_properties() {
    let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
    let source = "{% assign size_index = sizes | find_index: size %}{{ size_index }}|{% assign block_index = blocks | find_index: 'id', block_id %}{{ block_index }}|{{ sizes | find_index: 'absent' | default: 'missing' }}";
    let globals = liquid::object!({"sizes":["12","16","20"],"size":"16","blocks":[{"id":"first"},{"id":"second"},{"id":"second"}],"block_id":"second"});
    assert_eq!(
        parser.parse(source).unwrap().render(&globals).unwrap(),
        "1|1|missing"
    );
}
