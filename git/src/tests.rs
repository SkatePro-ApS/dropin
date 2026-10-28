//! Parsers on fixed text; the git steps on real repos: a bare origin and two clones in a temp folder.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::*;

// ---- parsers ----

#[test]
fn parse_log_reads_fields_and_body() {
    let text = "abc\x1fa\x1fAnn\x1f1700000000\x1fFix it\x1fann@x.dk\x1f14/11/2023 at 23:13\x1fWhy:\nbecause\n\x1e\n\
                def\x1fd\x1fBo\x1f1700000001\x1fAdd\x1fbo@x.dk\x1f14/11/2023 at 23:13\x1f\x1e\n";
    let log = parse_log(text);
    assert_eq!(log.len(), 2);
    assert_eq!(log[0].sha, "abc");
    assert_eq!(log[0].at, 1_700_000_000_000);
    assert_eq!(log[0].subject, "Fix it");
    assert_eq!(log[0].body, "Why:\nbecause");
    assert_eq!(log[1].author, "Bo");
    assert_eq!(log[1].body, "");
}

#[test]
fn parse_name_status_handles_renames() {
    let files = parse_name_status("M\0a.txt\0R100\0old/b.rs\0new/b.rs\0D\0c\0");
    assert_eq!(files.len(), 3);
    assert_eq!(files[1], FileChange { status: "R".into(), path: "new/b.rs".into(), from: Some("old/b.rs".into()) });
    assert_eq!(files[2].status, "D");
}

#[test]
fn tree_order_puts_top_level_files_last() {
    let f = |p: &str| FileChange { status: "M".into(), path: p.into(), from: None };
    let order: Vec<String> = in_tree_order(vec![f("z.txt"), f("b/x.rs"), f("A/y.rs"), f("a.txt"), f("b/W.rs")])
        .into_iter()
        .map(|f| f.path)
        .collect();
    assert_eq!(order, ["A/y.rs", "b/W.rs", "b/x.rs", "a.txt", "z.txt"]);
}

#[test]
fn small_helpers() {
    let v = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert_eq!(overlap_of(&v(&["Src/A.php", "b"]), &v(&["src/a.php", "c"])), v(&["Src/A.php"]));
    assert_eq!(missing_from(&v(&["x", "y", "x"]), &v(&["x", "y"])), v(&["x"]));
    let id = "a".repeat(40);
    assert_eq!(patch_ids_of(&format!("{id} {}\nnot an id\n", "b".repeat(40))), vec![id]);
    assert_eq!(tail("one\r\ntwo\n\nthree\n", 2), "two\nthree");
    assert_eq!(tail("Rebasing (1/1)\r\u{1b}[KSuccessfully rebased\n", 5), "Rebasing (1/1)\nSuccessfully rebased");
    assert_eq!(fit("abcdef", 4), "abc…");
}

// ---- real repos ----

static NEXT: AtomicUsize = AtomicUsize::new(0);

/// A bare origin with one commit on main, and clones `a` and `b` tracking it.
struct Repos {
    root: PathBuf,
    a: PathBuf,
    b: PathBuf,
}

impl Drop for Repos {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn sh(dir: &Path, args: &[&str]) -> String {
    git(dir, args).unwrap().must(&args.join(" ")).unwrap()
}

fn write(dir: &Path, file: &str, text: &str) {
    let path = dir.join(file);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn commit(dir: &Path, file: &str, text: &str, subject: &str) {
    write(dir, file, text);
    sh(dir, &["add", file]);
    sh(dir, &["commit", "-q", "-m", subject]);
}

/// Clones with the settings passed to the clone itself, so the person's global config (autocrlf,
/// signing, hooks) never reaches the first checkout.
fn clone(root: &Path, name: &str) -> PathBuf {
    sh(root, &[
        "clone",
        "-q",
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.com",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "core.autocrlf=false",
        "origin.git",
        name,
    ]);
    // Clone protection refuses a hooks path on the clone itself.
    let dir = root.join(name);
    sh(&dir, &["config", "core.hooksPath", "no-hooks"]);
    dir
}

fn repos() -> Repos {
    let n = NEXT.fetch_add(1, Ordering::SeqCst);
    let root = std::env::temp_dir().join(format!("dropin_git-test-{}-{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    sh(&root, &["init", "-q", "--bare", "-b", "main", "origin.git"]);
    let seed = clone(&root, "seed");
    commit(&seed, "shared.txt", "one\ntwo\nthree\n", "Start");
    sh(&seed, &["push", "-q", "-u", "origin", "main"]);
    let (a, b) = (clone(&root, "a"), clone(&root, "b"));
    Repos { root, a, b }
}

fn head(dir: &Path) -> String {
    sh(dir, &["rev-parse", "HEAD"]).trim().to_string()
}

fn subjects(o: &Outgoing) -> Vec<&str> {
    o.commits.iter().map(|c| c.subject.as_str()).collect()
}

#[test]
fn outgoing_lists_both_ways_after_fetch() {
    let r = repos();
    commit(&r.a, "a1.txt", "x", "A one");
    commit(&r.a, "a2.txt", "x", "A two");
    commit(&r.b, "b1.txt", "x", "B one");
    sh(&r.b, &["push", "-q"]);

    let o = read_outgoing(&r.a, false, None).unwrap();
    assert_eq!((o.branch.as_str(), o.upstream.as_str(), o.remote.as_str()), ("main", "origin/main", "origin"));
    assert_eq!((o.ahead, o.behind), (2, 0));
    assert_eq!(subjects(&o), ["A two", "A one"]);
    assert_eq!(o.head, head(&r.a));

    let o = read_outgoing(&r.a, true, None).unwrap();
    assert_eq!((o.ahead, o.behind), (2, 1));
    assert_eq!(o.incoming[0].subject, "B one");
    assert_eq!(o.history[0].subject, "B one");

    let files = files_of(&r.a, &o.commits[0].sha).unwrap();
    assert_eq!(files, [FileChange { status: "A".into(), path: "a2.txt".into(), from: None }]);
    let (diff, cut) = diff_of(&r.a, &o.commits[0].sha, &files[0]).unwrap();
    assert!(diff.contains("+x") && !cut);

    let p = read_outgoing(&r.a, false, Some("HEAD~1")).unwrap();
    assert_eq!((subjects(&p), p.preview.as_deref()), (vec!["A two"], Some("HEAD~1")));
}

#[test]
fn search_finds_upstream_commits_beyond_the_history() {
    let r = repos();
    for i in 0..25 {
        commit(&r.b, &format!("f{i}.txt"), "x", &format!("Filler {i}"));
    }
    write(&r.b, "pay.txt", "x");
    sh(&r.b, &["add", "pay.txt"]);
    sh(&r.b, &["commit", "-q", "-m", "Fix checkout", "-m", "The Payment step lost the basket."]);
    sh(&r.b, &["push", "-q"]);
    sh(&r.a, &["fetch", "-q"]);

    // "Start" is 26 commits back, beyond the 20 the history shows.
    let found = search_history(&r.a, "origin/main", "start", None, None).unwrap();
    assert_eq!((found.matched, found.searched, found.whole), (1, 27, true));
    assert_eq!(found.commits[0].subject, "Start");

    // Every word must match, in the subject, the body or the author.
    let found = search_history(&r.a, "origin/main", "PAYMENT checkout test", None, None).unwrap();
    assert_eq!(found.commits.iter().map(|c| c.subject.as_str()).collect::<Vec<_>>(), ["Fix checkout"]);
    assert_eq!(search_history(&r.a, "origin/main", "payment nothing", None, None).unwrap().matched, 0);

    let sha = found.commits[0].sha.clone();
    assert_eq!(search_history(&r.a, "origin/main", &sha[..7], None, None).unwrap().commits[0].sha, sha);
    assert_eq!(search_history(&r.a, "origin/main", "filler", None, None).unwrap().matched, 25);
    assert!(search_history(&r.a, "--all", "x", None, None).is_err());
}

#[test]
fn search_keeps_to_the_dates() {
    let r = repos();
    let dated = |file: &str, subject: &str, date: &str| {
        write(&r.b, file, "x");
        sh(&r.b, &["add", file]);
        sh(&r.b, &["commit", "-q", "-m", subject, "--date", date]);
    };
    dated("a.txt", "March fix", "2020-03-01T12:00:00Z");
    dated("b.txt", "April fix", "2020-04-15T12:00:00Z");
    sh(&r.b, &["push", "-q"]);
    sh(&r.a, &["fetch", "-q"]);
    let subjects = |query: &str, since: Option<i64>, until: Option<i64>| {
        let found = search_history(&r.a, "origin/main", query, since, until).unwrap();
        found.commits.into_iter().map(|c| c.subject).collect::<Vec<_>>()
    };
    let (march1, april1, april15) = (1_583_020_800_000, 1_585_699_200_000, 1_586_952_000_000);

    assert_eq!(subjects("fix", Some(march1), Some(april1)), ["March fix"]);
    assert_eq!(subjects("fix", Some(april1), None), ["April fix"]);
    // No words: every commit in the range ("Start" is from today); `since` is included and `until` is not.
    assert_eq!(subjects("", Some(april15), None), ["April fix", "Start"]);
    assert_eq!(subjects("", Some(march1), Some(april15)), ["March fix"]);
    assert!(subjects("", None, Some(march1)).is_empty());
}

#[test]
fn push_sends_the_reviewed_head_only() {
    let r = repos();
    commit(&r.a, "a1.txt", "x", "A one");
    let o = read_outgoing(&r.a, false, None).unwrap();

    commit(&r.a, "a2.txt", "x", "A two, not reviewed");
    let moved = push(&r.a, &o).unwrap();
    assert_eq!(moved.notice.tone, Tone::Warning);
    sh(&r.a, &["reset", "-q", "--hard", "HEAD~1"]);

    let pushed = push(&r.a, &o).unwrap();
    assert_eq!(pushed.notice.tone, Tone::Success, "{:?}", pushed.notice);
    assert_eq!(sh(&r.root.join("origin.git"), &["rev-parse", "main"]).trim(), o.head);
}

#[test]
fn push_refused_by_a_moved_upstream_asks_for_a_refetch() {
    let r = repos();
    commit(&r.b, "b1.txt", "x", "B one");
    sh(&r.b, &["push", "-q"]);
    commit(&r.a, "a1.txt", "x", "A one");
    // Read without a fetch: the list thinks the push can go.
    let o = read_outgoing(&r.a, false, None).unwrap();
    let pushed = push(&r.a, &o).unwrap();
    assert_eq!(pushed.notice.tone, Tone::Warning, "{:?}", pushed.notice);
    assert!(pushed.refetch);
}

#[test]
fn rebase_moves_the_branch_and_keeps_staged_work_staged() {
    let r = repos();
    let base = r.root.join("wt");
    commit(&r.b, "b1.txt", "x", "B one");
    sh(&r.b, &["push", "-q"]);
    commit(&r.a, "a1.txt", "x", "A one");
    write(&r.a, "staged.txt", "s");
    sh(&r.a, &["add", "staged.txt"]);
    write(&r.a, "a1.txt", "edited, not staged");

    let o = read_outgoing(&r.a, true, None).unwrap();
    let rebased = rebase_onto(&r.a, &o, &base).unwrap();
    let notice = rebased.notice.unwrap();
    assert_eq!(notice.tone, Tone::Success, "{notice:?}");
    assert!(rebased.conflict.is_none());

    let o = read_outgoing(&r.a, false, None).unwrap();
    assert_eq!((o.ahead, o.behind), (1, 0));
    assert_eq!(sh(&r.a, &["diff", "--cached", "--name-only"]).trim(), "staged.txt");
    assert_eq!(sh(&r.a, &["diff", "--name-only"]).trim(), "a1.txt");
    assert!(sh(&r.a, &["worktree", "list"]).lines().count() == 1, "the worktree is gone");
}

#[test]
fn rebase_with_nothing_of_its_own_just_moves() {
    let r = repos();
    commit(&r.b, "b1.txt", "x", "B one");
    sh(&r.b, &["push", "-q"]);
    let o = read_outgoing(&r.a, true, None).unwrap();
    let notice = rebase_onto(&r.a, &o, &r.root.join("wt")).unwrap().notice.unwrap();
    assert_eq!(notice.tone, Tone::Success, "{notice:?}");
    assert_eq!(head(&r.a), head(&r.b));
}

#[test]
fn rebase_stops_when_an_incoming_file_has_local_changes() {
    let r = repos();
    commit(&r.b, "shared.txt", "one\nTWO\nthree\n", "B edits shared");
    sh(&r.b, &["push", "-q"]);
    commit(&r.a, "a1.txt", "x", "A one");
    write(&r.a, "shared.txt", "local edit\n");
    let before = head(&r.a);

    let o = read_outgoing(&r.a, true, None).unwrap();
    let notice = rebase_onto(&r.a, &o, &r.root.join("wt")).unwrap().notice.unwrap();
    assert_eq!(notice.tone, Tone::Error);
    assert_eq!(notice.detail, "shared.txt");
    assert_eq!(head(&r.a), before);
}

/// The same commit is on both sides (pushed from another clone, then fetched) and the person
/// edits its file again: the rebase leaves that file as it is, so it runs and drops the copy.
#[test]
fn rebase_runs_when_the_incoming_file_already_matches_head() {
    let r = repos();
    commit(&r.b, "shared.txt", "one\nTWO\nthree\n", "Same change");
    sh(&r.b, &["push", "-q"]);
    commit(&r.b, "b1.txt", "x", "B one");
    sh(&r.b, &["push", "-q"]);
    commit(&r.a, "shared.txt", "one\nTWO\nthree\n", "Same change");
    commit(&r.a, "a1.txt", "x", "A one");
    write(&r.a, "shared.txt", "one\nTWO\nthree\nlocal edit\n");

    let o = read_outgoing(&r.a, true, None).unwrap();
    let notice = rebase_onto(&r.a, &o, &r.root.join("wt")).unwrap().notice.unwrap();
    assert_ne!(notice.tone, Tone::Error, "{notice:?}");
    let o = read_outgoing(&r.a, false, None).unwrap();
    assert_eq!((subjects(&o), o.behind), (vec!["A one"], 0));
    assert_eq!(std::fs::read_to_string(r.a.join("shared.txt")).unwrap(), "one\nTWO\nthree\nlocal edit\n");
}

/// Both sides change one line: the rebase stops, its worktree is kept back at the branch's
/// commit, someone finishes it there, and adopt moves the branch onto the result.
#[test]
fn conflict_is_kept_then_adopted() {
    let r = repos();
    commit(&r.b, "shared.txt", "one\nB\nthree\n", "B edits shared");
    sh(&r.b, &["push", "-q"]);
    commit(&r.a, "shared.txt", "one\nA\nthree\n", "A edits shared");
    commit(&r.a, "a2.txt", "x", "A two");
    let from = head(&r.a);

    let o = read_outgoing(&r.a, true, None).unwrap();
    let rebased = rebase_onto(&r.a, &o, &r.root.join("wt")).unwrap();
    let c = rebased.conflict.expect("a conflict");
    assert_eq!(c.files, ["shared.txt"]);
    assert_eq!((c.from.as_str(), c.branch.as_str()), (from.as_str(), "main"));
    let wt = PathBuf::from(&c.worktree);
    assert_eq!(head(&wt), from, "the worktree is back at the branch's commit");
    assert_eq!(head(&r.a), from, "the branch is untouched");

    // Not done yet: adopt refuses and keeps the worktree.
    assert_eq!(adopt(&r.a, &c).unwrap().notice.tone, Tone::Error);

    // Resolve the way Claude would.
    let ran = git(&wt, &["rebase", &c.onto]).unwrap();
    assert!(!ran.ok());
    write(&wt, "shared.txt", "one\nA and B\nthree\n");
    sh(&wt, &["add", "shared.txt"]);
    sh(&wt, &["-c", "core.editor=true", "rebase", "--continue"]);

    let adopted = adopt(&r.a, &c).unwrap();
    assert_eq!(adopted.notice.tone, Tone::Success, "{:?}", adopted.notice);
    assert_eq!(adopted.to.as_deref(), Some(head(&r.a).as_str()));
    assert!(adopted.dropped.is_empty());
    assert_eq!(std::fs::read_to_string(r.a.join("shared.txt")).unwrap(), "one\nA and B\nthree\n");
    let o = read_outgoing(&r.a, false, None).unwrap();
    assert_eq!((o.ahead, o.behind), (2, 0));

    remove_worktree(&r.a, &c.worktree);
    assert!(!wt.exists());
}

#[test]
fn adopt_refuses_conflict_markers_and_reports_dropped_commits() {
    let r = repos();
    commit(&r.b, "shared.txt", "one\nB\nthree\n", "B edits shared");
    sh(&r.b, &["push", "-q"]);
    commit(&r.a, "shared.txt", "one\nA\nthree\n", "A edits shared");
    commit(&r.a, "a2.txt", "x", "A two");

    let o = read_outgoing(&r.a, true, None).unwrap();
    let c = rebase_onto(&r.a, &o, &r.root.join("wt")).unwrap().conflict.unwrap();
    let wt = PathBuf::from(&c.worktree);

    // Markers committed as they stood.
    assert!(!git(&wt, &["rebase", &c.onto]).unwrap().ok());
    sh(&wt, &["add", "shared.txt"]);
    sh(&wt, &["-c", "core.editor=true", "rebase", "--continue"]);
    let before = head(&r.a);
    let adopted = adopt(&r.a, &c).unwrap();
    assert_eq!(adopted.notice.tone, Tone::Error);
    assert_eq!(adopted.notice.detail, format!("{}:shared.txt", head(&wt)));
    assert_eq!(head(&r.a), before);

    // Start over and skip the conflicting commit: adopt goes ahead and says what was dropped.
    sh(&wt, &["checkout", "-q", "--detach", &c.from]);
    assert!(!git(&wt, &["rebase", &c.onto]).unwrap().ok());
    sh(&wt, &["rebase", "--skip"]);
    let adopted = adopt(&r.a, &c).unwrap();
    assert_eq!(adopted.notice.tone, Tone::Warning, "{:?}", adopted.notice);
    assert_eq!(adopted.dropped, ["A edits shared"]);
    remove_worktree(&r.a, &c.worktree);
}

#[test]
fn outgoing_without_upstream_says_how_to_set_one() {
    let r = repos();
    sh(&r.a, &["checkout", "-q", "-b", "topic"]);
    let e = read_outgoing(&r.a, false, None).unwrap_err();
    assert!(e.contains("git push -u origin topic"), "{e}");
    sh(&r.a, &["checkout", "-q", "--detach"]);
    assert!(read_outgoing(&r.a, false, None).unwrap_err().contains("detached"));
}

// ---- branches ----

#[test]
fn parse_branches_reads_tracking_and_the_remote_default() {
    let s = |f: &[&str]| f.join("\x1f");
    let text = [
        s(&["refs/heads/feat/x", "origin/feat/x", "ahead 2, behind 1", " ", "abc", "1700000000", "", "C:/wt", "Feat"]),
        s(&["refs/heads/main", "origin/main", "", "*", "def", "1700000001", "", "C:/repo", "Main"]),
        s(&["refs/heads/old", "origin/old", "gone", " ", "123", "1", "", "", "Old"]),
        s(&["refs/remotes/origin/HEAD", "", "", " ", "def", "1", "refs/remotes/origin/main", "", "Main"]),
        s(&["refs/remotes/origin/main", "", "", " ", "def", "1", "", "", "Main"]),
    ]
    .join("\n");
    let b = parse_branches(&text);
    assert_eq!(b.head, "main");
    assert_eq!((b.local[0].ahead, b.local[0].behind, b.local[0].worktree.as_str()), (2, 1, "C:/wt"));
    assert!(b.local[1].current && b.local[1].worktree.is_empty());
    assert!(b.local[2].gone);
    assert_eq!(b.remote.len(), 1);
    assert_eq!((b.remote[0].remote.as_str(), b.remote[0].name.as_str(), b.remote[0].default), ("origin", "main", true));
}

fn names(list: &[Branch]) -> Vec<&str> {
    list.iter().map(|b| b.name.as_str()).collect()
}

#[test]
fn branches_list_switch_and_track_a_remote_one() {
    let r = repos();
    commit(&r.b, "b.txt", "x", "Only on b");
    sh(&r.b, &["switch", "-q", "-c", "feat/remote"]);
    sh(&r.b, &["push", "-q", "-u", "origin", "feat/remote"]);
    sh(&r.a, &["fetch", "-q"]);
    sh(&r.a, &["branch", "solo"]);
    commit(&r.a, "a.txt", "x", "A one");

    let b = list_branches(&r.a).unwrap();
    assert_eq!(b.head, "main");
    assert_eq!(names(&b.local), ["main", "solo"]);
    assert_eq!(b.local[0].ahead, 1);
    assert_eq!(names(&b.remote), ["feat/remote", "main"]);
    assert!(b.remote[1].default && !b.remote[0].default);

    let n = switch_branch(&r.a, "feat/remote", "origin").unwrap();
    assert_eq!(n.tone, Tone::Success, "{n:?}");
    let b = list_branches(&r.a).unwrap();
    assert_eq!(b.head, "feat/remote");
    assert_eq!(b.local[0].upstream, "origin/feat/remote");
    // git's "Your branch is ahead" line is no carried change.
    let n = switch_branch(&r.a, "main", "").unwrap();
    assert_eq!((n.tone, n.text.as_str()), (Tone::Success, "Switched to main."), "{n:?}");
    assert_eq!(list_branches(&r.a).unwrap().head, "main");

    sh(&r.a, &["branch", "carry"]);
    write(&r.a, "a.txt", "changed\n");
    let n = switch_branch(&r.a, "carry", "").unwrap();
    assert!(n.text.ends_with("your uncommitted changes came along."), "{n:?}");
    assert!(n.detail.contains("a.txt"), "{n:?}");
}

#[test]
fn switch_refuses_when_a_changed_file_is_in_the_way() {
    let r = repos();
    sh(&r.a, &["switch", "-q", "-c", "other"]);
    commit(&r.a, "shared.txt", "other\n", "Other");
    sh(&r.a, &["switch", "-q", "main"]);
    write(&r.a, "shared.txt", "mine\n");

    let n = switch_branch(&r.a, "other", "").unwrap();
    assert_eq!(n.tone, Tone::Error);
    assert!(n.detail.contains("shared.txt"), "{n:?}");
    assert_eq!(list_branches(&r.a).unwrap().head, "main");
    assert_eq!(std::fs::read_to_string(r.a.join("shared.txt")).unwrap(), "mine\n");
}

#[test]
fn create_never_tracks_its_base_and_checks_the_name() {
    let r = repos();
    let n = create_branch(&r.a, "feat/new", "origin/main", true).unwrap();
    assert_eq!(n.tone, Tone::Success, "{n:?}");
    let b = list_branches(&r.a).unwrap();
    assert_eq!(b.head, "feat/new");
    assert_eq!(b.local[0].upstream, "");
    assert_eq!(create_branch(&r.a, "bad..name", "HEAD", false).unwrap().tone, Tone::Error);
    assert_eq!(create_branch(&r.a, "-x", "HEAD", false).unwrap().tone, Tone::Error);
    assert_eq!(create_branch(&r.a, "main", "HEAD", false).unwrap().tone, Tone::Error);
    assert_eq!(create_branch(&r.a, "kept", "main", false).unwrap().tone, Tone::Success);
    assert_eq!(list_branches(&r.a).unwrap().head, "feat/new");
}

#[test]
fn rename_and_delete_ask_before_losing_commits() {
    let r = repos();
    sh(&r.a, &["switch", "-q", "-c", "work"]);
    commit(&r.a, "w.txt", "x", "Work");
    sh(&r.a, &["switch", "-q", "main"]);

    assert_eq!(rename_branch(&r.a, "work", "work2").unwrap().tone, Tone::Success);
    assert_eq!(names(&list_branches(&r.a).unwrap().local), ["main", "work2"]);

    let n = delete_branch(&r.a, "work2", false).unwrap();
    assert_eq!(n.tone, Tone::Warning, "{n:?}");
    assert_eq!(delete_branch(&r.a, "work2", true).unwrap().tone, Tone::Success);
    assert_eq!(delete_branch(&r.a, "main", true).unwrap().tone, Tone::Error);
    assert_eq!(names(&list_branches(&r.a).unwrap().local), ["main"]);
}

#[test]
fn delete_on_the_remote_leaves_the_local_one_gone_and_keeps_the_default() {
    let r = repos();
    sh(&r.a, &["switch", "-q", "-c", "feat/x"]);
    commit(&r.a, "x.txt", "x", "X");
    sh(&r.a, &["push", "-q", "-u", "origin", "feat/x"]);
    sh(&r.a, &["switch", "-q", "main"]);

    let n = delete_remote_branch(&r.a, "origin", "feat/x").unwrap();
    assert_eq!(n.tone, Tone::Success, "{n:?}");
    let b = list_branches(&r.a).unwrap();
    assert_eq!(names(&b.remote), ["main"]);
    assert!(b.local[0].gone);
    assert_eq!(delete_remote_branch(&r.a, "origin", "main").unwrap().tone, Tone::Error);
}

#[test]
fn fetch_all_prunes_branches_deleted_on_the_remote() {
    let r = repos();
    sh(&r.b, &["push", "-q", "origin", "main:refs/heads/temp"]);
    assert_eq!(fetch_all(&r.a).unwrap().tone, Tone::Success);
    assert_eq!(names(&list_branches(&r.a).unwrap().remote), ["main", "temp"]);
    sh(&r.b, &["push", "-q", "origin", "--delete", "temp"]);
    fetch_all(&r.a).unwrap();
    assert_eq!(names(&list_branches(&r.a).unwrap().remote), ["main"]);
}

#[test]
fn folders_rename_and_delete_every_branch_below() {
    let r = repos();
    for name in ["sw/a", "sw/deep/b", "swx"] {
        sh(&r.a, &["branch", name]);
    }
    sh(&r.a, &["switch", "-q", "-c", "sw/work"]);
    commit(&r.a, "w.txt", "x", "Work");
    assert_eq!(branches_in_folder(&r.a, "sw").unwrap(), ["sw/a", "sw/deep/b", "sw/work"]);

    // A taken name or a bad one changes nothing.
    sh(&r.a, &["branch", "new/a"]);
    let n = rename_folder(&r.a, "sw", "new").unwrap();
    assert_eq!((n.tone, n.detail.as_str()), (Tone::Error, "new/a"), "{n:?}");
    assert_eq!(rename_folder(&r.a, "sw", "bad..x").unwrap().tone, Tone::Error);
    assert_eq!(rename_folder(&r.a, "sw", "swx").unwrap().tone, Tone::Error);
    assert_eq!(branches_in_folder(&r.a, "sw").unwrap().len(), 3);

    let n = rename_folder(&r.a, "sw", "spider/web").unwrap();
    assert_eq!(n.tone, Tone::Success, "{n:?}");
    assert_eq!(branches_in_folder(&r.a, "spider").unwrap(), ["spider/web/a", "spider/web/deep/b", "spider/web/work"]);
    assert_eq!(list_branches(&r.a).unwrap().head, "spider/web/work");

    // The checked-out branch stays; unmerged ones wait for a yes.
    sh(&r.a, &["switch", "-q", "-c", "spider/web/other"]);
    sh(&r.a, &["switch", "-q", "spider/web/work"]);
    let below = branches_in_folder(&r.a, "spider").unwrap();
    let d = delete_branches(&r.a, &below, false).unwrap();
    assert_eq!(d.notice.tone, Tone::Success, "{d:?}");
    assert!(d.notice.text.contains("Kept spider/web/work"), "{d:?}");
    sh(&r.a, &["switch", "-q", "main"]);
    let d = delete_branches(&r.a, &branches_in_folder(&r.a, "spider").unwrap(), false).unwrap();
    assert_eq!((d.notice.tone, d.unmerged.clone()), (Tone::Warning, vec!["spider/web/work".to_string()]), "{d:?}");
    let d = delete_branches(&r.a, &d.unmerged, true).unwrap();
    assert_eq!(d.notice.tone, Tone::Success, "{d:?}");
    assert!(branches_in_folder(&r.a, "spider").unwrap().is_empty());
    assert_eq!(names(&list_branches(&r.a).unwrap().local), ["main", "new/a", "swx"]);
}

// ---- uncommitted changes ----

#[test]
fn parse_status_v2_reads_every_kind() {
    let text = "1 .M N... 100644 100644 100644 aa bb src/a.rs\0\
                1 A. N... 000000 100644 100644 00 cc new file.txt\0\
                1 .D N... 100644 100644 000000 aa aa gone.txt\0\
                2 R. N... 100644 100644 100644 aa aa R100 to/b.rs\0from/b.rs\0\
                u UU N... 100644 100644 100644 100644 aa bb cc conflict.txt\0\
                ? img/logo.png\0";
    let files = parse_status_v2(text);
    let got: Vec<(&str, &str)> = files.iter().map(|f| (f.status.as_str(), f.path.as_str())).collect();
    assert_eq!(got, [("M", "src/a.rs"), ("A", "new file.txt"), ("D", "gone.txt"), ("R", "to/b.rs"), ("U", "conflict.txt"), ("?", "img/logo.png")]);
    assert_eq!(files[3].from.as_deref(), Some("from/b.rs"));
}

#[test]
fn commit_takes_only_the_ticked_files() {
    let r = repos();
    write(&r.a, "shared.txt", "one\nTWO\nthree\n");
    write(&r.a, "staged.txt", "staged\n");
    sh(&r.a, &["add", "staged.txt"]);
    write(&r.a, "img/new [1].png", "\u{89}PNG fake");
    write(&r.a, "left.txt", "left alone\n");

    let c = read_changes(&r.a).unwrap();
    let paths: Vec<&str> = c.files.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(paths, ["shared.txt", "staged.txt", "img/new [1].png", "left.txt"]);
    assert!(c.head.as_ref().unwrap().pushed);
    assert!(!c.merging);

    let (raw, _) = working_diff(&r.a, &c.files[0]).unwrap();
    assert!(raw.contains("-two") && raw.contains("+TWO"), "{raw}");
    let (raw, _) = working_diff(&r.a, &c.files[3]).unwrap();
    assert!(raw.contains("+left alone"), "{raw}");

    let picked = vec![c.files[0].clone(), c.files[2].clone()];
    let done = commit_changes(&r.a, "Pick two\n\nWhy: a test", &picked, false).unwrap();
    assert_eq!(done.subject, "Pick two");
    let in_commit = files_of(&r.a, &done.sha).unwrap();
    let names: Vec<&str> = in_commit.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(names, ["img/new [1].png", "shared.txt"]);
    // The staged file stays staged, the untouched one untracked.
    let left = read_changes(&r.a).unwrap();
    let left: Vec<(&str, &str)> = left.files.iter().map(|f| (f.status.as_str(), f.path.as_str())).collect();
    assert_eq!(left, [("A", "staged.txt"), ("?", "left.txt")]);
    assert_eq!(sh(&r.a, &["diff", "--cached", "--name-only"]).trim(), "staged.txt");
    assert_eq!(image_of(&r.a, "HEAD", "img/new [1].png").unwrap(), "\u{89}PNG fake".as_bytes());
    assert_eq!(image_of(&r.a, "", "img/new [1].png").unwrap(), "\u{89}PNG fake".as_bytes());
    assert!(image_of(&r.a, "", "../x").is_err());

    // Not pushed yet: an amend may add a file and reword.
    let head = read_changes(&r.a).unwrap().head.unwrap();
    assert!(!head.pushed);
    assert_eq!(head.message, "Pick two\n\nWhy: a test");
    let left_file = FileChange { status: "?".into(), path: "left.txt".into(), from: None };
    let done = commit_changes(&r.a, "Pick three", &[left_file], true).unwrap();
    assert_eq!(files_of(&r.a, &done.sha).unwrap().len(), 3);
    assert_eq!(sh(&r.a, &["rev-list", "--count", "HEAD"]).trim(), "2");
    // A message-only amend.
    let done = commit_changes(&r.a, "Reworded", &[], true).unwrap();
    assert_eq!(done.subject, "Reworded");
    assert_eq!(files_of(&r.a, &done.sha).unwrap().len(), 3);

    // Pushed: no amend.
    sh(&r.a, &["push", "-q"]);
    let err = commit_changes(&r.a, "Again", &[], true).unwrap_err();
    assert!(err.contains("pushed already"), "{err}");
    assert!(commit_changes(&r.a, "  ", &[], false).is_err());
}

#[test]
fn rollback_restores_and_never_deletes_tracked_work() {
    let r = repos();
    write(&r.a, "shared.txt", "changed\n");
    write(&r.a, "added.txt", "keep me\n");
    sh(&r.a, &["add", "added.txt"]);
    write(&r.a, "scratch.txt", "throw away\n");
    let c = read_changes(&r.a).unwrap();
    rollback(&r.a, &c.files).unwrap();
    assert_eq!(std::fs::read_to_string(r.a.join("shared.txt")).unwrap(), "one\ntwo\nthree\n");
    assert!(r.a.join("added.txt").exists(), "an added file stays on disk");
    assert!(!r.a.join("scratch.txt").exists(), "an untracked file is deleted");
    let left = read_changes(&r.a).unwrap();
    let left: Vec<(&str, &str)> = left.files.iter().map(|f| (f.status.as_str(), f.path.as_str())).collect();
    assert_eq!(left, [("?", "added.txt")]);
}
