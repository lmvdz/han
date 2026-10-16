//! Every `Command::new` in non-test production code goes through `han-proc`.
//!
//! On Windows a console program started by a process with no console opens a window (see the crate docs), and
//! every HAN background service is such a process. A new spawn site that forgets `no_window` (or
//! `detached_service`, for a service that must own no console) brings the windows back, so this source scan fails
//! until the site calls the helper in the same function, or is listed in [`ALLOWED`] with the reason.

use std::fs;
use std::path::{Path, PathBuf};

/// `(file, text of the Command::new line, why it may show a window or must set its own flags)`.
const ALLOWED: &[(&str, &str, &str)] = &[];

const ROOTS: &[&str] = &["crates", "apps", "integrations"];

fn workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if p.is_dir() {
            if !matches!(name.as_str(), "target" | "tests" | "node_modules" | ".git") {
                sources(&p, out);
            }
        } else if name.ends_with(".rs") && name != "tests.rs" {
            out.push(p);
        }
    }
}

/// Offsets of `Command::new(` that start a path (`ShimCommand::new` is not a process).
fn spawn_sites(src: &str) -> Vec<usize> {
    src.match_indices("Command::new(")
        .map(|(i, _)| i)
        .filter(|&i| {
            !src[..i]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        })
        .collect()
}

#[test]
fn every_spawn_site_goes_through_the_helper() {
    let mut files = Vec::new();
    for root in ROOTS {
        // `crates/*/src`, `apps/*/src`, `integrations/*/src`
        let Ok(rd) = fs::read_dir(workspace().join(root)) else {
            continue;
        };
        for member in rd.flatten() {
            sources(&member.path().join("src"), &mut files);
        }
    }
    assert!(
        files.len() > 50,
        "the scan found only {} files",
        files.len()
    );
    let mut sites = 0;
    let mut missing = Vec::new();
    for f in &files {
        let full = fs::read_to_string(f).unwrap().replace("\r\n", "\n");
        // Unit tests sit at the end of a file, after the production code.
        let src = full.split("#[cfg(test)]").next().unwrap();
        let rel = f
            .strip_prefix(workspace())
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        for at in spawn_sites(src) {
            let line_start = src[..at].rfind('\n').map_or(0, |n| n + 1);
            let line = src[line_start..].lines().next().unwrap().trim();
            if line.starts_with("//") {
                continue;
            }
            sites += 1;
            // The helper call is anywhere in the same function, before or after the `Command::new`.
            let fn_start = src[..at]
                .rmatch_indices("\nfn ")
                .chain(src[..at].rmatch_indices("fn "))
                .map(|(i, _)| i)
                .max()
                .unwrap_or(0);
            let fn_end = src[at..]
                .find("\nfn ")
                .or_else(|| src[at..].find("\n    fn "))
                .or_else(|| src[at..].find("\n    pub fn "))
                .map_or(src.len(), |n| at + n);
            let body = &src[fn_start..fn_end];
            let wrapped = body.contains(".no_window(") || body.contains(".detached_service(");
            let allowed = ALLOWED
                .iter()
                .any(|(file, text, _)| *file == rel && line.contains(text));
            if !wrapped && !allowed {
                let n = src[..at].matches('\n').count() + 1;
                missing.push(format!("{rel}:{n}: {line}"));
            }
        }
    }
    assert!(sites > 30, "the scan found only {sites} spawn sites");
    assert!(
        missing.is_empty(),
        "these spawn sites do not call `no_window()` (use `han_proc::NoWindow`, see crates/han-proc), or \
         add them to ALLOWED with the reason:\n{}",
        missing.join("\n")
    );
}
