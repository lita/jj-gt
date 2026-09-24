//! Git-style pathspecs for `jj-gt split --by-file`.
//!
//! Graphite hands `--by-file` patterns to git, so we mirror git's default
//! pathspec rules rather than jj's fileset language:
//!
//!   * patterns are relative to the current directory;
//!   * a pattern without wildcards matches that path and everything under it;
//!   * `*`, `?` and `[...]` are shell globs where `*` and `?` may also match
//!     `/` (fnmatch without FNM_PATHNAME), so `*.json` matches at any depth.

use std::path::Path;

use anyhow::{Context, Result};
use jj_lib::repo_path::RepoPath;
use jj_lib::ui_path::RepoPathUiConverter;

#[derive(Debug, Clone)]
pub struct Pathspec {
    /// The pattern as the user typed it, for messages.
    pub display: String,
    /// Workspace-relative pattern with `/` separators.
    pattern: String,
    wildcard: bool,
}

impl Pathspec {
    pub fn parse(root: &Path, cwd: &Path, input: &str) -> Result<Self> {
        let converter = RepoPathUiConverter::Fs {
            cwd: cwd.to_path_buf(),
            base: root.to_path_buf(),
        };
        let path = converter
            .parse_file_path(input.trim_end_matches('/'))
            .with_context(|| format!("invalid pathspec {input:?}"))?;
        let pattern = path.as_internal_file_string().to_owned();
        let wildcard = pattern.contains(['*', '?', '[']);
        Ok(Pathspec {
            display: input.to_owned(),
            pattern,
            wildcard,
        })
    }

    pub fn matches(&self, path: &RepoPath) -> bool {
        let text = path.as_internal_file_string();
        if self.pattern.is_empty() {
            return true; // "." — the whole workspace (or cwd at the root)
        }
        if self.wildcard {
            glob_match(self.pattern.as_bytes(), text.as_bytes())
        } else {
            text == self.pattern
                || text
                    .strip_prefix(self.pattern.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        }
    }
}

pub fn any_matches(specs: &[Pathspec], path: &RepoPath) -> bool {
    specs.iter().any(|spec| spec.matches(path))
}

/// fnmatch(3) without FNM_PATHNAME: `*` and `?` match `/` too.
fn glob_match(pat: &[u8], text: &[u8]) -> bool {
    let (mut p, mut t) = (0, 0);
    // Where to resume after the most recent `*` if the literal tail fails.
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        if p < pat.len() && pat[p] == b'*' {
            p += 1;
            star = Some((p, t));
            continue;
        }
        if let Some(next_p) = match_one(pat, p, text[t]) {
            p = next_p;
            t += 1;
            continue;
        }
        match star {
            Some((star_p, star_t)) => {
                t = star_t + 1;
                p = star_p;
                star = Some((star_p, t));
            }
            None => return false,
        }
    }
    while p < pat.len() && pat[p] == b'*' {
        p += 1;
    }
    p == pat.len()
}

/// If the pattern element at `p` matches byte `c`, return the index after it.
fn match_one(pat: &[u8], p: usize, c: u8) -> Option<usize> {
    match pat.get(p)? {
        b'?' => Some(p + 1),
        b'\\' if p + 1 < pat.len() => (pat[p + 1] == c).then_some(p + 2),
        b'[' => match match_bracket(pat, p, c) {
            Some((matched, next_p)) => matched.then_some(next_p),
            // Unterminated bracket: a literal `[`.
            None => (c == b'[').then_some(p + 1),
        },
        &lit => (lit == c).then_some(p + 1),
    }
}

/// Match `c` against the bracket expression starting at `pat[start] == '['`.
/// Returns (matched, index after the closing `]`), or None if unterminated.
fn match_bracket(pat: &[u8], start: usize, c: u8) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negate = matches!(pat.get(i), Some(b'!') | Some(b'^'));
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    loop {
        let &ch = pat.get(i)?;
        if ch == b']' && !first {
            return Some((matched != negate, i + 1));
        }
        first = false;
        let lo = if ch == b'\\' {
            i += 1;
            *pat.get(i)?
        } else {
            ch
        };
        // A range like `a-z` (a trailing `-` before `]` is literal).
        if pat.get(i + 1) == Some(&b'-') && pat.get(i + 2).is_some_and(|&n| n != b']') {
            let mut hi = pat[i + 2];
            i += 2;
            if hi == b'\\' {
                i += 1;
                hi = *pat.get(i)?;
            }
            if lo <= c && c <= hi {
                matched = true;
            }
        } else if lo == c {
            matched = true;
        }
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::glob_match;

    fn m(pat: &str, text: &str) -> bool {
        glob_match(pat.as_bytes(), text.as_bytes())
    }

    #[test]
    fn stars_cross_directory_separators() {
        assert!(m("*.json", "a.json"));
        assert!(m("*.json", "dir/sub/a.json"));
        assert!(!m("*.json", "a.jsonx"));
        assert!(m("src/*", "src/a/b.rs"));
        assert!(m("src/**", "src/a/b.rs"));
        assert!(!m("src/*", "lib/a.rs"));
        assert!(m("*", "anything/at/all"));
    }

    #[test]
    fn question_marks_and_brackets() {
        assert!(m("a?c", "abc"));
        assert!(m("a?c", "a/c"));
        assert!(m("[abc].txt", "b.txt"));
        assert!(!m("[abc].txt", "d.txt"));
        assert!(m("[!abc].txt", "d.txt"));
        assert!(m("[a-c].txt", "b.txt"));
        assert!(m("[]x].txt", "].txt"));
        assert!(m("a[", "a["));
        assert!(m("a\\*", "a*"));
        assert!(!m("a\\*", "ab"));
    }
}
