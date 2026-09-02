// SPDX-License-Identifier: Apache-2.0

//! Path normalisation for sandbox-root containment checks (#1255).
//!
//! The shell engine reports its post-command working directory by running
//! `pwd`, and the *form* of that string depends on which shell is running:
//!
//! | form                 | produced by                        |
//! |----------------------|------------------------------------|
//! | `/c/dev/ws`          | Git-Bash / MSYS `pwd` on Windows   |
//! | `/cygdrive/c/dev/ws` | Cygwin-configured MSYS `pwd`       |
//! | `C:\dev\ws`          | plain Windows / the builtin engine |
//! | `\\?\C:\dev\ws`      | `std::fs::canonicalize` on Windows |
//!
//! All four can name the *same* directory. `Path::starts_with` already
//! matches whole components, so the defect was never prefix-vs-component:
//! it is that two spellings of one directory share no leading components at
//! all, so the sandbox concludes its own root is outside itself and a
//! legitimate `cd` is rejected and silently reverted. The fix is to bring
//! both sides to the same form *before* that comparison.
//!
//! Everything here is deliberately split into small pure transforms so the
//! Linux CI runner can exercise the Windows-shaped logic that it could never
//! reach through the platform-gated call sites.

use std::path::{Component, Path, PathBuf};

/// Whether path comparison should ignore ASCII case.
///
/// Windows filesystems are case-insensitive, so `C:\Dev\WS` and `c:\dev\ws`
/// are the same directory and a containment check that says otherwise is a
/// false negative. Unix filesystems are case-sensitive and must not fold.
///
/// Note this folds ASCII only. Windows also folds most non-ASCII case pairs,
/// but replicating the full Unicode table would need a dependency; ASCII
/// covers drive letters and realistic project paths, and the failure mode of
/// the gap is a *rejection* (fail-closed), never a spurious accept.
const FOLD_CASE: bool = cfg!(windows);

/// Convert an MSYS / Git-Bash absolute path to Windows form.
///
/// `/c/dev/ws` → `C:\dev\ws`, `/cygdrive/c/dev/ws` → `C:\dev\ws`, `/c` → `C:\`.
///
/// Returns `None` when the input is not a single-letter-drive MSYS path —
/// including `//server/share` (MSYS UNC) and `/usr/bin` (no drive letter),
/// both of which must be left alone.
///
/// This is compiled on every platform so its behaviour is unit-tested by CI,
/// but callers must only *apply* it under `cfg(windows)`: on Unix `/c/dev/ws`
/// is a perfectly ordinary absolute path and reinterpreting it as a drive
/// would be both wrong and a sandbox-escape hazard.
///
/// Consequently the only non-test caller lives behind `cfg(windows)`, so on
/// other targets this is dead code by construction. Keeping it compiled
/// there is deliberate: CI runs Linux, and this is where the Windows-shaped
/// logic actually gets test coverage. `allow` rather than `expect` because
/// on Windows the function *is* used and an expectation would go unfulfilled.
#[cfg_attr(not(windows), allow(dead_code))]
pub(crate) fn msys_to_windows(path: &Path) -> Option<PathBuf> {
    let raw = path.to_str()?;

    // MSYS always reports forward slashes; a backslash means this is already
    // some Windows form and must not be re-parsed as an MSYS path.
    if raw.contains('\\') {
        return None;
    }

    let rest = raw.strip_prefix('/')?;
    // `//server/share` is an MSYS UNC path, not a drive path.
    if rest.starts_with('/') {
        return None;
    }

    let rest = rest.strip_prefix("cygdrive/").unwrap_or(rest);

    let (drive, tail) = match rest.split_once('/') {
        Some((drive, tail)) => (drive, tail),
        None => (rest, ""),
    };

    // Exactly one ASCII letter, or it is a real directory named e.g. `usr`.
    let mut chars = drive.chars();
    let letter = chars.next()?;
    if chars.next().is_some() || !letter.is_ascii_alphabetic() {
        return None;
    }

    let mut out = String::with_capacity(raw.len() + 2);
    out.push(letter.to_ascii_uppercase());
    out.push_str(":\\");
    out.push_str(&tail.replace('/', "\\"));
    Some(PathBuf::from(out))
}

/// Strip the Windows extended-length (`\\?\`) prefix from a simple drive path.
///
/// `\\?\C:\dev\ws` → `C:\dev\ws`. Verbatim UNC paths (`\\?\UNC\server\share`)
/// are left untouched — there is no shorter equivalent spelling for them.
///
/// Implemented as a string transform rather than via [`Component::Prefix`] so
/// that it is testable on non-Windows hosts, where `\` is an ordinary
/// character and the prefix would never be parsed as a prefix component.
pub(crate) fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let Some(raw) = path.to_str() else {
        return path.to_path_buf();
    };
    let Some(rest) = raw.strip_prefix(r"\\?\") else {
        return path.to_path_buf();
    };
    // Only `X:` drive paths have an equivalent non-verbatim spelling.
    let bytes = rest.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        return PathBuf::from(rest);
    }
    path.to_path_buf()
}

/// A path normalised for containment comparison, plus whether that
/// normalisation actually happened.
///
/// The distinction is not cosmetic. [`normalise`] falls back to its raw
/// input when canonicalisation fails, so the containment test run against an
/// unresolved path supports only "not confirmed inside" — never "confirmed
/// outside". A caller that just needs a value to compare can ignore that and
/// use [`canonical_for_comparison`]; a caller that *reports the verdict to
/// the agent* must not state a cause the comparison never established
/// (#1262).
#[derive(Debug, Clone)]
pub(crate) struct Normalised {
    /// The path to compare with: canonical when `resolved`, the raw input
    /// otherwise.
    pub(crate) path: PathBuf,

    /// Whether [`std::fs::canonicalize`] succeeded on the input.
    pub(crate) resolved: bool,
}

/// Canonicalise `path` into the form used for containment comparison,
/// reporting whether the canonicalisation actually succeeded.
///
/// Resolves symlinks and `..`, then drops the `\\?\` prefix that
/// [`std::fs::canonicalize`] adds on Windows. Falls back to the input
/// unchanged when the path cannot be resolved (it does not exist, or the
/// volume is unreadable) — the caller still gets a well-defined value to
/// compare, and an unresolvable path simply will not match a resolvable root
/// — but `resolved: false` records that fallback rather than hiding it.
pub(crate) fn normalise(path: &Path) -> Normalised {
    match std::fs::canonicalize(path) {
        Ok(canonical) => Normalised {
            path: strip_verbatim_prefix(&canonical),
            resolved: true,
        },
        Err(_) => Normalised {
            path: path.to_path_buf(),
            resolved: false,
        },
    }
}

/// [`normalise`] for callers that only need the comparable path.
pub(crate) fn canonical_for_comparison(path: &Path) -> PathBuf {
    normalise(path).path
}

/// Resolve a working directory string reported by the shell engine into a
/// path this process can actually use with `Command::current_dir`.
///
/// On Windows an MSYS-form path is reinterpreted as a drive path, but *only*
/// when that reinterpretation names a real directory. Gating on existence
/// keeps the rewrite conservative: if the literal path already resolves, or
/// the drive-form does not exist, nothing is rewritten.
///
/// This matters beyond the containment check — storing the raw `/c/dev/ws`
/// string and handing it to `current_dir` would resolve it against the
/// current drive (`C:\c\dev\ws`) and break the next command outright.
pub(crate) fn resolve_reported(reported: &Path) -> Normalised {
    #[cfg(windows)]
    {
        if !reported.is_dir()
            && let Some(converted) = msys_to_windows(reported)
            && converted.is_dir()
        {
            return normalise(&converted);
        }
    }
    normalise(reported)
}

/// [`resolve_reported`] for callers that only need the resolved path.
pub(crate) fn resolve_reported_cwd(reported: &Path) -> PathBuf {
    resolve_reported(reported).path
}

/// Compare two path components under the platform's case-sensitivity rules.
fn components_eq(a: &Component<'_>, b: &Component<'_>, fold_case: bool) -> bool {
    if fold_case {
        a.as_os_str().eq_ignore_ascii_case(b.as_os_str())
    } else {
        a == b
    }
}

/// Does `candidate` still contain an unresolved `..`?
///
/// A successfully canonicalised path never does — `std::fs::canonicalize`
/// resolves `..` away. Its presence therefore means normalisation silently
/// did *not* happen: [`canonical_for_comparison`] fell back to the raw input
/// because the path could not be resolved.
///
/// That matters because the containment test below compares only the
/// *leading* components, to which `..` is just another component:
/// `<root>/../../Windows` matches `<root>` on every one of them and would
/// otherwise be accepted as contained.
///
/// Candidate side only. A `..` on the **root** side can never produce a
/// false accept: it would have to be matched by a `..` at the same position
/// in the candidate, which a canonical candidate does not have, so an
/// unresolved root can only ever reject. Nor is `.` checked — `components()`
/// normalises an interior `.` away, and a leading one only occurs in a
/// relative path, which fails the prefix component match regardless.
fn has_unresolved_parent_dir(candidate: &Path) -> bool {
    candidate
        .components()
        .any(|component| matches!(component, Component::ParentDir))
}

/// Component-wise containment test: is `candidate` the root itself or nested
/// inside it?
///
/// Compares whole path components rather than string prefixes, so
/// `/srv/workspace-evil` is correctly reported as *outside* `/srv/workspace`.
/// (`Path::starts_with` has the same property; the reason this exists is the
/// normalisation that has to happen before either can be trusted.)
fn is_within_impl(root: &Path, candidate: &Path, fold_case: bool) -> bool {
    // Fail closed on a candidate that was never resolved: the leading
    // component match below cannot be trusted for it.
    if has_unresolved_parent_dir(candidate) {
        return false;
    }

    let root_components: Vec<_> = root.components().collect();
    let candidate_components: Vec<_> = candidate.components().collect();

    if candidate_components.len() < root_components.len() {
        return false;
    }
    root_components
        .iter()
        .zip(candidate_components.iter())
        .all(|(r, c)| components_eq(r, c, fold_case))
}

/// Is `candidate` contained within `root`? Both are expected to have been
/// normalised via [`canonical_for_comparison`] / [`resolve_reported_cwd`].
///
/// That expectation is enforced rather than trusted for the one case where
/// trusting it is exploitable: a candidate still carrying `..` is rejected
/// outright. Both of those functions fall back to their raw input when
/// `std::fs::canonicalize` fails, so "already normalised" is precisely the
/// precondition that can silently not hold.
pub(crate) fn is_within(root: &Path, candidate: &Path) -> bool {
    is_within_impl(root, candidate, FOLD_CASE)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- msys_to_windows -------------------------------------------------
    // Pure string transform, so these assertions hold on every platform and
    // give the Linux CI runner real coverage of the Windows-shaped logic.

    #[test]
    fn msys_drive_path_becomes_windows_path() {
        assert_eq!(
            msys_to_windows(Path::new("/c/dev/alms-test-workspace")),
            Some(PathBuf::from(r"C:\dev\alms-test-workspace"))
        );
    }

    #[test]
    fn msys_bare_drive_becomes_drive_root() {
        assert_eq!(
            msys_to_windows(Path::new("/c")),
            Some(PathBuf::from(r"C:\"))
        );
    }

    #[test]
    fn msys_drive_letter_is_upper_cased() {
        assert_eq!(
            msys_to_windows(Path::new("/d/Projects")),
            Some(PathBuf::from(r"D:\Projects"))
        );
    }

    #[test]
    fn cygdrive_prefix_is_understood() {
        assert_eq!(
            msys_to_windows(Path::new("/cygdrive/c/dev/ws")),
            Some(PathBuf::from(r"C:\dev\ws"))
        );
    }

    #[test]
    fn nested_msys_path_keeps_every_segment() {
        assert_eq!(
            msys_to_windows(Path::new(
                "/c/dev/alms-test-workspace/.alms/tool-output/4e24735e"
            )),
            Some(PathBuf::from(
                r"C:\dev\alms-test-workspace\.alms\tool-output\4e24735e"
            ))
        );
    }

    #[test]
    fn non_drive_absolute_paths_are_not_msys() {
        // A multi-character first segment is a directory name, not a drive.
        assert_eq!(msys_to_windows(Path::new("/usr/bin")), None);
        assert_eq!(msys_to_windows(Path::new("/etc")), None);
        // Digits are not drive letters.
        assert_eq!(msys_to_windows(Path::new("/1/foo")), None);
    }

    #[test]
    fn msys_unc_and_relative_paths_are_rejected() {
        assert_eq!(msys_to_windows(Path::new("//server/share")), None);
        assert_eq!(msys_to_windows(Path::new("relative/path")), None);
    }

    #[test]
    fn windows_form_input_is_not_reparsed_as_msys() {
        assert_eq!(msys_to_windows(Path::new(r"C:\dev\ws")), None);
        assert_eq!(msys_to_windows(Path::new(r"\\?\C:\dev\ws")), None);
    }

    // -- strip_verbatim_prefix -------------------------------------------

    #[test]
    fn verbatim_drive_prefix_is_stripped() {
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\C:\dev\ws")),
            PathBuf::from(r"C:\dev\ws")
        );
    }

    #[test]
    fn verbatim_unc_prefix_is_preserved() {
        let unc = Path::new(r"\\?\UNC\server\share");
        assert_eq!(strip_verbatim_prefix(unc), unc.to_path_buf());
    }

    #[test]
    fn plain_paths_pass_through_unchanged() {
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"C:\dev\ws")),
            PathBuf::from(r"C:\dev\ws")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new("/srv/workspace")),
            PathBuf::from("/srv/workspace")
        );
    }

    // -- is_within --------------------------------------------------------
    // Exercised with an explicit case flag so both the Windows
    // (case-folding) and Unix (case-sensitive) policies are covered
    // regardless of which platform the tests run on.

    #[test]
    fn root_contains_itself() {
        let root = Path::new("/srv/workspace");
        assert!(is_within_impl(root, root, false));
        assert!(is_within_impl(root, root, true));
    }

    #[test]
    fn nested_path_is_contained() {
        assert!(is_within_impl(
            Path::new("/srv/workspace"),
            Path::new("/srv/workspace/.alms/tool-output/abc"),
            false
        ));
    }

    #[test]
    fn sibling_escape_is_rejected() {
        assert!(!is_within_impl(
            Path::new("/srv/workspace"),
            Path::new("/etc"),
            false
        ));
        assert!(!is_within_impl(
            Path::new("/srv/workspace"),
            Path::new("/srv"),
            false
        ));
    }

    #[test]
    fn string_prefix_sibling_is_not_containment() {
        // The bug a naive `str::starts_with` would introduce: this shares a
        // string prefix with the root but is a different directory.
        assert!(!is_within_impl(
            Path::new("/srv/workspace"),
            Path::new("/srv/workspace-evil"),
            false
        ));
    }

    #[test]
    fn unresolved_parent_dir_escape_is_rejected() {
        // Reachable only when `canonicalize` failed and
        // `canonical_for_comparison` handed back the raw path. Verified
        // non-vacuous: with the `has_unresolved_parent_dir` guard removed
        // this returns TRUE, because `/`, `srv` and `workspace` match the
        // root exactly and `..` is just another component to the zip.
        assert!(!is_within_impl(
            Path::new("/srv/workspace"),
            Path::new("/srv/workspace/../../etc"),
            false
        ));
        // Rejected even when the `..` lands back inside: the point is that
        // the path was never resolved, not where it happens to end up.
        assert!(!is_within_impl(
            Path::new("/srv/workspace"),
            Path::new("/srv/workspace/sub/../other"),
            false
        ));
    }

    #[test]
    fn parent_dir_guard_does_not_reject_ordinary_paths() {
        // `..` as a *substring* of a name is not traversal, and
        // `components()` normalises an interior `.` away.
        assert!(is_within_impl(
            Path::new("/srv/workspace"),
            Path::new("/srv/workspace/.alms/..hidden/./tool-output"),
            false
        ));
    }

    #[test]
    fn case_folding_matches_only_when_enabled() {
        let root = Path::new("/srv/workspace");
        let differently_cased = Path::new("/srv/WorkSpace/sub");
        assert!(is_within_impl(root, differently_cased, true));
        assert!(!is_within_impl(root, differently_cased, false));
    }

    #[test]
    fn case_folding_still_rejects_a_genuine_escape() {
        assert!(!is_within_impl(
            Path::new("/srv/workspace"),
            Path::new("/windows/system32"),
            true
        ));
    }

    #[test]
    fn fold_case_policy_tracks_the_platform() {
        assert_eq!(FOLD_CASE, cfg!(windows));
    }
}
