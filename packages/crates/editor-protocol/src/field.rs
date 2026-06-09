//! [`FieldValue`] — a single editable node setting flowing through the
//! `SetField` command.
//!
//! This is the *pure-data* half of the editor's `fields` module: the live
//! `Field`/`Control` reflection (and its `&'static str` interning) stays in the
//! editor; only this serializable value crosses the wire.

use serde::{Deserialize, Serialize};

/// A single editable value flowing through the `SetField` command.
#[cfg_attr(feature = "schemars", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "t", content = "v")]
pub enum FieldValue {
    Num(f64),
    Text(String),
    Bool(bool),
}
