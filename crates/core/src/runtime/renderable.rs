use std::fmt::Debug;
use std::io::Write;

use crate::error::Result;

use super::Runtime;

/// Any object (tag/block) that can be rendered by liquid must implement this trait.
pub trait Renderable: Send + Sync + Debug {
    /// Whether this node is statically blank during parsing. Variables and
    /// custom tags remain nonblank by default, regardless of their output.
    fn is_blank(&self) -> bool {
        false
    }

    /// Remove literal whitespace from a statically blank control-flow body.
    /// Silent tags retain their side effects; captures retain their own content.
    /// Custom tags are unchanged unless they explicitly implement this method.
    fn trim_blank(&mut self) {}

    /// Renders the Renderable instance given a Liquid runtime.
    fn render(&self, runtime: &dyn Runtime) -> Result<String> {
        let mut data = Vec::new();
        self.render_to(&mut data, runtime)?;
        Ok(String::from_utf8(data).expect("render only writes UTF-8"))
    }

    /// Renders the Renderable instance given a Liquid runtime.
    fn render_to(&self, writer: &mut dyn Write, runtime: &dyn Runtime) -> Result<()>;
}
