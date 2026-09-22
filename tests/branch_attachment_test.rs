//! Live branch attachment states and the untracked-branch write guard.

use std::path::Path;
use std::process::Command;

use tempfile::tempdir;
use tokensave::tokensave::{AutoSyncScope, BranchAttachment, BranchDrift, TokenSave};

fn git(root: &Path, args: &[&str]) {
    git_with_commit_date(root, args, None);
}

fn git_commit(root: &Path, message: &str, date: &str) {
    git_with_commit_date(root, &["commit", "-m", message], Some(date));
}

fn git_with_commit_date(root: &Path, args: &[&str], date: Option<&str>) {
    let mut command = Command::new("git");
    command
        .args(args)
        .current_dir(root)
        .env("XDG_CONFIG_HOME", root.join(".xdg-config"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "TokenSave Test")
        .env("GIT_AUTHOR_EMAIL", "tokensave@example.com")
        .env("GIT_COMMITTER_NAME", "TokenSave Test")
        .env("GIT_COMMITTER_EMAIL", "tokensave@example.com");
    if let Some(date) = date {
        command
            .env("GIT_AUTHOR_DATE", date)
            .env("GIT_COMMITTER_DATE", date);
    }
    let out = command.output().expect("run git");
    assert!(
        out.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn init_git(root: &Path) {
    git(root, &["init", "-b", "master"]);
    std::fs::write(root.join(".gitignore"), ".tokensave/\n").unwrap();
    std::fs::write(root.join("base.rs"), "fn base() {}").unwrap();
    git(root, &["add", "-A"]);
    git_commit(root, "base", "2000-01-01T00:00:00 +0000");
}

async fn tracked_parent_with_untracked_child() -> (tempfile::TempDir, TokenSave) {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_git(root);

    let master = TokenSave::init(root).await.unwrap();
    master.sync().await.unwrap();
    drop(master);

    git(root, &["checkout", "-b", "parent"]);
    std::fs::write(root.join("parent_only.rs"), "fn parent_only() {}").unwrap();
    git(root, &["add", "-A"]);
    git_commit(root, "parent", "2000-01-02T00:00:00 +0000");
    assert!(
        tokensave::branch::track_branch_copy(root, &root.join(".tokensave"), "parent")
            .await
            .unwrap(),
        "precondition: parent must become tracked"
    );

    let parent = TokenSave::open(root).await.unwrap();
    parent.sync().await.unwrap();

    git(root, &["checkout", "-b", "child"]);
    std::fs::write(root.join("child_only.rs"), "fn child_only() {}").unwrap();
    (tmp, parent)
}

#[tokio::test]
async fn current_tracked_branch_is_current() {
    let tmp = tempdir().unwrap();
    init_git(tmp.path());
    let cg = TokenSave::init(tmp.path()).await.unwrap();

    assert_eq!(cg.branch_attachment(), BranchAttachment::Current);
}

#[tokio::test]
async fn tracked_checkout_is_a_tracked_mismatch() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_git(root);
    let master = TokenSave::init(root).await.unwrap();
    master.sync().await.unwrap();
    drop(master);

    git(root, &["checkout", "-b", "feature"]);
    assert!(
        tokensave::branch::track_branch_copy(root, &root.join(".tokensave"), "feature")
            .await
            .unwrap()
    );
    git(root, &["checkout", "master"]);
    let cg = TokenSave::open(root).await.unwrap();
    git(root, &["checkout", "feature"]);

    assert_eq!(
        cg.branch_attachment(),
        BranchAttachment::TrackedMismatch(BranchDrift {
            serving: "master".to_string(),
            working_tree: "feature".to_string(),
        })
    );
}

#[tokio::test]
async fn metadata_free_database_is_shared_across_branch_names() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    std::fs::write(root.join(".gitignore"), ".tokensave/\n").unwrap();
    std::fs::write(root.join("base.rs"), "fn base() {}").unwrap();

    let initial = TokenSave::init(root).await.unwrap();
    initial.sync().await.unwrap();
    drop(initial);
    assert!(!root.join(".tokensave/branch-meta.json").exists());

    git(root, &["init", "-b", "master"]);
    git(root, &["add", "-A"]);
    git_commit(root, "base", "2000-01-01T00:00:00 +0000");
    let cg = TokenSave::open(root).await.unwrap();
    git(root, &["checkout", "-b", "legacy-child"]);
    std::fs::write(root.join("legacy_child.rs"), "fn legacy_child() {}").unwrap();

    assert_eq!(
        cg.branch_attachment(),
        BranchAttachment::SharedSingleDatabase {
            working_tree: "legacy-child".to_string(),
        }
    );
    cg.sync().await.unwrap();
    assert!(cg
        .get_all_files()
        .await
        .unwrap()
        .iter()
        .any(|file| file.path == "legacy_child.rs"));
}

#[tokio::test]
async fn default_only_metadata_is_shared_with_an_untracked_branch() {
    let tmp = tempdir().unwrap();
    let root = tmp.path();
    init_git(root);
    let cg = TokenSave::init(root).await.unwrap();
    cg.sync().await.unwrap();

    git(root, &["checkout", "-b", "default-child"]);
    std::fs::write(root.join("default_child.rs"), "fn default_child() {}").unwrap();

    assert_eq!(
        cg.branch_attachment(),
        BranchAttachment::SharedSingleDatabase {
            working_tree: "default-child".to_string(),
        }
    );
    cg.sync().await.unwrap();
    assert!(cg
        .get_all_files()
        .await
        .unwrap()
        .iter()
        .any(|file| file.path == "default_child.rs"));
}

#[tokio::test]
async fn untracked_child_of_a_tracked_branch_is_unsafe() {
    let (tmp, parent) = tracked_parent_with_untracked_child().await;

    assert_eq!(
        parent.branch_attachment(),
        BranchAttachment::Untracked {
            serving: "parent".to_string(),
            working_tree: "child".to_string(),
            fallback: "parent".to_string(),
        }
    );
    assert_eq!(
        parent.find_stale_files_bounded().await,
        AutoSyncScope::UntrackedBranch {
            serving: "parent".to_string(),
            working_tree: "child".to_string(),
            fallback: "parent".to_string(),
        }
    );

    let parent_db = TokenSave::open_read_only(tmp.path(), Some("parent"))
        .await
        .unwrap();
    let indexed: Vec<String> = parent_db
        .get_all_files()
        .await
        .unwrap()
        .into_iter()
        .map(|file| file.path)
        .collect();
    assert!(
        !indexed.iter().any(|path| path == "child_only.rs"),
        "the tracked parent database must not contain child-only files: {indexed:?}"
    );
}

#[tokio::test]
async fn explicit_sync_refuses_an_untracked_multi_database_branch() {
    let (tmp, parent) = tracked_parent_with_untracked_child().await;

    let error = parent
        .sync()
        .await
        .expect_err("sync must refuse")
        .to_string();
    assert!(error.contains("child"), "working branch missing: {error}");
    assert!(
        error.contains("tokensave_reopen"),
        "recovery tool missing: {error}"
    );

    let parent_db = TokenSave::open_read_only(tmp.path(), Some("parent"))
        .await
        .unwrap();
    assert!(parent_db
        .get_all_files()
        .await
        .unwrap()
        .iter()
        .all(|file| file.path != "child_only.rs"));
}

#[tokio::test]
async fn explicit_full_index_refuses_an_untracked_multi_database_branch() {
    let (tmp, parent) = tracked_parent_with_untracked_child().await;

    let error = match parent.index_all().await {
        Ok(_) => panic!("full index must refuse"),
        Err(error) => error.to_string(),
    };
    assert!(error.contains("child"), "working branch missing: {error}");
    assert!(
        error.contains("tokensave_reopen"),
        "recovery tool missing: {error}"
    );

    let parent_db = TokenSave::open_read_only(tmp.path(), Some("parent"))
        .await
        .unwrap();
    assert!(parent_db
        .get_all_files()
        .await
        .unwrap()
        .iter()
        .all(|file| file.path != "child_only.rs"));
}
