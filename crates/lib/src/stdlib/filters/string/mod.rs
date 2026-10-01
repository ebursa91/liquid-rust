use liquid_core::Expression;
use liquid_core::Result;
use liquid_core::Runtime;
use liquid_core::{
    Display_filter, Filter, FilterParameters, FilterReflection, FromFilterParameters, ParseFilter,
};
use liquid_core::{Value, ValueView};

pub(super) mod case;
pub(super) mod operate;
pub(super) mod strip;
pub(super) mod truncate;

#[derive(Debug, FilterParameters)]
struct SplitArgs {
    #[parameter(
        description = "The separator between each element in the string.",
        arg_type = "str"
    )]
    pattern: Expression,
}

#[derive(Clone, ParseFilter, FilterReflection)]
#[filter(
    name = "split",
    description = "Divides an input string into an array using the argument as a separator.",
    parameters(SplitArgs),
    parsed(SplitFilter)
)]
pub struct Split;

#[derive(Debug, FromFilterParameters, Display_filter)]
#[name = "split"]
struct SplitFilter {
    #[parameters]
    args: SplitArgs,
}

impl Filter for SplitFilter {
    fn evaluate(&self, input: &dyn ValueView, runtime: &dyn Runtime) -> Result<Value> {
        let args = self.args.evaluate(runtime)?;

        let input = input.to_kstr();

        let pattern = args.pattern.as_str();
        let fields = if pattern.is_empty() {
            input
                .chars()
                .map(|character| Value::scalar(character.to_string()))
                .collect()
        } else if pattern == " " {
            // Ruby String#split treats one ASCII space as a whitespace separator.
            input
                .split(|character| {
                    matches!(
                        character,
                        ' ' | '\t' | '\r' | '\n' | '\u{000b}' | '\u{000c}'
                    )
                })
                .filter(|field| !field.is_empty())
                .map(|field| Value::scalar(field.to_owned()))
                .collect()
        } else {
            let mut fields = Vec::new();
            let mut pending_empty = 0;
            for field in input.split(pattern) {
                if field.is_empty() {
                    pending_empty += 1;
                } else {
                    // Keep leading/interior empty fields; discard trailing ones.
                    fields.extend((0..pending_empty).map(|_| Value::scalar("")));
                    fields.push(Value::scalar(field.to_owned()));
                    pending_empty = 0;
                }
            }
            fields
        };
        Ok(Value::Array(fields))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unit_split() {
        assert_eq!(
            liquid_core::call_filter!(Split, "a, b, c", ", ").unwrap(),
            liquid_core::value!(["a", "b", "c"])
        );
        assert_eq!(
            liquid_core::call_filter!(Split, "a~b", "~").unwrap(),
            liquid_core::value!(["a", "b"])
        );
    }

    #[test]
    fn unit_split_bad_split_string() {
        assert_eq!(
            liquid_core::call_filter!(Split, "a,b,c", 1f64).unwrap(),
            liquid_core::value!(["a,b,c"])
        );
    }

    #[test]
    fn unit_split_no_args() {
        liquid_core::call_filter!(Split, "a,b,c").unwrap_err();
    }
}
