//! Rule conditions: a small boolean tree over field comparisons, evaluated
//! against the rule's JSON context (`thing`, `before`, `event`, `workspace`).
//!
//! ```json
//! { "all": [
//!     { "field": "thing.custom.budget", "op": "gte", "value": 50000 },
//!     { "any": [
//!         { "field": "thing.stage", "op": "eq", "value": "quoted" },
//!         { "not": { "field": "thing.custom.region", "op": "is_empty" } }
//!     ] }
//! ] }
//! ```

use std::cmp::Ordering;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Issue, path};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
    /// Text contains the value, or a list contains it as an element.
    Contains,
    NotContains,
    /// The field equals one of the values in a list.
    In,
    NotIn,
    IsEmpty,
    IsNotEmpty,
    /// The field differs from its value before the change (`before.*`).
    Changed,
}

impl Op {
    fn takes_value(self) -> bool {
        !matches!(self, Op::IsEmpty | Op::IsNotEmpty | Op::Changed)
    }

    fn as_str(self) -> &'static str {
        match self {
            Op::Eq => "equals",
            Op::Ne => "does not equal",
            Op::Gt => "is greater than",
            Op::Gte => "is at least",
            Op::Lt => "is less than",
            Op::Lte => "is at most",
            Op::Contains => "contains",
            Op::NotContains => "does not contain",
            Op::In => "is one of",
            Op::NotIn => "is not one of",
            Op::IsEmpty => "is empty",
            Op::IsNotEmpty => "is not empty",
            Op::Changed => "changed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Condition {
    All {
        all: Vec<Condition>,
    },
    Any {
        any: Vec<Condition>,
    },
    Not {
        not: Box<Condition>,
    },
    Compare {
        field: String,
        op: Op,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        value: Option<Value>,
    },
}

impl Default for Condition {
    fn default() -> Self {
        Condition::All { all: Vec::new() }
    }
}

pub const MAX_DEPTH: usize = 5;
pub const MAX_COMPARISONS: usize = 50;

/// Parses and checks a condition tree from its stored JSON.
pub fn parse(value: &Value) -> Result<Condition, Issue> {
    let cond: Condition = serde_json::from_value(value.clone()).map_err(|_| {
        Issue::new(
            "conditions",
            "use {\"all\": […]}, {\"any\": […]}, {\"not\": …} or {\"field\", \"op\", \"value\"}",
        )
    })?;
    let mut count = 0;
    check(&cond, "conditions", 0, &mut count)?;
    Ok(cond)
}

fn check(cond: &Condition, at: &str, depth: usize, count: &mut usize) -> Result<(), Issue> {
    if depth > MAX_DEPTH {
        return Err(Issue::new(
            at,
            format!("nest conditions at most {MAX_DEPTH} deep"),
        ));
    }
    match cond {
        Condition::All { all: list } | Condition::Any { any: list } => {
            let key = if matches!(cond, Condition::All { .. }) {
                "all"
            } else {
                "any"
            };
            for (i, c) in list.iter().enumerate() {
                check(c, &format!("{at}.{key}[{i}]"), depth + 1, count)?;
            }
            Ok(())
        }
        Condition::Not { not } => check(not, &format!("{at}.not"), depth + 1, count),
        Condition::Compare { field, op, value } => {
            *count += 1;
            if *count > MAX_COMPARISONS {
                return Err(Issue::new(
                    at,
                    format!("at most {MAX_COMPARISONS} comparisons"),
                ));
            }
            if !path::is_valid(field) {
                return Err(Issue::new(
                    format!("{at}.field"),
                    "use a dotted path such as `thing.custom.budget`",
                ));
            }
            match (op.takes_value(), value) {
                (true, None) => Err(Issue::new(format!("{at}.value"), "is required")),
                (false, Some(_)) => Err(Issue::new(
                    format!("{at}.value"),
                    format!("`{}` takes no value", op.as_str()),
                )),
                (true, Some(v)) if matches!(op, Op::In | Op::NotIn) && !v.is_array() => {
                    Err(Issue::new(format!("{at}.value"), "must be a list"))
                }
                _ => Ok(()),
            }
        }
    }
}

/// One comparison as evaluated, for the dry-run explanation.
#[derive(Debug, Clone, PartialEq, Serialize, utoipa::ToSchema)]
pub struct Check {
    pub field: String,
    pub op: Op,
    #[schema(value_type = Option<Object>)]
    pub expected: Option<Value>,
    /// What the field held (absent if it did not exist).
    #[schema(value_type = Option<Object>)]
    pub actual: Option<Value>,
    pub passed: bool,
}

impl std::fmt::Display for Check {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let actual = self
            .actual
            .as_ref()
            .map_or_else(|| "nothing".to_owned(), Value::to_string);
        write!(f, "{} ({actual}) {}", self.field, self.op.as_str())?;
        if let Some(e) = &self.expected {
            write!(f, " {e}")?;
        }
        f.write_str(if self.passed { ": yes" } else { ": no" })
    }
}

/// Evaluates `cond` against `ctx`, returning whether it holds and each
/// comparison made along the way (short-circuiting as `and`/`or` do).
pub fn evaluate(cond: &Condition, ctx: &Value) -> (bool, Vec<Check>) {
    let mut trace = Vec::new();
    let ok = eval(cond, ctx, &mut trace);
    (ok, trace)
}

fn eval(cond: &Condition, ctx: &Value, trace: &mut Vec<Check>) -> bool {
    match cond {
        Condition::All { all } => all.iter().all(|c| eval(c, ctx, trace)),
        Condition::Any { any } => any.iter().any(|c| eval(c, ctx, trace)),
        Condition::Not { not } => !eval(not, ctx, trace),
        Condition::Compare { field, op, value } => {
            let actual = path::lookup(ctx, field).filter(|v| !v.is_null());
            let passed = compare(ctx, field, *op, actual, value.as_ref());
            trace.push(Check {
                field: field.clone(),
                op: *op,
                expected: value.clone(),
                actual: actual.cloned(),
                passed,
            });
            passed
        }
    }
}

fn is_empty(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => true,
        Some(Value::String(s)) => s.trim().is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        Some(Value::Object(o)) => o.is_empty(),
        Some(_) => false,
    }
}

/// Money compares by its minor units, so `{"minor": 60000, …} gt 50000` works.
fn scalar(v: &Value) -> &Value {
    match v {
        Value::Object(o) if o.contains_key("minor") && o.contains_key("currency") => {
            o.get("minor").unwrap_or(v)
        }
        _ => v,
    }
}

fn order(a: &Value, b: &Value) -> Option<Ordering> {
    match (scalar(a), scalar(b)) {
        (Value::Number(x), Value::Number(y)) => x.as_f64()?.partial_cmp(&y.as_f64()?),
        // ISO dates and times order correctly as text.
        (Value::String(x), Value::String(y)) => Some(x.cmp(y)),
        (Value::Bool(x), Value::Bool(y)) => Some(x.cmp(y)),
        _ => None,
    }
}

fn equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(_), Value::Number(_)) => order(a, b) == Some(Ordering::Equal),
        (Value::Object(_), Value::Number(_)) => order(a, b) == Some(Ordering::Equal),
        (Value::String(x), Value::String(y)) => x == y,
        _ => a == b,
    }
}

fn compare(
    ctx: &Value,
    field: &str,
    op: Op,
    actual: Option<&Value>,
    expected: Option<&Value>,
) -> bool {
    match op {
        Op::IsEmpty => is_empty(actual),
        Op::IsNotEmpty => !is_empty(actual),
        Op::Changed => {
            // `thing.custom.x` changed when it differs from `before.custom.x`.
            let Some((_, rest)) = field.split_once('.') else {
                return false;
            };
            let before = path::lookup(ctx, &format!("before.{rest}")).filter(|v| !v.is_null());
            ctx.get("before").is_some_and(|b| !b.is_null()) && before != actual
        }
        _ => {
            let (Some(actual), Some(expected)) = (actual, expected) else {
                // A missing field equals nothing and is not in any list.
                return matches!(op, Op::Ne | Op::NotIn | Op::NotContains);
            };
            match op {
                Op::Eq => equal(actual, expected),
                Op::Ne => !equal(actual, expected),
                Op::Gt => order(actual, expected) == Some(Ordering::Greater),
                Op::Gte => matches!(
                    order(actual, expected),
                    Some(Ordering::Greater | Ordering::Equal)
                ),
                Op::Lt => order(actual, expected) == Some(Ordering::Less),
                Op::Lte => matches!(
                    order(actual, expected),
                    Some(Ordering::Less | Ordering::Equal)
                ),
                Op::Contains | Op::NotContains => {
                    let found = match (actual, expected) {
                        (Value::String(s), Value::String(needle)) => {
                            s.to_lowercase().contains(&needle.to_lowercase())
                        }
                        (Value::Array(items), needle) => items.iter().any(|i| equal(i, needle)),
                        _ => false,
                    };
                    found == (op == Op::Contains)
                }
                Op::In | Op::NotIn => {
                    let found = expected
                        .as_array()
                        .is_some_and(|list| list.iter().any(|e| equal(actual, e)));
                    found == (op == Op::In)
                }
                Op::IsEmpty | Op::IsNotEmpty | Op::Changed => unreachable!("handled above"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn holds(cond: Value, ctx: &Value) -> bool {
        evaluate(&parse(&cond).expect("valid"), ctx).0
    }

    #[test]
    fn comparisons() {
        let ctx = json!({
            "thing": { "name": "Spring Tour", "stage": "quoted",
                "custom": { "budget": {"minor": 60000, "currency": "GBP"}, "n": 3, "tags": ["vip"], "when": "2027-03-07" } },
            "before": { "stage": "new", "custom": { "n": 3 } }
        });
        assert!(holds(
            json!({"field": "thing.custom.budget", "op": "gt", "value": 50000}),
            &ctx
        ));
        assert!(holds(
            json!({"field": "thing.custom.n", "op": "eq", "value": 3.0}),
            &ctx
        ));
        assert!(holds(
            json!({"field": "thing.custom.n", "op": "lte", "value": 3}),
            &ctx
        ));
        assert!(holds(
            json!({"field": "thing.name", "op": "contains", "value": "tour"}),
            &ctx
        ));
        assert!(holds(
            json!({"field": "thing.custom.tags", "op": "contains", "value": "vip"}),
            &ctx
        ));
        assert!(holds(
            json!({"field": "thing.stage", "op": "in", "value": ["quoted", "won"]}),
            &ctx
        ));
        assert!(holds(
            json!({"field": "thing.custom.when", "op": "lt", "value": "2027-04-01"}),
            &ctx
        ));
        assert!(holds(
            json!({"field": "thing.custom.missing", "op": "is_empty"}),
            &ctx
        ));
        assert!(holds(
            json!({"field": "thing.custom.missing", "op": "ne", "value": 1}),
            &ctx
        ));
        assert!(!holds(
            json!({"field": "thing.custom.missing", "op": "eq", "value": 1}),
            &ctx
        ));
        assert!(!holds(
            json!({"field": "thing.custom.missing", "op": "gt", "value": 1}),
            &ctx
        ));
        assert!(holds(
            json!({"field": "thing.stage", "op": "changed"}),
            &ctx
        ));
        assert!(!holds(
            json!({"field": "thing.custom.n", "op": "changed"}),
            &ctx
        ));
        // Different types never match.
        assert!(!holds(
            json!({"field": "thing.custom.n", "op": "eq", "value": "3"}),
            &ctx
        ));
    }

    #[test]
    fn changed_is_false_without_a_before() {
        let ctx = json!({ "thing": { "stage": "new" }, "before": null });
        assert!(!holds(
            json!({"field": "thing.stage", "op": "changed"}),
            &ctx
        ));
    }

    #[test]
    fn trees_and_traces() {
        let ctx = json!({ "thing": { "a": 1, "b": 2 } });
        let cond = json!({ "all": [
            {"field": "thing.a", "op": "eq", "value": 1},
            {"any": [ {"field": "thing.b", "op": "eq", "value": 9}, {"not": {"field": "thing.b", "op": "is_empty"}} ]}
        ]});
        let (ok, trace) = evaluate(&parse(&cond).expect("valid"), &ctx);
        assert!(ok);
        assert_eq!(trace.len(), 3);
        assert_eq!(trace[1].to_string(), "thing.b (2) equals 9: no");
        assert!(holds(json!({"all": []}), &ctx), "empty all is true");
        assert!(!holds(json!({"any": []}), &ctx), "empty any is false");
    }

    #[test]
    fn malformed_conditions_are_rejected() {
        for bad in [
            json!({"field": "thing.a", "op": "eq"}),
            json!({"field": "thing.a", "op": "is_empty", "value": 1}),
            json!({"field": "thing.a", "op": "in", "value": 1}),
            json!({"field": "thing a", "op": "eq", "value": 1}),
            json!({"field": "thing.a", "op": "explode", "value": 1}),
            json!({"some": []}),
            json!({"not": {"not": {"not": {"not": {"not": {"not": {"field": "a", "op": "is_empty"}}}}}}}),
        ] {
            assert!(parse(&bad).is_err(), "{bad}");
        }
        let many: Vec<Value> = (0..51)
            .map(|_| json!({"field": "a", "op": "is_empty"}))
            .collect();
        assert!(parse(&json!({ "all": many })).is_err());
    }
}
