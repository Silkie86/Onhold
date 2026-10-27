//! Dotted paths into JSON context trees: `thing.custom.budget`.

use serde_json::Value;

/// A path is one or more segments of letters, digits and underscores
/// separated by dots; array elements are addressed by index (`tags.0`).
pub fn is_valid(path: &str) -> bool {
    !path.is_empty()
        && path.len() <= 200
        && path.split('.').all(|seg| {
            !seg.is_empty() && seg.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}

/// The value at `path`, or `None` if any segment is missing.
pub fn lookup<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(root, |node, seg| match node {
        Value::Object(map) => map.get(seg),
        Value::Array(items) => seg.parse::<usize>().ok().and_then(|i| items.get(i)),
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn looks_up_nested_values() {
        let ctx = json!({ "thing": { "custom": { "budget": 5, "tags": ["a", "b"] } } });
        assert_eq!(lookup(&ctx, "thing.custom.budget"), Some(&json!(5)));
        assert_eq!(lookup(&ctx, "thing.custom.tags.1"), Some(&json!("b")));
        assert_eq!(lookup(&ctx, "thing.custom.missing"), None);
        assert_eq!(lookup(&ctx, "thing.custom.budget.deeper"), None);
        assert!(is_valid("thing.custom.budget"));
        for bad in ["", "a..b", ".a", "a.", "a b", "a-b"] {
            assert!(!is_valid(bad), "{bad}");
        }
    }
}
