use liquid_core::{Language, ParseTag, Renderable, Result, TagReflection, TagTokenIter};

/// Outputs an expression, including filters, like `{{ expression }}`.
#[derive(Copy, Clone, Debug, Default)]
pub struct EchoTag;

impl TagReflection for EchoTag {
    fn tag(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "Outputs an expression."
    }
}

impl ParseTag for EchoTag {
    fn parse(
        &self,
        mut arguments: TagTokenIter<'_>,
        options: &Language,
    ) -> Result<Box<dyn Renderable>> {
        let Some(token) = arguments.next() else {
            return Ok(Box::new(EmptyEcho));
        };
        let expression = token.expect_filter_chain(options).into_result()?;
        arguments.expect_nothing()?;
        Ok(Box::new(expression))
    }
    fn reflection(&self) -> &dyn TagReflection {
        self
    }
}

#[derive(Debug)]
struct EmptyEcho;

impl Renderable for EmptyEcho {
    fn render_to(
        &self,
        _writer: &mut dyn std::io::Write,
        _runtime: &dyn liquid_core::Runtime,
    ) -> Result<()> {
        Ok(())
    }
}
