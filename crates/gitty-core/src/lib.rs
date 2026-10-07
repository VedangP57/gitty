//! gitty-core: the read/write engine behind gitty. The only crate that talks to gix.

pub mod ahead_behind;
pub mod branch;
pub mod commit_files;
pub mod compare;
pub mod diff;
pub mod diff_lines;
pub mod error;
pub mod forge;
pub mod git_bin;
pub mod git_cli;
pub mod history;
pub mod merge;
pub mod net;
pub mod refs;
pub mod repo;
pub mod search;
pub mod stage;
pub mod stash;
pub mod status;
pub mod tune;
pub mod types;
pub mod watch;

pub use error::GitError;
pub use repo::{Handle, Repo};
pub use types::{CommitId, Signature};
