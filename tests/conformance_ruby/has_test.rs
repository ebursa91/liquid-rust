//! Original cases checked against Shopify Liquid 5.14.0 (4e39ae4).

use liquid_core::Value;
use liquid_lib::stdlib::Has;

#[test]
fn has_matches_pinned_ruby_property_truthiness_types_and_lookup_failures() {
    let cases: serde_json::Value = serde_json::from_str(r#"[{"input":[],"property":"kind","expected":false},{"input":[],"property":"kind","target":"image","expected":false},{"input":null,"property":"kind","expected":false},{"input":null,"property":"kind","target":"image","expected":false},{"input":[[[]]],"property":"kind","expected":false},{"input":[[[]]],"property":"kind","target":"image","expected":false},{"input":{},"property":"kind","expected":false},{"input":{},"property":"kind","target":"image","expected":false},{"input":[{}],"property":"kind","expected":false},{"input":[{}],"property":"kind","target":"image","expected":false},{"input":[{"kind":"image"},{"kind":"video"}],"property":"kind","expected":true},{"input":[{"kind":"image"},{"kind":"video"}],"property":"kind","target":"image","expected":true},{"input":[{"flag":null}],"property":"flag","expected":false},{"input":[{"flag":null}],"property":"flag","target":false,"expected":false},{"input":[{"flag":null}],"property":"flag","target":null,"expected":false},{"input":[{"flag":false}],"property":"flag","expected":false},{"input":[{"flag":false}],"property":"flag","target":false,"expected":true},{"input":[{"flag":false}],"property":"flag","target":null,"expected":false},{"input":[{"flag":true}],"property":"flag","expected":true},{"input":[{"flag":true}],"property":"flag","target":false,"expected":false},{"input":[{"flag":true}],"property":"flag","target":null,"expected":true},{"input":[{"flag":""}],"property":"flag","expected":true},{"input":[{"flag":""}],"property":"flag","target":false,"expected":false},{"input":[{"flag":""}],"property":"flag","target":null,"expected":true},{"input":[{"flag":0}],"property":"flag","expected":true},{"input":[{"flag":0}],"property":"flag","target":false,"expected":false},{"input":[{"flag":0}],"property":"flag","target":null,"expected":true},{"input":[{"flag":[]}],"property":"flag","expected":true},{"input":[{"flag":[]}],"property":"flag","target":false,"expected":false},{"input":[{"flag":[]}],"property":"flag","target":null,"expected":true},{"input":[{"flag":{}}],"property":"flag","expected":true},{"input":[{"flag":{}}],"property":"flag","target":false,"expected":false},{"input":[{"flag":{}}],"property":"flag","target":null,"expected":true},{"input":[{"flag":"yes"}],"property":"flag","expected":true},{"input":[{"flag":"yes"}],"property":"flag","target":false,"expected":false},{"input":[{"flag":"yes"}],"property":"flag","target":null,"expected":true},{"input":[{"kind":"image"},true],"property":"kind","target":"image","expected":true},{"input":[true,{"kind":"image"}],"property":"kind","target":"image","expected":null},{"input":[false,{"kind":"image"}],"property":"kind","expected":null},{"input":[null,{"kind":"image"}],"property":"kind","expected":null},{"input":[{"kind":"video"},3],"property":"kind","target":"image","error":true},{"input":[[{"kind":"video"}],[],[[{"kind":"image"}]]],"property":"kind","target":"image","expected":true},{"input":"116","property":"16","target":"16","expected":true},{"input":"é雪","property":1,"target":"雪","expected":true},{"input":"é雪","property":-1,"target":"雪","expected":true},{"input":"é雪","property":1.9,"target":"雪","expected":true},{"input":"","property":"","target":"","expected":true},{"input":5,"property":0,"target":1,"expected":true},{"input":5,"property":1,"target":0,"expected":true},{"input":-1,"property":99,"target":1,"expected":true},{"input":1.5,"property":"x","target":"x","expected":null},{"input":"a","property":null,"target":"a","error":true},{"input":3,"property":"1","target":1,"error":true},{"input":[],"property":null,"expected":false},{"input":{"flag":true},"property":null,"expected":false},{"input":[{"x":9007199254740993},{"x":9007199254740992}],"property":"x","target":9007199254740992.0,"expected":true},{"input":[{"x":9007199254740993}],"property":"x","target":9007199254740992.0,"expected":false},{"input":[{"x":3}],"property":"x","target":3.0,"expected":true},{"input":[{"x":3}],"property":"x","target":true,"expected":false},{"input":[{"x":[1,2]}],"property":"x","target":[1,2],"expected":true},{"input":[{"x":{"a":1}}],"property":"x","target":{"a":1},"expected":true},{"input":[{"foo":{"bar":true}}],"property":"foo.bar","expected":false},{"input":[{"foo.bar":true}],"property":"foo.bar","expected":true}]"#).unwrap();
    for case in cases.as_array().unwrap() {
        let input = liquid_core::model::to_value(&case["input"]).unwrap();
        let property = liquid_core::model::to_value(&case["property"]).unwrap();
        let result = if let Some(target) = case.get("target") {
            let target = liquid_core::model::to_value(target).unwrap();
            liquid_core::call_filter!(Has, input, property, target)
        } else {
            liquid_core::call_filter!(Has, input, property)
        };
        if case.get("error").is_some() {
            assert!(result.is_err(), "{case}: {result:?}");
        } else {
            let expected = liquid_core::model::to_value(&case["expected"]).unwrap();
            assert_eq!(result.unwrap(), expected, "{case}");
        }
    }
}

#[test]
fn has_requires_one_property_and_accepts_only_one_optional_target() {
    let input = liquid_core::value!([]);
    assert!(liquid_core::call_filter!(Has, input.clone()).is_err());
    assert!(liquid_core::call_filter!(Has, input, "id", 1, 2).is_err());
}

#[test]
fn has_is_registered_and_renders_the_product_media_boolean_path() {
    let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
    let template = parser.parse("{% assign has_image = media | has: 'media_type', 'image' %}{% if has_image %}image{% else %}none{% endif %}|{{ media | has: 'media_type', 'model' }}").unwrap();
    let values = liquid_core::object!({"media":[{"media_type":"image"},{"media_type":"video"}]});
    assert_eq!(template.render(&values).unwrap(), "image|false");
    let values = liquid_core::object!({"media":Value::Nil});
    assert_eq!(template.render(&values).unwrap(), "none|false");
}
