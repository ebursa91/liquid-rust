use liquid_core::error::ResultLiquidExt;
use liquid_core::{
    parser, runtime, Error, Language, ParseTag, Renderable, Result, TagReflection, TagTokenIter,
};

/// Executes newline-separated Liquid statements without outputting the newlines.
#[derive(Copy, Clone, Debug, Default)]
pub struct LiquidTag;

impl TagReflection for LiquidTag {
    fn tag(&self) -> &'static str {
        "liquid"
    }
    fn description(&self) -> &'static str {
        "Executes newline-separated Liquid statements."
    }
}

impl ParseTag for LiquidTag {
    fn parse(
        &self,
        mut arguments: TagTokenIter<'_>,
        options: &Language,
    ) -> Result<Box<dyn Renderable>> {
        let body = arguments.expect_next("Liquid statements expected.")?;
        arguments.expect_nothing()?;
        // Parsing a separate template keeps block delimiters within this liquid
        // tag. The outer grammar has already retained all statement boundaries.
        let mut statements = String::new();
        let mut comment_depth = 0usize;
        for line in body.as_raw_str().lines() {
            let statement = line.trim_matches(|c: char| c.is_ascii_whitespace() || c == '\u{000B}');
            if statement.is_empty() || statement.starts_with('#') {
                continue;
            }
            let name = statement.split_whitespace().next().unwrap_or("");
            // Ruby raw requires brace-delimited endraw even inside a liquid
            // tag. A bare endraw line cannot close it. Do not invent a valid
            // raw block by adding delimiters here.
            if name == "raw" {
                return Err(Error::with_msg("'raw' tag was never closed").trace("{% liquid %}"));
            }
            let tag = format!("{{% {statement} %}}");
            // Validate each line independently so an unterminated quoted string
            // cannot absorb delimiters added around subsequent statements.
            if comment_depth == 0 {
                let parsed = parser::Tag::new(&tag)?;
                if parsed.as_str() != tag {
                    return Err(Error::with_msg("Invalid liquid statement."));
                }
            }
            if name == "comment" {
                comment_depth += 1;
            }
            if name == "endcomment" {
                comment_depth = comment_depth.saturating_sub(1);
            }
            statements.push_str(&tag);
        }
        parser::parse(&statements, options)
            .map(runtime::Template::new)
            .map(|template| Box::new(template) as Box<dyn Renderable>)
            .trace("{% liquid %}")
    }
    fn reflection(&self) -> &dyn TagReflection {
        self
    }
}
