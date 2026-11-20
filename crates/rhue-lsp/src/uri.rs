//! lsp-types moved to its own `Uri`, which doesn't know about files. These go
//! between it and paths through `url`, which does (drive letters and all).

use std::path::{Path, PathBuf};

use lsp_types::Uri;
use url::Url;

#[expect(
    clippy::result_unit_err,
    reason = "the same shape as url's own to_file_path"
)]
pub trait UriExt {
    /// The file a `file://` URI points at.
    fn to_file_path(&self) -> Result<PathBuf, ()>;
}

impl UriExt for Uri {
    fn to_file_path(&self) -> Result<PathBuf, ()> {
        Url::parse(self.as_str()).map_err(|_| ())?.to_file_path()
    }
}

/// A `file://` URI for an absolute path.
#[expect(
    clippy::result_unit_err,
    reason = "the same shape as url's own from_file_path"
)]
pub fn from_file_path(path: impl AsRef<Path>) -> Result<Uri, ()> {
    Url::from_file_path(path)?.as_str().parse().map_err(|_| ())
}
