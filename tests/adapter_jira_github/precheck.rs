//! job: precheck. The lint, the specs, and the migrations of the project before a push. The
//! commands of the project in these tests are shell scripts of the repo: `bin/lint` prints an
//! offense in the form `path:line:column: message` for each line with `TODO`, and `bin/spec`
//! runs each spec file with `sh` and prints a line `rspec ./<file>:1 # ...` for each one that
//! fails, as rspec does.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::jira_github_support::{git, AdapterTest, Outcome, BRANCH};

const LINT: &str = "#!/bin/sh\n# An offense for each line with TODO of each file.\nstatus=0\nfor f in \"$@\"; do\n  grep -n TODO \"$f\" | while IFS=: read -r n rest; do echo \"$PWD/$f:$n:1: W: a todo is left\"; done\n  if grep -q TODO \"$f\"; then status=1; fi\ndone\nexit $status\n";
const SPEC: &str = "#!/bin/sh\n# Each spec file is a shell script that passes with exit code 0.\nstatus=0\nfor id in \"$@\"; do\n  file=${id%%:*}\n  file=${file#./}\n  if ! sh \"$file\" >/dev/null 2>&1; then echo \"rspec ./$file:1 # $file fails\"; status=1; fi\ndone\nif [ $status = 0 ]; then echo \"$# files, 0 failures\"; fi\nexit $status\n";
const MIGRATE: &str = "#!/bin/sh\n# Writes the schema and a log, as a migration of a framework does.\necho \"$1 $2 $MIGRATE_ENV\" >> migrate.log\necho \"version $2 after $1\" > db/schema.rb\n";

/// A clone with a base commit and a branch, and the commands of the project in `bin/`.
struct Repo {
    t: AdapterTest,
    clone: PathBuf,
    base: String,
}

impl Repo {
    fn new() -> Repo {
        let t = AdapterTest::new();
        let clone = t.path("clone");
        std::fs::create_dir_all(&clone).unwrap();
        git(&["init", "--quiet"], &clone);
        let repo = Repo {
            t,
            clone,
            base: String::new(),
        };
        repo.write(&[
            ("bin/lint", LINT),
            ("bin/spec", SPEC),
            ("bin/migrate", MIGRATE),
            ("app/models/user.rb", "def name\nend\n"),
            (
                "spec/models/user_spec.rb",
                "! grep -q BROKEN app/models/user.rb\n",
            ),
            ("lib/old.rb", "OLD = 1\n"),
            ("spec/lib/old_spec.rb", "exit 1\n"),
            ("spec/smoke_spec.rb", "true\n"),
            ("db/schema.rb", "version 1\n"),
            ("notes.txt", "a TODO of the base\n"),
        ]);
        let base = repo.commit("base");
        git(&["checkout", "--quiet", "-b", BRANCH], &repo.clone);
        Repo { base, ..repo }
    }

    fn write(&self, files: &[(&str, &str)]) {
        for (name, text) in files {
            let path = self.clone.join(name);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
    }

    fn commit(&self, message: &str) -> String {
        git(&["add", "-A"], &self.clone);
        git(&["commit", "--quiet", "-m", message], &self.clone);
        git(&["rev-parse", "HEAD"], &self.clone)
    }

    fn run(&self, head: &str, table: Value, config: Value) -> Outcome {
        let mut settings = json!({});
        for (k, v) in config.as_object().cloned().unwrap_or_default() {
            settings[k] = v;
        }
        self.t
            .job("precheck")
            .config(self.t.config(settings))
            .field("table", table)
            .field("clone", json!(self.clone.display().to_string()))
            .field("base", json!(self.base))
            .field("head", json!(head))
            .run()
    }

    fn precheck(&self, head: &str, table: Value, config: Value) -> Value {
        let result = self.run(head, table, config);
        assert_eq!(result.code, 0, "{result:?}");
        result.json
    }
}

fn doc(answer: &Value) -> String {
    assert_eq!(answer["documents"][0]["name"], "precheck.md", "{answer}");
    answer["documents"][0]["content"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn ok(answer: &Value) -> bool {
    answer["ok"] == true
}

fn kinds(answer: &Value) -> Vec<(String, String)> {
    answer["checks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|c| {
            (
                c["kind"].as_str().unwrap().to_string(),
                c["result"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn worktrees(clone: &Path) -> usize {
    git(&["worktree", "list"], clone).lines().count()
}

#[test]
fn a_lint_offense_counts_only_on_a_changed_line_and_a_broken_lint_fails() {
    let r = Repo::new();
    // The base has a TODO in notes.txt; the branch changes another line of it and adds one
    // TODO to user.rb.
    r.write(&[
        ("notes.txt", "a TODO of the base\nmore notes\n"),
        ("app/models/user.rb", "def name\n  # TODO: a name\nend\n"),
    ]);
    let head = r.commit("work");
    let table = json!({"lint": [
        "sh bin/lint {files}",
        {"run": "echo never; exit 1", "globs": ["**/*.js"]},
    ]});
    let answer = r.precheck(&head, table, json!({}));
    assert!(!ok(&answer), "{answer}");
    let text = doc(&answer);
    assert!(
        text.contains(
            "## Lint offenses on changed lines\n\n- app/models/user.rb:2: W: a todo is left"
        ),
        "{text}"
    );
    let offenses = text
        .split("## Lint offenses on changed lines")
        .nth(1)
        .and_then(|rest| rest.split("\n## ").next())
        .unwrap();
    assert!(
        !offenses.contains("notes.txt"),
        "an unchanged line never counts: {text}"
    );
    assert!(
        text.contains("not run, because no changed file matches its globs"),
        "{text}"
    );
    assert!(
        answer["detail"]
            .as_str()
            .unwrap()
            .contains("found 1 offense(s) on changed lines"),
        "{answer}"
    );
    assert_eq!(kinds(&answer), [("precheck-lint".into(), "fail".into())]);

    // Only the old TODO: the lint fails, but on no changed line, so the precheck passes.
    r.write(&[("app/models/user.rb", "def name\n  :x\nend\n")]);
    let head = r.commit("no todo");
    let answer = r.precheck(&head, json!({"lint": ["sh bin/lint {files}"]}), json!({}));
    assert!(ok(&answer), "{answer}");
    assert!(
        doc(&answer).contains("found 1 offense(s), none on a changed line"),
        "{}",
        doc(&answer)
    );

    // A lint that fails and names no offense is broken.
    let broken = json!({"lint": ["echo 'cannot load the config' >&2; exit 2"]});
    let answer = r.precheck(&head, broken, json!({}));
    assert!(!ok(&answer));
    let text = doc(&answer);
    assert!(
        text.contains("named no offense in the form path:line: message, so it is broken"),
        "{text}"
    );
    assert!(text.contains("cannot load the config"), "{text}");
}

#[test]
fn a_red_spec_runs_again_on_the_base_and_a_spec_red_there_too_is_not_ours() {
    let r = Repo::new();
    r.write(&[
        // user_spec.rb stays the same, and it fails at the head because of the change.
        ("app/models/user.rb", "def name\n  BROKEN\nend\n"),
        // old_spec.rb stays the same, and it fails on the base too.
        ("lib/old.rb", "OLD = 2\n"),
        // A new spec file of the branch is ours.
        ("spec/new_spec.rb", "exit 1\n"),
    ]);
    let head = r.commit("work");
    let table = json!({"specs": "sh bin/spec {files}", "specs_map": "rails"});
    let answer = r.precheck(
        &head,
        table.clone(),
        json!({"test_canary": "spec/smoke_spec.rb"}),
    );
    let text = doc(&answer);
    assert!(!ok(&answer), "{text}");
    for part in [
        "- spec/models/user_spec.rb:1: red at the head",
        "- spec/new_spec.rb:1: red at the head",
        "- spec/lib/old_spec.rb:1: red on the base",
        "too, so it is not ours: it passes",
        "the canary spec/smoke_spec.rb on the base",
    ] {
        assert!(text.contains(part), "{part}:\n{text}");
    }
    let detail = answer["detail"].as_str().unwrap();
    assert!(detail.contains("2 spec(s) fail at the head"), "{detail}");
    assert!(!detail.contains("old_spec"), "{detail}");
    assert_eq!(worktrees(&r.clone), 1, "the worktree of the base is gone");

    // With the code fixed and the new spec green, only the spec that is red on the base is
    // left: the precheck passes and names it.
    r.write(&[
        ("app/models/user.rb", "def name\nend\n"),
        ("spec/new_spec.rb", "true\n"),
    ]);
    let head = r.commit("fix");
    let answer = r.precheck(&head, table, json!({}));
    let text = doc(&answer);
    assert!(ok(&answer), "{text}");
    assert!(
        text.contains("spec/lib/old_spec.rb:1: red on the base"),
        "{text}"
    );
    assert!(
        text.contains("no canary guards the run on the base"),
        "{text}"
    );
}

#[test]
fn a_base_that_cannot_run_specs_leaves_each_red_spec_ours() {
    let r = Repo::new();
    r.write(&[("lib/old.rb", "OLD = 2\n")]);
    let head = r.commit("work");
    let table = json!({"specs": "sh bin/spec {files}", "specs_map": "rails"});
    let answer = r.precheck(
        &head,
        table,
        json!({"test_canary": ["spec/missing_spec.rb"]}),
    );
    let text = doc(&answer);
    assert!(!ok(&answer), "{text}");
    assert!(
        text.contains("the canary spec/missing_spec.rb is missing in the worktree of the base"),
        "{text}"
    );
    assert!(
        text.contains("- spec/lib/old_spec.rb:1: red at the head"),
        "{text}"
    );
}

#[test]
fn the_cap_stops_the_specs_with_a_note_and_the_lint_as_a_hang() {
    let r = Repo::new();
    r.write(&[("app/models/user.rb", "def name\n  :x\nend\n")]);
    let head = r.commit("work");
    let cap = json!({"cap": 1, "cap_per_file": 0, "cap_max": 1});
    let mut table = cap.clone();
    table["specs"] = json!("sleep 30");
    table["specs_map"] = json!("rails");
    let started = std::time::Instant::now();
    let answer = r.precheck(&head, table, json!({}));
    assert!(started.elapsed().as_secs() < 20, "the cap stops the specs");
    let text = doc(&answer);
    assert!(ok(&answer), "{text}");
    assert!(
        text.contains(
            "unfinished: the specs did not end within the cap of 1s. CI is their only run"
        ),
        "{text}"
    );
    assert!(
        text.contains("1s = cap 1s + cap_per_file 0s x 1 checked file(s), at most cap_max 1s"),
        "{text}"
    );
    assert!(
        answer["detail"]
            .as_str()
            .unwrap()
            .contains("with a note: unfinished"),
        "{answer}"
    );

    let mut table = cap;
    table["lint"] = json!(["sleep 30"]);
    let answer = r.precheck(&head, table, json!({}));
    let text = doc(&answer);
    assert!(!ok(&answer), "{text}");
    assert!(
        text.contains("did not end within the cap of 1s, so it counts as a hang"),
        "{text}"
    );
}

#[test]
fn each_added_migration_runs_its_commands_and_the_schema_goes_back() {
    let r = Repo::new();
    r.write(&[
        (
            "db/migrate/20261101120000_add_name.rb",
            "class AddName; end\n",
        ),
        (
            "db/migrate/20261102120000_add_age.rb",
            "class AddAge; end\n",
        ),
        ("spec/db/migrate/20261103_helper_spec.rb", "true\n"),
    ]);
    let head = r.commit("migrations");
    let table = json!({
        "migrations": ["sh bin/migrate up {version}", "sh bin/migrate down {version}"],
        "migrations_env": {"MIGRATE_ENV": "test"},
    });
    let answer = r.precheck(&head, table, json!({}));
    let text = doc(&answer);
    assert!(ok(&answer), "{text}");
    let log = std::fs::read_to_string(r.clone.join("migrate.log")).unwrap();
    assert_eq!(
        log,
        "up 20261101120000 test\ndown 20261101120000 test\nup 20261102120000 test\ndown 20261102120000 test\n"
    );
    assert_eq!(
        std::fs::read_to_string(r.clone.join("db/schema.rb")).unwrap(),
        "version 1\n",
        "the schema goes back to the state of the branch"
    );
    for part in [
        "- db/migrate/20261101120000_add_name.rb (version 20261101120000): each command passed",
        "The commands changed db/schema.rb, which went back to the state of the branch.",
        "The commands left new files that git does not track: migrate.log. They stay.",
    ] {
        assert!(text.contains(part), "{part}:\n{text}");
    }
    assert!(!text.contains("helper_spec"), "{text}");
    assert_eq!(
        kinds(&answer),
        vec![("precheck-migration".to_string(), "pass".to_string()); 4]
    );

    // A command that fails stops the migrations and fails the job.
    std::fs::remove_file(r.clone.join("migrate.log")).unwrap();
    let failing = json!({"migrations": ["sh bin/migrate up {version}", "exit 3"]});
    let answer = r.precheck(&head, failing, json!({}));
    assert!(!ok(&answer));
    let text = doc(&answer);
    assert!(
        text.contains("the migration db/migrate/20261101120000_add_name.rb failed: `exit 3`"),
        "{text}"
    );
    assert!(text.contains("fail with the exit code 3"), "{text}");
}

#[test]
fn a_table_that_names_a_remote_console_fails_and_nothing_runs() {
    let r = Repo::new();
    r.write(&[("app/models/user.rb", "def name\n  :x\nend\n")]);
    let head = r.commit("work");
    let result = r.run(
        &head,
        json!({"lint": ["kubectl exec -it web -- touch ran.txt"]}),
        json!({}),
    );
    assert_eq!(result.code, 1, "{result:?}");
    assert!(
        result
            .stderr
            .contains("which the executor refuses: a shell in a pod (kubectl exec)"),
        "{result:?}"
    );
    assert!(!r.clone.join("ran.txt").exists());
}

#[test]
fn nothing_to_check_passes() {
    let r = Repo::new();
    r.write(&[("README.md", "hello\n")]);
    let head = r.commit("docs");
    let answer = r.precheck(&head, json!({"specs": "sh bin/spec {files}"}), json!({}));
    assert!(ok(&answer), "{answer}");
    assert!(
        doc(&answer).contains("specs: not run, because no changed file has a spec file"),
        "{}",
        doc(&answer)
    );
}

#[test]
fn a_file_named_like_a_denied_program_is_only_a_file() {
    let r = Repo::new();
    r.write(&[("tools/ssh", "echo a helper\n")]);
    let head = r.commit("a helper named ssh");
    let answer = r.precheck(&head, json!({"lint": ["sh bin/lint {files}"]}), json!({}));
    assert!(ok(&answer), "{answer}");
    assert!(
        doc(&answer).contains("- lint: `sh bin/lint 'tools/ssh'`: pass"),
        "{}",
        doc(&answer)
    );
}
