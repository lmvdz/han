//! A small glob matcher for exclusion lists and `.gitignore` lines, over relative `/`-separated paths.
//!
//! * A pattern without `/` (other than a trailing one) matches the last segment of the path, or any
//!   directory segment (so `node_modules` excludes everything under it).
//! * A pattern with `/` matches from the root; a leading `/` is dropped.
//! * `*` matches within a segment, `?` one character, `**` any number of whole segments, and a trailing
//!   `/**` everything under a directory.

/// Whether `pattern` matches `path` (relative, `/`-separated, no leading `/`).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }
    let dir_only = pattern.ends_with('/');
    let pattern = pattern.trim_end_matches('/');
    let anchored = pattern.contains('/');
    let pattern = pattern.trim_start_matches('/');
    let segs: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    if segs.is_empty() {
        return false;
    }
    if !anchored {
        // Any segment; a directory segment covers everything under it.
        return segs.iter().enumerate().any(|(i, s)| {
            let is_dir = i + 1 < segs.len();
            (!dir_only || is_dir) && seg_match(pattern.as_bytes(), s.as_bytes())
        });
    }
    let pats: Vec<&str> = pattern.split('/').collect();
    // A pattern matching a leading directory covers what is under it.
    (1..=segs.len()).any(|n| {
        let is_dir = n < segs.len();
        (!dir_only || is_dir) && path_match(&pats, &segs[..n])
    })
}

fn path_match(pats: &[&str], segs: &[&str]) -> bool {
    match pats.split_first() {
        None => segs.is_empty(),
        Some((&"**", rest)) => (0..=segs.len()).any(|i| path_match(rest, &segs[i..])),
        Some((p, rest)) => match segs.split_first() {
            Some((s, srest)) => seg_match(p.as_bytes(), s.as_bytes()) && path_match(rest, srest),
            None => false,
        },
    }
}

fn seg_match(p: &[u8], s: &[u8]) -> bool {
    match p.split_first() {
        None => s.is_empty(),
        Some((b'*', rest)) => (0..=s.len()).any(|i| seg_match(rest, &s[i..])),
        Some((b'?', rest)) => !s.is_empty() && seg_match(rest, &s[1..]),
        Some((c, rest)) => s.first() == Some(c) && seg_match(rest, &s[1..]),
    }
}

/// Patterns read from a `.gitignore` text: comments and negations are skipped (a crude reading; the host
/// passes a `git check-ignore`-backed predicate when it has git).
pub fn gitignore_patterns(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with('!'))
        .map(str::to_owned)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    #[test]
    fn patterns() {
        assert!(glob_match(".env", ".env"));
        assert!(glob_match(".env", "app/.env"));
        assert!(!glob_match(".env", ".envrc"));
        assert!(glob_match(".env.*", "a/.env.local"));
        assert!(glob_match("*.pem", "certs/server.pem"));
        assert!(glob_match(".ssh/**", ".ssh/id_rsa"));
        assert!(!glob_match(".ssh/**", "x/.ssh/id_rsa"));
        assert!(glob_match("**/credentials*", "a/b/credentials.json"));
        assert!(glob_match("**/credentials*", "credentials"));
        assert!(glob_match("target", "target/debug/x"));
        assert!(glob_match("target/", "target/debug/x"));
        assert!(!glob_match("target/", "target"));
        assert!(glob_match("/build", "build/out.o"));
        assert!(!glob_match("/build", "src/build"));
        assert!(glob_match("src/*.rs", "src/main.rs"));
        assert!(!glob_match("src/*.rs", "src/a/main.rs"));
        assert!(glob_match("id_rsa*", "home/id_rsa.pub"));
        assert!(!glob_match("*.key", "keyboard.rs"));
    }
}
