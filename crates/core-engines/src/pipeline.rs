//! Pipelines: an ordered list of stages an object moves through, with
//! optional allowed transitions, fields required to enter a stage, and
//! won/lost terminal stages.

use std::collections::{BTreeSet, HashSet};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Issue;
use crate::fields::{has_value, is_valid_key};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    /// Work in progress.
    #[default]
    Open,
    /// Terminal: the deal happened.
    Won,
    /// Terminal: it didn't.
    Lost,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Open => "open",
            Outcome::Won => "won",
            Outcome::Lost => "lost",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "open" => Some(Outcome::Open),
            "won" => Some(Outcome::Won),
            "lost" => Some(Outcome::Lost),
            _ => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Stage {
    pub key: String,
    pub name: String,
    pub outcome: Outcome,
    /// Custom field keys that must have a value before an object may enter.
    pub required_fields: Vec<String>,
    /// Stage keys this stage may move to; `None` means any.
    pub allowed_next: Option<Vec<String>>,
    pub sla_days: Option<i32>,
    pub archived: bool,
}

pub const MAX_STAGES: usize = 50;

/// Checks a pipeline's active stages as a whole. `field_keys` are the
/// entity's active custom field keys.
pub fn check_stages(stages: &[Stage], field_keys: &HashSet<&str>) -> Result<(), Issue> {
    let active: Vec<&Stage> = stages.iter().filter(|s| !s.archived).collect();
    if active.is_empty() || active.len() > MAX_STAGES {
        return Err(Issue::new(
            "stages",
            format!("a pipeline needs 1 to {MAX_STAGES} stages"),
        ));
    }
    let mut keys = BTreeSet::new();
    for (i, s) in stages.iter().enumerate() {
        if !is_valid_key(&s.key) {
            return Err(Issue::new(
                format!("stages[{i}].key"),
                "start with a lowercase letter; use lowercase letters, digits and `_`",
            ));
        }
        if !keys.insert(s.key.as_str()) {
            return Err(Issue::new(
                format!("stages[{i}].key"),
                format!("`{}` appears twice", s.key),
            ));
        }
        let name = s.name.trim();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(Issue::new(
                format!("stages[{i}].name"),
                "must be 1 to 200 characters",
            ));
        }
        if s.sla_days.is_some_and(|d| !(1..=3650).contains(&d)) {
            return Err(Issue::new(
                format!("stages[{i}].sla_days"),
                "must be 1 to 3650",
            ));
        }
    }
    for (i, s) in stages.iter().enumerate().filter(|(_, s)| !s.archived) {
        for f in &s.required_fields {
            if !field_keys.contains(f.as_str()) {
                return Err(Issue::new(
                    format!("stages[{i}].required_fields"),
                    format!("there is no custom field `{f}`"),
                ));
            }
        }
        for next in s.allowed_next.iter().flatten() {
            if !active.iter().any(|a| &a.key == next) {
                return Err(Issue::new(
                    format!("stages[{i}].allowed_next"),
                    format!("there is no stage `{next}`"),
                ));
            }
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MoveError {
    #[error("the stage `{0}` has been removed")]
    Archived(String),
    #[error("cannot move from `{from}` to `{to}`")]
    NotAllowed { from: String, to: String },
    #[error("fill in {} first", .0.join(", "))]
    MissingFields(Vec<String>),
}

/// Whether an object with `custom` values may move from `from` (none when
/// it first enters the pipeline) to `to`.
pub fn check_move(
    from: Option<&Stage>,
    to: &Stage,
    custom: &Map<String, Value>,
) -> Result<(), MoveError> {
    if to.archived {
        return Err(MoveError::Archived(to.key.clone()));
    }
    if let Some(from) = from
        && let Some(allowed) = &from.allowed_next
        && from.key != to.key
        && !allowed.contains(&to.key)
    {
        return Err(MoveError::NotAllowed {
            from: from.key.clone(),
            to: to.key.clone(),
        });
    }
    let missing: Vec<String> = to
        .required_fields
        .iter()
        .filter(|f| !has_value(custom, f))
        .cloned()
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(MoveError::MissingFields(missing))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn stage(key: &str) -> Stage {
        Stage {
            key: key.into(),
            name: key.into(),
            outcome: Outcome::Open,
            required_fields: vec![],
            allowed_next: None,
            sla_days: None,
            archived: false,
        }
    }

    #[test]
    fn stages_are_checked_together() {
        let fields: HashSet<&str> = ["budget"].into();
        let mut a = stage("new");
        a.allowed_next = Some(vec!["won".into()]);
        let mut b = stage("won");
        b.outcome = Outcome::Won;
        b.required_fields = vec!["budget".into()];
        assert!(check_stages(&[a.clone(), b.clone()], &fields).is_ok());

        assert!(check_stages(&[], &fields).is_err());
        assert!(check_stages(&[a.clone(), a.clone()], &fields).is_err());
        let mut bad_next = a.clone();
        bad_next.allowed_next = Some(vec!["nowhere".into()]);
        assert!(check_stages(&[bad_next, b.clone()], &fields).is_err());
        let mut bad_field = b;
        bad_field.required_fields = vec!["nope".into()];
        assert!(check_stages(&[a, bad_field], &fields).is_err());
    }

    #[test]
    fn moves_follow_the_rules() {
        let mut new = stage("new");
        new.allowed_next = Some(vec!["quoted".into()]);
        let mut quoted = stage("quoted");
        quoted.required_fields = vec!["budget".into()];
        let mut lost = stage("lost");
        lost.archived = true;
        let empty = Map::new();
        let filled = json!({"budget": 10})
            .as_object()
            .cloned()
            .unwrap_or_default();

        assert!(check_move(None, &new, &empty).is_ok());
        assert_eq!(
            check_move(Some(&new), &quoted, &empty),
            Err(MoveError::MissingFields(vec!["budget".into()]))
        );
        assert!(check_move(Some(&new), &quoted, &filled).is_ok());
        assert!(matches!(check_move(Some(&quoted), &new, &empty), Ok(())));
        let mut won = stage("won");
        won.outcome = Outcome::Won;
        assert!(matches!(
            check_move(Some(&new), &won, &empty),
            Err(MoveError::NotAllowed { .. })
        ));
        assert_eq!(
            check_move(Some(&new), &lost, &empty),
            Err(MoveError::Archived("lost".into()))
        );
    }
}
