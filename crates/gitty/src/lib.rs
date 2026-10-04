//! gitty: a fast terminal git client. The binary is a thin wrapper over [`run`].

pub mod text;
pub mod dates;
pub mod editor;
pub mod theme;
pub mod config;
pub mod exec;
pub mod msg;
pub mod workers;
pub mod write;
pub mod ui;
pub mod app;
pub mod input;
pub mod term;
pub mod run;
pub use run::run;
