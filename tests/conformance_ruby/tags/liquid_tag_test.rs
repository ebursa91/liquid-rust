//! Original cases checked against Shopify Liquid 5.14.0 (4e39ae4).

fn render(source: &str) -> String {
    liquid::ParserBuilder::with_stdlib()
        .build()
        .unwrap()
        .parse(source)
        .unwrap()
        .render(&liquid::object!({ "numbers": [1, 2, 3], "liquid": "liquid" }))
        .unwrap()
}

#[test]
fn echo_filters_and_liquid_control_flow() {
    assert_eq!(render("{% echo %}"), "");
    for spacing in ["\t", "\u{000B}", "\u{000C}"] {
        assert_eq!(render(&format!("{{% liquid{spacing}echo 42 %}}")), "42");
        assert_eq!(render(&format!("{{% echo{spacing}42 %}}")), "42");
        assert_eq!(
            render(&format!(
                "{{% liquid\n assign{spacing}answer{spacing}={spacing}42\n echo answer\n%}}"
            )),
            "42"
        );
    }
    assert_eq!(render("{% echo numbers | join: ', ' %}"), "1, 2, 3");
    assert_eq!(render("{% liquid\n for number in numbers\n assign doubled = number | times: 2\n echo doubled\n unless forloop.last\n echo ', ' \n endunless\n endfor\n%}{{ doubled }}"), "2, 4, 66");
}

#[test]
fn comments_blank_lines_and_crlf_do_not_emit_whitespace() {
    assert_eq!(
        render("{% liquid\n comment\n echo \"\n endcomment\n echo 'safe'\n%}"),
        "safe"
    );
    assert_eq!(render("a{%- liquid\r\n # ignored comment\r\n\r\n echo 'b'\r\n comment\r\n arbitrary invalid syntax @!\r\n endcomment\r\n echo 'c'\r\n-%} d"), "abcd");
}

#[test]
fn nested_liquid_and_variables_named_liquid() {
    assert_eq!(render("{% liquid liquid liquid echo liquid %}"), "liquid");
    assert_eq!(
        render("{% liquid\n liquid\n if true\n echo 'good'\n endif\n%}"),
        "good"
    );
    assert_eq!(
        render("{% raw %}{% liquid echo 'literal' %}{% endraw %}"),
        "{% liquid echo 'literal' %}"
    );
}

#[test]
fn quoted_delimiters_are_data() {
    assert_eq!(
        render("{% liquid echo '{{ fake }} and {% fake' %}"),
        "{{ fake }} and {% fake"
    );
}

#[test]
fn invalid_liquid_and_echo_fail_during_parse() {
    let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
    for source in [
        "{% liquid unknown_tag %}",
        "{% liquid echo 'hello\nworld' %}",
        "{% liquid\n\u{00a0}echo 42\n%}",
        "{% liquid\n comment\n raw\n endraw\n endcomment\n%}",
        "{% liquid\n if true\n%}{% endif %}",
        "{% if true %}{% liquid endif %}{% endif %}",
        "{% liquid\n liquid if true\n echo 'bad'\n endif\n%}",
        "{% liquid\n raw\n echo 42\n endraw\n%}",
        "{% echo 'one' 'two' %}",
        "{% liquid echo 'one' 'two' %}",
    ] {
        assert!(parser.parse(source).is_err(), "accepted {source:?}");
    }
}

#[test]
fn inline_comments_accept_only_prefixed_continuation_lines() {
    assert_eq!(
        render("before{% # arbitrary @! {{ text \n # more text\n \n%}after"),
        "beforeafter"
    );
    assert_eq!(render("x {%- # one line -%} y"), "xy");
    assert_eq!(render("{% # first\n\u{000B}# continuation %}"), "");
    let parser = liquid::ParserBuilder::with_stdlib().build().unwrap();
    for source in [
        "{% #\n illegal continuation %}",
        "{% # first\n # second\n illegal %}",
        "{% # first\n\u{00a0}# second %}",
    ] {
        assert!(parser.parse(source).is_err());
    }
}

#[test]
fn special_tag_bodies_work_inside_brace_delimited_blocks() {
    assert_eq!(
        render("{% if true %}{% liquid echo 'yes' %}{% # ignored %}{% endif %}"),
        "yes"
    );
    assert_eq!(
        render(
            "{% capture result %}{% liquid echo 42 %}{% # comment %}{% endcapture %}{{ result }}"
        ),
        "42"
    );
    assert_eq!(
        render("{% comment %}{% liquid endcomment %}{% # endcomment %}{% endcomment %}ok"),
        "ok"
    );
}
