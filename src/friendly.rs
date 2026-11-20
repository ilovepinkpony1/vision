//! Friendly words for an error or a warning that the person sees.
//!
//! The pipeline, the executor, and the scripts give a reason as a chain of clauses, such as
//! `the job regression before the step regression did not pass: the command of the job
//! regression failed with the exit code 1. The last lines of its standard error: ...`. The
//! ledger and the log keep that chain, because it is the evidence. A card, a notice, an error
//! card, and the command line show [`explain`] of it instead:
//!
//! - `headline`: what happened, in one plain sentence.
//! - `action`: what the person can do now, when a rule knows it.
//! - `details`: the raw text, for a person who wants the evidence.
//!
//! Each rule matches one shape of reason that Vision gives. A reason that no rule knows gets
//! its first clause as the headline and the whole text as the details.

/// An error or a warning in the words of the person.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Friendly {
    /// What happened, in one sentence that ends with a period.
    pub headline: String,
    /// What to do now, when a rule knows it.
    pub action: Option<String>,
    /// The raw text, when it says more than the headline.
    pub details: Option<String>,
}

impl Friendly {
    fn new(headline: impl Into<String>, action: Option<String>, raw: &str) -> Friendly {
        let headline = sentence(&headline.into());
        let raw = raw.trim();
        let details = (!raw.is_empty() && !same_words(raw, &headline)).then(|| raw.to_string());
        Friendly {
            headline,
            action: action.map(|a| sentence(&a)),
            details,
        }
    }

    /// The headline inside a sentence: a small first letter, unless it starts a name such as
    /// `PR-12` or `API`, and no period at the end.
    pub fn inline(&self) -> String {
        let text = self.headline.trim_end_matches('.');
        if NAMES.iter().any(|name| {
            text.strip_prefix(name)
                .is_some_and(|rest| rest.starts_with([' ', ',', ':', '.']))
        }) {
            return text.to_string();
        }
        let mut chars = text.chars();
        match chars.next() {
            Some(c) if chars.clone().next().is_some_and(char::is_lowercase) => {
                c.to_lowercase().collect::<String>() + chars.as_str()
            }
            Some(c) => c.to_string() + chars.as_str(),
            None => String::new(),
        }
    }

    /// The headline and the action as one text, such as for a notice of one line.
    pub fn short(&self) -> String {
        match &self.action {
            Some(action) => format!("{} {action}", self.headline),
            None => self.headline.clone(),
        }
    }
}

/// The friendly words of a raw reason.
pub fn explain(raw: &str) -> Friendly {
    let text = raw.trim().trim_start_matches("error: ").trim();
    if text.is_empty() {
        return Friendly::new("Something went wrong, and Vision gave no reason", None, raw);
    }
    // A job before a step, or a job of an end fact: the context, then the cause.
    if let Some((job, step, rest)) = job_before_step(text) {
        let (cause, action) = job_cause(job, rest);
        return Friendly::new(
            format!("Before the step {step}, the job {job} failed: {cause}"),
            action.or_else(|| Some(TRY_AGAIN.into())),
            raw,
        );
    }
    if let Some(rest) = text.strip_prefix("the end fact job:") {
        let job = rest.split_whitespace().next().unwrap_or("");
        let cause = rest
            .split_once(&format!("the job {job} did not pass at "))
            .and_then(|(_, after)| after.split_once(": "))
            .map_or(rest, |(_, cause)| cause);
        let (cause, action) = job_cause(job, cause);
        let headline = if rest.contains("after a resume") {
            format!("The job {job} still fails after the agent tried again: {cause}")
        } else {
            format!("The job {job} failed: {cause}")
        };
        let action = action.or_else(|| {
            Some(format!(
                "Read {job}.md in the folder of the ticket. Then press r to try the step again"
            ))
        });
        return Friendly::new(headline, action, raw);
    }
    if let Some(rest) = text.strip_prefix("the end fact commits did not hold") {
        let again = if rest.contains("after a resume") {
            ", also after a second try"
        } else {
            ""
        };
        return Friendly::new(
            format!("The agent finished the step but made no commit{again}"),
            Some("Press o to look at its session, or r to try the step again".into()),
            raw,
        );
    }
    if let Some(friendly) = cap(text, raw) {
        return friendly;
    }
    if let Some(friendly) = shadow(text, raw) {
        return friendly;
    }
    if let Some((verb, rest)) = text.split_once(" was refused by the size gate: ") {
        let (lines, limit) = numbers_of(rest);
        let headline = match (lines, limit) {
            (Some(lines), Some(limit)) => format!(
                "Vision did not run {verb}: the change has {lines} lines, and the limit is {limit}"
            ),
            _ => format!("Vision did not run {verb}: the change is larger than the limit"),
        };
        return Friendly::new(
            headline,
            Some("Split the ticket, or raise gates.max_diff_lines in ~/.vision/config.toml".into()),
            raw,
        );
    }
    if let Some((verb, gate)) = text
        .split_once(" was refused by the ")
        .and_then(|(verb, rest)| rest.split_once(" gate").map(|(gate, _)| (verb, gate)))
    {
        return Friendly::new(
            format!("Vision did not run {verb}, because the {gate} gate refused it"),
            Some("Read the details, fix the cause, then press r to try the step again".into()),
            raw,
        );
    }
    if let Some(rest) = text.strip_prefix("deploy.merge failed: the merge in ") {
        let place = rest.split(' ').next().unwrap_or(".vision/merge");
        return Friendly::new(
            "The merge into the staging branch has conflicts that nobody resolved",
            Some(format!(
                "Resolve each conflict in {place} of the clone, commit the merge, then press r"
            )),
            raw,
        );
    }
    if let Some(rest) = text.strip_prefix("deploy failed: conflict: ") {
        let into = rest
            .split_once(" does not merge into ")
            .map(|(_, after)| after.split(':').next().unwrap_or(after).trim());
        let headline = match into {
            Some(branch) => format!("The branch does not merge into {branch}: a merge conflict"),
            None => "The deploy stopped on a merge conflict".to_string(),
        };
        return Friendly::new(
            headline,
            Some("Merge it by hand, or fix the branch, then press r".into()),
            raw,
        );
    }
    if let Some(rest) = text.strip_prefix("tracker.move failed: no transition of ") {
        if let Some((key, moves)) = rest.split_once(" leads from ") {
            let moves = moves.split(". The transitions").next().unwrap_or(moves);
            if let Some((from, to)) = moves.split_once(" to ") {
                return Friendly::new(
                    format!("Jira has no transition that moves {key} from {from} to {to}"),
                    Some(format!(
                        "Move {key} in Jira by hand, or fix statuses in [adapter] of ~/.vision/config.toml"
                    )),
                    raw,
                );
            }
        }
    }
    if text.contains("got the signal 15") || text.contains("got the signal 9") {
        let who = text
            .strip_prefix("the member ")
            .and_then(|r| r.split(' ').next())
            .map(|m| format!("The turn of {m}"))
            .unwrap_or_else(|| "A turn".into());
        return Friendly::new(
            format!("{who} was killed before it ended, as when the Mac restarts"),
            Some(TRY_AGAIN.into()),
            raw,
        );
    }
    if text.contains("Request timed out") {
        return Friendly::new(
            "The Claude API did not answer in time",
            Some("Wait a few minutes, then press r to try again".into()),
            raw,
        );
    }
    if let Some(friendly) = secret_command(text, raw) {
        return friendly;
    }
    if let Some(friendly) = missing_secret(text, raw) {
        return friendly;
    }
    if let Some((job, rest)) = text
        .strip_prefix("the job ")
        .and_then(|r| r.split_once(" did not pass: "))
    {
        let job = job.split_whitespace().next().unwrap_or(job);
        let (cause, action) = job_cause(job, rest);
        return Friendly::new(format!("The job {job} failed: {cause}"), action, raw);
    }
    if text.contains(" did not end in ") && text.contains("killed it") {
        return Friendly::new(
            first_clause(text),
            Some("Raise its timeout in adapter.toml if it needs more time, then press r".into()),
            raw,
        );
    }
    // A short reason of one line reads well as it is. A long one gives its first clause, and
    // the details keep the rest.
    if text.lines().count() <= 1 && text.chars().count() <= SHORT_REASON {
        return Friendly::new(text, None, raw);
    }
    Friendly::new(headline_of(text), action_of(text), raw)
}

/// The words that start an instruction to the person, such as `Run `vision init` first`.
const IMPERATIVES: [&str; 22] = [
    "Run ", "Add ", "Set ", "Fix ", "Make ", "Merge ", "Resolve ", "Name ", "Start ", "Press ",
    "Turn ", "Wait ", "Open ", "Install ", "Remove ", "Raise ", "Split ", "Move ", "Unlock ",
    "Use ", "Give ", "Read ",
];

/// The first clause of the first line. A clause of one or two words, such as a ticket key,
/// takes the next clause with it: `DEMO-15: no clone is free`.
fn headline_of(text: &str) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    let line = line.split(". ").next().unwrap_or(line);
    let mut clauses = line.split(": ").filter(|c| !c.trim().is_empty());
    let mut headline = clauses.next().unwrap_or("").trim().to_string();
    while headline.split_whitespace().count() < 3 {
        match clauses.next() {
            Some(next) => headline = format!("{headline}: {}", next.trim()),
            None => break,
        }
    }
    headline.trim_end_matches(':').to_string()
}

/// The last sentence of the text that tells the person what to do.
fn action_of(text: &str) -> Option<String> {
    text.lines()
        .flat_map(|line| line.split(". "))
        .map(|s| s.trim().trim_end_matches('.'))
        .filter(|s| IMPERATIVES.iter().any(|w| s.starts_with(w)))
        .last()
        .map(str::to_string)
}

/// The longest reason that the headline shows whole.
const SHORT_REASON: usize = 140;

/// Names that keep their capital letter inside a sentence.
const NAMES: [&str; 4] = ["Vision", "Jira", "GitHub", "Claude"];

/// The usual action after a stop of a step.
const TRY_AGAIN: &str = "Press r to try the step again";

/// `the job J before the step S did not pass: REST`.
fn job_before_step(text: &str) -> Option<(&str, &str, &str)> {
    let rest = text.strip_prefix("the job ")?;
    let (job, rest) = rest.split_once(" before the step ")?;
    let (step, rest) = rest.split_once(" did not pass: ")?;
    Some((job, step, rest))
}

/// Why a job failed, as a phrase for the end of a sentence, and an action when a rule knows
/// one: a missing secret, a time limit, the last line of its error, or what the job said.
fn job_cause(job: &str, text: &str) -> (String, Option<String>) {
    if let Some(name) = missing_secret_name(text) {
        return (
            format!("it needs the secret {name}, and Vision cannot get it"),
            Some(secret_action(&name)),
        );
    }
    if let Some((_, rest)) = text.split_once(" did not end in ") {
        let secs = rest
            .split_whitespace()
            .next()
            .and_then(|n| n.parse::<u64>().ok());
        let limit = match secs {
            Some(s) if s >= 120 => format!("{} minutes", s / 60),
            Some(s) => format!("{s} seconds"),
            None => "its time limit".into(),
        };
        return (
            format!("it ran longer than its limit of {limit}, so Vision stopped it"),
            Some("Raise its timeout in adapter.toml if it needs more time, then press r".into()),
        );
    }
    if let Some((_, rest)) = text.split_once("failed with the exit code ") {
        let code = rest
            .split(|c: char| !c.is_ascii_digit())
            .next()
            .unwrap_or("");
        let why = rest
            .split_once("standard error:")
            .map(|(_, err)| last_error_line(err))
            .filter(|l| !l.is_empty());
        return match why {
            Some(why) => (trim_period(&why).to_string(), None),
            None => (format!("its command ended with the exit code {code}"), None),
        };
    }
    // `the precheck failed at abc123: jest failed ...` -> `jest failed ...`
    let cause = text
        .strip_prefix(&format!("the {job} failed at "))
        .and_then(|rest| rest.split_once(": "))
        .map_or(text, |(_, cause)| cause);
    let cause = cause.split(". ").next().unwrap_or(cause).trim();
    (trim_period(cause).to_string(), None)
}

/// `the step S reached its cap NAME = V: this is entry N. ...`.
fn cap(text: &str, raw: &str) -> Option<Friendly> {
    let rest = text.strip_prefix("the step ")?;
    let (step, rest) = rest.split_once(" reached its cap ")?;
    let (setting, rest) = rest.split_once(" = ")?;
    let limit = rest.split(':').next()?.trim();
    let entry = rest
        .split_once("this is entry ")
        .and_then(|(_, after)| after.split(|c: char| !c.is_ascii_digit()).next())
        .unwrap_or("?");
    let open = rest
        .split_once("Open: ")
        .map(|(_, o)| o.trim_end_matches('.'));
    let headline = format!(
        "The step {step} came back {entry} times, and its limit is {limit}, so Vision stopped the loop"
    );
    let action = match open {
        Some(open) => format!(
            "It still lists: {open}. Answer these items, then press r, or raise caps.{setting} in ~/.vision/config.toml"
        ),
        None => format!(
            "Nothing open is listed, so look at the pull request first. Then press r to try once more, or raise caps.{setting} in ~/.vision/config.toml"
        ),
    };
    Some(Friendly::new(headline, Some(action), raw))
}

/// `V is in the mode shadow: ... Run `vision promote V` ...`.
fn shadow(text: &str, raw: &str) -> Option<Friendly> {
    let at = text.find(" is in the mode shadow")?;
    let verb = text[..at].rsplit([' ', ':']).next().unwrap_or("").trim();
    if verb.is_empty() {
        return None;
    }
    Some(Friendly::new(
        format!("{verb} is in the mode shadow, so Vision only recorded what it would do"),
        Some(format!(
            "Run `vision promote {verb}` so that it asks you before it runs, then press r"
        )),
        raw,
    ))
}

/// The name of a secret that a text says is missing: `X is not in [secrets]`, or `the secret X
/// ... which is not set` or `is empty`.
fn missing_secret_name(text: &str) -> Option<String> {
    if let Some(at) = text.find(" is not in [secrets]") {
        return text[..at]
            .rsplit(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
            .next()
            .filter(|n| !n.is_empty())
            .map(str::to_string);
    }
    let (_, rest) = text.split_once("the secret ")?;
    if !(rest.contains("which is not set") || rest.contains("is empty")) {
        return None;
    }
    rest.split_whitespace().next().map(str::to_string)
}

fn secret_action(name: &str) -> String {
    format!(
        "Add {name} to [secrets] in ~/.vision/config.toml, such as {name} = {{ env = \"VARIABLE\" }}, then press r"
    )
}

fn missing_secret(text: &str, raw: &str) -> Option<Friendly> {
    let name = missing_secret_name(text)?;
    Some(Friendly::new(
        format!("A script needs the secret {name}, and Vision cannot get it"),
        Some(secret_action(&name)),
        raw,
    ))
}

/// The command of a secret that did not end, such as a locked Keychain.
fn secret_command(text: &str, raw: &str) -> Option<Friendly> {
    let (_, rest) = text.split_once("the command of the secret ")?;
    let name = rest.split_whitespace().next()?;
    if !rest.contains("did not end") {
        return None;
    }
    Some(Friendly::new(
        format!("Vision could not read the secret {name}: its command did not answer in time"),
        Some("If the Mac is locked, unlock it. The next pass tries again".into()),
        raw,
    ))
}

/// The last useful line of standard error: no path prefix of a shell, no line number, and no
/// variable name that the message repeats.
fn last_error_line(err: &str) -> String {
    let line = err
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .unwrap_or("");
    // `/path/to/script: line 9: NAME: message` -> `message`
    let mut parts: Vec<&str> = line.split(": ").collect();
    while parts.len() > 1 {
        let first = parts[0];
        let is_path = first.starts_with('/') || first.starts_with("~/");
        let is_line = first.starts_with("line ") && first[5..].chars().all(|c| c.is_ascii_digit());
        let is_name = first.chars().all(|c| c.is_ascii_uppercase() || c == '_');
        if is_path || is_line || is_name {
            parts.remove(0);
        } else {
            break;
        }
    }
    parts.join(": ")
}

/// The numbers of a size gate text: the changed lines, then the limit.
fn numbers_of(text: &str) -> (Option<u64>, Option<u64>) {
    let numbers: Vec<u64> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|n| n.parse().ok())
        .collect();
    (numbers.first().copied(), numbers.get(1).copied())
}

/// The text up to the first `: `, or the first sentence.
fn first_clause(text: &str) -> String {
    let clause = text.split(": ").next().unwrap_or(text);
    let clause = clause.split(". ").next().unwrap_or(clause);
    clause.trim().to_string()
}

fn trim_period(text: &str) -> &str {
    text.trim_end_matches('.')
}

/// A sentence: a capital first letter and one period at the end.
fn sentence(text: &str) -> String {
    let text = text.trim();
    // An identifier, such as `git.push` or `job_run`, keeps its case.
    let first = text.split_whitespace().next().unwrap_or("");
    let identifier = first.contains(['.', '_', '/', '`']);
    let mut chars = text.chars();
    let mut out = match chars.next() {
        Some(c) if identifier => c.to_string() + chars.as_str(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
        None => return String::new(),
    };
    // A sentence that ends in code, such as `vision init`, gets its period too.
    if !out.ends_with(['.', '!', '?']) {
        out.push('.');
    }
    out
}

/// True when the two texts have the same words, case and the last period aside.
fn same_words(a: &str, b: &str) -> bool {
    let norm = |t: &str| t.trim().trim_end_matches('.').to_lowercase();
    norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_secret_of_a_job_before_a_step_says_what_to_add() {
        let raw = "the job regression before the step regression did not pass: the command of the job regression failed with the exit code 1. The last lines of its standard error:\n  /home/me/.vision/adapters/x/bin/check-runs: line 9: CI_USER: CI_USER is not in [secrets]";
        let f = explain(raw);
        assert_eq!(
            f.headline,
            "Before the step regression, the job regression failed: it needs the secret CI_USER, and Vision cannot get it."
        );
        assert_eq!(
            f.action.as_deref(),
            Some("Add CI_USER to [secrets] in ~/.vision/config.toml, such as CI_USER = { env = \"VARIABLE\" }, then press r.")
        );
        assert_eq!(f.details.as_deref(), Some(raw));
    }

    #[test]
    fn a_failed_job_gives_the_last_line_of_its_error_without_the_path() {
        let raw = "the job browser before the step qa-void did not pass: the command of the job browser failed with the exit code 1. The last lines of its standard error:\n  [browse] Starting\n  /x/bin/browser-start: line 4: the browser did not start on port 9222";
        let f = explain(raw);
        assert_eq!(
            f.headline,
            "Before the step qa-void, the job browser failed: the browser did not start on port 9222."
        );
        assert_eq!(f.action.as_deref(), Some("Press r to try the step again."));
    }

    #[test]
    fn a_cap_names_the_loop_and_the_setting() {
        let f = explain(
            "the step comments reached its cap comments = 3: this is entry 5. Nothing open is listed",
        );
        assert_eq!(
            f.headline,
            "The step comments came back 5 times, and its limit is 3, so Vision stopped the loop."
        );
        assert!(f
            .action
            .as_deref()
            .unwrap()
            .contains("raise caps.comments in ~/.vision/config.toml"));
        let open = explain("the step comments reached its cap comments = 3: this is entry 4. Open: 2 review comments");
        assert!(open
            .action
            .as_deref()
            .unwrap()
            .starts_with("It still lists: 2 review comments."));
    }

    #[test]
    fn a_shadowed_verb_says_how_to_promote_it() {
        let f = explain("tracker.move before the step refine: shadowed: tracker.move is in the mode shadow: the executor recorded what it would do, and called nothing. Run `vision promote tracker.move` to ask before it runs");
        assert_eq!(
            f.headline,
            "tracker.move is in the mode shadow, so Vision only recorded what it would do."
        );
        assert_eq!(
            f.action.as_deref(),
            Some("Run `vision promote tracker.move` so that it asks you before it runs, then press r.")
        );
    }

    #[test]
    fn the_size_gate_gives_the_numbers() {
        let f = explain("git.push was refused by the size gate: the diff changes 6000 lines, more than gates.max_diff_lines (3000). Split the change into smaller tickets, or raise gates.max_diff_lines in config.toml");
        assert_eq!(
            f.headline,
            "Vision did not run git.push: the change has 6000 lines, and the limit is 3000."
        );
    }

    #[test]
    fn merges_transitions_kills_and_timeouts_have_their_words() {
        assert_eq!(
            explain("deploy.merge failed: the merge in .vision/merge/void is not committed. Resolve each conflict, then commit the merge").headline,
            "The merge into the staging branch has conflicts that nobody resolved."
        );
        assert_eq!(
            explain("deploy failed: conflict: abc does not merge into staging-void: Merge conflict. Merge it by hand, or fix the branch").headline,
            "The branch does not merge into staging-void: a merge conflict."
        );
        assert_eq!(
            explain("tracker.move failed: no transition of PR-1 leads from In Testing to Ready for Regression. The transitions: Closed -> Closed").headline,
            "Jira has no transition that moves PR-1 from In Testing to Ready for Regression."
        );
        assert_eq!(
            explain("the member cr-correctness of the step review-lanes: the turn x failed: killed: the supervisor got the signal 15").headline,
            "The turn of cr-correctness was killed before it ended, as when the Mac restarts."
        );
        assert_eq!(
            explain("the session ended with the error result `success`: Request timed out.")
                .headline,
            "The Claude API did not answer in time."
        );
        assert_eq!(
            explain("the secrets of [secrets] could not be read, so no entry is written: the command of the secret FORGE_TOKEN did not end in 30 s. Fix it in [secrets] of config.toml").headline,
            "Vision could not read the secret FORGE_TOKEN: its command did not answer in time."
        );
    }

    #[test]
    fn a_job_of_an_end_fact_that_fails_after_a_resume_says_so() {
        let f = explain("the end fact job:precheck did not hold after a resume: the job precheck did not pass at 66c3c9e90950: the precheck failed at 66c3c9e90950: jest failed on the changed files; 2 spec(s) fail");
        assert_eq!(
            f.headline,
            "The job precheck still fails after the agent tried again: jest failed on the changed files; 2 spec(s) fail."
        );
        assert_eq!(
            f.action.as_deref(),
            Some(
                "Read precheck.md in the folder of the ticket. Then press r to try the step again."
            )
        );
        let commits = explain("the end fact commits did not hold after a resume: the branch is still at abc: it has no commit since the start of the step");
        assert_eq!(
            commits.headline,
            "The agent finished the step but made no commit, also after a second try."
        );
    }

    #[test]
    fn the_inline_form_drops_the_capital_and_the_period() {
        assert_eq!(
            explain("the step comments reached its cap comments = 3: this is entry 5").inline(),
            "the step comments came back 5 times, and its limit is 3, so Vision stopped the loop"
        );
        assert_eq!(
            explain("PR-12 is done, so there is nothing to stop").inline(),
            "PR-12 is done, so there is nothing to stop"
        );
        assert_eq!(
            explain("git.push was refused by the size gate: the diff changes 9 lines, more than gates.max_diff_lines (5)").inline(),
            "Vision did not run git.push: the change has 9 lines, and the limit is 5"
        );
        assert_eq!(
            explain("pr.open is in the mode shadow: the executor recorded what it would do")
                .inline(),
            "pr.open is in the mode shadow, so Vision only recorded what it would do"
        );
    }

    #[test]
    fn a_long_unknown_error_gives_its_first_clause_and_its_instruction() {
        let f = explain("Vision is not set up at /work/home. Run `vision init` first\nwarning: a second line of text");
        assert_eq!(f.headline, "Vision is not set up at /work/home.");
        assert_eq!(f.action.as_deref(), Some("Run `vision init` first."));
        let take = explain("DEMO-15: no clone is free:\n  /work/clones/demo-1: DEMO-12 holds it (step build)\nAdd a clone to `clones` in config.toml, or wait until a ticket ends");
        assert_eq!(take.headline, "DEMO-15: no clone is free.");
        assert_eq!(
            take.action.as_deref(),
            Some("Add a clone to `clones` in config.toml, or wait until a ticket ends.")
        );
    }

    #[test]
    fn an_unknown_reason_keeps_its_first_clause_and_its_details() {
        let short =
            explain("the proof did not pass at 1a2b3c: test_export.py failed on the change");
        assert_eq!(
            short.headline,
            "The proof did not pass at 1a2b3c: test_export.py failed on the change."
        );
        assert_eq!(short.details, None);
        let long = format!(
            "could not read the ledger: {}",
            "the database is locked by another process. ".repeat(4)
        );
        let f = explain(&long);
        assert_eq!(f.headline, "Could not read the ledger.");
        assert_eq!(f.action, None);
        assert_eq!(f.details.as_deref(), Some(long.trim()));
        let plain = explain("the plan waits");
        assert_eq!(plain.headline, "The plan waits.");
        assert_eq!(plain.details, None, "no details that repeat the headline");
    }
}
