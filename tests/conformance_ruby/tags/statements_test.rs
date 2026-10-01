#[test]
fn test_true_eql_true() {
    let text = " {% if true == true %} true {% else %} false {% endif %} ";
    assert_template_result!("  true  ", text);
}

#[test]
fn test_true_not_eql_true() {
    let text = " {% if true != true %} true {% else %} false {% endif %} ";
    assert_template_result!("  false  ", text);
}

#[test]
fn test_true_lq_true() {
    let text = " {% if 0 > 0 %} true {% else %} false {% endif %} ";
    assert_template_result!("  false  ", text);
}

#[test]
fn test_one_lq_zero() {
    let text = " {% if 1 > 0 %} true {% else %} false {% endif %} ";
    assert_template_result!("  true  ", text);
}

#[test]
fn test_zero_lq_one() {
    let text = " {% if 0 < 1 %} true {% else %} false {% endif %} ";
    assert_template_result!("  true  ", text);
}

#[test]
fn test_zero_lq_or_equal_one() {
    let text = " {% if 0 <= 0 %} true {% else %} false {% endif %} ";
    assert_template_result!("  true  ", text);
}

#[test]
fn test_zero_lq_or_equal_one_involving_nil() {
    let text = " {% if null <= 0 %} true {% else %} false {% endif %} ";
    assert_template_result!("  false  ", text);

    let text = " {% if 0 <= null %} true {% else %} false {% endif %} ";
    assert_template_result!("  false  ", text);
}

#[test]
fn test_zero_lqq_or_equal_one() {
    let text = " {% if 0 >= 0 %} true {% else %} false {% endif %} ";
    assert_template_result!("  true  ", text);
}

#[test]
fn test_strings() {
    let text = " {% if 'test' == 'test' %} true {% else %} false {% endif %} ";
    assert_template_result!("  true  ", text);
}

#[test]
fn test_strings_not_equal() {
    let text = " {% if 'test' != 'test' %} true {% else %} false {% endif %} ";
    assert_template_result!("  false  ", text);
}

#[test]
fn test_var_strings_equal() {
    let text = r#" {% if var == "hello there!" %} true {% else %} false {% endif %} "#;
    assert_template_result!("  true  ", text, o!({"var": "hello there!"}));
}

#[test]
fn test_var_strings_are_not_equal() {
    let text = r#" {% if "hello there!" == var %} true {% else %} false {% endif %} "#;
    assert_template_result!("  true  ", text, o!({"var": "hello there!"}));
}

#[test]
fn test_var_and_long_string_are_equal() {
    let text = r#" {% if var == "hello there!" %} true {% else %} false {% endif %} "#;
    assert_template_result!("  true  ", text, o!({"var": "hello there!"}));
}

#[test]
fn test_var_and_long_string_are_equal_backwards() {
    let text = r#" {% if "hello there!" == var %} true {% else %} false {% endif %} "#;
    assert_template_result!("  true  ", text, o!({"var": "hello there!"}));
}

/*
  # def test_is_nil
  #  text = %| {% if var != nil %} true {% else %} false {% end %} |
  #  @template.assigns = { "var": "hello there!"}
  #  expected = %|  true  |
  #  assert_equal expected, @template.parse(text)
  # end
*/

#[test]
fn test_is_collection_empty() {
    let text = " {% if array == empty %} true {% else %} false {% endif %} ";
    assert_template_result!("  true  ", text, o!({"array": []}));
}

#[test]
fn test_is_not_collection_empty() {
    let text = " {% if array == empty %} true {% else %} false {% endif %} ";
    assert_template_result!("  false  ", text, o!({"array": [1, 2, 3]}));
}

#[test]
fn test_nil() {
    let text = " {% if var == nil %} true {% else %} false {% endif %} ";
    assert_template_result!("  true  ", text, o!({ "var": nil }));

    let text = " {% if var == null %} true {% else %} false {% endif %} ";
    assert_template_result!("  true  ", text, o!({ "var": nil }));
}

#[test]
fn test_not_nil() {
    let text = " {% if var != nil %} true {% else %} false {% endif %} ";
    assert_template_result!("  true  ", text, o!({"var": 1}));

    let text = " {% if var != null %} true {% else %} false {% endif %} ";
    assert_template_result!("  true  ", text, o!({"var": 1}));
}

#[test]
fn nil_false_equality_matches_ruby_without_changing_blank_or_truthiness() {
    // Original probes checked against Shopify Liquid 5.14.0 (4e39ae4).
    for (source, expected) in [
        (
            "{% if nil == false %}equal{% else %}different{% endif %}",
            "different",
        ),
        (
            "{% if false == nil %}equal{% else %}different{% endif %}",
            "different",
        ),
        (
            "{% if nil != false %}different{% else %}equal{% endif %}",
            "different",
        ),
        (
            "{% unless absent == false %}infinite{% endunless %}",
            "infinite",
        ),
        (
            "{% unless false == absent %}infinite{% endunless %}",
            "infinite",
        ),
        (
            "{% case nil %}{% when false %}false{% when nil %}nil{% endcase %}",
            "nil",
        ),
        (
            "{% case false %}{% when nil %}nil{% when false %}false{% endcase %}",
            "false",
        ),
        ("{% if nil == blank %}blank{% endif %}", "blank"),
        ("{% if blank == nil %}blank{% endif %}", "blank"),
        ("{% if false == blank %}blank{% endif %}", "blank"),
        (
            "{% if nil == empty %}empty{% else %}not empty{% endif %}",
            "not empty",
        ),
        (
            "{% if empty == nil %}empty{% else %}not empty{% endif %}",
            "not empty",
        ),
        ("{% if null != empty %}not empty{% endif %}", "not empty"),
        (
            "{% if nil or false %}true{% else %}falsy{% endif %}",
            "falsy",
        ),
        ("{% if absent == null %}nil{% endif %}", "nil"),
        (
            "{% if values contains nil %}yes{% else %}no{% endif %}",
            "no",
        ),
        (
            "{% if values == other %}same{% else %}different{% endif %}",
            "different",
        ),
    ] {
        // The conformance helper retains strict variables; Ruby's absent value
        // is supplied explicitly as nil so only equality is under test.
        assert_template_result!(
            expected,
            source,
            o!({"absent":nil,"values":[false],"other":[nil]})
        );
    }
}

#[test]
fn mixed_logical_conditions_follow_ruby_right_association() {
    let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
    for (condition, expected) in [
        ("false and true or true", "no"),
        ("false and false or true", "no"),
        ("true or false and false", "yes"),
        ("true and false or true", "yes"),
        ("false and true or true and true", "no"),
        ("false == true and false == false or true == true", "no"),
    ] {
        for (tag, end, positive, negative) in [
            ("if", "endif", "yes", "no"),
            ("unless", "endunless", "no", "yes"),
        ] {
            let source =
                format!("{{% {tag} {condition} %}}{positive}{{% else %}}{negative}{{% {end} %}}");
            assert_eq!(
                parser
                    .parse(&source)
                    .unwrap()
                    .render(&liquid::object!({}))
                    .unwrap(),
                expected,
                "{source}"
            );
        }
    }
    let source =
        "{% if false %}wrong{% elsif false and true or true %}wrong{% else %}right{% endif %}";
    assert_eq!(
        parser
            .parse(source)
            .unwrap()
            .render(&liquid::object!({}))
            .unwrap(),
        "right"
    );
}
