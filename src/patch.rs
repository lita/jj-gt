//! File changes between two trees, chopped into `git add -p`-style hunks that
//! can be selected one by one and re-applied onto a base tree.
//!
//! jj-lib gives us the pieces: `MergedTree::diff_stream` for the changed
//! paths, `jj_lib::diff::ContentDiff::by_line` for the line-level hunks, and
//! `Store::write_file` + `MergedTreeBuilder` to write the partial result back
//! as a real tree. Nothing here touches the working copy.

use std::ops::Range;

use anyhow::{Result, anyhow};
use futures::AsyncReadExt as _;
use futures::StreamExt as _;
use jj_lib::backend::{CopyId, FileId, MergedTreeValue, TreeValue};
use jj_lib::diff::{ContentDiff, DiffHunkKind};
use jj_lib::matchers::EverythingMatcher;
use jj_lib::merge::{Diff, Merge};
use jj_lib::merged_tree::MergedTree;
use jj_lib::repo_path::{RepoPath, RepoPathBuf};
use jj_lib::store::Store;
use owo_colors::OwoColorize;
use pollster::FutureExt as _;

/// Lines of unchanged context shown around a hunk (git's default).
const CONTEXT: usize = 3;

/// Every path whose value differs between `base` and `tree`, in tree order.
pub fn tree_changes(
    base: &MergedTree,
    tree: &MergedTree,
) -> Result<Vec<(RepoPathBuf, Diff<MergedTreeValue>)>> {
    let mut stream = base.diff_stream(tree, &EverythingMatcher);
    let mut changes = Vec::new();
    while let Some(entry) = stream.next().block_on() {
        changes.push((entry.path, entry.values?));
    }
    Ok(changes)
}

/// One changed path and its selectable hunks.
pub struct FileChange {
    pub path: RepoPathBuf,
    pub before: MergedTreeValue,
    pub after: MergedTreeValue,
    body: Body,
}

enum Body {
    /// Text on both sides (an absent side reads as empty): line hunks.
    Text { segments: Vec<Segment>, hunks: Vec<Hunk> },
    /// Taken or left as a whole (binary, symlink, conflict, mode-only, ...).
    Whole { summary: String },
}

enum Segment {
    Same(Vec<u8>),
    Changed { before: Vec<u8>, after: Vec<u8> },
}

enum Hunk {
    /// The executable bit flipped alongside a content change.
    Mode { executable: bool },
    Lines {
        segments: Range<usize>,
        old_start: usize,
        old_count: usize,
        new_start: usize,
        new_count: usize,
    },
}

/// Simplified view of a resolved tree value for the cases split can handle.
enum Side<'a> {
    Absent,
    File {
        id: &'a FileId,
        executable: bool,
        copy_id: &'a CopyId,
    },
    Other(&'static str),
    Conflict,
}

fn side(value: &MergedTreeValue) -> Side<'_> {
    match value.as_resolved() {
        None => Side::Conflict,
        Some(None) => Side::Absent,
        Some(Some(TreeValue::File {
            id,
            executable,
            copy_id,
        })) => Side::File {
            id,
            executable: *executable,
            copy_id,
        },
        Some(Some(TreeValue::Symlink(_))) => Side::Other("symlink"),
        Some(Some(TreeValue::Tree(_))) => Side::Other("directory"),
        Some(Some(TreeValue::GitSubmodule(_))) => Side::Other("submodule"),
    }
}

fn read_file(store: &Store, path: &RepoPath, id: &FileId) -> Result<Vec<u8>> {
    let mut reader = store.read_file(path, id).block_on()?;
    let mut contents = Vec::new();
    reader.read_to_end(&mut contents).block_on()?;
    Ok(contents)
}

fn is_binary(contents: &[u8]) -> bool {
    contents[..contents.len().min(8000)].contains(&0)
}

fn count_lines(text: &[u8]) -> usize {
    text.split_inclusive(|&b| b == b'\n').count()
}

impl FileChange {
    pub fn load(store: &Store, path: RepoPathBuf, values: Diff<MergedTreeValue>) -> Result<Self> {
        let Diff { before, after } = values;
        let body = Self::body(store, &path, &before, &after)?;
        Ok(FileChange {
            path,
            before,
            after,
            body,
        })
    }

    fn body(
        store: &Store,
        path: &RepoPath,
        before: &MergedTreeValue,
        after: &MergedTreeValue,
    ) -> Result<Body> {
        let whole = |summary: &str| Body::Whole {
            summary: summary.to_owned(),
        };
        let (old, new) = match (side(before), side(after)) {
            (Side::Conflict, _) | (_, Side::Conflict) => {
                return Ok(whole("conflicted — taken as a whole"));
            }
            (Side::Other(kind), _) | (_, Side::Other(kind)) => {
                return Ok(whole(&format!("{kind} changed")));
            }
            (
                Side::File {
                    id: old_id,
                    executable: old_exec,
                    ..
                },
                Side::File {
                    id: new_id,
                    executable: new_exec,
                    ..
                },
            ) if old_id == new_id => {
                let mode = if new_exec { "+x" } else { "-x" };
                debug_assert_ne!(old_exec, new_exec);
                return Ok(whole(&format!("mode change ({mode})")));
            }
            (old, new) => (old, new),
        };
        let load = |side: &Side| -> Result<(Vec<u8>, Option<bool>)> {
            match side {
                Side::File { id, executable, .. } => {
                    Ok((read_file(store, path, id)?, Some(*executable)))
                }
                _ => Ok((Vec::new(), None)),
            }
        };
        let (old_text, old_exec) = load(&old)?;
        let (new_text, new_exec) = load(&new)?;
        if is_binary(&old_text) || is_binary(&new_text) {
            let what = match (old_exec, new_exec) {
                (None, _) => "binary file added",
                (_, None) => "binary file deleted",
                _ => "binary file changed",
            };
            return Ok(whole(what));
        }
        let mode_hunk = match (old_exec, new_exec) {
            (Some(o), Some(n)) if o != n => Some(Hunk::Mode { executable: n }),
            _ => None,
        };

        let diff = ContentDiff::by_line([&old_text, &new_text]);
        let segments: Vec<Segment> = diff
            .hunks()
            .map(|hunk| match hunk.kind {
                DiffHunkKind::Matching => Segment::Same(hunk.contents[0].to_vec()),
                DiffHunkKind::Different => Segment::Changed {
                    before: hunk.contents[0].to_vec(),
                    after: hunk.contents[1].to_vec(),
                },
            })
            .collect();
        let mut hunks: Vec<Hunk> = mode_hunk.into_iter().collect();
        hunks.extend(group_hunks(&segments));
        Ok(Body::Text { segments, hunks })
    }

    /// How many yes/no questions this file poses.
    pub fn hunk_count(&self) -> usize {
        match &self.body {
            Body::Text { hunks, .. } => hunks.len(),
            Body::Whole { .. } => 1,
        }
    }

    /// Total added/removed line counts, for summaries.
    pub fn line_stats(&self) -> (usize, usize) {
        match &self.body {
            Body::Text { segments, .. } => segments.iter().fold((0, 0), |(a, r), seg| match seg {
                Segment::Same(_) => (a, r),
                Segment::Changed { before, after } => {
                    (a + count_lines(after), r + count_lines(before))
                }
            }),
            Body::Whole { .. } => (0, 0),
        }
    }

    /// What happened to the path, in git's vocabulary.
    pub fn status(&self) -> &'static str {
        match (side(&self.before), side(&self.after)) {
            (Side::Absent, _) => "new file",
            (_, Side::Absent) => "deleted",
            _ => "modified",
        }
    }

    pub fn render_header(&self) -> String {
        let (added, removed) = self.line_stats();
        let stats = match &self.body {
            Body::Text { .. } => format!(
                "{} {}",
                format!("+{added}").green(),
                format!("-{removed}").red()
            ),
            Body::Whole { summary } => summary.dimmed().to_string(),
        };
        format!(
            "{} {}  {}  {stats}",
            "━━".dimmed(),
            self.path.as_internal_file_string().bold(),
            format!("({})", self.status()).dimmed(),
        )
    }

    pub fn render_hunk(&self, index: usize) -> String {
        match &self.body {
            Body::Whole { summary } => format!("  {summary}\n"),
            Body::Text { segments, hunks } => match &hunks[index] {
                Hunk::Mode { executable } => format!(
                    "  {}\n",
                    if *executable {
                        "mode change: make executable (+x)"
                    } else {
                        "mode change: clear executable bit (-x)"
                    }
                ),
                Hunk::Lines {
                    segments: range,
                    old_start,
                    old_count,
                    new_start,
                    new_count,
                } => {
                    let mut out = format!(
                        "{}\n",
                        format!("@@ -{old_start},{old_count} +{new_start},{new_count} @@").cyan()
                    );
                    if let Some(Segment::Same(text)) = range.start.checked_sub(1).map(|i| &segments[i]) {
                        for line in tail_lines(text, CONTEXT) {
                            push_line(&mut out, ' ', line);
                        }
                    }
                    for segment in &segments[range.clone()] {
                        match segment {
                            Segment::Same(text) => {
                                for line in text.split_inclusive(|&b| b == b'\n') {
                                    push_line(&mut out, ' ', line);
                                }
                            }
                            Segment::Changed { before, after } => {
                                for line in before.split_inclusive(|&b| b == b'\n') {
                                    push_line(&mut out, '-', line);
                                }
                                for line in after.split_inclusive(|&b| b == b'\n') {
                                    push_line(&mut out, '+', line);
                                }
                            }
                        }
                    }
                    if let Some(Segment::Same(text)) = segments.get(range.end) {
                        for line in text.split_inclusive(|&b| b == b'\n').take(CONTEXT) {
                            push_line(&mut out, ' ', line);
                        }
                    }
                    out
                }
            },
        }
    }

    /// The tree value to record given which hunks were picked. `None` means
    /// nothing was picked and the path should keep its base-tree value.
    pub fn apply(&self, store: &Store, selected: &[bool]) -> Result<Option<MergedTreeValue>> {
        assert_eq!(selected.len(), self.hunk_count());
        if selected.iter().all(|&s| !s) {
            return Ok(None);
        }
        if selected.iter().all(|&s| s) {
            return Ok(Some(self.after.clone()));
        }
        let Body::Text { segments, hunks } = &self.body else {
            unreachable!("a whole-file change has exactly one hunk");
        };
        // Partial pick: rebuild the file from the base content, swapping in
        // the chosen hunks, and let the store hash it.
        let mut executable = match side(&self.before) {
            Side::File { executable, .. } => executable,
            _ => false,
        };
        let mut take_after = vec![false; segments.len()];
        for (hunk, &picked) in hunks.iter().zip(selected) {
            match hunk {
                Hunk::Mode { executable: exec } if picked => executable = *exec,
                Hunk::Mode { .. } => {}
                Hunk::Lines { segments: range, .. } => {
                    take_after[range.clone()].fill(picked);
                }
            }
        }
        let mut contents = Vec::new();
        for (segment, &take) in segments.iter().zip(&take_after) {
            match segment {
                Segment::Same(text) => contents.extend_from_slice(text),
                Segment::Changed { before, after } => {
                    contents.extend_from_slice(if take { after } else { before });
                }
            }
        }
        let copy_id = match (side(&self.after), side(&self.before)) {
            (Side::File { copy_id, .. }, _) | (_, Side::File { copy_id, .. }) => copy_id.clone(),
            _ => return Err(anyhow!("partial hunk selection on a non-file path")),
        };
        let id = store
            .write_file(&self.path, &mut contents.as_slice())
            .block_on()?;
        Ok(Some(Merge::normal(TreeValue::File {
            id,
            executable,
            copy_id,
        })))
    }
}

/// Group changed segments into hunks the way git does: changes separated by
/// more than 2×CONTEXT unchanged lines become separate hunks.
fn group_hunks(segments: &[Segment]) -> Vec<Hunk> {
    let mut hunks = Vec::new();
    let (mut old_line, mut new_line) = (1usize, 1usize);
    // (first changed segment index, old/new line at its start, unchanged
    // lines since the last change inside this hunk)
    let mut open: Option<(usize, usize, usize)> = None;
    let mut gap_lines = 0usize;
    let mut last_changed = 0usize;
    let close = |hunks: &mut Vec<Hunk>,
                 start: usize,
                 end: usize,
                 old_at: usize,
                 new_at: usize,
                 old_line: usize,
                 new_line: usize| {
        let lead = match start.checked_sub(1).map(|i| &segments[i]) {
            Some(Segment::Same(text)) => count_lines(text).min(CONTEXT),
            _ => 0,
        };
        let trail = match segments.get(end) {
            Some(Segment::Same(text)) => count_lines(text).min(CONTEXT),
            _ => 0,
        };
        hunks.push(Hunk::Lines {
            segments: start..end,
            old_start: old_at - lead,
            old_count: old_line - old_at + lead + trail,
            new_start: new_at - lead,
            new_count: new_line - new_at + lead + trail,
        });
    };
    for (i, segment) in segments.iter().enumerate() {
        match segment {
            Segment::Same(text) => {
                let n = count_lines(text);
                gap_lines += n;
                old_line += n;
                new_line += n;
            }
            Segment::Changed { before, after } => {
                if let Some((start, old_at, new_at)) = open
                    && gap_lines > 2 * CONTEXT
                {
                    // Rewind the line counters to just after the last change.
                    close(
                        &mut hunks,
                        start,
                        last_changed + 1,
                        old_at,
                        new_at,
                        old_line - gap_lines,
                        new_line - gap_lines,
                    );
                    open = None;
                }
                if open.is_none() {
                    open = Some((i, old_line, new_line));
                }
                gap_lines = 0;
                last_changed = i;
                old_line += count_lines(before);
                new_line += count_lines(after);
            }
        }
    }
    if let Some((start, old_at, new_at)) = open {
        close(
            &mut hunks,
            start,
            last_changed + 1,
            old_at,
            new_at,
            old_line - gap_lines,
            new_line - gap_lines,
        );
    }
    hunks
}

fn tail_lines(text: &[u8], n: usize) -> impl Iterator<Item = &[u8]> {
    let lines: Vec<&[u8]> = text.split_inclusive(|&b| b == b'\n').collect();
    let skip = lines.len().saturating_sub(n);
    lines.into_iter().skip(skip)
}

fn push_line(out: &mut String, sign: char, line: &[u8]) {
    let text = String::from_utf8_lossy(line);
    let text = text.strip_suffix('\n').unwrap_or(&text);
    let rendered = format!("{sign}{text}");
    let colored = match sign {
        '+' => rendered.green().to_string(),
        '-' => rendered.red().to_string(),
        _ => rendered,
    };
    out.push_str(&colored);
    out.push('\n');
    if !line.ends_with(b"\n") {
        out.push_str(&"\\ No newline at end of file".dimmed().to_string());
        out.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segs(pairs: &[(&str, &str)]) -> Vec<Segment> {
        pairs
            .iter()
            .map(|(b, a)| {
                if b == a {
                    Segment::Same(b.as_bytes().to_vec())
                } else {
                    Segment::Changed {
                        before: b.as_bytes().to_vec(),
                        after: a.as_bytes().to_vec(),
                    }
                }
            })
            .collect()
    }

    #[test]
    fn nearby_changes_share_a_hunk_and_far_ones_do_not() {
        let near = segs(&[("a\n", "a\n"), ("b\n", "B\n"), ("c\nd\n", "c\nd\n"), ("e\n", "E\n")]);
        let hunks = group_hunks(&near);
        assert_eq!(hunks.len(), 1);
        let Hunk::Lines {
            segments,
            old_start,
            old_count,
            new_start,
            new_count,
        } = &hunks[0]
        else {
            panic!("expected a line hunk");
        };
        assert_eq!(*segments, 1..4);
        assert_eq!((*old_start, *old_count, *new_start, *new_count), (1, 5, 1, 5));

        let gap = "1\n2\n3\n4\n5\n6\n7\n";
        let far = segs(&[("b\n", "B\n"), (gap, gap), ("e\n", "E\nF\n")]);
        let hunks = group_hunks(&far);
        assert_eq!(hunks.len(), 2);
        let Hunk::Lines {
            old_start,
            old_count,
            new_start,
            new_count,
            ..
        } = &hunks[1]
        else {
            panic!("expected a line hunk");
        };
        assert_eq!((*old_start, *old_count, *new_start, *new_count), (6, 4, 6, 5));
    }
}
