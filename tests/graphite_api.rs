use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::TempDir;

const TOKEN: &str = "graphite-test-token";
const AUTH: &str = "/v1/graphite/check-auth";
const INFO: &str = "/v1/graphite/cli/pull-request-info";
const SUBMIT: &str = "/v1/graphite/submit/pull-requests";

struct MockApi {
    url: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<Value>>>,
    thread: Option<JoinHandle<()>>,
}

impl MockApi {
    fn new(token: &str, replies: Vec<(&'static str, u16, Value)>) -> Self {
        Self::serve(
            format!("token {token}"),
            replies
                .into_iter()
                .map(|(path, status, body)| (format!("POST {path}"), status, body))
                .collect(),
            "/v1",
        )
    }

    fn github(replies: Vec<(&'static str, u16, Value)>) -> Self {
        Self::serve(
            "Bearer git-test-token".into(),
            replies
                .into_iter()
                .map(|(request, status, body)| (request.into(), status, body))
                .collect(),
            "",
        )
    }

    fn serve(authorization: String, replies: Vec<(String, u16, Value)>, api_path: &str) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}{api_path}", listener.local_addr().unwrap());
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stopping = stop.clone();
        let received = requests.clone();
        let thread = thread::spawn(move || {
            let mut replies = VecDeque::from(replies);
            while !stopping.load(Ordering::Relaxed) {
                let (mut stream, _) = match listener.accept() {
                    Ok(connection) => connection,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(err) => panic!("accept: {err}"),
                };
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let (request, status, body) = replies.pop_front().expect("unexpected API request");
                assert_eq!(line.trim(), format!("{request} HTTP/1.1"));
                let mut length = 0;
                let mut auth = String::new();
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let (name, value) = line.split_once(':').unwrap();
                    if name.eq_ignore_ascii_case("content-length") {
                        length = value.trim().parse().unwrap();
                    }
                    if name.eq_ignore_ascii_case("authorization") {
                        auth = value.trim().into();
                    }
                }
                assert_eq!(auth, authorization);
                let mut bytes = vec![0; length];
                reader.read_exact(&mut bytes).unwrap();
                received.lock().unwrap().push(if bytes.is_empty() {
                    Value::Null
                } else {
                    serde_json::from_slice(&bytes).unwrap()
                });
                let body = body.to_string();
                write!(stream, "HTTP/1.1 {status} Test\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
            }
            assert!(
                replies.is_empty(),
                "{} expected API requests were never made",
                replies.len()
            );
        });
        Self {
            url,
            stop,
            requests,
            thread: Some(thread),
        }
    }

    fn finish(mut self) -> Vec<Value> {
        self.stop.store(true, Ordering::Relaxed);
        self.thread.take().unwrap().join().unwrap();
        self.requests.lock().unwrap().clone()
    }
}

impl Drop for MockApi {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn gt(dir: &Path, home: &Path, api: &MockApi, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jj-gt"));
    command
        .args(args)
        .current_dir(dir)
        .env("HOME", home)
        .env("USER", "test-user")
        .env("GITHUB_TOKEN", "git-test-token")
        .env_remove("JJ_GT_GITHUB_API_URL")
        .env_remove("GRAPHITE_AUTH_TOKEN")
        .env("JJ_GT_GRAPHITE_API_URL", &api.url);
    command
}

fn success(output: Output) -> Output {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

fn git(dir: &Path, args: &[&str]) -> String {
    let output = success(
        Command::new("git")
            .current_dir(dir)
            .args(args)
            .output()
            .unwrap(),
    );
    String::from_utf8(output.stdout).unwrap().trim().into()
}

fn configured_home(path: &Path) {
    std::fs::create_dir_all(path.join(".jj-gt")).unwrap();
    std::fs::write(
        path.join(".jj-gt/config"),
        json!({"authToken": TOKEN}).to_string(),
    )
    .unwrap();
}

#[test]
fn auth_works_outside_a_repo_validates_and_saves_private_config() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    configured_home(&home);
    let config = home.join(".jj-gt/config");
    std::fs::write(&config, r#"{"otherSetting":true}"#).unwrap();
    let api = MockApi::new(TOKEN, vec![(AUTH, 200, json!({"githubLogin":"alice"}))]);
    let output = success(
        gt(temp.path(), &home, &api, &["auth", "--token", TOKEN])
            .env_remove("GITHUB_TOKEN")
            .output()
            .unwrap(),
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(TOKEN));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(TOKEN));
    let saved: Value = serde_json::from_slice(&std::fs::read(&config).unwrap()).unwrap();
    assert_eq!(saved, json!({"authToken": TOKEN, "otherSetting": true}));
    assert_eq!(
        std::fs::metadata(&config).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        std::fs::metadata(config.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert!(!temp.path().join(".jj").exists());
    assert_eq!(api.finish(), vec![json!({})]);
}

#[test]
fn rejected_auth_preserves_previous_token_and_hides_response_secrets() {
    let temp = tempfile::tempdir().unwrap();
    configured_home(temp.path());
    let api = MockApi::new("bad-token", vec![(AUTH, 401, json!({"error":"bad-token"}))]);
    let output = gt(temp.path(), temp.path(), &api, &["auth", "-t", "bad-token"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(!String::from_utf8_lossy(&output.stderr).contains("bad-token"));
    let config: Value =
        serde_json::from_slice(&std::fs::read(temp.path().join(".jj-gt/config")).unwrap()).unwrap();
    assert_eq!(config["authToken"], TOKEN);
    api.finish();
}

#[test]
fn environment_token_overrides_config_and_auth_without_token_checks_it() {
    let temp = tempfile::tempdir().unwrap();
    configured_home(temp.path());
    let api = MockApi::new(
        "override-token",
        vec![(AUTH, 200, json!({"githubLogin":"bob"}))],
    );
    success(
        gt(temp.path(), temp.path(), &api, &["auth"])
            .env("GRAPHITE_AUTH_TOKEN", "override-token")
            .output()
            .unwrap(),
    );
    api.finish();
}

#[test]
fn empty_token_and_malformed_config_fail_without_network() {
    let temp = tempfile::tempdir().unwrap();
    let api = MockApi::new(TOKEN, vec![]);
    assert!(
        !gt(temp.path(), temp.path(), &api, &["auth", "--token", " "])
            .output()
            .unwrap()
            .status
            .success()
    );
    configured_home(temp.path());
    std::fs::write(temp.path().join(".jj-gt/config"), "invalid JSON").unwrap();
    assert!(
        !gt(temp.path(), temp.path(), &api, &["auth"])
            .output()
            .unwrap()
            .status
            .success()
    );
    api.finish();
}

struct Fixture {
    _temp: TempDir,
    home: PathBuf,
    work: PathBuf,
    seed: PathBuf,
    remote: PathBuf,
}

impl Fixture {
    fn new(api: &MockApi) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let remote = temp.path().join("origin.git");
        let seed = temp.path().join("seed");
        let work = temp.path().join("work");
        configured_home(&home);
        git(temp.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(temp.path(), &["init", "-b", "main", seed.to_str().unwrap()]);
        git(&seed, &["config", "user.name", "Test"]);
        git(&seed, &["config", "user.email", "test@example.com"]);
        std::fs::write(seed.join("base.txt"), "base\n").unwrap();
        git(&seed, &["add", "."]);
        git(&seed, &["commit", "-m", "seed"]);
        git(
            &seed,
            &["remote", "add", "origin", remote.to_str().unwrap()],
        );
        git(&seed, &["push", "origin", "main"]);
        git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        git(
            temp.path(),
            &["clone", remote.to_str().unwrap(), work.to_str().unwrap()],
        );
        git(&work, &["config", "user.name", "Test"]);
        git(&work, &["config", "user.email", "test@example.com"]);
        // Present a GitHub remote to the API client, but keep every git
        // operation local. No real account or repository is touched.
        git(
            &work,
            &[
                "remote",
                "set-url",
                "origin",
                "https://github.com/test-owner/test-repo.git",
            ],
        );
        // `remote get-url` expands insteadOf, so use a wrapper to rewrite the
        // URL only for git operations, leaving repository discovery intact.
        let bin = home.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let real_git =
            String::from_utf8(Command::new("which").arg("git").output().unwrap().stdout).unwrap();
        let wrapper = format!(
            "#!/bin/sh\nif [ \"$3\" = remote ] && [ \"$4\" = get-url ]; then\n  echo https://github.com/test-owner/test-repo.git\nelse\n  exec '{}' -c 'url.{}.insteadOf=https://github.com/test-owner/test-repo.git' \"$@\"\nfi\n",
            real_git.trim(),
            remote.display()
        );
        std::fs::write(bin.join("git"), wrapper).unwrap();
        std::fs::set_permissions(bin.join("git"), std::fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self {
            _temp: temp,
            home,
            work,
            seed,
            remote,
        };
        success(fixture.gt(api, &["init"]));
        fixture
    }

    fn command(&self, api: &MockApi, args: &[&str]) -> Command {
        let mut command = gt(&self.work, &self.home, api, args);
        command.env(
            "PATH",
            format!(
                "{}:{}",
                self.home.join("bin").display(),
                std::env::var("PATH").unwrap()
            ),
        );
        command
    }

    fn gt(&self, api: &MockApi, args: &[&str]) -> Output {
        self.command(api, args).output().unwrap()
    }

    fn stack(&self, api: &MockApi) {
        std::fs::write(self.work.join("first.txt"), "first\n").unwrap();
        success(self.gt(api, &["create", "-m", "First\n\nFirst body"]));
        std::fs::write(self.work.join("second.txt"), "second\n").unwrap();
        success(self.gt(api, &["create", "-m", "Second"]));
    }

    fn state(&self) -> Value {
        serde_json::from_slice(&std::fs::read(self.work.join(".jj/gt.json")).unwrap()).unwrap()
    }
}

fn access() -> (&'static str, u16, Value) {
    (
        AUTH,
        200,
        json!({"githubLogin":"alice", "canSubmitPrs":true}),
    )
}
fn info(prs: Value) -> (&'static str, u16, Value) {
    (INFO, 200, json!({"result":{"status":"ok", "prs":prs}}))
}
fn submitted() -> (&'static str, u16, Value) {
    (
        SUBMIT,
        200,
        json!({"prs":[
            {"status":"created", "head":"test-user/first", "prNumber":1, "prURL":"https://app.graphite.com/github/pr/test-owner/test-repo/1"},
            {"status":"updated", "head":"test-user/second", "prNumber":2, "prURL":"https://app.graphite.com/github/pr/test-owner/test-repo/2"}
        ]}),
    )
}

#[test]
fn submit_creates_a_stack_then_updates_existing_prs_without_overwriting_metadata() {
    let api = MockApi::new(
        TOKEN,
        vec![
            access(),
            info(json!([])),
            submitted(),
            access(),
            info(json!([
                {"prNumber":1, "headRefName":"test-user/first", "state":"OPEN"},
                {"prNumber":2, "headRefName":"test-user/second", "state":"OPEN"}
            ])),
            submitted(),
        ],
    );
    let fixture = Fixture::new(&api);
    fixture.stack(&api);
    success(fixture.gt(&api, &["submit"]));
    success(fixture.gt(&api, &["submit"]));
    let requests = api.finish();
    assert_eq!(
        requests[0],
        json!({"repoOwner":"test-owner", "repoName":"test-repo"})
    );
    assert_eq!(
        requests[1]["prHeadRefNames"],
        json!(["test-user/first", "test-user/second"])
    );
    let create = &requests[2];
    assert_eq!(create["trunkBranchName"], "main");
    assert_eq!(create["prs"][0]["base"], "main");
    assert_eq!(create["prs"][0]["body"], "First body");
    assert_eq!(create["prs"][1]["base"], "test-user/first");
    assert_eq!(create["prs"][1]["baseSha"], create["prs"][0]["headSha"]);
    assert_eq!(
        create["prs"][0]["headSha"],
        git(&fixture.remote, &["rev-parse", "test-user/first"])
    );
    for pr in requests[5]["prs"].as_array().unwrap() {
        assert_eq!(pr["action"], "update");
        assert!(pr.get("title").is_none() && pr.get("body").is_none());
        assert!(pr["prNumber"].is_number());
    }
    assert_eq!(
        fixture.state()["prs"],
        json!({"test-user/first":1, "test-user/second":2})
    );
}

#[test]
fn partial_submit_saves_success_and_fails_with_branch_error() {
    let api = MockApi::new(
        TOKEN,
        vec![
            access(),
            info(json!([])),
            (
                SUBMIT,
                200,
                json!({"prs":[
                    {"status":"created", "head":"test-user/first", "prNumber":1, "prURL":"https://example.com/1"},
                    {"status":"error", "head":"test-user/second", "error":"permission denied"}
                ]}),
            ),
        ],
    );
    let fixture = Fixture::new(&api);
    fixture.stack(&api);
    let output = fixture.gt(&api, &["submit"]);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("test-user/second: permission denied")
    );
    assert_eq!(fixture.state()["prs"], json!({"test-user/first":1}));
    api.finish();
}

fn github_pr(number: u64, head: &str, base: &str) -> Value {
    json!({
        "number":number, "html_url":format!("https://github.com/test-owner/test-repo/pull/{number}"),
        "state":"open", "head":{"ref":head}, "base":{"ref":base}
    })
}

#[test]
fn denied_graphite_access_falls_back_to_github_and_reuses_stacked_prs() {
    let denied = || {
        (
            AUTH,
            200,
            json!({"githubLogin":"alice", "canSubmitPrs":false}),
        )
    };
    let api = MockApi::new(TOKEN, vec![denied(), denied()]);
    let first = github_pr(1, "test-user/first", "main");
    let second = github_pr(2, "test-user/second", "test-user/first");
    let github = MockApi::github(vec![
        (
            "GET /repos/test-owner/test-repo/pulls?state=open&head=test-owner:test-user/first",
            200,
            json!([]),
        ),
        ("POST /repos/test-owner/test-repo/pulls", 201, first.clone()),
        (
            "GET /repos/test-owner/test-repo/pulls?state=open&head=test-owner:test-user/second",
            200,
            json!([]),
        ),
        (
            "POST /repos/test-owner/test-repo/pulls",
            201,
            second.clone(),
        ),
        (
            "PATCH /repos/test-owner/test-repo/pulls/1",
            200,
            first.clone(),
        ),
        (
            "PATCH /repos/test-owner/test-repo/pulls/2",
            200,
            second.clone(),
        ),
        (
            "GET /repos/test-owner/test-repo/pulls/1",
            200,
            first.clone(),
        ),
        // A later submit must fix an existing PR's base rather than recreate it.
        (
            "GET /repos/test-owner/test-repo/pulls/2",
            200,
            github_pr(2, "test-user/second", "main"),
        ),
        (
            "PATCH /repos/test-owner/test-repo/pulls/2",
            200,
            second.clone(),
        ),
        ("PATCH /repos/test-owner/test-repo/pulls/1", 200, first),
        ("PATCH /repos/test-owner/test-repo/pulls/2", 200, second),
    ]);
    let fixture = Fixture::new(&api);
    fixture.stack(&api);
    for _ in 0..2 {
        let output = success(
            fixture
                .command(&api, &["submit"])
                .env("JJ_GT_GITHUB_API_URL", &github.url)
                .output()
                .unwrap(),
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("falling back to GitHub"));
        assert!(
            String::from_utf8_lossy(&output.stdout)
                .contains("https://github.com/test-owner/test-repo/pull/2")
        );
    }
    assert_eq!(
        fixture.state()["prs"],
        json!({"test-user/first":1, "test-user/second":2})
    );
    assert_eq!(
        git(&fixture.remote, &["rev-parse", "test-user/second"]),
        git(&fixture.work, &["rev-parse", "test-user/second"])
    );
    let requests = github.finish();
    assert_eq!(
        requests[1],
        json!({"head":"test-user/first", "base":"main", "title":"First", "body":""})
    );
    assert_eq!(requests[3]["base"], "test-user/first");
    assert!(requests[4]["body"].as_str().unwrap().contains("#2"));
    assert_eq!(requests[8], json!({"base":"test-user/first"}));
    // Only access checks reach Graphite; the saved token remains configured.
    assert_eq!(api.finish().len(), 2);
    let config: Value =
        serde_json::from_slice(&std::fs::read(fixture.home.join(".jj-gt/config")).unwrap())
            .unwrap();
    assert_eq!(config["authToken"], TOKEN);
}

#[test]
fn denied_graphite_access_without_github_credentials_fails_before_push() {
    let api = MockApi::new(
        TOKEN,
        vec![(
            AUTH,
            200,
            json!({"githubLogin":"alice", "canSubmitPrs":false}),
        )],
    );
    let fixture = Fixture::new(&api);
    fixture.stack(&api);
    std::fs::write(fixture.home.join("bin/gh"), "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(
        fixture.home.join("bin/gh"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    let output = fixture
        .command(&api, &["submit"])
        .env_remove("GITHUB_TOKEN")
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("GitHub fallback requires GitHub credentials")
    );
    assert!(
        git(
            &fixture.remote,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/test-user"
            ]
        )
        .is_empty()
    );
    api.finish();
}

#[test]
fn sync_refreshes_pr_numbers_without_submitting_or_requiring_github_api_auth() {
    let api = MockApi::new(
        TOKEN,
        vec![info(json!([
            {"prNumber":7, "headRefName":"test-user/first", "state":"OPEN"},
            {"prNumber":8, "headRefName":"test-user/second", "state":"CLOSED"}
        ]))],
    );
    let fixture = Fixture::new(&api);
    fixture.stack(&api);
    // Force GitHub credential discovery to fail, while git uses local transport.
    std::fs::write(fixture.home.join("bin/gh"), "#!/bin/sh\nexit 1\n").unwrap();
    std::fs::set_permissions(
        fixture.home.join("bin/gh"),
        std::fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    success(
        fixture
            .command(&api, &["sync"])
            .env_remove("GITHUB_TOKEN")
            .output()
            .unwrap(),
    );
    assert_eq!(fixture.state()["prs"], json!({"test-user/first":7}));
    assert!(!git(&fixture.work, &["rev-parse", "test-user/second"]).is_empty());
    api.finish();
}

#[test]
fn sync_uses_confirmed_merge_and_submit_retargets_surviving_pr() {
    let setup = MockApi::new(TOKEN, vec![access(), info(json!([])), submitted()]);
    let fixture = Fixture::new(&setup);
    fixture.stack(&setup);
    success(fixture.gt(&setup, &["submit"]));
    setup.finish();
    let submitted_head = git(&fixture.work, &["rev-parse", "test-user/first"]);
    // Model a server-side squash with an extra edit: tree comparison alone
    // cannot recognize this as the original commit having landed.
    std::fs::write(fixture.seed.join("first.txt"), "first\nserver edit\n").unwrap();
    git(&fixture.seed, &["add", "."]);
    git(&fixture.seed, &["commit", "-m", "squash first"]);
    git(&fixture.seed, &["push", "origin", "main"]);
    let merge_sha = git(&fixture.seed, &["rev-parse", "HEAD"]);
    let surviving = json!({"prNumber":2, "headRefName":"test-user/second", "state":"OPEN"});
    let api = MockApi::new(
        TOKEN,
        vec![
            info(json!([
                {"prNumber":1, "headRefName":"test-user/first", "state":"MERGED", "mergeCommitSha":merge_sha,
                 "versions":[{"headSha":submitted_head, "createdAt":"2026-09-21T00:00:00Z"}]},
                surviving.clone()
            ])),
            access(),
            info(json!([surviving])),
            (
                SUBMIT,
                200,
                json!({"prs":[
                    {"status":"updated", "head":"test-user/second", "prNumber":2, "prURL":"https://example.com/2"}
                ]}),
            ),
        ],
    );
    success(fixture.gt(&api, &["sync"]));
    assert!(
        git(
            &fixture.work,
            &[
                "for-each-ref",
                "--format=%(refname)",
                "refs/heads/test-user/first"
            ]
        )
        .is_empty()
    );
    assert_eq!(fixture.state()["prs"], json!({"test-user/second":2}));
    assert_eq!(
        std::fs::read_to_string(fixture.work.join("first.txt")).unwrap(),
        "first\nserver edit\n"
    );
    success(fixture.gt(&api, &["submit"]));
    let requests = api.finish();
    assert_eq!(requests[3]["prs"][0]["base"], "main");
    assert_eq!(requests[3]["prs"][0]["prNumber"], 2);
}

#[test]
fn sync_keeps_unsubmitted_changes_on_a_merged_pr() {
    let setup = MockApi::new(TOKEN, vec![]);
    let fixture = Fixture::new(&setup);
    fixture.stack(&setup);
    let submitted_head = git(&fixture.work, &["rev-parse", "test-user/second"]);
    std::fs::write(fixture.work.join("local.txt"), "keep me\n").unwrap();
    success(fixture.gt(&setup, &["modify"]));
    let local_head = git(&fixture.work, &["rev-parse", "test-user/second"]);
    setup.finish();
    let merge_sha = git(&fixture.seed, &["rev-parse", "HEAD"]);
    let api = MockApi::new(
        TOKEN,
        vec![info(json!([
            {"prNumber":2, "headRefName":"test-user/second", "state":"MERGED", "mergeCommitSha":merge_sha,
             "versions":[{"headSha":submitted_head, "createdAt":"2026-09-21T00:00:00Z"}]}
        ]))],
    );
    success(fixture.gt(&api, &["sync"]));
    assert_eq!(
        git(&fixture.work, &["rev-parse", "test-user/second"]),
        local_head
    );
    assert_eq!(
        std::fs::read_to_string(fixture.work.join("local.txt")).unwrap(),
        "keep me\n"
    );
    api.finish();
}
