use tracing_subscriber::fmt::format::FmtSpan;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_span_events(FmtSpan::ENTER | FmtSpan::CLOSE)
        .with_writer(std::io::stderr)
        .finish();
    let template = liquid::ParserBuilder::with_stdlib().build()?.parse(
        "{% assign label = 'Numbers' %}{{ label | upcase }}:{% for number in numbers %} {{ number | times: 2 }}{% endfor %}",
    )?;
    let globals = liquid::object!({"numbers": [1, 2, 3]});

    // The caller's subscriber is restored when this synchronous operation ends.
    let output = tracing::subscriber::with_default(subscriber, || template.render(&globals))?;
    assert_eq!(output, "NUMBERS: 2 4 6");
    println!("{output}");
    Ok(())
}
