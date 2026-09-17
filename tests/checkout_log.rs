use std::collections::HashMap;
use std::path::Path;
use std::process::{Command, Output};

use tempfile::TempDir;

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "Checkout Test"]);
        fixture.git(&["config", "user.email", "checkout@example.invalid"]);
        fixture.write("file.txt", "base\n");
        fixture.write("delete.txt", "keep\n");
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-m", "base"]);
        fixture.gt(&["init"]);
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

    fn run(&self, program: &str, args: &[&str]) -> Output {
        Command::new(program)
            .args(args)
            .current_dir(self.path())
            .env("USER", "checkout-test")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap()
    }

    fn git(&self, args: &[&str]) -> String {
        stdout(self.run("git", args))
    }

    fn gt(&self, args: &[&str]) -> String {
        stdout(self.run(env!("CARGO_BIN_EXE_jj-gt"), args))
    }

    fn reject_checkout(&self, target: &str, message: &str) {
        let output = self.run(env!("CARGO_BIN_EXE_jj-gt"), &["checkout", target]);
        assert!(!output.status.success(), "checkout unexpectedly succeeded");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(message), "{stderr}");
    }
}

fn stdout(output: Output) -> String {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    // The stack renderer uses explicit ANSI colors, even when piped.
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
fn checkout_saves_edits_and_resumes_them_by_short_or_full_id_for_create() {
    for use_full_id in [false, true] {
        let f = Fixture::new();
        f.write("file.txt", "unfinished work\n");
        f.write("new.txt", "new file\n");
        std::fs::remove_file(f.path().join("delete.txt")).unwrap();

        // No create or log before switching: checkout itself must save the edits.
        let output = f.gt(&["checkout", "main"]);
        let id = output
            .lines()
            .find_map(|line| line.strip_prefix("Saved unbookmarked work at "))
            .unwrap()
            .split(';')
            .next()
            .unwrap();
        assert_eq!(f.read("file.txt"), "base\n");
        assert!(!f.path().join("new.txt").exists());
        assert_eq!(f.read("delete.txt"), "keep\n");

        let log = f.gt(&["log"]);
        let saved = log
            .split("Saved work (checkout by commit ID)")
            .nth(1)
            .unwrap();
        assert!(saved.contains(&format!("{id}  (unbookmarked)")), "{log}");
        let full_id = f.git(&["rev-parse", id]);
        let target = if use_full_id { &full_id } else { id };
        let output = f.gt(&["checkout", target]);
        assert!(output.contains("Resumed saved work"), "{output}");
        assert_eq!(f.read("file.txt"), "unfinished work\n");
        assert_eq!(f.read("new.txt"), "new file\n");
        assert!(!f.path().join("delete.txt").exists());

        let log = f.gt(&["log"]);
        let here = log
            .lines()
            .find(|line| line.contains("← you are here"))
            .unwrap();
        assert!(
            here.contains(id) && here.contains("(unbookmarked)"),
            "{log}"
        );
        assert_eq!(log.matches(id).count(), 1, "snapshot listed twice: {log}");
        f.gt(&["create", "-m", "Recovered"]);
        assert_eq!(
            f.git(&["show", "checkout-test/recovered:file.txt"]),
            "unfinished work"
        );
        assert_eq!(
            f.git(&["show", "checkout-test/recovered:new.txt"]),
            "new file"
        );
    }
}

#[test]
fn log_lists_other_stacks_and_bookmark_aliases_as_checkout_targets() {
    let f = Fixture::new();
    f.write("file.txt", "first branch\n");
    f.gt(&["create", "-m", "First"]);
    f.git(&["branch", "first-alias", "checkout-test/first"]);
    f.git(&["branch", "release", "main"]);
    f.gt(&["checkout", "main"]);
    f.write("file.txt", "second branch\n");
    f.gt(&["create", "-m", "Second"]);

    let log = f.gt(&["log"]);
    let other = log
        .split("Other bookmarks (checkout by name)")
        .nth(1)
        .unwrap();
    for name in ["checkout-test/first", "first-alias", "release"] {
        let line = other.lines().find(|line| line.contains(name)).unwrap();
        let id = f.git(&["rev-parse", name]);
        assert!(line.contains(&id[..8]), "{line}");
    }
    assert!(log.contains("checkout-test/second"), "{log}");
    f.gt(&["checkout", "first-alias"]);
    assert_eq!(f.read("file.txt"), "first branch\n");
    let log = f.gt(&["log"]);
    assert!(
        log.contains("checkout-test/first") && log.contains("first-alias"),
        "{log}"
    );
}

#[test]
fn invalid_and_ambiguous_ids_preserve_edits_and_bookmark_names_take_precedence() {
    let f = Fixture::new();
    let tree = f.git(&["rev-parse", "main^{tree}"]);
    let mut prefixes = HashMap::new();
    let mut ambiguous = None;
    // Seventeen distinct IDs guarantee a shared first hexadecimal digit.
    for i in 0..17 {
        let id = f.git(&[
            "commit-tree",
            &tree,
            "-p",
            "main",
            "-m",
            &format!("candidate {i}"),
        ]);
        f.git(&["update-ref", &format!("refs/heads/candidate-{i}"), &id]);
        if prefixes.insert(id[..1].to_owned(), id.clone()).is_some() {
            ambiguous = Some(id[..1].to_owned());
        }
    }
    f.write("file.txt", "must survive\n");
    let ambiguous = ambiguous.unwrap();
    f.reject_checkout(&ambiguous, "is ambiguous");
    f.reject_checkout("not-a-bookmark", "no bookmark or commit matching");
    f.reject_checkout(&"f".repeat(40), "no bookmark or commit matching");
    assert_eq!(f.read("file.txt"), "must survive\n");

    f.git(&["branch", &ambiguous, "main"]);
    f.gt(&["checkout", &ambiguous]);
    assert_eq!(f.git(&["rev-parse", "HEAD"]), f.git(&["rev-parse", "main"]));
    assert!(
        f.gt(&["log"])
            .contains("Saved work (checkout by commit ID)")
    );
}

#[test]
fn checkout_by_bookmarked_or_historical_id_uses_a_fresh_working_copy() {
    let f = Fixture::new();
    let main = f.git(&["rev-parse", "main"]);
    f.gt(&["checkout", &main]);
    f.write("file.txt", "new branch\n");
    f.gt(&["log"]);
    assert_eq!(f.git(&["rev-parse", "main"]), main);
    f.gt(&["create", "-m", "Feature"]);

    // Make the original main commit an unbookmarked historical ancestor.
    f.gt(&["checkout", "main"]);
    f.git(&[
        "update-ref",
        "refs/heads/main",
        "refs/heads/checkout-test/feature",
    ]);
    f.gt(&["log"]);
    let output = f.gt(&["checkout", &main]);
    assert!(output.contains("Checked out"), "{output}");
    assert!(!output.contains("Resumed saved work"), "{output}");
    assert_eq!(f.git(&["rev-parse", "HEAD"]), main);
    assert_eq!(f.read("file.txt"), "base\n");
}
