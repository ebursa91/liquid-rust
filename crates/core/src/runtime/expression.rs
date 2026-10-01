use std::fmt;

use crate::error::Result;
use crate::model::Scalar;
use crate::model::Value;
use crate::model::ValueCow;
use crate::model::ValueView;

use super::variable::Variable;
use super::Runtime;

/// An un-evaluated `Value`.
#[derive(Debug, Clone, PartialEq)]
pub enum Expression {
    /// Un-evaluated.
    Variable(Variable),
    /// Evaluated.
    Literal(Value),
}

impl Expression {
    /// Create an expression from a scalar literal.
    pub fn with_literal<S: Into<Scalar>>(literal: S) -> Self {
        Expression::Literal(Value::scalar(literal))
    }

    /// Convert into a literal if possible.
    pub fn into_literal(self) -> Option<Value> {
        match self {
            Expression::Literal(x) => Some(x),
            Expression::Variable(_) => None,
        }
    }

    /// Convert into a variable, if possible.
    pub fn into_variable(self) -> Option<Variable> {
        match self {
            Expression::Literal(_) => None,
            Expression::Variable(x) => Some(x),
        }
    }

    /// Convert to a `Value`.
    pub fn try_evaluate<'c>(&'c self, runtime: &'c dyn Runtime) -> Option<ValueCow<'c>> {
        match self {
            Expression::Literal(ref x) => {
                Some(runtime.project_value(ValueCow::Borrowed(x), runtime, None))
            }
            Expression::Variable(ref x) => {
                let path = x.try_evaluate(runtime)?;
                runtime
                    .try_get(&path)
                    .map(|value| runtime.project_value(value, runtime, Some(&path)))
            }
        }
    }

    /// Convert to a `Value`.
    pub fn evaluate<'c>(&'c self, runtime: &'c dyn Runtime) -> Result<ValueCow<'c>> {
        let (val, path) = match self {
            Expression::Literal(ref x) => (ValueCow::Borrowed(x), None),
            Expression::Variable(ref x) => {
                if runtime.strict_variables() {
                    let path = x.evaluate(runtime)?;
                    (runtime.get(&path)?, Some(path))
                } else {
                    let path = x.evaluate_optional(runtime)?;
                    let value = path
                        .as_ref()
                        .and_then(|path| runtime.try_get(path))
                        .unwrap_or(ValueCow::Owned(Value::Nil));
                    (value, path)
                }
            }
        };
        Ok(runtime.project_value(val, runtime, path.as_deref()))
    }
}

impl fmt::Display for Expression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expression::Literal(ref x) => write!(f, "{}", x.source()),
            Expression::Variable(ref x) => write!(f, "{}", x),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Error;
    use crate::parser::{parse_variable, Filter, FilterChain};
    use crate::runtime::RuntimeBuilder;

    fn variable(source: &str) -> Expression {
        Expression::Variable(parse_variable(source).unwrap())
    }

    #[test]
    fn missing_paths_and_dynamic_selectors_are_optional_only_when_requested() {
        let globals = crate::object!({
            "object": {"key": "ok"},
            "values": ["one", "two"],
            "absent_value": nil,
            "index_array": [],
            "index_object": {},
        });
        let strict = RuntimeBuilder::new().set_globals(&globals).build();
        let optional = RuntimeBuilder::new()
            .set_strict_variables(false)
            .set_globals(&globals)
            .build();

        for source in [
            "missing",
            "missing.foo.bar",
            "absent_value.foo.bar",
            "object.absent.deep",
            "values[99]",
            "values[missing]",
            "object[missing]",
            "values[absent_value]",
            "values[index_array]",
            "object[index_object]",
            "values[object.absent]",
        ] {
            let expression = variable(source);
            assert!(expression.evaluate(&strict).is_err(), "{source}");
            assert!(expression.evaluate(&optional).unwrap().is_nil(), "{source}");
        }
    }

    #[test]
    fn optional_lookup_preserves_existing_values_and_dynamic_indexes() {
        let globals = crate::object!({
            "object": {"key": "ok"},
            "values": ["one", "two"],
            "absent_value": nil,
            "key": "key",
            "index": 1,
        });
        for strict_variables in [true, false] {
            let runtime = RuntimeBuilder::new()
                .set_globals(&globals)
                .set_strict_variables(strict_variables)
                .build();
            for (source, expected) in [
                ("object[key]", Value::scalar("ok")),
                ("object.key", Value::scalar("ok")),
                ("values[index]", Value::scalar("two")),
                ("values[0]", Value::scalar("one")),
                ("values[-1]", Value::scalar("two")),
                ("values.size", Value::scalar(2)),
                ("absent_value", Value::Nil),
            ] {
                let expression = variable(source);
                assert_eq!(
                    expression.evaluate(&runtime).unwrap().into_owned(),
                    expected,
                    "{source}, strict_variables={strict_variables}"
                );
            }
        }
    }

    #[test]
    fn strict_lookup_keeps_existing_diagnostics() {
        let globals = crate::object!({"object": {}, "selector": []});
        let runtime = RuntimeBuilder::new().set_globals(&globals).build();
        for (source, message) in [
            ("missing", "Unknown variable"),
            ("object.absent", "Unknown index"),
            ("object[selector]", "Expected scalar"),
        ] {
            let error = variable(source).evaluate(&runtime).unwrap_err().to_string();
            assert!(error.contains(message), "{source}: {error}");
        }
    }

    #[derive(Debug)]
    struct RejectFilter;

    impl fmt::Display for RejectFilter {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("reject")
        }
    }

    impl Filter for RejectFilter {
        fn evaluate(&self, input: &dyn ValueView, _runtime: &dyn Runtime) -> Result<Value> {
            assert!(input.is_nil());
            Err(Error::with_msg("Intentional filter failure"))
        }
    }

    #[test]
    fn optional_variables_do_not_suppress_filter_errors() {
        let runtime = RuntimeBuilder::new().set_strict_variables(false).build();
        let chain = FilterChain::new(variable("missing"), vec![Box::new(RejectFilter)]);
        let error = chain.evaluate(&runtime).unwrap_err().to_string();
        assert!(error.contains("Intentional filter failure"), "{error}");
        assert!(error.contains("Filter error"), "{error}");
    }
}
