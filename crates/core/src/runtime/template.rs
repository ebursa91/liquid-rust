use std::io::Write;

use crate::error::Result;

use super::Renderable;
use super::Runtime;

/// An executable template block.
#[derive(Debug)]
pub struct Template {
    elements: Vec<Box<dyn Renderable>>,
    blank: bool,
}

impl Template {
    /// Create an executable template block.
    pub fn new(elements: Vec<Box<dyn Renderable>>) -> Template {
        let blank = elements.iter().all(|element| element.is_blank());
        Template { elements, blank }
    }
}

impl Renderable for Template {
    fn is_blank(&self) -> bool {
        self.blank
    }

    fn trim_blank(&mut self) {
        if self.blank {
            for element in &mut self.elements {
                element.trim_blank();
            }
        }
    }

    fn render_to(&self, writer: &mut dyn Write, runtime: &dyn Runtime) -> Result<()> {
        for el in &self.elements {
            el.render_to(writer, runtime)?;

            // Did the last element we processed set an interrupt? If so, we
            // need to abandon the rest of our child elements and just
            // return what we've got. This is usually in response to a
            // `break` or `continue` tag being rendered.
            if runtime
                .registers()
                .get_mut::<super::InterruptRegister>()
                .interrupted()
            {
                break;
            }
        }
        Ok(())
    }
}
