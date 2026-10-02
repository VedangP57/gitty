/// A git CLI invocation that exited unsuccessfully. Downcast from `anyhow::Error` to inspect.
#[derive(Debug, thiserror::Error)]
#[error("git {args:?} failed (code {code:?}): {stderr}")]
pub struct GitError {
    pub args: Vec<String>,
    pub code: Option<i32>,
    pub stderr: String,
}
