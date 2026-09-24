use std::io::Write as _;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use jj_lib::commit::Commit;
use jj_lib::config::StackedConfig;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;
use jj_lib::settings::UserSettings;
use jj_lib::workspace::Workspace;
use pollster::FutureExt as _;
use tempfile::TempDir;

const BASE: &str = "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";
const CHANGED: &str = "A\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nL\n";
const TOP_ONLY: &str = "A\nb\nc\nd\ne\nf\ng\nh\ni\nj\nk\nl\n";

struct Fixture(TempDir);

impl Fixture {
    /// A repo with one branch, `split-test/feat-big-change`, touching three files:
    /// `file.txt` (two hunks far apart), `cfg.json` (one hunk), and a new
    /// `src/new.rs`.
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "Split Test"]);
        fixture.git(&["config", "user.email", "split@example.invalid"]);
        fixture.write("file.txt", BASE);
        fixture.write("cfg.json", "{}\n");
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-m", "base"]);
        fixture.gt(&["init"], "");
        fixture.write("file.txt", CHANGED);
        fixture.write("cfg.json", "{\"x\": 1}\n");
        std::fs::create_dir(fixture.path().join("src")).unwrap();
        fixture.write("src/new.rs", "fn main() {}\n");
        fixture.gt(&["create", "-m", "feat: big change"], "");
        fixture
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn write(&self, path: &str, contents: &str) {
        std::fs::write(self.path().join(path), contents).unwrap();
    }

    fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.path().join(path)).unwrap()
    }

    fn run(&self, program: &str, args: &[&str], stdin: &str, cwd: &Path) -> Output {
        let mut child = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .env("USER", "split-test")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(stdin.as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        stdout(self.run("git", args, "", self.path()))
    }

    fn gt(&self, args: &[&str], stdin: &str) -> String {
        stdout(self.run(env!("CARGO_BIN_EXE_jj-gt"), args, stdin, self.path()))
    }

    fn gt_in(&self, subdir: &str, args: &[&str], stdin: &str) -> String {
        stdout(self.run(
            env!("CARGO_BIN_EXE_jj-gt"),
            args,
            stdin,
            &self.path().join(subdir),
        ))
    }

    fn gt_fails(&self, args: &[&str], stdin: &str, message: &str) {
        let output = self.run(env!("CARGO_BIN_EXE_jj-gt"), args, stdin, self.path());
        assert!(
            !output.status.success(),
            "jj-gt {args:?} unexpectedly succeeded:\n{}",
            String::from_utf8_lossy(&output.stdout)
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{stderr}");
    }

    fn wc_commit(&self) -> Commit {
        let settings = UserSettings::from_config(StackedConfig::with_defaults()).unwrap();
        let workspace = Workspace::load(
            &settings,
            self.path(),
            &jj_lib::default_backend_factories::default_backend_factories(),
            &jj_lib::default_backend_factories::default_working_copy_factories(),
        )
        .unwrap();
        let repo = workspace.repo_loader().load_at_head().block_on().unwrap();
        let id = repo
            .view()
            .get_wc_commit_id(workspace.workspace_name())
            .unwrap();
        repo.store().get_commit(id).unwrap()
    }

    fn files_changed(&self, rev: &str) -> String {
        self.git(&["diff", "--name-only", &format!("{rev}^"), rev])
    }

    fn message(&self, rev: &str) -> String {
        self.git(&["show", "-s", "--format=%B", rev])
    }

    /// jj records its change id as a git commit header.
    fn change_id(&self, rev: &str) -> String {
        self.git(&["cat-file", "commit", rev])
            .lines()
            .find_map(|line| line.strip_prefix("change-id "))
            .unwrap()
            .to_owned()
    }

    fn ref_exists(&self, name: &str) -> bool {
        self.run(
            "git",
            &["rev-parse", "--verify", "--quiet", name],
            "",
            self.path(),
        )
        .status
        .success()
    }

    fn assert_clean_stack_top(&self, branch: &str) {
        let wc = self.wc_commit();
        assert!(wc.description().is_empty());
        assert_eq!(wc.parent_ids()[0].hex(), self.git(&["rev-parse", branch]));
        assert_eq!(
            self.git(&["rev-parse", "HEAD"]),
            self.git(&["rev-parse", branch])
        );
        assert_eq!(self.git(&["status", "--porcelain"]), "");
    }
}

fn stdout(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let mut text = String::new();
    let mut escape = false;
    for ch in String::from_utf8(output.stdout).unwrap().chars() {
        if ch == '\x1b' {
            escape = true;
        } else if escape {
            if ch == 'm' {
                escape = false;
            }
        } else {
            text.push(ch);
        }
    }
    text.trim().to_owned()
}

#[test]
fn by_file_extracts_matching_files_into_a_new_parent_branch_non_interactively() {
    let f = Fixture::new();
    let old_tip = f.git(&["rev-parse", "split-test/feat-big-change"]);
    let old_change_id = f.change_id(&old_tip);
    let trunk = f.git(&["rev-parse", "main"]);

    let output = f.gt(&["split", "--by-file", "*.json", "-m", "chore: config"], "");
    assert!(output.contains("cfg.json"), "{output}");
    assert!(
        output.contains("Created branch split-test/chore-config below split-test/feat-big-change"),
        "{output}"
    );

    // New parent branch holds only the json change, sitting on trunk.
    assert_eq!(f.files_changed("split-test/chore-config"), "cfg.json");
    assert_eq!(f.message("split-test/chore-config"), "chore: config");
    assert_eq!(f.git(&["rev-parse", "split-test/chore-config^"]), trunk);
    assert_eq!(
        f.git(&["show", "split-test/chore-config:cfg.json"]),
        "{\"x\": 1}"
    );

    // The original branch keeps its name, message and full tree, minus the json.
    let tip = f.git(&["rev-parse", "split-test/feat-big-change"]);
    assert_ne!(tip, old_tip);
    assert_eq!(
        f.git(&["rev-parse", "split-test/feat-big-change^"]),
        f.git(&["rev-parse", "split-test/chore-config"])
    );
    assert_eq!(
        f.files_changed("split-test/feat-big-change"),
        "file.txt\nsrc/new.rs"
    );
    assert_eq!(f.message("split-test/feat-big-change"), "feat: big change");
    assert_eq!(
        f.git(&["rev-parse", "split-test/feat-big-change^{tree}"]),
        f.git(&["rev-parse", &format!("{old_tip}^{{tree}}")])
    );
    // Same change id: the branch was rewritten, not replaced.
    assert_eq!(f.change_id(&tip), old_change_id);
    assert_ne!(f.change_id("split-test/chore-config"), old_change_id);
    f.assert_clean_stack_top("split-test/feat-big-change");
    assert_eq!(f.read("cfg.json"), "{\"x\": 1}\n");
    assert_eq!(f.read("file.txt"), CHANGED);
}

#[test]
fn by_file_pathspecs_are_cwd_relative_and_repeatable_and_default_names_derive_from_the_branch() {
    let f = Fixture::new();
    // From inside src/, a bare directory pathspec means "everything under here";
    // a second pattern is unioned in.
    let output = f.gt_in("src", &["split", "-f", ".", "-f", "../*.json"], "");
    assert!(
        output.contains("src/new.rs") && output.contains("cfg.json"),
        "{output}"
    );
    let new_branch = "split-test/feat-big-change-json";
    assert!(
        output.contains(&format!("Created branch {new_branch}")),
        "{output}"
    );
    assert_eq!(f.files_changed(new_branch), "cfg.json\nsrc/new.rs");
    assert_eq!(f.message(new_branch), "feat: big change (. ../*.json)");
    assert_eq!(f.files_changed("split-test/feat-big-change"), "file.txt");
    f.assert_clean_stack_top("split-test/feat-big-change");
}

#[test]
fn by_file_rejects_empty_and_total_matches_and_dirty_working_copies() {
    let f = Fixture::new();
    f.gt_fails(
        &["split", "-f", "*.nomatch"],
        "",
        "no changed files on split-test/feat-big-change match",
    );
    f.gt_fails(
        &["split", "-f", "*", "-m", "x"],
        "",
        "nothing would be left on the branch",
    );
    f.gt_fails(&["split", "-m", "x"], "", "--by-file");
    f.gt_fails(&["split", "-c", "-h"], "", "cannot be used with");
    f.write("dirty.txt", "dirty\n");
    f.gt_fails(
        &["split", "-f", "*.json"],
        "",
        "the working copy has changes",
    );
    // The failed split still snapshotted the edit; nothing else moved.
    assert_eq!(
        f.files_changed("split-test/feat-big-change"),
        "cfg.json\nfile.txt\nsrc/new.rs"
    );
    std::fs::remove_file(f.path().join("dirty.txt")).unwrap();
    f.gt(&["checkout", "main"], "");
    f.gt_fails(&["split", "-h"], "", "sits directly on main");
}

#[test]
fn by_hunk_builds_branches_bottom_up_from_picked_hunks_including_partial_files() {
    let f = Fixture::new();
    let old_tip = f.git(&["rev-parse", "split-test/feat-big-change"]);
    let trunk = f.git(&["rev-parse", "main"]);
    // Hunks are visited in path order: cfg.json (1), file.txt (2), src/new.rs (1).
    // Round 1: only the first file.txt hunk.  Round 2: the new file.
    // Everything else stays on the original branch.
    let answers = "\
n\n\
y\n\
n\n\
n\n\
First half\n\
\n\
y\n\
n\n\
n\n\
y\n\
\n\
split-test/added\n\
n\n";
    let output = f.gt(&["split", "--by-hunk"], answers);
    assert!(output.contains("3 changed file(s), 4 hunk(s)"), "{output}");
    assert!(
        output.contains("@@ -1,4 +1,4 @@") && output.contains("@@ -9,4 +9,4 @@"),
        "{output}"
    );
    assert!(output.contains("-a") && output.contains("+A"), "{output}");
    assert!(
        output.contains("Created 2 branch(es) below split-test/feat-big-change: split-test/first-half, split-test/added"),
        "{output}"
    );

    let first = "split-test/first-half";
    let second = "split-test/added";
    assert_eq!(f.git(&["rev-parse", &format!("{first}^")]), trunk);
    assert_eq!(f.files_changed(first), "file.txt");
    assert_eq!(f.message(first), "First half");
    assert_eq!(
        f.git(&["show", &format!("{first}:file.txt")]),
        TOP_ONLY.trim_end()
    );

    assert_eq!(
        f.git(&["rev-parse", &format!("{second}^")]),
        f.git(&["rev-parse", first])
    );
    assert_eq!(f.files_changed(second), "src/new.rs");
    assert_eq!(f.message(second), "feat: big change (part 2)");

    assert_eq!(
        f.git(&["rev-parse", "split-test/feat-big-change^"]),
        f.git(&["rev-parse", second])
    );
    assert_eq!(
        f.files_changed("split-test/feat-big-change"),
        "cfg.json\nfile.txt"
    );
    assert_eq!(f.message("split-test/feat-big-change"), "feat: big change");
    assert_eq!(
        f.git(&["rev-parse", "split-test/feat-big-change^{tree}"]),
        f.git(&["rev-parse", &format!("{old_tip}^{{tree}}")])
    );
    f.assert_clean_stack_top("split-test/feat-big-change");
    assert_eq!(f.read("file.txt"), CHANGED);

    // The whole command is one undo group.
    f.gt(&["undo"], "");
    assert_eq!(f.git(&["rev-parse", "split-test/feat-big-change"]), old_tip);
    assert!(!f.ref_exists(first));
    assert!(!f.ref_exists(second));
    f.assert_clean_stack_top("split-test/feat-big-change");
}

#[test]
fn by_hunk_supports_file_wide_answers_quit_and_selecting_everything() {
    let f = Fixture::new();
    let old_tip = f.git(&["rev-parse", "split-test/feat-big-change"]);

    // `q` aborts before anything is written.
    f.gt_fails(&["split", "-h"], "y\nq\n", "split aborted");
    assert_eq!(f.git(&["rev-parse", "split-test/feat-big-change"]), old_tip);
    assert_eq!(f.git(&["status", "--porcelain"]), "");

    // Picking every hunk leaves nothing to split.
    f.gt_fails(&["split", "-h"], "a\na\na\n", "nothing to split");
    assert_eq!(f.git(&["rev-parse", "split-test/feat-big-change"]), old_tip);

    // An empty round offers a retry; EOF at a prompt is a clean error.
    f.gt_fails(
        &["split", "-h"],
        "n\nn\nn\nn\n",
        "interactive input required",
    );
    f.gt_fails(&["split", "-h"], "n\nn\nn\nn\nn\n", "split aborted");

    // `?` prints help and re-asks; `d` skips the rest of file.txt; `a` takes
    // the rest of a file. Round 2 then takes all that remains, which keeps
    // the original branch as the top.
    let answers = "?\ny\nd\nn\n\n\ny\na\na\n";
    let output = f.gt(&["split", "-h"], answers);
    assert!(output.contains("q - quit"), "{output}");
    assert!(
        output.contains("All remaining changes selected"),
        "{output}"
    );
    let first = "split-test/feat-big-change-part-1";
    assert_eq!(f.files_changed(first), "cfg.json");
    assert_eq!(
        f.files_changed("split-test/feat-big-change"),
        "file.txt\nsrc/new.rs"
    );
    assert_eq!(
        f.git(&["rev-parse", "split-test/feat-big-change^"]),
        f.git(&["rev-parse", first])
    );
    f.assert_clean_stack_top("split-test/feat-big-change");
}

#[test]
fn by_commit_bookmarks_split_points_and_the_bare_command_prompts_for_a_strategy() {
    let f = Fixture::new();
    // A single-commit branch has no commit boundaries to split at.
    f.gt_fails(&["split", "--by-commit"], "", "has a single commit");

    // Grow the branch with raw git commits, then move the branch ref so jj-gt
    // adopts them as the branch's history.
    f.write("two.txt", "2\n");
    f.git(&["add", "two.txt"]);
    f.git(&["commit", "-m", "second: more"]);
    f.write("three.txt", "3\n");
    f.git(&["add", "three.txt"]);
    f.git(&["commit", "-m", "third: even more"]);
    f.git(&[
        "update-ref",
        "refs/heads/split-test/feat-big-change",
        "HEAD",
    ]);
    let log = f.gt(&["log"], "");
    assert_eq!(log.matches("(unbookmarked)").count(), 2, "{log}");
    let first_id = f.git(&["rev-parse", "split-test/feat-big-change~2"]);
    let second_id = f.git(&["rev-parse", "split-test/feat-big-change~1"]);
    let tip_id = f.git(&["rev-parse", "split-test/feat-big-change"]);

    // No option + several commits: prompt for a strategy. Pick commit mode,
    // split after commits 1 and 2, accept the first default, name the second.
    let answers = "c\n1, 2\n\nsplit-test/more\n";
    let output = f.gt(&["split"], answers);
    assert!(
        output.contains("has 3 commits. How do you want to split it?"),
        "{output}"
    );
    assert!(output.contains("Branch name for commits 1..1"), "{output}");
    assert!(output.contains("Branch name for commits 2..2"), "{output}");
    assert!(
        output.contains("Split split-test/feat-big-change into 3 branches"),
        "{output}"
    );

    // Bookmark-only: commit ids are unchanged. The first commit's default
    // name comes from its message, made unique against the existing branch.
    assert!(
        output.contains("[split-test/feat-big-change-2]"),
        "{output}"
    );
    assert_eq!(
        f.git(&["rev-parse", "split-test/feat-big-change-2"]),
        first_id
    );
    assert_eq!(f.git(&["rev-parse", "split-test/more"]), second_id);
    assert_eq!(f.git(&["rev-parse", "split-test/feat-big-change"]), tip_id);
    f.assert_clean_stack_top("split-test/feat-big-change");
    let log = f.gt(&["log"], "");
    assert!(!log.contains("(unbookmarked)"), "{log}");
    for name in ["split-test/feat-big-change-2", "split-test/more"] {
        assert!(log.contains(name), "{log}");
    }

    // Names must be fresh; the prompt re-asks until they are.
    f.gt(&["undo"], "");
    let output = f.gt(
        &["split", "-c"],
        "1\nsplit-test/feat-big-change\nhas space\nsplit-test/ok\n",
    );
    assert!(output.contains("already exists"), "{output}");
    assert!(output.contains("cannot contain whitespace"), "{output}");
    assert_eq!(f.git(&["rev-parse", "split-test/ok"]), first_id);
}
