//! Apply operations to an in-memory ELF and write it back atomically.

use std::path::Path;

use crate::cli::Operation;
use crate::error::{Error, Result};
use crate::ops::{Modifiers, Report, apply};
use crate::{layout, parser};

/// Apply `ops` to an ELF already held in `data`, streaming the result to a temp
/// file beside `path` and renaming it over the original, so a crash never
/// leaves a half-written binary and the original mode is preserved.
///
/// # Errors
/// Returns an error if the bytes cannot be parsed, patched, or replaced.
pub fn patch_data(
    path: &Path,
    data: &[u8],
    ops: &[Operation],
    mods: &Modifiers,
    page_size: Option<u64>,
) -> Result<()> {
    let io = |source| Error::Io {
        path: path.to_path_buf(),
        source,
    };
    let mut image = parser::parse(data)?;
    let mut report = Report::default();
    for op in ops {
        apply(&mut image, op, mods, &mut report)?;
    }

    let dir = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let mut tmp = tempfile::NamedTempFile::new_in(dir).map_err(io)?;
    {
        let mut out = std::io::BufWriter::new(tmp.as_file_mut());
        layout::finalize(&mut image, data, page_size, mods.debug, false, &mut out)?;
        std::io::Write::flush(&mut out).map_err(io)?;
    }
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = tmp.as_file().set_permissions(meta.permissions());
    }
    tmp.persist(path).map_err(|e| Error::Io {
        path: path.to_path_buf(),
        source: e.error,
    })?;
    Ok(())
}
