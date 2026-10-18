//! Minimal `*`/`?` glob matcher shared by CLI `--exclude` patterns and
//! test-input globs — deliberately not a full glob library (no `**`,
//! no character classes, no alternation).

/// Match `name` against a glob `pattern` where `*` spans any run of
/// characters, `?` matches exactly one character, and `[abc]` / `[!a-z]`
/// matches character classes. Operates directly on UTF-8 slices with zero
/// heap allocations.
pub fn glob_match(pattern: &str, name: &str) -> bool {
    let (mut pi, mut ni) = (0usize, 0usize);
    let (mut star_pi, mut star_ni) = (usize::MAX, 0usize);

    while ni < name.len() {
        let p_rest = &pattern[pi..];
        let n_ch = name[ni..].chars().next().unwrap();

        if p_rest.starts_with('*') {
            star_ni = ni;
            while pattern[pi..].starts_with('*') {
                pi += 1;
            }
            star_pi = pi - 1;
        } else if p_rest.starts_with('?') {
            pi += 1;
            ni += n_ch.len_utf8();
        } else if p_rest.starts_with('[') {
            if let Some((matched, class_len)) = match_char_class(p_rest, n_ch) {
                if matched {
                    pi += class_len;
                    ni += n_ch.len_utf8();
                    continue;
                }
            } else if n_ch == '[' {
                pi += 1;
                ni += 1;
                continue;
            }
            if star_pi != usize::MAX {
                pi = star_pi + 1;
                let skip_ch = name[star_ni..].chars().next().unwrap();
                star_ni += skip_ch.len_utf8();
                ni = star_ni;
            } else {
                return false;
            }
        } else if let Some(p_ch) = p_rest.chars().next() {
            if p_ch == n_ch {
                pi += p_ch.len_utf8();
                ni += n_ch.len_utf8();
            } else if star_pi != usize::MAX {
                pi = star_pi + 1;
                let skip_ch = name[star_ni..].chars().next().unwrap();
                star_ni += skip_ch.len_utf8();
                ni = star_ni;
            } else {
                return false;
            }
        } else if star_pi != usize::MAX {
            pi = star_pi + 1;
            let skip_ch = name[star_ni..].chars().next().unwrap();
            star_ni += skip_ch.len_utf8();
            ni = star_ni;
        } else {
            return false;
        }
    }

    while pattern[pi..].starts_with('*') {
        pi += 1;
    }
    pi == pattern.len()
}

fn match_char_class(p_rest: &str, target: char) -> Option<(bool, usize)> {
    let inner = p_rest.strip_prefix('[')?;
    let (negated, inner, prefix_len) =
        if let Some(rest) = inner.strip_prefix('!').or_else(|| inner.strip_prefix('^')) {
            (true, rest, 2usize)
        } else {
            (false, inner, 1usize)
        };
    let mut chars = inner.char_indices().peekable();
    let mut matched = false;
    let mut first = true;
    while let Some((idx, ch)) = chars.next() {
        if ch == ']' && !first {
            let total_len = prefix_len + idx + 1;
            return Some((if negated { !matched } else { matched }, total_len));
        }
        first = false;
        if let Some(&(_, '-')) = chars.peek() {
            let mut lookahead = chars.clone();
            lookahead.next(); // consume '-'
            if let Some((_, end_ch)) = lookahead.next() {
                if end_ch != ']' {
                    chars.next(); // '-'
                    chars.next(); // end_ch
                    if target >= ch && target <= end_ch {
                        matched = true;
                    }
                    continue;
                }
            }
        }
        if target == ch {
            matched = true;
        }
    }
    None
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
        assert!(glob_match("file[0-9].txt", "file3.txt"));
        assert!(!glob_match("file[!0-9].txt", "file3.txt"));
        assert!(glob_match("file[!0-9].txt", "fileA.txt"));
        assert!(glob_match("한글*.log", "한글테스트.log"));
    }
}
