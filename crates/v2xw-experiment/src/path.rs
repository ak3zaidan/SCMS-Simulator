//! Writing a value into a scenario document at a dotted path.
//!
//! The engine publishes [`v2xw_engine::scenario::resolve_path`], which *reads* `a.b[2].c`
//! out of a document and is what the scenario validator checks a sweep path with. A sweep
//! has to write one, and writing is not the mirror image of reading: a path that resolves
//! for the reader can still be unwritable (an array index past the end, a key on something
//! that is not an object), and each of those is a mistake the author should be told about
//! by name rather than discovering as a run that silently swept nothing.
//!
//! The grammar is the reader's, exactly: dot-separated segments, each of which is a name
//! followed by zero or more `[index]` suffixes, and a leading `[0]` on an empty name
//! indexes the current value. Keeping the two in step matters, because the validator
//! accepts a sweep path using the reader and this module then has to write it.
//!
//! # The path must already exist
//!
//! [`set_path`] replaces a value; it never creates one. A scenario is a closed schema
//! (`deny_unknown_fields`), so a path that is not already in the serialised document is a
//! path the scenario has no field for, and inserting it would produce a document the
//! loader then refuses with a message about an unknown key rather than about the sweep.

use serde_json::Value;

use crate::error::{ExperimentError, Result};

/// One step of a walk into a document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step<'a> {
    /// An object key.
    Key(&'a str),
    /// An array index.
    Index(usize),
}

/// Replaces the value at `path` in `doc`.
///
/// # Errors
/// [`ExperimentError::BadSweepPath`] if the path is malformed, does not exist in the
/// document, or names a key on something that is not an object.
pub fn set_path(doc: &mut Value, path: &str, value: Value) -> Result<()> {
    let steps = parse_path(path)?;
    if steps.is_empty() {
        return Err(bad(path, "is empty, so it names no field"));
    }
    walk(doc, &steps, value, path)
}

/// Splits `a.b[2].c` into its steps, with the same grammar the engine's reader uses.
fn parse_path(path: &str) -> Result<Vec<Step<'_>>> {
    let mut steps = Vec::new();
    for segment in path.split('.') {
        let (name, indices) = split_indices(segment)
            .ok_or_else(|| bad(path, format!("segment `{segment}` has malformed brackets")))?;
        if !name.is_empty() {
            steps.push(Step::Key(name));
        }
        for i in indices {
            steps.push(Step::Index(i));
        }
    }
    Ok(steps)
}

/// `foo[1][2]` into `("foo", [1, 2])`; `None` if the brackets are malformed.
///
/// A transcription of the engine's `split_indices`, which is private to its validator.
fn split_indices(segment: &str) -> Option<(&str, Vec<usize>)> {
    let Some(open) = segment.find('[') else {
        return Some((segment, Vec::new()));
    };
    let (name, rest) = segment.split_at(open);
    let mut indices = Vec::new();
    let mut rest = rest;
    while !rest.is_empty() {
        if !rest.starts_with('[') {
            return None;
        }
        let close = rest.find(']')?;
        indices.push(rest[1..close].parse::<usize>().ok()?);
        rest = &rest[close + 1..];
    }
    Some((name, indices))
}

/// Walks the remaining steps and assigns at the last one.
///
/// Recursive rather than a loop with a reassigned `&mut`, because the recursion is three
/// or four deep on a scenario path and reads as what it does.
fn walk(cur: &mut Value, steps: &[Step<'_>], value: Value, path: &str) -> Result<()> {
    match steps {
        [] => Err(bad(path, "is empty, so it names no field")),
        [Step::Key(key)] => {
            let object = cur
                .as_object_mut()
                .ok_or_else(|| bad(path, format!("`{key}` is not a key of an object")))?;
            if !object.contains_key(*key) {
                return Err(bad(
                    path,
                    format!("`{key}` is not a field of this scenario at that position"),
                ));
            }
            object.insert((*key).to_string(), value);
            Ok(())
        }
        [Step::Index(index)] => {
            let array = cur
                .as_array_mut()
                .ok_or_else(|| bad(path, format!("[{index}] is not an index into an array")))?;
            // Read before the element borrow, so the message can state the real length.
            let len = array.len();
            let slot = array.get_mut(*index).ok_or_else(|| {
                bad(
                    path,
                    format!("[{index}] is past the end of a {len}-element array"),
                )
            })?;
            *slot = value;
            Ok(())
        }
        [head, tail @ ..] => {
            let next = match head {
                Step::Key(key) => cur.get_mut(*key).ok_or_else(|| {
                    bad(
                        path,
                        format!("`{key}` is not a field of this scenario at that position"),
                    )
                })?,
                Step::Index(index) => cur
                    .get_mut(*index)
                    .ok_or_else(|| bad(path, format!("[{index}] is past the end of the array")))?,
            };
            walk(next, tail, value, path)
        }
    }
}

fn bad(path: &str, problem: impl Into<String>) -> ExperimentError {
    ExperimentError::BadSweepPath {
        path: path.to_string(),
        problem: problem.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> Value {
        serde_json::json!({
            "a": {"b": 1, "c": [10, 20, 30]},
            "d": "x"
        })
    }

    #[test]
    fn a_nested_key_is_replaced() {
        let mut d = doc();
        set_path(&mut d, "a.b", Value::from(7)).expect("a.b exists");
        assert_eq!(d["a"]["b"], Value::from(7));
        // Negative control: the sibling is untouched, so the write is a write and not a
        // rebuild of the document.
        assert_eq!(d["d"], Value::from("x"));
    }

    #[test]
    fn an_array_element_is_replaced() {
        let mut d = doc();
        set_path(&mut d, "a.c[1]", Value::from(99)).expect("a.c[1] exists");
        assert_eq!(d["a"]["c"], serde_json::json!([10, 99, 30]));
    }

    #[test]
    fn an_absent_field_is_refused_by_name() {
        let mut d = doc();
        let e = set_path(&mut d, "a.nope", Value::from(1)).expect_err("a.nope does not exist");
        assert!(format!("{e}").contains("a.nope"), "{e}");
    }

    #[test]
    fn an_index_past_the_end_is_refused() {
        let mut d = doc();
        assert!(set_path(&mut d, "a.c[9]", Value::from(1)).is_err());
        // Negative control: the in-range index is accepted, so the check is about the
        // index and not about arrays in general.
        assert!(set_path(&mut d, "a.c[2]", Value::from(1)).is_ok());
    }
}
