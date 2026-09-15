use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

struct Fixture {
    _temp: TempDir,
    remote: PathBuf,
    alice: PathBuf,
    bob: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let remote = temp.path().join("origin.git");
        let seed = temp.path().join("seed");
        let alice = temp.path().join("alice");
        let bob = temp.path().join("bob");

        git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(temp.path(), &["init", "-b", "main", seed.to_str().unwrap()]);
        configure_user(&seed, "Seed");
        std::fs::write(seed.join("file.txt"), "base\n").unwrap();
        git(&seed, &["add", "file.txt"]);
        git(&seed, &["commit", "-m", "seed"]);
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "-u", "origin", "main"]);
        git_dir(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);

        git(
            temp.path(),
            &["clone", remote.to_str().unwrap(), alice.to_str().unwrap()],
        );
        git(
            temp.path(),
            &["clone", remote.to_str().unwrap(), bob.to_str().unwrap()],
        );
        configure_user(&alice, "Alice");
        configure_user(&bob, "Bob");
        assert_success(gt(&alice, &["init"]));
        assert_success(gt(&bob, &["init"]));

        Self {
            _temp: temp,
            remote,
            alice,
            bob,
        }
    }

    fn collaborator_rewrites_branch(&self) -> String {
        std::fs::write(self.alice.join("file.txt"), "alice A1\n").unwrap();
        assert_success(gt(&self.alice, &["create", "-m", "A1"]));
        assert_local_submit_pushed(gt(&self.alice, &["submit"]));

        assert_success(gt(&self.bob, &["sync"]));
        assert_success(gt(&self.bob, &["checkout", "test-user/a1"]));
        std::fs::write(self.bob.join("file.txt"), "bob update\n").unwrap();
        assert_success(gt(&self.bob, &["modify"]));
        assert_local_submit_pushed(gt(&self.bob, &["submit"]));

        git_dir_output(&self.remote, &["rev-parse", "refs/heads/test-user/a1"])
    }
}

#[test]
fn sync_preserves_checkout_across_collaborator_rewrite_then_modify_succeeds() {
    let fixture = Fixture::new();
    let collaborator_commit = fixture.collaborator_rewrites_branch();

    assert_success(gt(&fixture.alice, &["sync"]));
    let log = assert_success(gt(&fixture.alice, &["log"]));
    assert!(
        String::from_utf8_lossy(&log.stdout).contains("test-user/a1"),
        "sync detached the working copy from the updated branch:\n{}",
        String::from_utf8_lossy(&log.stdout)
    );

    std::fs::write(fixture.alice.join("file.txt"), "alice after sync\n").unwrap();
    assert_success(gt(&fixture.alice, &["modify"]));

    assert_eq!(
        git_dir_output(&fixture.remote, &["rev-parse", "refs/heads/test-user/a1"]),
        collaborator_commit,
        "modify must not update the remote"
    );
}

#[test]
fn stale_submit_after_sync_and_modify_keeps_force_with_lease_safety() {
    let fixture = Fixture::new();
    fixture.collaborator_rewrites_branch();
    assert_success(gt(&fixture.alice, &["sync"]));

    std::fs::write(fixture.alice.join("file.txt"), "alice local rewrite\n").unwrap();
    assert_success(gt(&fixture.alice, &["modify"]));

    std::fs::write(fixture.bob.join("file.txt"), "bob second rewrite\n").unwrap();
    assert_success(gt(&fixture.bob, &["modify"]));
    assert_local_submit_pushed(gt(&fixture.bob, &["submit"]));
    let remote_before = git_dir_output(&fixture.remote, &["rev-parse", "refs/heads/test-user/a1"]);

    let rejected = gt(&fixture.alice, &["submit"]);
    assert!(
        !rejected.status.success(),
        "stale lease unexpectedly pushed"
    );
    assert!(
        String::from_utf8_lossy(&rejected.stderr).contains("push failed"),
        "unexpected submit failure:\n{}",
        String::from_utf8_lossy(&rejected.stderr)
    );
    assert_eq!(
        git_dir_output(&fixture.remote, &["rev-parse", "refs/heads/test-user/a1"]),
        remote_before,
        "rejected push changed the remote branch"
    );
}

fn gt(dir: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_jj-gt"))
        .args(args)
        .current_dir(dir)
        .env("GITHUB_TOKEN", "dummy")
        .env("USER", "test-user")
        .output()
        .unwrap()
}

fn git(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert_success(output);
}

fn git_dir(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .arg(format!("--git-dir={}", dir.display()))
        .args(args)
        .output()
        .unwrap();
    assert_success(output);
}

fn git_dir_output(dir: &Path, args: &[&str]) -> String {
    let output = Command::new("git")
        .arg(format!("--git-dir={}", dir.display()))
        .args(args)
        .output()
        .unwrap();
    String::from_utf8(assert_success(output).stdout)
        .unwrap()
        .trim()
        .to_owned()
}

fn configure_user(dir: &Path, name: &str) {
    git(dir, &["config", "user.name", name]);
    git(
        dir,
        &["config", "user.email", &format!("{name}@example.com")],
    );
}

fn assert_local_submit_pushed(output: Output) {
    assert!(
        !output.status.success(),
        "local remote unexpectedly passed GitHub setup"
    );
    assert!(
        String::from_utf8_lossy(&output.stdout).contains("pushed refs/heads/test-user/a1"),
        "submit did not push before the expected non-GitHub error:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stderr).contains("is not a GitHub repo"));
}

fn assert_success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "command failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
