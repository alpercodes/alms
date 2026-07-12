//! Parse-sweep over every JS module under `static/ui/`.
//!
//! Motivated by issue #829: PR #822 shipped a broken module (literal backticks
//! inside a tagged-template `html` literal closed the template prematurely),
//! which blanked the boot screen in v0.2.2 because the browser refused the
//! module and `app.js` could not resolve its imports. Cargo CI was green —
//! nothing in the Rust test suite ever parsed the `static/ui/` tree.
//!
//! This test plugs that gap. For every `*.js` and `*.mjs` file under the
//! crate's `static/ui/` directory it runs the swc ECMAScript parser in
//! module mode at `EsVersion::latest()` and fails the suite if any file does
//! not parse. Errors are aggregated so a single run reports every broken
//! module, with `path:line:col` plus the parser's diagnostic message.
//!
//! What this catches: hand-typed module-syntax bugs that browsers would
//! reject — unbalanced template literals, mis-quoted strings, stray tokens,
//! invalid `import` / `export` shapes, etc.
//!
//! What this does NOT catch: module resolution (broken import paths still
//! parse fine), runtime errors, type errors, or behaviour. Those need a
//! different harness; see issue #829's "Out of scope" section.

use std::path::PathBuf;

use swc_common::input::StringInput;
use swc_common::sync::Lrc;
use swc_common::{FileName, SourceMap, Spanned};
use swc_ecma_ast::EsVersion;
use swc_ecma_parser::{EsSyntax, Parser, Syntax, lexer::Lexer};
use walkdir::WalkDir;

/// Root of the editable legacy UI source tree (relative to the crate manifest).
const UI_RELATIVE_ROOT: &str = "static/ui";
/// Root of the reproducible Vite output embedded by the gateway.
const UI_DIST_RELATIVE_ROOT: &str = "static/ui-dist";

/// Single parse failure, surfaced from `collect_parse_errors`.
struct ParseFailure {
    path: PathBuf,
    line: usize,
    col: usize,
    message: String,
}

impl std::fmt::Display for ParseFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{}:{}:{}: {}",
            self.path.display(),
            self.line,
            self.col,
            self.message
        )
    }
}

/// Walks the static UI tree and runs each `.js` / `.mjs` file through the
/// swc parser in module mode. Returns the list of files that did not parse,
/// plus the count of files inspected.
fn collect_parse_errors(ui_root: &std::path::Path) -> (Vec<ParseFailure>, usize) {
    let mut failures = Vec::new();
    let mut inspected = 0usize;

    for entry in WalkDir::new(ui_root)
        .follow_links(false)
        .into_iter()
        .filter_map(Result::ok)
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
        if !matches!(ext, "js" | "mjs") {
            continue;
        }

        inspected += 1;

        let source = match std::fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) => {
                failures.push(ParseFailure {
                    path: path.to_path_buf(),
                    line: 0,
                    col: 0,
                    message: format!("read error: {e}"),
                });
                continue;
            }
        };

        let cm: Lrc<SourceMap> = Default::default();
        let fm = cm.new_source_file(FileName::Real(path.to_path_buf()).into(), source);

        let lexer = Lexer::new(
            Syntax::Es(EsSyntax {
                jsx: false,
                ..Default::default()
            }),
            EsVersion::latest(),
            StringInput::from(&*fm),
            None,
        );
        let mut parser = Parser::new_from(lexer);

        match parser.parse_module() {
            Ok(_) => {
                // Even on a successful parse, swc may report recoverable
                // errors via `take_errors`. Treat any of those as failures
                // too — they'd be hard browser rejects in module mode.
                for err in parser.take_errors() {
                    let loc = cm.lookup_char_pos(err.span().lo);
                    failures.push(ParseFailure {
                        path: path.to_path_buf(),
                        line: loc.line,
                        col: loc.col_display + 1,
                        message: err.into_kind().msg().into_owned(),
                    });
                }
            }
            Err(err) => {
                let loc = cm.lookup_char_pos(err.span().lo);
                failures.push(ParseFailure {
                    path: path.to_path_buf(),
                    line: loc.line,
                    col: loc.col_display + 1,
                    message: err.into_kind().msg().into_owned(),
                });
            }
        }
    }

    (failures, inspected)
}

#[test]
fn static_ui_modules_parse_cleanly() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let ui_root = manifest_dir.join(UI_RELATIVE_ROOT);
    assert!(
        ui_root.is_dir(),
        "expected static UI root at {} (relative to the crate manifest)",
        ui_root.display()
    );

    let (failures, inspected) = collect_parse_errors(&ui_root);

    // Sanity check: if the inspector ever finds zero files something is wrong
    // with the walk (rename, gitignore, mis-mounted worktree, etc.) and we
    // would silently start passing this test on an empty tree.
    assert!(
        inspected > 0,
        "no .js / .mjs files found under {} — the walk is broken",
        ui_root.display()
    );

    if !failures.is_empty() {
        let mut report =
            String::from("static/ui parse-sweep failed — the following modules do not parse:\n");
        for f in &failures {
            report.push_str("  ");
            report.push_str(&f.to_string());
            report.push('\n');
        }
        report.push_str(&format!(
            "\n{} file(s) failed out of {} inspected.\n",
            failures.len(),
            inspected
        ));
        panic!("{report}");
    }
}

#[test]
fn built_ui_bundle_is_present_and_self_contained() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dist_root = manifest_dir.join(UI_DIST_RELATIVE_ROOT);
    let index_path = dist_root.join("index.html");
    let index = std::fs::read_to_string(&index_path)
        .unwrap_or_else(|e| panic!("expected built UI at {}: {e}", index_path.display()));

    assert!(
        !index.contains("esm.sh"),
        "production UI must bundle dependencies instead of relying on a CDN"
    );
    assert!(
        index.contains("/ui/assets/"),
        "built index must reference Vite assets under /ui/: {index}"
    );

    for token in index.split('"') {
        let Some(relative) = token.strip_prefix("/ui/") else {
            continue;
        };
        if !relative.starts_with("assets/") {
            continue;
        }
        assert!(
            dist_root.join(relative).is_file(),
            "built index references missing asset {relative}"
        );
    }
}

#[cfg(test)]
mod tests {
    //! Self-tests for the parse-sweep harness. These run against fixtures we
    //! materialise into a tempdir so the production tree stays clean.

    use super::*;
    use std::io::Write;

    fn write(dir: &std::path::Path, name: &str, body: &str) {
        let p = dir.join(name);
        if let Some(parent) = p.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let mut f = std::fs::File::create(p).unwrap();
        f.write_all(body.as_bytes()).unwrap();
    }

    #[test]
    fn clean_modules_pass() {
        let tmp = tempfile::tempdir().unwrap();
        write(
            tmp.path(),
            "a.js",
            "import x from './x.js';\nexport const y = 1;\n",
        );
        write(
            tmp.path(),
            "nested/b.mjs",
            "export default function () { return 42; }\n",
        );
        // Non-JS files must be ignored.
        write(tmp.path(), "ignored.txt", "this is not javascript {{{");
        write(
            tmp.path(),
            "page.html",
            "<!doctype html><html><body></body></html>",
        );

        let (failures, inspected) = collect_parse_errors(tmp.path());
        assert!(
            failures.is_empty(),
            "unexpected failures: {:?}",
            failures.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
        assert_eq!(inspected, 2, "expected exactly 2 JS/MJS files inspected");
    }

    #[test]
    fn unterminated_template_fails() {
        // This is the exact bug class from PR #828 / #822 — a template
        // literal that never closes. swc must reject it.
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "broken.js", "export const tpl = html`<div>oops");

        let (failures, _) = collect_parse_errors(tmp.path());
        assert!(
            !failures.is_empty(),
            "expected at least one failure for unterminated template"
        );
        assert!(
            failures.iter().all(|f| f.path.ends_with("broken.js")),
            "all reported failures should be on broken.js, got: {:?}",
            failures.iter().map(ToString::to_string).collect::<Vec<_>>()
        );
    }

    #[test]
    fn stray_token_fails() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "broken.js", "export const x = ;\n");

        let (failures, _) = collect_parse_errors(tmp.path());
        assert!(
            !failures.is_empty(),
            "expected at least one failure for stray-token input"
        );
    }

    #[test]
    fn bad_import_fails() {
        let tmp = tempfile::tempdir().unwrap();
        write(tmp.path(), "broken.js", "import from './x.js';\n");

        let (failures, _) = collect_parse_errors(tmp.path());
        assert!(
            !failures.is_empty(),
            "expected at least one failure for malformed import"
        );
    }
}
