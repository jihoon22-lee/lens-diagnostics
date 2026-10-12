use std::path::Path;

use lens_abi::{diff_reports, inspect_elf, DiffReport, ElfReport, InputStatus};
use lens_core::{Compatibility, LensError, Result};

use crate::{Report, Severity};

/// Non-ELF input is a usage error on the CLI (exit 2): printing a
/// `status: non-elf` report would look like a successful inspection of
/// something we never inspected. The library report stays honest for
/// MCP and other programmatic callers (`strict: false`).
fn reject_non_elf(path: &Path, report: &ElfReport) -> Result<()> {
    if report.status == InputStatus::NonElf {
        return Err(LensError::Usage {
            message: format!("{} is not an ELF file", path.display()),
        });
    }
    Ok(())
}

/// Inspect one ELF binary. `strict` rejects non-ELF input as a usage
/// error; non-strict callers get the honest `status: non-elf` report.
pub fn inspect(path: &Path, strict: bool) -> Result<Report<ElfReport>> {
    let bytes = std::fs::read(path).map_err(|e| LensError::Io {
        path: path.to_path_buf(),
        source: e,
    })?;
    let report = inspect_elf(path, &bytes);
    if strict {
        reject_non_elf(path, &report)?;
    }
    Ok(Report::new(report))
}

/// Diff two ELF binaries. An `incompatible` or `uncertain` verdict is a
/// Fail finding (uncertain means human review is required).
pub fn diff(baseline: &Path, candidate: &Path, strict: bool) -> Result<Report<DiffReport>> {
    let bytes_base = std::fs::read(baseline).map_err(|e| LensError::Io {
        path: baseline.to_path_buf(),
        source: e,
    })?;
    let bytes_cand = std::fs::read(candidate).map_err(|e| LensError::Io {
        path: candidate.to_path_buf(),
        source: e,
    })?;
    let report_base = inspect_elf(baseline, &bytes_base);
    let report_cand = inspect_elf(candidate, &bytes_cand);
    if strict {
        reject_non_elf(baseline, &report_base)?;
        reject_non_elf(candidate, &report_cand)?;
    }
    let diff = diff_reports(&report_base, &report_cand);
    let mut report = Report::new(diff);
    match report.data.compatibility {
        Compatibility::Incompatible => report.push(
            Severity::Fail,
            "abi.incompatible",
            format!(
                "{} -> {}: incompatible ABI change",
                baseline.display(),
                candidate.display()
            ),
        ),
        Compatibility::Uncertain => report.push(
            Severity::Fail,
            "abi.uncertain",
            format!(
                "{} -> {}: ABI compatibility uncertain — review required",
                baseline.display(),
                candidate.display()
            ),
        ),
        Compatibility::Compatible => {}
    }
    Ok(report)
}
