//! gitty-core: the read/write engine behind gitty. The only crate that talks to gix.

pub mod error;
pub mod git_bin;
pub mod history;
pub mod refs;
pub mod repo;
pub mod types;

pub use error::GitError;
pub use repo::{Handle, Repo};
pub use types::{CommitId, Signature};
