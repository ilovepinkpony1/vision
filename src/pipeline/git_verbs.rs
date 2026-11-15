//! The read verbs of git (section 7.2): `git.log` and `git.blame`. A reader turn has no Bash,
//! so the core serves them in the clone, with no adapter call and no token. They answer
//! through the result `requests` of a turn, as the read verbs of an adapter do.

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::machine::Ticket;
use crate::ctx::Ctx;
use crate::executor::Secrets;
use crate::util::fs::atomic_write;
use crate::util::{git, ids};

/// The names of the verbs.
pub const NAMES: [&str; 2] = ["git.log", "git.blame"];

/// What each verb gives, for the facts file of a turn.
pub fn about(verb: &str) -> &'static str {
    match verb {
        "git.log" => "The commits that touched a file, with their messages, as git-log-<hash>.txt; with from and to, the history of those lines (git log -L). args: path, from, to, limit (default 20)",
        _ => "The commit, the author, and the date of each line from..to of a file, as git-blame-<hash>.txt. args: path, from, to",
    }
}

/// The most bytes of one answer. A longer output is cut, with a note at its end.
pub const MAX_BYTES: usize = 200 * 1024;

/// The most commits of `git.log`.
pub const MAX_LIMIT: u64 = 200;

/// Runs one verb in the clone of the ticket, writes its answer to a document of the ticket
/// folder, and gives the name of the document and a short detail.
pub(crate) fn serve(
    ctx: &Ctx,
    ticket: &Ticket,
    verb: &str,
    args: &Value,
    secrets: &Secrets,
) -> Result<(String, String)> {
    let command = command(verb, args)?;
    let clone = ticket.clone_dir()?;
    let refs: Vec<&str> = command.iter().map(String::as_str).collect();
    let output = git::run(&clone, &refs)?;
    let text = bound(crate::executor::gates::mask_token_forms(
        &secrets.mask(&output),
    ));
    let hash = ids::sha256_hex(json!({"verb": verb, "args": args}).to_string().as_bytes());
    let name = format!("{}-{}.txt", verb.replace('.', "-"), &hash[..16]);
    let dir = ctx.paths.ticket_dir(&ticket.key);
    std::fs::create_dir_all(&dir).with_context(|| format!("could not make {}", dir.display()))?;
    atomic_write(&ctx.paths.ticket_doc(&ticket.key, &name), text.as_bytes())?;
    let count = text.lines().count();
    Ok((name, format!("{count} line(s) of `git {}`", command[0])))
}

/// The arguments of `git` for one verb. A path outside the clone, a wrong line, or a missing
/// argument is refused.
fn command(verb: &str, args: &Value) -> Result<Vec<String>> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .with_context(|| format!("{verb} needs args.path, a file of the clone such as src/a.rs"))?;
    let unsafe_path = path.starts_with('/')
        || path.starts_with('-')
        || path.contains('\0')
        || path.split('/').any(|part| part == "..");
    if unsafe_path {
        bail!("`{path}` is not a relative path in the clone");
    }
    let number = |key: &str| -> Result<Option<u64>> {
        match args.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(value) => value
                .as_u64()
                .or_else(|| value.as_str().and_then(|s| s.trim().parse().ok()))
                .filter(|n| *n > 0)
                .map(Some)
                .with_context(|| format!("args.{key} of {verb} must be a line number from 1")),
        }
    };
    let (from, to) = (number("from")?, number("to")?);
    let lines = match (from, to) {
        (Some(from), Some(to)) if from <= to => Some(format!("{from},{to}")),
        (Some(_), Some(_)) => bail!("args.from of {verb} is after args.to"),
        (None, None) => None,
        _ => bail!("{verb} needs both args.from and args.to, or neither"),
    };
    let mut command: Vec<String> = Vec::new();
    match verb {
        "git.log" => {
            let limit = number("limit")?.unwrap_or(20).min(MAX_LIMIT);
            command.extend(["log".into(), "--no-color".into(), format!("-n{limit}")]);
            match &lines {
                Some(range) => command.push(format!("-L{range}:{path}")),
                None => {
                    command.extend([
                        "--format=commit %H%nAuthor: %an%nDate: %ad%n%n%B".into(),
                        "--name-status".into(),
                        "--".into(),
                        path.to_string(),
                    ]);
                }
            }
        }
        "git.blame" => {
            let Some(range) = &lines else {
                bail!("git.blame needs args.from and args.to, the lines of the file");
            };
            command.extend([
                "blame".into(),
                "--date=short".into(),
                format!("-L{range}"),
                "--".into(),
                path.to_string(),
            ]);
        }
        other => bail!("{other} is not a read verb of git"),
    }
    Ok(command)
}

/// The text cut at `MAX_BYTES`, with a note, and with a last newline.
fn bound(mut text: String) -> String {
    if text.len() > MAX_BYTES {
        let mut cut = MAX_BYTES;
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
        text.push_str("\n[the answer is cut at 200 KB]\n");
    }
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn error(verb: &str, args: Value) -> String {
        command(verb, &args).unwrap_err().to_string()
    }

    #[test]
    fn each_verb_gives_its_command_of_git() {
        assert_eq!(
            command("git.log", &json!({"path": "src/a.rs"})).unwrap(),
            [
                "log",
                "--no-color",
                "-n20",
                "--format=commit %H%nAuthor: %an%nDate: %ad%n%n%B",
                "--name-status",
                "--",
                "src/a.rs"
            ]
        );
        assert_eq!(
            command(
                "git.log",
                &json!({"path": "a.rs", "from": 3, "to": "9", "limit": 999})
            )
            .unwrap(),
            ["log", "--no-color", "-n200", "-L3,9:a.rs"]
        );
        assert_eq!(
            command("git.blame", &json!({"path": "a.rs", "from": 1, "to": 4})).unwrap(),
            ["blame", "--date=short", "-L1,4", "--", "a.rs"]
        );
    }

    #[test]
    fn a_path_outside_the_clone_and_wrong_lines_are_refused() {
        assert!(error("git.log", json!({})).contains("needs args.path"));
        for path in ["/etc/passwd", "../x", "a/../../x", "--output=x"] {
            let err = error("git.log", json!({ "path": path }));
            assert!(err.contains("is not a relative path in the clone"), "{err}");
        }
        assert!(error("git.log", json!({"path": "a", "from": 5, "to": 2})).contains("is after"));
        assert!(error("git.log", json!({"path": "a", "from": 5})).contains("or neither"));
        assert!(error("git.log", json!({"path": "a", "from": 0, "to": 2})).contains("from 1"));
        assert!(error("git.blame", json!({"path": "a"})).contains("needs args.from and args.to"));
        assert!(error("git.push", json!({"path": "a"})).contains("is not a read verb of git"));
    }

    #[test]
    fn a_long_answer_is_cut_at_200_kb() {
        let long = bound("é".repeat(MAX_BYTES));
        assert!(long.len() <= MAX_BYTES + 40, "{}", long.len());
        assert!(long.ends_with("[the answer is cut at 200 KB]\n"));
        assert_eq!(bound("one".into()), "one\n");
    }
}
