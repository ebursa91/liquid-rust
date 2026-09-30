use liquid_core::{Error, Language, ParseTag, Renderable, Result, TagReflection, TagTokenIter};

/// Ignores a comment; each subsequent nonblank line must start with `#`.
#[derive(Copy, Clone, Debug, Default)]
pub struct InlineCommentTag;

impl TagReflection for InlineCommentTag {
    fn tag(&self) -> &'static str {
        "#"
    }
    fn description(&self) -> &'static str {
        "Ignores an inline comment."
    }
}

impl ParseTag for InlineCommentTag {
    fn parse(
        &self,
        mut arguments: TagTokenIter<'_>,
        _options: &Language,
    ) -> Result<Box<dyn Renderable>> {
        let body = arguments.expect_next("Comment expected.")?;
        arguments.expect_nothing()?;
        if body.as_raw_str().lines().skip(1).any(|line| {
            !line
                .trim_matches(|c: char| c.is_ascii_whitespace() || c == '\u{000B}')
                .is_empty()
                && !line
                    .trim_start_matches(|c: char| c.is_ascii_whitespace() || c == '\u{000B}')
                    .starts_with('#')
        }) {
            return Err(Error::with_msg(
                "Every line of an inline comment must start with '#'.",
            ));
        }
        Ok(Box::new(liquid_core::runtime::Template::new(vec![])))
    }
    fn reflection(&self) -> &dyn TagReflection {
        self
    }
}
