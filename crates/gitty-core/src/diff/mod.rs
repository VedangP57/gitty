//! The diff engine: blobs → `FileDiff` (classification + op lists) → `DiffView` rows.

pub mod classify;
pub mod ops;
pub mod text;
pub mod view;
