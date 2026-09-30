use liquid::ValueView;

#[test]
fn test_simple_variable() {
    assert_template_result!(r#"worked"#, r#"{{test}}"#, o!({"test": "worked"}));

    assert_template_result!(
        r#"worked wonderfully"#,
        r#"{{test}}"#,
        o!({"test": "worked wonderfully"}),
    );
}

#[test]
#[should_panic]
fn test_variable_render_calls_to_liquid() {
    panic!("to_liquid is implementation specific");
}

#[test]
fn test_simple_with_whitespaces() {
    assert_template_result!(r#"  worked  "#, r#"  {{ test }}  "#, o!({"test": "worked"}));

    assert_template_result!(
        r#"  worked wonderfully  "#,
        r#"  {{ test }}  "#,
        o!({"test": "worked wonderfully"}),
    );
}

#[test]
#[should_panic]
fn test_ignore_unknown() {
    assert_template_result!(r#""#, r#"{{ test }}"#);
    panic!("Requires `strict_variables: false`");
}

#[test]
fn test_using_blank_as_variable_name() {
    assert_template_result!(r#""#, r#"{% assign foo = blank %}{{ foo }}"#);
}

#[test]
fn test_using_empty_as_variable_name() {
    assert_template_result!(r#""#, r#"{% assign foo = empty %}{{ foo }}"#);
}

#[test]
fn test_hash_scoping() {
    assert_template_result!(
        r#"worked"#,
        r#"{{ test.test }}"#,
        o!({"test": {"test": "worked"}}),
    );
}

#[test]
fn test_false_renders_as_false() {
    assert_template_result!(r#"false"#, r#"{{ foo }}"#, o!({"foo": false}));

    assert_template_result!(r#"false"#, r#"{{ false }}"#);
}

#[test]
fn test_nil_renders_as_empty_string() {
    assert_template_result!(r#""#, r#"{{ nil }}"#);

    assert_template_result!(r#"cat"#, r#"{{ nil | append: 'cat' }}"#);
}

#[test]
#[should_panic]
fn test_preset_assigns() {
    panic!("Preset assigns are implementation specific");
}

#[test]
fn test_reuse_parsed_template() {
    let template = liquid::ParserBuilder::with_stdlib()
        .build()
        .unwrap()
        .parse(r#"{{ greeting }} {{ name }}"#)
        .unwrap();

    let globals = o!({"greeting": "Hello", "name": "Tobi"});
    let rendered = template.render(&globals).unwrap();
    assert_eq!("Hello Tobi", rendered);

    // Modified due to strict_variables: true
    let globals = o!({"greeting": "Hello", "unknown": "Tobi"});
    template.render(globals.as_object().unwrap()).unwrap_err();

    let globals = o!({"greeting": "Hello", "name": "Brian"});
    let rendered = template.render(&globals).unwrap();
    assert_eq!("Hello Brian", rendered);

    // Preset assign cases ("Goodbye")( are implementation specific
}

#[test]
fn test_assigns_not_polluted_from_template() {
    let template = liquid::ParserBuilder::with_stdlib()
        .build()
        .unwrap()
        .parse(r#"{{ test }}{% assign test = 'bar' %}{{ test }}"#)
        .unwrap();

    // All modified due to strict_variables: true

    let globals = o!({"test": "baz"});
    let rendered = template.render(&globals).unwrap();
    assert_eq!("bazbar", rendered);

    let globals = o!({"test": "baz"});
    let rendered = template.render(&globals).unwrap();
    assert_eq!("bazbar", rendered);

    let globals = o!({"test": "foo"});
    let rendered = template.render(&globals).unwrap();
    assert_eq!("foobar", rendered);

    let globals = o!({"test": "baz"});
    let rendered = template.render(&globals).unwrap();
    assert_eq!("bazbar", rendered);
}

#[test]
#[should_panic]
fn test_hash_with_default_proc() {
    panic!("Default proc is implementation specific");
}

#[test]
fn test_multiline_variable() {
    assert_template_result!(r#"worked"#, "{{\ntest\n}}", o!({"test": "worked"}));
}

#[test]
#[should_panic]
fn test_render_symbol() {
    panic!("Symbols are implementation specific");
}

#[test]
fn question_mark_properties_render_and_participate_in_conditions() {
    let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
    let globals = liquid::object!({"font":{"system?":true}, "enabled?":false});
    let source = "{{ font.system? }}|{{ enabled? }}|{% unless font.system? %}remote{% else %}local{% endunless %}";
    assert_eq!(
        parser.parse(source).unwrap().render(&globals).unwrap(),
        "true|false|local"
    );
}

#[test]
fn literal_prefixes_do_not_split_identifiers() {
    let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
    for name in [
        "empty_cart_drawer_content",
        "blank_label",
        "nil_image",
        "null_state",
        "true_flag",
        "false_flag",
    ] {
        let source = format!("{{% capture {name} %}}captured{{% endcapture %}}{{{{ {name} }}}}");
        assert_eq!(
            parser
                .parse(&source)
                .unwrap()
                .render(&liquid::object!({}))
                .unwrap(),
            "captured",
            "{source}"
        );
    }
    assert_eq!(
        parser
            .parse("left {{- nil -}} right|{{ true }}|{{ false }}")
            .unwrap()
            .render(&liquid::object!({}))
            .unwrap(),
        "leftright|true|false"
    );
}

#[test]
fn literal_names_with_selectors_are_object_lookups() {
    let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
    let globals = liquid::object!({"nil":{"field":"selected"},"null":{"field":"selected"},"empty":{"field":"selected"},"blank":{"field":"selected"},"true":{"field":"selected"},"false":{"field":"selected"},"nil?":"question"});
    for name in ["nil", "null", "empty", "blank", "true", "false"] {
        for source in [
            format!("{{{{ {name}.field }}}}"),
            format!("{{{{ {name}['field'] }}}}"),
            format!("{{% assign value = {name}.field %}}{{{{ value }}}}"),
            format!("{{% if {name}.field %}}selected{{% else %}}wrong{{% endif %}}"),
        ] {
            assert_eq!(
                parser.parse(&source).unwrap().render(&globals).unwrap(),
                "selected",
                "{source}"
            );
        }
    }
    assert_eq!(
        parser
            .parse("{{ nil? }}")
            .unwrap()
            .render(&globals)
            .unwrap(),
        "question"
    );
}

#[test]
fn question_mark_lookup_names_are_not_assignment_declarations() {
    let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
    for source in [
        "{% assign nil? = 'assigned' %}",
        "{% assign enabled? = true %}",
    ] {
        assert!(parser.parse(source).is_err(), "accepted {source}");
    }
}

#[test]
fn keyword_literals_remain_literals_before_range_delimiters() {
    let mut tag = liquid_core::parser::Tag::new("{% range (nil..2) %}").unwrap();
    let (start, end) = tag
        .tokens()
        .expect_next("range")
        .unwrap()
        .expect_range()
        .into_result()
        .unwrap();
    assert_eq!(
        start,
        liquid_core::Expression::Literal(liquid_core::Value::Nil)
    );
    assert_eq!(end, liquid_core::Expression::with_literal(2i64));
}
