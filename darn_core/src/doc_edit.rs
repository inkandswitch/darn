//! Generic Automerge document editing operations.
//!
//! Provides operations for manipulating arbitrary Automerge documents by path,
//! without knowledge of their schema. Used by `darn doc edit` to modify any
//! document stored in Subduction.
//!
//! # Supported Operations
//!
//! - **Append**: Push a string value to a list at a given path
//! - **Clear**: Remove all elements from a list at a given path
//!
//! # Examples
//!
//! ```text
//! darn doc edit automerge:XYZ append modules "automerge:ABC" "automerge:DEF"
//! darn doc edit automerge:XYZ clear modules
//! ```

use automerge::{Automerge, ObjType, ReadDoc, ScalarValue, Value, transaction::Transactable};
use thiserror::Error;

/// An edit operation to apply to a document.
#[derive(Debug, Clone)]
pub enum EditOp {
    /// Append one or more string values to a list, skipping any already present.
    Append {
        /// Dot-separated path to the target list (e.g., `"modules"`).
        path: String,
        /// Values to append.
        values: Vec<String>,
    },

    /// Remove all elements from a list at the given path.
    Clear {
        /// Dot-separated path to the target list (e.g., `"modules"`).
        path: String,
    },
}

/// Apply an edit operation to an Automerge document.
///
/// Returns `true` if the document was modified, `false` if no change was needed
/// (e.g., value already present for idempotent append).
///
/// # Errors
///
/// Returns an error if the path doesn't exist, points to the wrong type,
/// or the Automerge transaction fails.
pub fn apply_edit(doc: &mut Automerge, op: &EditOp) -> Result<bool, EditError> {
    match op {
        EditOp::Append { path, values } => append_to_list(doc, path, values),
        EditOp::Clear { path } => clear_list(doc, path),
    }
}

/// Append string values to a list at the given path.
///
/// The path is a dot-separated key sequence from the document root.
/// For example, `"modules"` navigates to `doc.modules`.
///
/// Idempotent: values already in the list are skipped. All new values
/// are inserted in a single transaction.
fn append_to_list(doc: &mut Automerge, path: &str, values: &[String]) -> Result<bool, EditError> {
    let list_id = navigate_to_list(doc, path)?;

    // Collect existing values for dedup
    let length = doc.length(&list_id);
    let mut existing: std::collections::HashSet<String> =
        std::collections::HashSet::with_capacity(length);
    for i in 0..length {
        if let Some((Value::Scalar(scalar), _)) = doc.get(&list_id, i)?
            && let ScalarValue::Str(s) = scalar.as_ref()
        {
            existing.insert(s.to_string());
        }
    }

    let new_values: Vec<&str> = values
        .iter()
        .filter(|v| !existing.contains(v.as_str()))
        .map(String::as_str)
        .collect();

    if new_values.is_empty() {
        return Ok(false);
    }

    doc.transact::<_, _, EditError>(|tx| {
        for (i, value) in new_values.iter().enumerate() {
            tx.insert(&list_id, length + i, ScalarValue::Str((*value).into()))?;
        }
        Ok(())
    })
    .map_err(|failure| failure.error)?;

    Ok(true)
}

/// Remove all elements from a list at the given path.
///
/// The path is a dot-separated key sequence from the document root.
/// Returns `true` if elements were removed, `false` if the list was already empty.
fn clear_list(doc: &mut Automerge, path: &str) -> Result<bool, EditError> {
    let list_id = navigate_to_list(doc, path)?;
    let length = doc.length(&list_id);

    if length == 0 {
        return Ok(false);
    }

    doc.transact::<_, _, EditError>(|tx| {
        // Delete from the end to avoid index shifting
        for i in (0..length).rev() {
            tx.delete(&list_id, i)?;
        }
        Ok(())
    })
    .map_err(|failure| failure.error)?;

    Ok(true)
}

/// Navigate a dot-separated path to a list object, returning its `ObjId`.
fn navigate_to_list(doc: &Automerge, path: &str) -> Result<automerge::ObjId, EditError> {
    let segments: Vec<&str> = path.split('.').collect();
    let mut current = automerge::ROOT;

    for (i, segment) in segments.iter().enumerate() {
        let is_last = i == segments.len() - 1;

        match doc.get(&current, *segment)? {
            Some((Value::Object(obj_type), obj_id)) => {
                if is_last {
                    if obj_type != ObjType::List {
                        return Err(EditError::NotAList {
                            path: path.to_string(),
                            actual: format!("{obj_type:?}"),
                        });
                    }
                    current = obj_id;
                } else if obj_type != ObjType::Map {
                    return Err(EditError::NotAMap {
                        segment: (*segment).to_string(),
                        path: path.to_string(),
                    });
                } else {
                    current = obj_id;
                }
            }
            Some((Value::Scalar(_), _)) => {
                return Err(EditError::NotAnObject {
                    segment: (*segment).to_string(),
                    path: path.to_string(),
                });
            }
            None => {
                return Err(EditError::PathNotFound {
                    segment: (*segment).to_string(),
                    path: path.to_string(),
                });
            }
        }
    }

    Ok(current)
}

/// Create a new Automerge document with the given dot-separated path initialized as an empty list.
///
/// Intermediate path segments are created as maps. For example, `"a.b.modules"`
/// produces `{ a: { b: { modules: [] } } }`.
///
/// # Errors
///
/// Returns an error if the Automerge transaction fails.
pub fn create_with_empty_list(path: &str) -> Result<Automerge, EditError> {
    let mut doc = Automerge::new();
    let segments: Vec<&str> = path.split('.').collect();
    doc.transact::<_, _, automerge::AutomergeError>(|tx| {
        let mut current = automerge::ROOT;
        for (i, segment) in segments.iter().enumerate() {
            if i == segments.len() - 1 {
                tx.put_object(&current, *segment, ObjType::List)?;
            } else {
                current = tx.put_object(&current, *segment, ObjType::Map)?;
            }
        }
        Ok(())
    })
    .map_err(|failure| failure.error)?;
    Ok(doc)
}

/// Errors from document edit operations.
#[derive(Debug, Error)]
pub enum EditError {
    /// Path segment not found in the document.
    #[error("path segment '{segment}' not found (full path: {path})")]
    PathNotFound {
        /// The missing segment.
        segment: String,
        /// The full path being navigated.
        path: String,
    },

    /// Path segment points to a scalar, not an object.
    #[error("'{segment}' is a scalar value, not an object (full path: {path})")]
    NotAnObject {
        /// The segment that was a scalar.
        segment: String,
        /// The full path.
        path: String,
    },

    /// Expected a map but found a different object type.
    #[error("'{segment}' is not a map (full path: {path})")]
    NotAMap {
        /// The segment.
        segment: String,
        /// The full path.
        path: String,
    },

    /// Target path points to a non-list object.
    #[error("'{path}' is {actual}, not a list")]
    NotAList {
        /// The full path.
        path: String,
        /// The actual type found.
        actual: String,
    },

    /// Automerge operation failed.
    #[error(transparent)]
    Automerge(#[from] automerge::AutomergeError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use testresult::TestResult;

    /// Helper: create an Automerge doc with a "modules" list, optionally pre-populated.
    fn doc_with_list(initial: &[&str]) -> Result<Automerge, EditError> {
        let mut doc = Automerge::new();
        doc.transact::<_, _, automerge::AutomergeError>(|tx| {
            let list = tx.put_object(automerge::ROOT, "modules", ObjType::List)?;
            for (i, val) in initial.iter().enumerate() {
                tx.insert(&list, i, ScalarValue::Str((*val).into()))?;
            }
            Ok(())
        })
        .map_err(|f| f.error)?;
        Ok(doc)
    }

    /// Helper: get the length of the "modules" list.
    fn modules_len(doc: &Automerge) -> Result<usize, EditError> {
        let (_, list_id) =
            doc.get(automerge::ROOT, "modules")?
                .ok_or_else(|| EditError::PathNotFound {
                    segment: "modules".into(),
                    path: "modules".into(),
                })?;
        Ok(doc.length(&list_id))
    }

    #[test]
    fn append_to_empty_list() -> TestResult {
        let mut doc = doc_with_list(&[])?;

        let op = EditOp::Append {
            path: "modules".to_string(),
            values: vec!["automerge:abc123".to_string()],
        };

        let changed = apply_edit(&mut doc, &op)?;
        assert!(changed);
        assert_eq!(modules_len(&doc)?, 1);

        Ok(())
    }

    #[test]
    fn append_multiple_values() -> TestResult {
        let mut doc = doc_with_list(&[])?;

        let op = EditOp::Append {
            path: "modules".to_string(),
            values: vec![
                "automerge:aaa".to_string(),
                "automerge:bbb".to_string(),
                "automerge:ccc".to_string(),
            ],
        };

        let changed = apply_edit(&mut doc, &op)?;
        assert!(changed);
        assert_eq!(modules_len(&doc)?, 3);

        Ok(())
    }

    #[test]
    fn append_deduplicates_within_batch() -> TestResult {
        let mut doc = doc_with_list(&["automerge:existing"])?;

        let op = EditOp::Append {
            path: "modules".to_string(),
            values: vec![
                "automerge:existing".to_string(),
                "automerge:new".to_string(),
            ],
        };

        let changed = apply_edit(&mut doc, &op)?;
        assert!(changed);
        assert_eq!(modules_len(&doc)?, 2);

        Ok(())
    }

    #[test]
    fn append_is_idempotent() -> TestResult {
        let mut doc = doc_with_list(&["automerge:abc123"])?;

        let op = EditOp::Append {
            path: "modules".to_string(),
            values: vec!["automerge:abc123".to_string()],
        };

        let changed = apply_edit(&mut doc, &op)?;
        assert!(!changed, "should not modify when value already present");
        assert_eq!(modules_len(&doc)?, 1);

        Ok(())
    }

    #[test]
    fn append_to_nonexistent_path() {
        let mut doc = Automerge::new();

        let op = EditOp::Append {
            path: "modules".to_string(),
            values: vec!["automerge:abc123".to_string()],
        };

        let result = apply_edit(&mut doc, &op);
        assert!(result.is_err());
    }

    #[test]
    fn clear_populated_list() -> TestResult {
        let mut doc = doc_with_list(&["automerge:aaa", "automerge:bbb", "automerge:ccc"])?;

        let op = EditOp::Clear {
            path: "modules".to_string(),
        };

        let changed = apply_edit(&mut doc, &op)?;
        assert!(changed);
        assert_eq!(modules_len(&doc)?, 0);

        Ok(())
    }

    #[test]
    fn clear_empty_list() -> TestResult {
        let mut doc = doc_with_list(&[])?;

        let op = EditOp::Clear {
            path: "modules".to_string(),
        };

        let changed = apply_edit(&mut doc, &op)?;
        assert!(!changed, "should report no change for already-empty list");

        Ok(())
    }

    #[test]
    fn clear_then_append() -> TestResult {
        let mut doc = doc_with_list(&["automerge:old"])?;

        apply_edit(
            &mut doc,
            &EditOp::Clear {
                path: "modules".to_string(),
            },
        )?;

        apply_edit(
            &mut doc,
            &EditOp::Append {
                path: "modules".to_string(),
                values: vec!["automerge:new".to_string()],
            },
        )?;

        assert_eq!(modules_len(&doc)?, 1);

        let (_, list_id) = doc
            .get(automerge::ROOT, "modules")?
            .ok_or("modules missing")?;

        let (value, _) = doc.get(&list_id, 0)?.ok_or("first item missing")?;
        assert_eq!(value.to_str(), Some("automerge:new"));

        Ok(())
    }
}
