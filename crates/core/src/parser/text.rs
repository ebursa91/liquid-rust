use std::io::Write;

use crate::error::{Result, ResultLiquidReplaceExt};
use crate::runtime::Renderable;
use crate::runtime::Runtime;

/// A raw template expression.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Text {
    text: String,
}

impl Text {
    /// Create a raw template expression.
    pub(crate) fn new<S: Into<String>>(text: S) -> Text {
        Text { text: text.into() }
    }
}

impl Renderable for Text {
    fn is_blank(&self) -> bool {
        self.text
            .bytes()
            .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0b | 0x0c))
    }

    fn trim_blank(&mut self) {
        if self.is_blank() {
            self.text.clear();
        }
    }

    fn render_to(&self, writer: &mut dyn Write, _runtime: &dyn Runtime) -> Result<()> {
        write!(writer, "{}", self.text).replace("Failed to render")?;
        Ok(())
    }
}
