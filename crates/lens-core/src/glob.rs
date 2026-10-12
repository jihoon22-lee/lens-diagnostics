//! Minimal `*`/`?` glob matcher shared by CLI `--exclude` patterns and
//! test-input globs — deliberately not a full glob library (no `**`,
//! no character classes, no alternation).

/// Match `name` against a glob `pattern` where `*` spans any run of
/// characters and `?` matches exactly one. Whole-string match — there is
/// no implicit anchoring issue: `*.log` matches `app.log`, not `app.logs`.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star, mut star_next) = (usize::MAX, 0usize);
    while ni < n.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == n[ni]) {
            pi += 1;
            ni += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            star_next = ni;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            star_next += 1;
            ni = star_next;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_match_semantics() {
        assert!(glob_match("*.log", "app.log"));
        assert!(!glob_match("*.log", "app.logs"));
        assert!(glob_match("a?c", "abc"));
        assert!(!glob_match("a?c", "ac"));
        assert!(glob_match("*", "anything"));
        assert!(glob_match(".git", ".git"));
        // No magic without metacharacters: exact equality only.
        assert!(!glob_match(".git", ".github"));
        assert!(!glob_match(".git", ".gitignore"));
        assert!(glob_match("a/*/c", "a/b/c"));
        // `*` is a plain character-level matcher — it spans `/` too,
        // so this is NOT path-segment-aware globbing.
        assert!(glob_match("a/*/c", "a/b/d/c"));
        assert!(glob_match("*test*", "contest"));
    }
}
