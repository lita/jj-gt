use std::path::Path;
use std::process::Command;

use jj_lib::commit::Commit;
use jj_lib::config::StackedConfig;
use jj_lib::object_id::ObjectId as _;
use jj_lib::repo::Repo as _;
use jj_lib::settings::UserSettings;
use jj_lib::workspace::Workspace;
use pollster::FutureExt as _;
use tempfile::TempDir;

struct Fixture(TempDir);

impl Fixture {
    fn new() -> Self {
        let fixture = Self(tempfile::tempdir().unwrap());
        fixture.git(&["init", "-b", "main"]);
        fixture.git(&["config", "user.name", "Modify Test"]);
        fixture.git(&["config", "user.email", "modify@example.invalid"]);
        fixture.write("file.txt", "base\n");
        fixture.write("delete.txt", "keep\n");
        fixture.git(&["add", "."]);
        fixture.git(&["commit", "-m", "base"]);
        fixture.gt(&["init"]);
        fixture.write("file.txt", "feature\n");
        fixture.gt(&["create", "-m", "Feature"]);
        fixture
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn write(&self, path: &str, contents: &str) {
        std::fs::write(self.path().join(path), contents).unwrap();
    }

    fn run(&self, program: &str, args: &[&str]) -> String {
        let output = Command::new(program)
            .args(args)
            .current_dir(self.path())
            .env("USER", "modify-test")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{program} {args:?} failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn git(&self, args: &[&str]) -> String {
        self.run("git", args)
    }

    fn gt(&self, args: &[&str]) -> String {
        self.run(env!("CARGO_BIN_EXE_jj-gt"), args)
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
}

#[test]
fn modify_amends_all_file_changes_and_leaves_a_fresh_empty_commit() {
    for args in [&["modify"][..], &["modify", "--all"][..]] {
        let f = Fixture::new();
        let old_wc = f.wc_commit();
        let old_parent = f.git(&["rev-parse", "modify-test/feature"]);
        let trunk = f.git(&["rev-parse", "main"]);
        f.write("file.txt", "amended\n");
        f.write("new.txt", "new file\n");
        std::fs::remove_file(f.path().join("delete.txt")).unwrap();

        f.gt(args);

        let parent = f.git(&["rev-parse", "modify-test/feature"]);
        assert_ne!(parent, old_parent);
        assert_eq!(f.git(&["show", "modify-test/feature:file.txt"]), "amended");
        assert_eq!(f.git(&["show", "modify-test/feature:new.txt"]), "new file");
        assert_eq!(
            f.git(&["ls-tree", "--name-only", "modify-test/feature"]),
            "file.txt\nnew.txt"
        );
        assert_eq!(f.git(&["show", "-s", "--format=%B", &parent]), "Feature");
        assert_eq!(f.git(&["rev-parse", "main"]), trunk);
        assert_eq!(f.git(&["rev-parse", "modify-test/feature^"]), trunk);

        let wc = f.wc_commit();
        assert_ne!(wc.id(), old_wc.id());
        assert_ne!(wc.change_id(), old_wc.change_id());
        assert!(wc.description().is_empty());
        assert_eq!(wc.parent_ids().len(), 1);
        assert_eq!(wc.parent_ids()[0].hex(), parent);
        assert_eq!(
            f.git(&["rev-parse", &format!("{}^{{tree}}", wc.id().hex())]),
            f.git(&["rev-parse", "modify-test/feature^{tree}"])
        );
        assert_eq!(f.git(&["rev-parse", "HEAD"]), parent);
        assert_eq!(f.git(&["status", "--porcelain"]), "");
        assert_eq!(
            std::fs::read_to_string(f.path().join("file.txt")).unwrap(),
            "amended\n"
        );
        assert_eq!(
            std::fs::read_to_string(f.path().join("new.txt")).unwrap(),
            "new file\n"
        );
        assert!(!f.path().join("delete.txt").exists());
    }
}

#[test]
fn modify_restacks_descendants_and_keeps_the_working_copy_on_the_amended_branch() {
    let f = Fixture::new();
    f.write("child.txt", "child\n");
    f.gt(&["create", "-m", "Child"]);
    let old_child = f.git(&["rev-parse", "modify-test/child"]);
    f.gt(&["checkout", "modify-test/feature"]);
    f.write("file.txt", "amended\n");

    f.gt(&["modify"]);

    let parent = f.git(&["rev-parse", "modify-test/feature"]);
    assert_ne!(f.git(&["rev-parse", "modify-test/child"]), old_child);
    assert_eq!(f.git(&["rev-parse", "modify-test/child^"]), parent);
    assert_eq!(f.git(&["show", "modify-test/child:file.txt"]), "amended");
    assert_eq!(f.git(&["show", "modify-test/child:child.txt"]), "child");
    assert_eq!(
        f.git(&["show", "-s", "--format=%B", "modify-test/child"]),
        "Child"
    );
    assert_eq!(f.wc_commit().parent_ids()[0].hex(), parent);
    assert_eq!(f.git(&["status", "--porcelain"]), "");
    assert!(!f.path().join("child.txt").exists());
}
