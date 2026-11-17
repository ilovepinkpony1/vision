//! Where a ticket goes on when it comes back to the person (design section 11.3): the
//! `[[resume]]` rules of the adapter, the order of the steps of the path, and the head of the
//! pull request on the forge against the clone.
//!
//! A rule never moves a ticket by itself. When the person continues a stopped or a paused
//! ticket, Vision reads the facts of the ticket first: one call of observe, the call that a
//! wait step makes, and the head of the pull request against the clone. Then the rules give a
//! suggestion, and the person takes it or picks another step. A rule suggests a step after the
//! step of the ticket on the path ([`PathOrder`]), or, with `back = true`, also another step.
//! A plain retry, a tick, and a running turn run no rule.

use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use super::machine::{dirty_lines, Ticket};
use crate::adapter::{Adapter, ResumeRule, Step, StepKind, HEAD_STATE};
use crate::ctx::Ctx;
use crate::executor::{self, short, PrFacts, TicketFacts};
use crate::ledger::Ledger;
use crate::paths::{tilde_display, TicketKey};
use crate::util::git;

/// The event of a continue that ran the rules. It replaces the event that the same continue
/// writes with no rule: `take`, `move`, or `retry`.
pub const RESUMED: &str = "resumed";

/// The kind of the `human` row of such a continue.
pub const HUMAN: &str = "resume";

// ----- The order of the path ---------------------------------------------------------------

/// Where a step lies on the path, seen from another step.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Order {
    /// The step list has the second step after the first, and the links lead there.
    After,
    /// The step list has the second step before the first, and the links lead from it to the
    /// first.
    Before,
    /// It is the same step.
    Same,
    /// No link leads from one step to the other, such as two branches of a `when`, or a step
    /// that the adapter does not have.
    Apart,
    /// The links lead from one step to the other only against the order of the step list, so
    /// the order is not clear.
    Against,
}

/// The order of the steps of an adapter. The step list gives it: a step comes after another
/// step when the list has it later and the links of the steps lead there (`next`, the targets
/// of `on` and of `fail`, and the `fail` of a check step). In a loop each step leads to the
/// other, so the list alone decides, and a loop never makes a step look both before and after
/// another step. Two steps with no link between them, or whose links lead only against the
/// list, have no clear order.
#[derive(Clone, Debug)]
pub struct PathOrder {
    ids: Vec<String>,
    /// `reach[a][b]`: one link or more lead from the step `a` to the step `b`.
    reach: Vec<Vec<bool>>,
}

impl PathOrder {
    pub fn of(adapter: &Adapter) -> PathOrder {
        let ids: Vec<String> = adapter.steps.iter().map(|s| s.id.clone()).collect();
        let links: Vec<Vec<usize>> = adapter
            .steps
            .iter()
            .map(|step| {
                links_of(step)
                    .filter_map(|to| ids.iter().position(|id| id == to))
                    .collect()
            })
            .collect();
        let reach = (0..ids.len())
            .map(|from| {
                let mut seen = vec![false; ids.len()];
                let mut todo = links[from].clone();
                while let Some(at) = todo.pop() {
                    if !seen[at] {
                        seen[at] = true;
                        todo.extend(&links[at]);
                    }
                }
                seen
            })
            .collect();
        PathOrder { ids, reach }
    }

    /// Where the step `to` lies, seen from the step `from`.
    pub fn order(&self, from: &str, to: &str) -> Order {
        let index = |id: &str| self.ids.iter().position(|i| i == id);
        let (Some(a), Some(b)) = (index(from), index(to)) else {
            return Order::Apart;
        };
        if a == b {
            return Order::Same;
        }
        let (forward, back) = (self.reach[a][b], self.reach[b][a]);
        match (b > a, forward, back) {
            (true, true, _) => Order::After,
            (false, _, true) => Order::Before,
            (_, false, false) => Order::Apart,
            _ => Order::Against,
        }
    }
}

/// The steps that one step links to, also the steps of the interrupts of a wait step.
fn links_of(step: &Step) -> impl Iterator<Item = &str> {
    let check = match &step.kind {
        StepKind::Check(check) => Some(check.fail.as_str()),
        _ => None,
    };
    let interrupts = match &step.kind {
        StepKind::Wait(wait) => wait
            .interrupt
            .values()
            .flat_map(|values| values.values().map(String::as_str))
            .collect(),
        _ => Vec::new(),
    };
    step.next
        .iter()
        .map(String::as_str)
        .chain(step.on.values().map(String::as_str))
        .chain(step.fail.values().map(String::as_str))
        .chain(check)
        .chain(interrupts)
}

// ----- The rules ---------------------------------------------------------------------------

/// What the rules found for a ticket.
#[derive(Clone, Debug, PartialEq)]
pub enum Found {
    /// The first rule that applies: it holds, and its step comes after the step of the ticket,
    /// or the rule has `back = true`.
    Step {
        /// The number of the rule in the file, from 1.
        rule: usize,
        step: String,
        /// The facts of the rule in words, such as `the pull request #7 is open and ready`.
        reason: String,
        /// For a step that does not come after the step of the ticket, a move back: what pulls
        /// the ticket back, and the work that the move leaves behind, in words (see
        /// [`back_sentence`]).
        back: Option<String>,
    },
    /// No suggestion, and why in words, such as `no rule holds`.
    Nothing(String),
    /// The call of observe failed: why.
    Failed(String),
}

impl Found {
    /// The suggested step, when a rule names one.
    pub fn step(&self) -> Option<&str> {
        match self {
            Found::Step { step, .. } => Some(step),
            _ => None,
        }
    }
}

/// The value of a fact of a rule: [`HEAD_STATE`] from the head, and each other path from the
/// answer of observe, as the text that `watch` of a wait step compares. A path that the answer
/// does not have, and a null, give `none`. A list or a table gives no value.
fn fact_value(path: &str, facts: &TicketFacts, head: &Head) -> Option<String> {
    if path == HEAD_STATE {
        return Some(head.state().to_string());
    }
    facts.watch_text(path)
}

/// True when each fact of the rule holds: its value is one of the values of the rule.
pub fn holds(rule: &ResumeRule, facts: &TicketFacts, head: &Head) -> bool {
    rule.when.iter().all(|(path, values)| {
        fact_value(path, facts, head).is_some_and(|value| values.contains(&value))
    })
}

/// What the rules find for a ticket at the step `stored`, from the facts that Vision read. The
/// search reads the rules in order. A rule that holds and names `stored` ends it: the facts
/// confirm the step of the ticket. A rule that holds and names a step after `stored` is the
/// suggestion. A rule that holds and names another step is the suggestion when it has
/// `back = true`, a move back. Else it does not apply, and the search reads the next rule.
pub fn find(adapter: &Adapter, stored: &str, facts: &TicketFacts, head: &Head) -> Found {
    let order = PathOrder::of(adapter);
    let mut skipped = Vec::new();
    for (i, rule) in adapter.resume.iter().enumerate() {
        if !holds(rule, facts, head) {
            continue;
        }
        let n = i + 1;
        let step = &rule.step;
        let back = match order.order(stored, step) {
            Order::Same => {
                return Found::Nothing(format!(
                    "the rule {n} holds and names the step {stored}, so the facts confirm the step where the ticket is"
                ))
            }
            Order::After => None,
            _ if rule.back => Some(back_reason(rule, facts, head)),
            _ => {
                skipped.push(format!("{n} ({step})"));
                continue;
            }
        };
        return Found::Step {
            rule: n,
            step: step.clone(),
            reason: reason(rule, facts, head),
            back,
        };
    }
    match skipped.as_slice() {
        [] => Found::Nothing("no rule holds".into()),
        [one] => Found::Nothing(format!(
            "no rule applies: the rule {one} holds, but its step does not come after the step {stored}, and the rule has no back = true"
        )),
        many => Found::Nothing(format!(
            "no rule applies: the rules {} hold, but their steps do not come after the step {stored}, and the rules have no back = true",
            join_and(many)
        )),
    }
}

/// The facts of a rule that holds, in words: the pull request first, then each other fact of
/// the rule, then what waits on the pull request. Such as `the pull request #7 is open and
/// ready, and 2 review comments wait`.
pub fn reason(rule: &ResumeRule, facts: &TicketFacts, head: &Head) -> String {
    let mut parts = Vec::new();
    if let Some(pr) = &facts.pr {
        parts.push(pr_words(pr));
    }
    parts.extend(rule_parts(rule, facts, head));
    join_and(&parts)
}

/// What pulls a ticket back for a rule with `back = true`, in words: the facts of the rule and
/// what waits on the pull request, then each env that holds or runs the head, the work that
/// the move leaves behind. Such as `2 review comments wait, and the head runs on void`.
pub fn back_reason(rule: &ResumeRule, facts: &TicketFacts, head: &Head) -> String {
    let mut parts = rule_parts(rule, facts, head);
    if parts.is_empty() {
        if let Some(pr) = &facts.pr {
            parts.push(pr_words(pr));
        }
    }
    parts.extend(env_holds_words(facts));
    join_and(&parts)
}

/// The sentence of a move back from the step `stored`, such as `This goes back from
/// ready-to-merge: 2 review comments wait, and the head runs on void`.
pub fn back_sentence(stored: &str, why: &str) -> String {
    format!("This goes back from {stored}: {why}")
}

/// The facts of a rule in words, with no word of the state of the pull request: each fact of
/// the rule that the words of the pull request do not say, then what waits on it.
fn rule_parts(rule: &ResumeRule, facts: &TicketFacts, head: &Head) -> Vec<String> {
    let mut parts = Vec::new();
    for path in rule.when.keys() {
        if path == HEAD_STATE {
            parts.extend(head.words());
        } else if facts.pr.is_none() || !PR_PATHS.contains(&path.as_str()) {
            parts.push(fact_words(path, facts));
        }
    }
    if let Some(pr) = &facts.pr {
        if pr.changes_requested {
            parts.push("a reviewer asks for changes".to_string());
        }
        match pr.comments_waiting {
            0 => {}
            1 => parts.push("1 review comment waits".to_string()),
            n => parts.push(format!("{n} review comments wait")),
        }
    }
    parts
}

/// The paths whose facts the words of the pull request already say.
const PR_PATHS: [&str; 5] = [
    "pr.merged",
    "pr.closed",
    "pr.draft",
    "pr.comments_waiting",
    "pr.changes_requested",
];

fn pr_words(pr: &PrFacts) -> String {
    let state = if pr.merged {
        "is merged"
    } else if pr.closed {
        "is closed"
    } else if pr.draft {
        "is a draft"
    } else {
        "is open and ready"
    };
    format!("the pull request #{} {state}", pr.number)
}

fn fact_words(path: &str, facts: &TicketFacts) -> String {
    let value = facts
        .watch_text(path)
        .map_or_else(|| "a list or a table".to_string(), |v| plain(&v, 60));
    match path {
        "tracker_status" => format!("the tracker status is {value}"),
        "review.state" => format!("the review state is {value}"),
        "ci.state" => format!("CI is {value}"),
        _ => format!("{path} is {value}"),
    }
}

/// A text from outside on one line: no control character, cut to `max` characters.
fn plain(text: &str, max: usize) -> String {
    let flat: String = text.chars().filter(|c| !c.is_control()).collect();
    if flat.chars().count() > max {
        let mut out: String = flat.chars().take(max).collect();
        out.push_str(" ...");
        out
    } else {
        flat
    }
}

/// `a`, `a and b`, or `a, b, and c`.
fn join_and(parts: &[String]) -> String {
    match parts {
        [] => String::new(),
        [one] => one.clone(),
        [a, b] => format!("{a} and {b}"),
        [rest @ .., last] => format!("{}, and {last}", rest.join(", ")),
    }
}

/// The rules applied to a ticket: what they found, the answer of observe, and the head of the
/// pull request against the clone.
#[derive(Clone, Debug, PartialEq)]
pub struct Checked {
    pub found: Found,
    /// The answer of observe. `None` when the call failed.
    pub facts: Option<TicketFacts>,
    /// The head of the open pull request, which Vision reads before the rules.
    pub head: Head,
}

impl Checked {
    /// The suggested step, when a rule names one.
    pub fn suggested(&self) -> Option<&str> {
        self.found.step()
    }

    /// The number of the pull request that observe gave.
    pub fn pr(&self) -> Option<i64> {
        self.facts.as_ref()?.pr.as_ref().map(|pr| pr.number)
    }

    /// Each fact that Vision read, in words, in a fixed order (see [`facts_words`]). Empty when
    /// the call of observe failed.
    pub fn words(&self) -> Vec<String> {
        self.facts
            .as_ref()
            .map(|facts| facts_words(facts, &self.head))
            .unwrap_or_default()
    }
}

/// Applies the rules of the adapter to a ticket at its step. Vision reads the facts first: one
/// call of observe, the call of a wait step, and then the head of the pull request against the
/// branch of the clone, with `switches` as in [`head`]. `None` when the adapter has no
/// `[[resume]]` rule: then nothing runs. A call that fails gives [`Found::Failed`] and no
/// facts.
pub(crate) fn check(
    ctx: &Ctx,
    ledger: &Ledger,
    adapter: &Adapter,
    ticket: &Ticket,
    switches: bool,
) -> Option<Checked> {
    if adapter.resume.is_empty() {
        return None;
    }
    Some(match executor::observe_ticket(ctx, ledger, &ticket.key) {
        Ok(facts) => {
            let branch = ticket
                .row
                .branch
                .clone()
                .filter(|b| !b.trim().is_empty())
                .unwrap_or_else(|| adapter.branch_for(&ticket.key));
            let head = head(ticket, &branch, &facts, switches);
            Checked {
                found: find(adapter, ticket.state(), &facts, &head),
                facts: Some(facts),
                head,
            }
        }
        Err(e) => Checked {
            found: Found::Failed(plain(&format!("{e:#}"), 300)),
            facts: None,
            head: Head::None,
        },
    })
}

/// Each fact that Vision read for a continue, in words, in a fixed order: the status of the
/// tracker; the pull request (its number, draft or ready, merged, closed, a conflict); the
/// review state, the comments that wait, and a change request; CI; each env (holds the head,
/// runs it); and the head of the pull request against the clone.
pub fn facts_words(facts: &TicketFacts, head: &Head) -> Vec<String> {
    let mut out = Vec::new();
    out.push(match facts.tracker_status.as_deref().map(str::trim) {
        Some(status) if !status.is_empty() => {
            format!("the tracker status is {}", plain(status, 60))
        }
        _ => "the tracker gives no status".to_string(),
    });
    let Some(pr) = &facts.pr else {
        out.push("there is no pull request".to_string());
        out.extend(ci_words(facts));
        out.extend(env_words(facts));
        return out;
    };
    let mut words = pr_words(pr);
    if pr.conflict {
        words.push_str(", and it conflicts with its base");
    }
    out.push(words);
    let mut review = Vec::new();
    if let Some(state) = facts.watch_text("review.state").filter(|s| s != "none") {
        review.push(format!("the review state is {}", plain(&state, 40)));
    }
    review.push(match pr.comments_waiting {
        0 => "no review comment waits".to_string(),
        1 => "1 review comment waits".to_string(),
        n => format!("{n} review comments wait"),
    });
    if pr.changes_requested {
        review.push("a reviewer asks for changes".to_string());
    }
    out.push(join_and(&review));
    out.extend(ci_words(facts));
    out.extend(env_words(facts));
    out.extend(head.words());
    out
}

fn ci_words(facts: &TicketFacts) -> Option<String> {
    let ci = facts.ci.as_ref()?;
    let state = plain(ci.state.trim(), 40);
    Some(match (state.as_str(), ci.head.as_deref()) {
        ("" | "none", _) => "CI has no run".to_string(),
        (state, Some(head)) if is_commit(head) => format!("CI is {state} at {}", short(head)),
        (state, _) => format!("CI is {state}"),
    })
}

/// Each env of the answer, by name: whether its branch holds the head, and whether it runs it.
fn env_words(facts: &TicketFacts) -> Vec<String> {
    facts
        .envs
        .iter()
        .map(|(name, env)| {
            let name = plain(name, 40);
            let holds = env.get("holds").and_then(Value::as_bool) == Some(true);
            match (holds, env.get("deployed").and_then(Value::as_bool)) {
                (true, Some(true)) => format!("the head runs on {name}"),
                (true, Some(false)) => format!("{name} holds the head, but does not run it yet"),
                (true, None) => format!("{name} holds the head"),
                (false, _) => format!("{name} does not hold the head"),
            }
        })
        .collect()
}

/// The facts of an env that holds or runs the head, for the words of a move back: the work
/// that the move leaves behind.
fn env_holds_words(facts: &TicketFacts) -> Vec<String> {
    env_words(facts)
        .into_iter()
        .filter(|w| !w.ends_with("does not hold the head"))
        .collect()
}

// ----- The head of the pull request --------------------------------------------------------

/// The head of the open pull request on the forge, seen from the branch of the ticket in its
/// clone.
#[derive(Clone, Debug, PartialEq)]
pub enum Head {
    /// Nothing to compare: no open pull request, or no head of it.
    None,
    /// The branch is at the head of the pull request: the commit.
    Same { forge: String },
    /// The clone has the head of the pull request, one fast-forward after the branch: someone
    /// pushed it by hand. The continue moves the branch forward to it.
    Forward { from: String, to: String },
    /// The branch has this many commits after the head of the pull request, and keeps them.
    Ahead { forge: String, commits: usize },
    /// The clone does not have the head of the pull request, also after a fetch: `fetch` is
    /// why the fetch failed, or `None` when it worked.
    Behind {
        forge: String,
        fetch: Option<String>,
    },
    /// The branch in the clone and the branch of the pull request went apart.
    Apart { forge: String },
    /// The clone has the head of the pull request, but changes in its work tree stop the
    /// fast-forward.
    Dirty { forge: String, changes: usize },
    /// The clone is not on the branch of the ticket: the branch that it is on.
    Elsewhere { branch: String },
    /// Vision could not read the clone: why.
    Unread(String),
}

/// What stops a continue at the head of the pull request: what is wrong, and what the person
/// does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeadProblem {
    pub what: String,
    pub fix: String,
}

impl Head {
    /// What stops the continue, or `None` when the continue can go on. Nothing moves while the
    /// head has a problem. `clone` names the clone in the text.
    pub fn problem(&self, pr: Option<i64>, clone: &str) -> Option<HeadProblem> {
        let pr = pr_name(pr);
        let (what, fix) = match self {
            Head::Behind { forge, fetch: None } => (
                format!(
                    "the clone is behind the remote: Vision fetched, and the head {} of {pr} is still not in the clone {clone}",
                    short(forge)
                ),
                "Push that commit to the remote of the clone, or fetch it into the clone by hand"
                    .to_string(),
            ),
            Head::Behind {
                forge,
                fetch: Some(why),
            } => (
                format!(
                    "the clone is behind the remote: the head {} of {pr} is not in the clone {clone}, and the fetch of the clone failed: {why}",
                    short(forge)
                ),
                format!("Fix the fetch, which `git -C {clone} fetch --all --prune` shows"),
            ),
            Head::Apart { forge } => (
                format!(
                    "the branch in the clone {clone} and the branch of {pr} went apart: neither holds the head of the other, and the head of {pr} is {}",
                    short(forge)
                ),
                "Merge or reset the branch in the clone by hand".to_string(),
            ),
            Head::Dirty { forge, changes } => (
                format!(
                    "the clone {clone} has {changes} change(s) in its work tree, so Vision cannot move its branch to the head {} of {pr}",
                    short(forge)
                ),
                "Commit or clean them".to_string(),
            ),
            Head::Elsewhere { branch } => (
                format!("the clone {clone} is on {branch}, not on the branch of the ticket"),
                "Switch it back to the branch of the ticket".to_string(),
            ),
            Head::Unread(why) => (
                format!("Vision could not read the clone {clone}: {why}"),
                "Fix the clone".to_string(),
            ),
            Head::None | Head::Same { .. } | Head::Forward { .. } | Head::Ahead { .. } => {
                return None
            }
        };
        Some(HeadProblem { what, fix })
    }

    /// What the continue does with the head, in words, or `None` when it does nothing.
    pub fn note(&self, pr: Option<i64>) -> Option<String> {
        let pr = pr_name(pr);
        match self {
            Head::Forward { to, .. } => Some(format!(
                "Someone pushed the head {} of {pr} by hand, and the clone has it: the branch of the clone moves forward to it",
                short(to)
            )),
            Head::Ahead { commits, .. } => Some(format!(
                "The clone has {} that {pr} does not have. The next push sends them",
                count(*commits, "commit")
            )),
            _ => None,
        }
    }
}

impl Head {
    /// The value of [`HEAD_STATE`] for a `[[resume]]` rule.
    pub fn state(&self) -> &'static str {
        match self {
            Head::None => "none",
            Head::Same { .. } => "same",
            Head::Forward { .. } => "forward",
            Head::Ahead { .. } => "ahead",
            Head::Behind { .. } => "behind",
            Head::Apart { .. } => "apart",
            Head::Dirty { .. } => "dirty",
            Head::Elsewhere { .. } => "elsewhere",
            Head::Unread(_) => "unread",
        }
    }

    /// The head in words, for the facts that Vision read. Nothing with no open pull request.
    pub fn words(&self) -> Option<String> {
        Some(match self {
            Head::None => return None,
            Head::Same { .. } => {
                "the branch of the clone is at the head of the pull request".into()
            }
            Head::Forward { .. } => {
                "the pull request has later commits than the clone, such as a push by hand".into()
            }
            Head::Ahead { commits, .. } => format!(
                "the clone has {} that the pull request does not have",
                count(*commits, "commit")
            ),
            Head::Behind { .. } => {
                "the clone does not have the head of the pull request, also after a fetch".into()
            }
            Head::Apart { .. } => {
                "the branch of the clone and the branch of the pull request went apart".into()
            }
            Head::Dirty { changes, .. } => {
                format!("the work tree of the clone has {changes} change(s)")
            }
            Head::Elsewhere { branch } => format!("the clone is on {branch}"),
            Head::Unread(_) => "Vision could not read the clone".into(),
        })
    }
}

fn pr_name(pr: Option<i64>) -> String {
    pr.map_or("the pull request".to_string(), |n| {
        format!("the pull request #{n}")
    })
}

fn count(n: usize, word: &str) -> String {
    if n == 1 {
        format!("1 {word}")
    } else {
        format!("{n} {word}s")
    }
}

/// True for a commit id: hex digits, from 7 to 64 of them.
fn is_commit(text: &str) -> bool {
    (7..=64).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The head of the open pull request against the branch of the ticket in its clone. With
/// `switches`, the continue switches the clone to the branch first, as `vision take` does for
/// a stopped ticket, so a clone on another branch is no problem. When the clone does not have
/// the head, it fetches first: the person can push from another checkout. The fetch changes no
/// branch and no work tree.
pub(crate) fn head(ticket: &Ticket, branch: &str, facts: &TicketFacts, switches: bool) -> Head {
    let Some(pr) = facts.pr.as_ref().filter(|pr| !pr.merged && !pr.closed) else {
        return Head::None;
    };
    let Some(forge) = pr.head.as_deref().map(str::trim).filter(|h| is_commit(h)) else {
        return Head::None;
    };
    let clone = match ticket.clone_dir() {
        Ok(clone) => clone,
        Err(e) => return Head::Unread(format!("{e:#}")),
    };
    let on = git::run(&clone, &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default();
    if !switches && on != branch {
        let branch = if on.is_empty() {
            "no branch".to_string()
        } else {
            format!("the branch {on}")
        };
        return Head::Elsewhere { branch };
    }
    let local = match git::run(
        &clone,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    ) {
        Ok(local) => local,
        Err(_) => return Head::Unread(format!("the branch {branch} is not in the clone")),
    };
    let commit = |clone: &Path| {
        git::run(
            clone,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{forge}^{{commit}}"),
            ],
        )
        .ok()
    };
    let forge = match commit(&clone) {
        Some(full) => full,
        None => {
            if let Err(e) = git::fetch_all(&clone) {
                let why = format!("{e:#}");
                let why = why.split_whitespace().collect::<Vec<_>>().join(" ");
                return Head::Behind {
                    forge: forge.to_string(),
                    fetch: Some(plain(&why, 300)),
                };
            }
            match commit(&clone) {
                Some(full) => full,
                None => {
                    return Head::Behind {
                        forge: forge.to_string(),
                        fetch: None,
                    }
                }
            }
        }
    };
    if forge == local {
        return Head::Same { forge };
    }
    let ancestor =
        |a: &str, b: &str| git::run(&clone, &["merge-base", "--is-ancestor", a, b]).is_ok();
    if ancestor(&local, &forge) {
        // The fast-forward changes the work tree, so a change there stops it. A clone on
        // another branch has a clean tree, or the switch of the take refuses it first.
        if on == branch {
            match dirty_lines(&clone) {
                Ok(lines) if !lines.is_empty() => {
                    return Head::Dirty {
                        forge,
                        changes: lines.len(),
                    }
                }
                Ok(_) => {}
                Err(e) => return Head::Unread(format!("{e:#}")),
            }
        }
        return Head::Forward {
            from: local,
            to: forge,
        };
    }
    if ancestor(&forge, &local) {
        let commits = git::run(
            &clone,
            &["rev-list", "--count", &format!("{forge}..{local}")],
        )
        .ok()
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(0);
        return Head::Ahead { forge, commits };
    }
    Head::Apart { forge }
}

/// Moves the branch that the clone is on forward to `to`: a fast-forward only, so no commit
/// of the clone goes away.
pub fn take_head(clone: &Path, to: &str) -> Result<()> {
    if !is_commit(to) {
        bail!("`{to}` is not a commit id");
    }
    git::run(clone, &["merge", "--ff-only", "--quiet", to]).with_context(|| {
        format!(
            "could not move the branch of the clone {} forward to {}",
            clone.display(),
            short(to)
        )
    })?;
    Ok(())
}

// ----- What a continue records and says ----------------------------------------------------

/// What a continue that ran the rules records in its event [`RESUMED`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Resumed {
    /// The step that the rules suggested.
    pub suggestion: Option<String>,
    /// The number of the rule of the suggestion, from 1.
    pub rule: Option<usize>,
    /// The facts of the suggestion in words.
    pub reason: Option<String>,
    /// Why the rules suggested nothing.
    pub nothing: Option<String>,
    /// True when the call of observe failed.
    pub failed: bool,
    pub pr: Option<i64>,
    /// The number of the open pull request when the ticket row names none: the continue writes
    /// it in the row.
    pub pr_written: Option<i64>,
    /// The other number that the ticket row names, when it is not the number of the open pull
    /// request: the continue keeps it, and says so.
    pub pr_kept: Option<i64>,
    /// The head of the open pull request when the branch of the clone holds it after the
    /// continue: the continue writes it as the head of the ticket row.
    pub pr_head: Option<String>,
    /// The head of the pull request that the continue took: from, to.
    pub head: Option<(String, String)>,
    /// The commits of the clone that the pull request does not have.
    pub ahead: Option<usize>,
    /// True when the suggestion goes back from the stored step.
    pub back: bool,
    /// The sentence of a suggestion that goes back (see [`back_sentence`]).
    pub back_text: Option<String>,
    /// Each fact that Vision read, in words (see [`facts_words`]).
    pub facts: Vec<String>,
}

impl Resumed {
    /// What a continue of a ticket at the step `stored` records, with `row_pr`, the number of
    /// the pull request that the ticket row names.
    pub fn of(checked: &Checked, stored: &str, row_pr: Option<i64>) -> Resumed {
        let mut out = Resumed {
            pr: checked.pr(),
            facts: checked.words(),
            ..Resumed::default()
        };
        let open = checked
            .facts
            .as_ref()
            .and_then(|f| f.pr.as_ref())
            .filter(|pr| !pr.merged && !pr.closed && pr.number > 0)
            .map(|pr| pr.number);
        match (row_pr, open) {
            (None, Some(number)) => out.pr_written = Some(number),
            (Some(row), Some(number)) if row != number => out.pr_kept = Some(row),
            _ => {}
        }
        match &checked.found {
            Found::Step {
                rule,
                step,
                reason,
                back,
            } => {
                out.suggestion = Some(step.clone());
                out.rule = Some(*rule);
                out.reason = Some(reason.clone());
                out.back = back.is_some();
                out.back_text = back.as_deref().map(|why| back_sentence(stored, why));
            }
            Found::Nothing(why) => out.nothing = Some(why.clone()),
            Found::Failed(why) => {
                out.nothing = Some(why.clone());
                out.failed = true;
            }
        }
        match &checked.head {
            Head::Forward { from, to } => {
                out.head = Some((from.clone(), to.clone()));
                out.pr_head = Some(to.clone());
            }
            Head::Same { forge } => out.pr_head = Some(forge.clone()),
            Head::Ahead { commits, .. } => out.ahead = Some(*commits),
            _ => {}
        }
        out
    }

    /// The fields of the event: the stored step `from`, the chosen step, how the ticket
    /// entered it, what the rules found, and the head that the continue took.
    pub fn detail(&self, from: &str, step: &str, how: &str) -> Value {
        json!({
            "from": from,
            "step": step,
            "how": how,
            "suggestion": self.suggestion,
            "took": self.suggestion.as_deref() == Some(step),
            "rule": self.rule,
            "reason": self.reason,
            "nothing": self.nothing,
            "failed": self.failed,
            "pr": self.pr,
            "pr_written": self.pr_written,
            "pr_kept": self.pr_kept,
            "pr_head": self.pr_head,
            "head": self.head.as_ref().map(|(from, to)| json!({"from": from, "to": to})),
            "ahead": self.ahead,
            "back": self.back,
            "facts": self.facts,
        })
    }

    /// What the rules found, the facts that Vision read, and what the continue did with the
    /// head, in sentences for the output of a command, for a continue at `step`.
    pub fn lines(&self, step: &str) -> Vec<String> {
        let mut out = Vec::new();
        let rule = self.rule.unwrap_or(0);
        match (&self.suggestion, &self.reason, &self.back_text) {
            (Some(s), Some(_), Some(back)) if s == step => {
                out.push(format!("Vision suggested this step (rule {rule}). {back}"))
            }
            (Some(s), Some(reason), None) if s == step => {
                out.push(format!("Vision suggested this step (rule {rule}): {reason}"))
            }
            (Some(s), Some(_), Some(back)) => out.push(format!(
                "Vision suggested the step {s} (rule {rule}). {back}"
            )),
            (Some(s), Some(reason), None) => out.push(format!(
                "Vision suggested the step {s} (rule {rule}): {reason}"
            )),
            _ if self.failed => out.push(format!(
                "Vision could not check the tracker and the pull request, so it suggested nothing: {}",
                self.nothing.as_deref().unwrap_or("the call failed")
            )),
            _ => out.push(format!(
                "Vision found nothing further: {}",
                self.nothing.as_deref().unwrap_or("no rule holds")
            )),
        }
        for (i, fact) in self.facts.iter().enumerate() {
            out.push(match i {
                0 => format!("Vision read: {fact}"),
                _ => capital(fact),
            });
        }
        if let Some((_, to)) = &self.head {
            out.push(format!(
                "It took the head {} of {} that someone pushed by hand: the branch of the clone moved forward to it",
                short(to),
                pr_name(self.pr)
            ));
        }
        if let Some(n) = self.ahead.filter(|n| *n > 0) {
            out.push(format!(
                "The clone has {} that {} does not have. The next push sends them",
                count(n, "commit"),
                pr_name(self.pr)
            ));
        }
        if let (Some(kept), Some(open)) = (self.pr_kept, self.pr) {
            out.push(format!(
                "The ticket row names the pull request #{kept}, and the open pull request of the branch is #{open}. Vision kept #{kept} in the row"
            ));
        }
        out
    }
}

/// The words of an event [`RESUMED`] with no actor, for `vision log` and the activity, such
/// as `resumed the ticket at in-review, as Vision suggested`.
pub fn resumed_words(detail: &Value) -> String {
    let text = |key: &str| detail.get(key).and_then(Value::as_str);
    let step = text("step").unwrap_or("?");
    let back = detail.get("back").and_then(Value::as_bool) == Some(true);
    let mut out = format!("resumed the ticket at {step}");
    match text("suggestion") {
        Some(s) if s == step && back => out.push_str(&format!(
            ", as Vision suggested, back from {}",
            text("from").unwrap_or("?")
        )),
        Some(s) if s == step => out.push_str(", as Vision suggested"),
        Some(s) => out.push_str(&format!("; Vision suggested {s}")),
        None if detail.get("failed").and_then(Value::as_bool) == Some(true) => {
            out.push_str("; Vision could not check where it is")
        }
        None => out.push_str("; Vision found nothing further"),
    }
    if let Some(to) = detail
        .get("head")
        .and_then(|h| h.get("to"))
        .and_then(Value::as_str)
    {
        out.push_str(&format!(
            ", and took the head {} of the pull request",
            short(to)
        ));
    }
    out
}

/// What a plain `vision take` prints when it does not continue: the suggestion with its
/// reason, each fact that Vision read, and the two commands. Empty when the rules suggest
/// nothing.
pub fn suggest_text(key: &TicketKey, stored: &str, checked: &Checked) -> String {
    let Found::Step {
        rule,
        step,
        reason,
        back,
    } = &checked.found
    else {
        return String::new();
    };
    let width = step.len().max(stored.len());
    let line = |to: &str| {
        let label = format!("To continue at {to}:");
        format!(
            "{label:<w$}  vision take {key} --step {to}",
            w = width + "To continue at :".len()
        )
    };
    let first = match back {
        Some(why) => format!(
            "Vision thinks that {key} is at the step {step}, not at the step {stored} where it stopped (rule {rule}). {}.",
            back_sentence(stored, why)
        ),
        None => format!(
            "Vision thinks that {key} is at the step {step}, not at the step {stored} where it stopped: {reason} (rule {rule})."
        ),
    };
    let mut out = format!("{first} It did not continue the ticket.\nVision read:\n");
    for fact in checked.words() {
        out.push_str(&format!("  {fact}\n"));
    }
    out.push_str(&format!("{}\n{}", line(step), line(stored)));
    out
}

/// A text with a capital first letter.
fn capital(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// The answer of a plain `vision take` that does not continue, because a rule suggests another
/// step: the command prints it to standard output and exits with code 1. Nothing changes.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Suggests(pub String);

// ----- A ticket that is in progress outside Vision -----------------------------------------

/// Applies the rules of the adapter to a ticket that Vision does not hold yet, such as a ticket
/// that someone works on outside Vision: one call of observe with the branch and the pull
/// request of the queue. The search starts at the first step of the path, so each rule that
/// holds names a later step. Vision has no clone of the ticket yet, so it reads no head. A rule
/// that names an end step suggests nothing, because Vision takes no ticket that is done. `None`
/// when the adapter has no `[[resume]]` rule.
pub(crate) fn check_outside(
    ctx: &Ctx,
    adapter: &Adapter,
    key: &TicketKey,
    branch: &str,
    pr: Option<i64>,
) -> Option<Checked> {
    if adapter.resume.is_empty() {
        return None;
    }
    Some(match executor::observe_outside(ctx, key, branch, pr) {
        Ok(facts) => {
            let mut found = find(adapter, &adapter.entry().id, &facts, &Head::None);
            if let Found::Step {
                rule, step, reason, ..
            } = &found
            {
                if adapter
                    .step(step)
                    .is_some_and(|s| matches!(s.kind, StepKind::End))
                {
                    found = Found::Nothing(format!(
                        "the rule {rule} holds and names the end step {step}: {reason}. Vision takes no ticket that is done"
                    ));
                }
            }
            Checked {
                found,
                facts: Some(facts),
                head: Head::None,
            }
        }
        Err(e) => Checked {
            found: Found::Failed(plain(&format!("{e:#}"), 300)),
            facts: None,
            head: Head::None,
        },
    })
}

/// Where a ticket that is in progress outside Vision is, in words: its branch, and its pull
/// request when it has one.
pub fn outside_place(branch: &str, pr: Option<i64>) -> String {
    match pr {
        Some(n) => format!("on the branch {branch}, with the pull request #{n}"),
        None => format!("on the branch {branch}, with no pull request"),
    }
}

/// What a plain `vision take` of a ticket that is in progress outside Vision prints: where it
/// is, each fact that Vision read, what the rules found, and the command that takes it.
pub fn outside_text(
    key: &TicketKey,
    branch: &str,
    pr: Option<i64>,
    checked: Option<&Checked>,
) -> String {
    let mut out = format!(
        "{key} is in progress outside Vision, {}. Vision did not take it.\n",
        outside_place(branch, pr)
    );
    let words = checked.map(Checked::words).unwrap_or_default();
    if !words.is_empty() {
        out.push_str("Vision read:\n");
        for fact in words {
            out.push_str(&format!("  {fact}\n"));
        }
    }
    let any = format!("To take it at a step:  vision take {key} --step STEP");
    match checked.map(|c| &c.found) {
        Some(Found::Step {
            rule, step, reason, ..
        }) => out.push_str(&format!(
            "Vision thinks that it is at the step {step}: {reason} (rule {rule}).\nTo take it there:      vision take {key} --step {step}"
        )),
        Some(Found::Nothing(why)) => {
            out.push_str(&format!("Vision found no step: {why}.\n{any}"));
        }
        Some(Found::Failed(why)) => {
            out.push_str(&format!("Vision could not read its facts: {why}.\n{any}"));
        }
        None => out.push_str(&format!(
            "The adapter has no resume rules, so Vision read no fact.\n{any}"
        )),
    }
    out
}

/// The view of the start card of a ticket that is in progress outside Vision: what the rules
/// found from its facts, and the steps that the person can pick. It has no stored step, and no
/// end step to pick. It makes the one call of observe of the rules, and it changes nothing.
pub fn read_outside_view(
    ctx: &Ctx,
    key: &TicketKey,
    branch: &str,
    pr: Option<i64>,
) -> Result<View> {
    ctx.require_setup()?;
    let adapter = crate::adapter::load_with(ctx.config()?, &ctx.paths)?;
    let checked = check_outside(ctx, &adapter, key, branch, pr);
    let steps = adapter
        .steps
        .iter()
        .filter(|s| !matches!(s.kind, StepKind::End))
        .map(|s| s.id.clone())
        .collect();
    Ok(View {
        key: key.to_string(),
        stored: String::new(),
        steps,
        rules: checked.is_some(),
        pr: checked.as_ref().and_then(Checked::pr).or(pr),
        facts: checked.as_ref().map(Checked::words).unwrap_or_default(),
        back: None,
        head: Head::None,
        found: checked.map(|c| c.found),
        clone: None,
        stopped: false,
        paused: false,
        needs_you: false,
    })
}

// ----- The continue card ---------------------------------------------------------------------

/// What the continue card of the app shows for a stopped or paused ticket: what the rules
/// found, the head of the pull request, and the steps that the person can pick.
#[derive(Clone, Debug, PartialEq)]
pub struct View {
    pub key: String,
    /// The step that the ticket holds.
    pub stored: String,
    /// The steps that the person can pick, in the order of the step list. The end step is one
    /// of them only when the rules suggest it, because only a rule moves a ticket there.
    pub steps: Vec<String>,
    /// False when the adapter has no `[[resume]]` rule: then no call of observe ran.
    pub rules: bool,
    /// What the rules found. `None` with no rule.
    pub found: Option<Found>,
    pub head: Head,
    pub pr: Option<i64>,
    /// The clone of the ticket, with `~` for the home folder of the person.
    pub clone: Option<String>,
    pub stopped: bool,
    pub paused: bool,
    /// True for the flag needs-you of a rule of the tick. The test of the head of a continue
    /// also sets that flag, but the next continue clears such a flag by itself.
    pub needs_you: bool,
    /// Each fact that Vision read, in words (see [`facts_words`]).
    pub facts: Vec<String>,
    /// The sentence of a suggestion that goes back (see [`back_sentence`]).
    pub back: Option<String>,
}

impl View {
    /// The suggested step, when a rule names one.
    pub fn suggested(&self) -> Option<&str> {
        self.found.as_ref().and_then(Found::step)
    }
}

/// The view of the continue card of the ticket `key`, with a context of its own, as each read
/// of the app makes it. The ticket must be stopped or paused. It reads the ledger and makes
/// the one call of observe of the rules, and it changes nothing.
pub fn read_view(ctx: &Ctx, key: &TicketKey) -> Result<View> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let adapter = crate::adapter::load_with(ctx.config()?, &ctx.paths)?;
    let row = ledger
        .ticket(key.as_str())?
        .filter(|row| row.state.as_deref().is_some_and(|s| !s.is_empty()));
    let Some(row) = row else {
        bail!("Vision has no ticket {key}");
    };
    view(ctx, &ledger, &adapter, &Ticket::of(row)?)
}

/// True when the test of the head of a continue set the flag needs-you of the ticket.
pub(crate) fn resume_flag(ticket: &Ticket) -> bool {
    ticket
        .counts
        .reasons
        .get(super::state::NEEDS_YOU)
        .is_some_and(|r| r.by.as_deref() == Some(super::state::RESUME))
}

fn view(ctx: &Ctx, ledger: &Ledger, adapter: &Adapter, ticket: &Ticket) -> Result<View> {
    let key = &ticket.key;
    if ticket.counts.finished.is_some() {
        bail!("{key} is done, so there is nothing to continue");
    }
    let has = |flag: &str| ticket.flags.iter().any(|f| f == flag);
    let (stopped, paused) = (has(super::state::STOPPED), has(super::state::PAUSED));
    if !stopped && !paused {
        bail!("{key} is neither stopped nor paused, so there is nothing to continue");
    }
    let checked = check(ctx, ledger, adapter, ticket, stopped);
    let back = checked.as_ref().and_then(|c| match &c.found {
        Found::Step {
            back: Some(why), ..
        } => Some(back_sentence(ticket.state(), why)),
        _ => None,
    });
    let suggested = checked.as_ref().and_then(Checked::suggested);
    let steps = adapter
        .steps
        .iter()
        .filter(|s| !matches!(s.kind, StepKind::End) || suggested == Some(s.id.as_str()))
        .map(|s| s.id.clone())
        .collect();
    Ok(View {
        key: key.to_string(),
        stored: ticket.state().to_string(),
        steps,
        rules: checked.is_some(),
        pr: checked.as_ref().and_then(Checked::pr),
        facts: checked.as_ref().map(Checked::words).unwrap_or_default(),
        back,
        head: checked.as_ref().map_or(Head::None, |c| c.head.clone()),
        found: checked.map(|c| c.found),
        clone: ticket.clone_dir().ok().map(|c| tilde_display(&c)),
        stopped,
        paused,
        needs_you: has(super::state::NEEDS_YOU) && !resume_flag(ticket),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn adapter(text: &str) -> Adapter {
        Adapter::parse(text, Path::new("/adapters/toy")).unwrap()
    }

    /// A path with a loop of review and fix, a loop of the code review, and two branches of
    /// a `when`.
    const PATH: &str = r#"
[[step]]
id = "build"
kind = "turn"
agent = "builder"
turn = "worker"
next = "review"

[[step]]
id = "review"
kind = "turn"
agent = "reviewer"
turn = "worker"
next = "qa"

[[step]]
id = "qa"
kind = "turn"
agent = "tester"
turn = "tester"
findings = "qa"
on = { findings = "fix", none = "open-pr" }

[[step]]
id = "fix"
kind = "turn"
agent = "builder"
turn = "worker"
next = "review"

[[step]]
id = "open-pr"
kind = "act"
run = ["git.push"]
next = "in-review"

[[step]]
id = "in-review"
kind = "wait"
watch = "review.state"
on = { comments = "comments", approved = "wide", merged = "done" }

[[step]]
id = "comments"
kind = "turn"
agent = "shepherd"
turn = "worker"
next = "in-review"

[[step]]
id = "wide"
kind = "turn"
agent = "reviewer"
turn = "reader"
next = "done"

[[step]]
id = "narrow"
kind = "turn"
agent = "reviewer"
turn = "reader"
next = "done"

[[step]]
id = "done"
kind = "end"

[verbs."git.push"]
"#;

    #[test]
    fn the_step_list_orders_the_steps_along_their_links_also_in_a_loop() {
        let order = PathOrder::of(&adapter(PATH));
        // Forward along the path, also out of a loop and across a turn step with on.
        for (from, to) in [
            ("build", "review"),
            ("build", "in-review"),
            ("review", "open-pr"),
            ("qa", "comments"),
            ("fix", "in-review"),
            ("comments", "done"),
            ("open-pr", "done"),
        ] {
            assert_eq!(order.order(from, to), Order::After, "{from} -> {to}");
            assert_eq!(order.order(to, from), Order::Before, "{to} -> {from}");
        }
        // The steps of one loop: each leads to the other, so the step list alone decides. A
        // step is never both before and after another step.
        for (a, b) in [
            ("review", "qa"),
            ("qa", "fix"),
            ("review", "fix"),
            ("in-review", "comments"),
        ] {
            assert_eq!(order.order(a, b), Order::After, "{a} -> {b}");
            assert_eq!(order.order(b, a), Order::Before, "{b} -> {a}");
        }
        // No link leads between two branches, nor to a step that nothing links to.
        assert_eq!(order.order("wide", "narrow"), Order::Apart);
        assert_eq!(order.order("build", "narrow"), Order::Apart);
        assert_eq!(order.order("build", "nowhere"), Order::Apart);
        assert_eq!(order.order("qa", "qa"), Order::Same);
        // A step list against its links: first leads to third, and third to second. The links
        // lead from third to second, but the list has second first.
        let against = PathOrder::of(&adapter(
            r#"
[[step]]
id = "first"
kind = "turn"
agent = "builder"
turn = "worker"
next = "third"

[[step]]
id = "second"
kind = "end"

[[step]]
id = "third"
kind = "turn"
agent = "builder"
turn = "worker"
next = "second"
"#,
        ));
        assert_eq!(against.order("first", "third"), Order::After);
        assert_eq!(against.order("first", "second"), Order::After);
        assert_eq!(against.order("third", "second"), Order::Against);
        assert_eq!(against.order("second", "third"), Order::Against);
    }

    fn facts(answer: Value) -> TicketFacts {
        let pr = answer
            .get("pr")
            .filter(|p| !p.is_null())
            .map(|p| serde_json::from_value(p.clone()).unwrap());
        TicketFacts {
            issue: Value::Null,
            tracker_status: answer
                .get("tracker_status")
                .and_then(Value::as_str)
                .map(str::to_string),
            pr,
            ci: None,
            envs: Default::default(),
            answer,
        }
    }

    fn with_rules(rules: &str) -> Adapter {
        adapter(&format!("{PATH}\n{rules}"))
    }

    const RULES: &str = r#"
[[resume]]
when = { "pr.merged" = true }
step = "done"

[[resume]]
when = { "review.state" = ["comments", "changes"] }
step = "comments"
back = true

[[resume]]
when = { "head.state" = "forward", "pr.closed" = false }
step = "open-pr"
back = true

[[resume]]
when = { "pr.draft" = "false", "pr.closed" = false }
step = "in-review"

[[resume]]
when = { "pr.draft" = "true" }
step = "open-pr"
"#;

    fn ready(review: &str, waiting: u64) -> TicketFacts {
        facts(json!({
            "tracker_status": "In Review",
            "pr": {"number": 7, "draft": false, "head": "abc1234", "merged": false, "closed": false,
                   "comments_waiting": waiting},
            "review": {"state": review},
        }))
    }

    fn same() -> Head {
        Head::Same {
            forge: "abc1234".into(),
        }
    }

    #[test]
    fn the_first_rule_that_applies_wins_and_a_back_rule_also_names_an_earlier_step() {
        let adapter = with_rules(RULES);
        // A ticket in the loop of the self-review: the open and ready pull request.
        assert_eq!(
            find(&adapter, "review", &ready("waiting", 0), &same()),
            Found::Step {
                rule: 4,
                step: "in-review".into(),
                reason: "the pull request #7 is open and ready".into(),
                back: None,
            }
        );
        // Comments wait: the second rule holds first, and its step comes after qa.
        assert_eq!(
            find(&adapter, "qa", &ready("comments", 2), &same()),
            Found::Step {
                rule: 2,
                step: "comments".into(),
                reason: "the pull request #7 is open and ready, the review state is comments, and 2 review comments wait".into(),
                back: None,
            }
        );
        // A late comment after the approval: the rule has back = true, so it takes the ticket
        // back from wide to comments, and it names the env that runs the head.
        let mut late = ready("comments", 2);
        late.envs
            .insert("void".into(), json!({"holds": true, "deployed": true}));
        assert_eq!(
            find(&adapter, "wide", &late, &same()),
            Found::Step {
                rule: 2,
                step: "comments".into(),
                reason: "the pull request #7 is open and ready, the review state is comments, and 2 review comments wait".into(),
                back: Some("the review state is comments, 2 review comments wait, and the head runs on void".into()),
            }
        );
        // A push by hand: head.state is forward, so the third rule takes the ticket back.
        let pushed = Head::Forward {
            from: "abc1234".into(),
            to: "def5678".into(),
        };
        assert_eq!(
            find(&adapter, "wide", &ready("waiting", 0), &pushed),
            Found::Step {
                rule: 3,
                step: "open-pr".into(),
                reason: "the pull request #7 is open and ready and the pull request has later commits than the clone, such as a push by hand".into(),
                back: Some("the pull request has later commits than the clone, such as a push by hand".into()),
            }
        );
        // A rule with no back that names an earlier step does not apply: the search reads on,
        // and when no rule applies, the reason names the rules that held.
        assert_eq!(
            find(&adapter, "comments", &ready("waiting", 0), &same()),
            Found::Nothing("no rule applies: the rule 4 (in-review) holds, but its step does not come after the step comments, and the rule has no back = true".into())
        );
        // A rule that names the stored step ends the search: the facts confirm the step.
        assert_eq!(
            find(&adapter, "in-review", &ready("waiting", 0), &same()),
            Found::Nothing("the rule 4 holds and names the step in-review, so the facts confirm the step where the ticket is".into())
        );
        // In the loop of the code review, the step list puts comments after in-review, and a
        // fix of the QA leads out of its loop to the code review.
        assert_eq!(
            find(&adapter, "in-review", &ready("comments", 1), &same()),
            Found::Step {
                rule: 2,
                step: "comments".into(),
                reason: "the pull request #7 is open and ready, the review state is comments, and 1 review comment waits".into(),
                back: None,
            }
        );
        assert_eq!(
            find(&adapter, "fix", &ready("waiting", 0), &same()).step(),
            Some("in-review")
        );
        // A merged pull request ends the ticket from each step that leads to the end.
        let merged = facts(json!({"pr": {"number": 7, "merged": true, "closed": true}}));
        assert_eq!(
            find(&adapter, "comments", &merged, &Head::None).step(),
            Some("done")
        );
        // No pull request: no rule holds, because each path of pr gives none.
        let none = facts(json!({"pr": null, "review": {"state": "none"}}));
        assert_eq!(
            find(&adapter, "review", &none, &Head::None),
            Found::Nothing("no rule holds".into())
        );
        // The value none matches a path that the answer does not have.
        let rules = with_rules(
            "[[resume]]\nwhen = { \"pr.draft\" = \"none\", tracker_status = \"In Review\" }\nstep = \"open-pr\"\n",
        );
        let found = find(
            &rules,
            "build",
            &facts(json!({"tracker_status": "In Review"})),
            &Head::None,
        );
        assert_eq!(
            found,
            Found::Step {
                rule: 1,
                step: "open-pr".into(),
                reason: "pr.draft is none and the tracker status is In Review".into(),
                back: None,
            }
        );
    }

    #[test]
    fn a_skipped_rule_lets_a_later_rule_win_and_the_stored_step_ends_the_search() {
        let adapter = with_rules(
            r#"
[[resume]]
when = { "pr.draft" = "false" }
step = "in-review"

[[resume]]
when = { "pr.draft" = "false" }
step = "done"
"#,
        );
        // From comments, the first rule names an earlier step and has no back, so the second
        // rule wins.
        assert_eq!(
            find(&adapter, "comments", &ready("waiting", 0), &same()).step(),
            Some("done")
        );
        // From in-review, the first rule names the stored step: the search ends there, and the
        // second rule does not count.
        assert!(matches!(
            find(&adapter, "in-review", &ready("waiting", 0), &same()),
            Found::Nothing(why) if why.contains("the facts confirm the step where the ticket is")
        ));
        // Two rules that hold and name earlier steps: the reason names both.
        let adapter = with_rules(
            r#"
[[resume]]
when = { "pr.draft" = "false" }
step = "in-review"

[[resume]]
when = { "pr.draft" = "false" }
step = "open-pr"
"#,
        );
        assert_eq!(
            find(&adapter, "comments", &ready("waiting", 0), &same()),
            Found::Nothing("no rule applies: the rules 1 (in-review) and 2 (open-pr) hold, but their steps do not come after the step comments, and the rules have no back = true".into())
        );
    }

    #[test]
    fn a_rule_holds_only_when_each_of_its_facts_holds() {
        let rule = ResumeRule {
            when: BTreeMap::from([
                ("pr.draft".to_string(), vec!["false".to_string()]),
                (
                    "review.state".to_string(),
                    vec!["comments".to_string(), "changes".to_string()],
                ),
            ]),
            step: "comments".into(),
            back: false,
        };
        assert!(holds(&rule, &ready("changes", 0), &same()));
        assert!(!holds(&rule, &ready("approved", 0), &same()));
        let draft =
            facts(json!({"pr": {"number": 7, "draft": true}, "review": {"state": "comments"}}));
        assert!(!holds(&rule, &draft, &same()));
        // A list or a table at the path matches no value.
        let list =
            facts(json!({"pr": {"number": 7, "draft": false}, "review": {"state": ["comments"]}}));
        assert!(!holds(&rule, &list, &same()));
        // head.state comes from the head, not from the answer of observe.
        let rule = ResumeRule {
            when: BTreeMap::from([("head.state".to_string(), vec!["forward".to_string()])]),
            step: "open-pr".into(),
            back: true,
        };
        let pushed = Head::Forward {
            from: "a".into(),
            to: "b".into(),
        };
        assert!(holds(&rule, &ready("waiting", 0), &pushed));
        assert!(!holds(&rule, &ready("waiting", 0), &same()));
        let mut answer = ready("waiting", 0);
        answer.answer["head"] = json!({"state": "forward"});
        assert!(
            !holds(&rule, &answer, &same()),
            "observe cannot set head.state"
        );
        for head in [
            Head::None,
            same(),
            pushed,
            Head::Ahead {
                forge: "a".into(),
                commits: 1,
            },
            Head::Behind {
                forge: "a".into(),
                fetch: None,
            },
            Head::Apart { forge: "a".into() },
            Head::Dirty {
                forge: "a".into(),
                changes: 1,
            },
            Head::Elsewhere {
                branch: "main".into(),
            },
            Head::Unread("x".into()),
        ] {
            assert!(
                crate::adapter::HEAD_STATES.contains(&head.state()),
                "{head:?}"
            );
        }
    }

    #[test]
    fn the_facts_that_vision_read_come_in_words_in_a_fixed_order() {
        let mut answer = facts(json!({
            "tracker_status": "In Review",
            "pr": {"number": 7, "draft": false, "head": "abc1234", "conflict": true,
                   "comments_waiting": 2, "changes_requested": true},
            "review": {"state": "comments"},
        }));
        answer.ci = Some(crate::executor::CiFacts {
            head: Some("abc1234def5678".into()),
            state: "green".into(),
        });
        answer
            .envs
            .insert("bastion".into(), json!({"holds": true, "deployed": false}));
        answer
            .envs
            .insert("void".into(), json!({"holds": true, "deployed": true}));
        answer.envs.insert("prod".into(), json!({"holds": false}));
        assert_eq!(
            facts_words(&answer, &same()),
            vec![
                "the tracker status is In Review",
                "the pull request #7 is open and ready, and it conflicts with its base",
                "the review state is comments, 2 review comments wait, and a reviewer asks for changes",
                "CI is green at abc1234def56",
                "bastion holds the head, but does not run it yet",
                "prod does not hold the head",
                "the head runs on void",
                "the branch of the clone is at the head of the pull request",
            ]
        );
        assert_eq!(
            facts_words(&facts(json!({"pr": null})), &Head::None),
            vec!["the tracker gives no status", "there is no pull request"]
        );
    }

    #[test]
    fn the_words_of_a_continue_name_the_suggestion_the_facts_and_the_head() {
        let took = json!({"step": "in-review", "suggestion": "in-review", "head": {"from": "aaa1111", "to": "bbb2222ccc333444555"}});
        assert_eq!(
            resumed_words(&took),
            "resumed the ticket at in-review, as Vision suggested, and took the head bbb2222ccc33 of the pull request"
        );
        let back = json!({"from": "ready-to-merge", "step": "comments", "suggestion": "comments", "back": true});
        assert_eq!(
            resumed_words(&back),
            "resumed the ticket at comments, as Vision suggested, back from ready-to-merge"
        );
        let other = json!({"step": "review", "suggestion": "in-review"});
        assert_eq!(
            resumed_words(&other),
            "resumed the ticket at review; Vision suggested in-review"
        );
        let nothing = json!({"step": "review", "suggestion": null, "failed": false});
        assert_eq!(
            resumed_words(&nothing),
            "resumed the ticket at review; Vision found nothing further"
        );
        let failed = json!({"step": "review", "suggestion": null, "failed": true});
        assert_eq!(
            resumed_words(&failed),
            "resumed the ticket at review; Vision could not check where it is"
        );
        let key = TicketKey::parse("DEMO-12").unwrap();
        let checked = Checked {
            found: Found::Step {
                rule: 4,
                step: "in-review".into(),
                reason: "the pull request #7 is open and ready".into(),
                back: None,
            },
            facts: Some(ready("waiting", 0)),
            head: same(),
        };
        assert_eq!(
            suggest_text(&key, "review", &checked),
            "Vision thinks that DEMO-12 is at the step in-review, not at the step review where it stopped: the pull request #7 is open and ready (rule 4). It did not continue the ticket.\nVision read:\n  the tracker status is In Review\n  the pull request #7 is open and ready\n  the review state is waiting and no review comment waits\n  the branch of the clone is at the head of the pull request\nTo continue at in-review:  vision take DEMO-12 --step in-review\nTo continue at review:     vision take DEMO-12 --step review"
        );
        let back = Checked {
            found: Found::Step {
                rule: 2,
                step: "comments".into(),
                reason: "the pull request #7 is open and ready, the review state is comments, and 2 review comments wait".into(),
                back: Some("2 review comments wait, and the head runs on void".into()),
            },
            facts: Some(ready("comments", 2)),
            head: same(),
        };
        let text = suggest_text(&key, "ready-to-merge", &back);
        assert!(
            text.starts_with("Vision thinks that DEMO-12 is at the step comments, not at the step ready-to-merge where it stopped (rule 2). This goes back from ready-to-merge: 2 review comments wait, and the head runs on void. It did not continue the ticket.\nVision read:\n"),
            "{text}"
        );
        let nothing = Checked {
            found: Found::Nothing("x".into()),
            facts: None,
            head: Head::None,
        };
        assert_eq!(suggest_text(&key, "review", &nothing), "");
        // The output of a continue lists the facts after the suggestion.
        let resumed = Resumed::of(&back, "ready-to-merge", Some(7));
        assert_eq!(
            resumed.lines("comments")[..3],
            [
                "Vision suggested this step (rule 2). This goes back from ready-to-merge: 2 review comments wait, and the head runs on void".to_string(),
                "Vision read: the tracker status is In Review".to_string(),
                "The pull request #7 is open and ready".to_string(),
            ]
        );
        let detail = resumed.detail("ready-to-merge", "comments", "move");
        assert_eq!(detail["back"], json!(true));
        assert_eq!(detail["facts"][0], json!("the tracker status is In Review"));
    }

    #[test]
    fn a_head_problem_names_the_fix_and_stops_the_continue() {
        let behind = Head::Behind {
            forge: "abc1234def5678".into(),
            fetch: None,
        };
        let problem = behind.problem(Some(7), "/work/clone").unwrap();
        assert_eq!(
            problem.what,
            "the clone is behind the remote: Vision fetched, and the head abc1234def56 of the pull request #7 is still not in the clone /work/clone"
        );
        assert_eq!(
            problem.fix,
            "Push that commit to the remote of the clone, or fetch it into the clone by hand"
        );
        let failed = Head::Behind {
            forge: "abc1234def5678".into(),
            fetch: Some(
                "`git fetch --quiet --all --prune` failed in /work/clone: fatal: no remote".into(),
            ),
        };
        let problem = failed.problem(Some(7), "/work/clone").unwrap();
        assert_eq!(
            problem.what,
            "the clone is behind the remote: the head abc1234def56 of the pull request #7 is not in the clone /work/clone, and the fetch of the clone failed: `git fetch --quiet --all --prune` failed in /work/clone: fatal: no remote"
        );
        assert_eq!(
            problem.fix,
            "Fix the fetch, which `git -C /work/clone fetch --all --prune` shows"
        );
        for fine in [
            Head::None,
            Head::Same { forge: "a".into() },
            Head::Forward {
                from: "a".into(),
                to: "b".into(),
            },
            Head::Ahead {
                forge: "a".into(),
                commits: 2,
            },
        ] {
            assert_eq!(fine.problem(Some(7), "/c"), None, "{fine:?}");
        }
        assert!(is_commit("abc1234"));
        assert!(!is_commit("abc123"));
        assert!(!is_commit("--upload-pack=x"));
    }

    // ----- The head against temporary git repos ----------------------------------------------

    const BRANCH: &str = "vision/DEMO-12";

    fn git_in(dir: &Path, args: &[&str]) -> String {
        let author = git::Author {
            name: "test",
            email: "test@example.invalid",
        };
        git::run_as(dir, args, author).unwrap()
    }

    /// A bare remote, the clone of Vision on the branch of the ticket, and a checkout of the
    /// person, in a temporary folder. The branch holds one commit, `base`, on the remote too.
    struct Repos {
        _dir: tempfile::TempDir,
        clone: std::path::PathBuf,
        person: std::path::PathBuf,
        base: String,
    }

    impl Repos {
        fn new() -> Repos {
            let dir = tempfile::tempdir().unwrap();
            let root = dir.path().to_path_buf();
            let remote = root.join("remote.git");
            let clone = root.join("clone");
            let person = root.join("person");
            git_in(
                &root,
                &["init", "--quiet", "--bare", remote.to_str().unwrap()],
            );
            git_in(
                &root,
                &[
                    "clone",
                    "--quiet",
                    remote.to_str().unwrap(),
                    clone.to_str().unwrap(),
                ],
            );
            Repos::write(&clone, "calc.sh", "add() { :; }\n");
            git_in(&clone, &["add", "--all"]);
            git_in(&clone, &["commit", "--quiet", "-m", "Start"]);
            git_in(
                &clone,
                &["push", "--quiet", "origin", "HEAD:refs/heads/main"],
            );
            git_in(&clone, &["switch", "--quiet", "-c", BRANCH]);
            let base = Repos::commit(&clone, "a.txt", "A", "A");
            git_in(&clone, &["push", "--quiet", "origin", BRANCH]);
            git_in(
                &root,
                &[
                    "clone",
                    "--quiet",
                    "--branch",
                    BRANCH,
                    remote.to_str().unwrap(),
                    person.to_str().unwrap(),
                ],
            );
            Repos {
                _dir: dir,
                clone,
                person,
                base,
            }
        }

        fn write(dir: &Path, name: &str, text: &str) {
            std::fs::write(dir.join(name), text).unwrap();
        }

        /// A commit of one file in `dir`. Gives the commit.
        fn commit(dir: &Path, name: &str, text: &str, message: &str) -> String {
            Repos::write(dir, name, text);
            git_in(dir, &["add", "--all"]);
            git_in(dir, &["commit", "--quiet", "-m", message]);
            git_in(dir, &["rev-parse", "HEAD"])
        }

        /// A commit of the person, pushed to the remote. Gives the commit.
        fn pushed(&self, name: &str) -> String {
            let commit = Repos::commit(&self.person, name, name, name);
            git_in(&self.person, &["push", "--quiet", "origin", BRANCH]);
            commit
        }

        fn ticket(&self, branch: &str) -> Ticket {
            Ticket::of(crate::ledger::TicketRow {
                key: "DEMO-12".into(),
                state: Some("review".into()),
                clone: Some(self.clone.display().to_string()),
                branch: Some(branch.into()),
                ..Default::default()
            })
            .unwrap()
        }

        /// The head of the open pull request `forge`, seen from the clone.
        fn head(&self, forge: &str, switches: bool) -> Head {
            head(&self.ticket(BRANCH), BRANCH, &pr_at(forge), switches)
        }

        fn branch(&self) -> String {
            git_in(&self.clone, &["rev-parse", &format!("refs/heads/{BRANCH}")])
        }
    }

    /// The facts of observe with the open pull request #7 at the head `forge`.
    fn pr_at(forge: &str) -> TicketFacts {
        facts(json!({"pr": {"number": 7, "draft": false, "head": forge}}))
    }

    #[test]
    fn the_continue_fetches_a_head_that_the_person_pushed_and_moves_the_branch_forward_to_it() {
        let repos = Repos::new();
        assert_eq!(
            repos.head(&repos.base, false),
            Head::Same {
                forge: repos.base.clone()
            }
        );
        // The person pushes from another checkout: the clone does not have the commit, so the
        // continue fetches it, and the branch can move forward to it.
        let pushed = repos.pushed("b.txt");
        assert_eq!(
            repos.head(&pushed, false),
            Head::Forward {
                from: repos.base.clone(),
                to: pushed.clone()
            }
        );
        assert_eq!(
            git_in(
                &repos.clone,
                &["rev-parse", &format!("refs/remotes/origin/{BRANCH}")]
            ),
            pushed,
            "the fetch brought the head"
        );
        assert_eq!(repos.branch(), repos.base, "the test moves nothing");
        take_head(&repos.clone, &pushed).unwrap();
        assert_eq!(repos.branch(), pushed);
        assert!(repos.clone.join("b.txt").is_file());
    }

    #[test]
    fn the_clone_keeps_its_own_commits_and_the_fast_forward_never_resets_the_branch() {
        let repos = Repos::new();
        let pushed = repos.pushed("b.txt");
        git_in(&repos.clone, &["fetch", "--quiet", "--all"]);
        git_in(&repos.clone, &["merge", "--quiet", "--ff-only", &pushed]);
        let own = Repos::commit(&repos.clone, "c.txt", "C", "C");
        // Ahead: the clone has a commit that the pull request does not have, and it stays.
        assert_eq!(
            repos.head(&pushed, false),
            Head::Ahead {
                forge: pushed.clone(),
                commits: 1
            }
        );
        assert_eq!(repos.branch(), own);
        // A fast-forward to an older commit changes nothing.
        take_head(&repos.clone, &pushed).unwrap();
        assert_eq!(repos.branch(), own, "no reset to an older commit");
        // Apart: the person pushes another commit after the shared one. Neither branch holds
        // the head of the other, and the fast-forward refuses it.
        let other = repos.pushed("d.txt");
        assert_eq!(
            repos.head(&other, false),
            Head::Apart {
                forge: other.clone()
            }
        );
        assert!(take_head(&repos.clone, &other).is_err());
        assert_eq!(repos.branch(), own, "no reset to the commit of the person");
        assert!(repos.clone.join("c.txt").is_file());
    }

    #[test]
    fn a_change_in_the_work_tree_another_branch_and_a_missing_branch_stop_the_continue() {
        let repos = Repos::new();
        let pushed = repos.pushed("b.txt");
        // Dirty: the head comes one fast-forward after the branch, but the work tree changed.
        Repos::write(&repos.clone, "a.txt", "changed by hand");
        assert_eq!(
            repos.head(&pushed, false),
            Head::Dirty {
                forge: pushed.clone(),
                changes: 1
            }
        );
        git_in(&repos.clone, &["checkout", "--quiet", "--", "a.txt"]);
        // Elsewhere: the clone is on another branch. A take that switches back first sees the
        // branch of the ticket.
        git_in(&repos.clone, &["switch", "--quiet", "main"]);
        assert_eq!(
            repos.head(&pushed, false),
            Head::Elsewhere {
                branch: "the branch main".into()
            }
        );
        assert_eq!(
            repos.head(&pushed, true),
            Head::Forward {
                from: repos.base.clone(),
                to: pushed.clone()
            }
        );
        // Unread: the branch of the ticket is not in the clone, or the row names no clone.
        let other = repos.ticket("vision/DEMO-99");
        assert_eq!(
            head(&other, "vision/DEMO-99", &pr_at(&pushed), true),
            Head::Unread("the branch vision/DEMO-99 is not in the clone".into())
        );
        let mut none = repos.ticket(BRANCH);
        none.row.clone = None;
        assert!(matches!(
            head(&none, BRANCH, &pr_at(&pushed), true),
            Head::Unread(why) if why.contains("DEMO-12 has no clone")
        ));
    }

    #[test]
    fn a_head_that_the_fetch_does_not_bring_is_behind_and_a_failed_fetch_says_why() {
        let repos = Repos::new();
        // The person commits and does not push: the fetch cannot bring the head.
        let local = Repos::commit(&repos.person, "b.txt", "B", "B");
        assert_eq!(
            repos.head(&local, false),
            Head::Behind {
                forge: local.clone(),
                fetch: None
            }
        );
        // The remote of the clone is gone: the fetch fails, and the reason names its error.
        git_in(
            &repos.clone,
            &["remote", "set-url", "origin", "/nowhere/remote.git"],
        );
        let Head::Behind {
            forge,
            fetch: Some(why),
        } = repos.head(&local, false)
        else {
            panic!("not behind with a failed fetch");
        };
        assert_eq!(forge, local);
        assert!(why.contains("git fetch --quiet --all --prune"), "{why}");
        assert_eq!(repos.branch(), repos.base, "nothing moved");
    }

    #[test]
    fn the_fetch_and_the_head_of_a_continue_work_in_a_clone_that_is_a_git_worktree() {
        let repos = Repos::new();
        // The clone of the ticket is a worktree of the clone of Vision: the main checkout goes
        // to main, and the worktree holds the branch of the ticket.
        git_in(&repos.clone, &["switch", "--quiet", "main"]);
        let slot = repos.clone.parent().unwrap().join("slot");
        git_in(
            &repos.clone,
            &["worktree", "add", "--quiet", slot.to_str().unwrap(), BRANCH],
        );
        assert!(git::worktree_common_dir(&slot).is_some());
        let mut ticket = repos.ticket(BRANCH);
        ticket.row.clone = Some(slot.display().to_string());
        let head_at = |forge: &str| head(&ticket, BRANCH, &pr_at(forge), false);
        assert_eq!(
            head_at(&repos.base),
            Head::Same {
                forge: repos.base.clone()
            }
        );
        // The person pushes from another checkout: the worktree fetches the head into the
        // common repo, and its branch moves forward to it.
        let pushed = repos.pushed("b.txt");
        assert_eq!(
            head_at(&pushed),
            Head::Forward {
                from: repos.base.clone(),
                to: pushed.clone()
            }
        );
        take_head(&slot, &pushed).unwrap();
        assert_eq!(git_in(&slot, &["rev-parse", "HEAD"]), pushed);
        assert!(slot.join("b.txt").is_file());
        assert!(
            !repos.clone.join("b.txt").exists(),
            "the main checkout stays on main"
        );
        // A change in the work tree of the worktree stops the next fast-forward.
        let next = repos.pushed("c.txt");
        Repos::write(&slot, "a.txt", "changed by hand");
        assert_eq!(
            head_at(&next),
            Head::Dirty {
                forge: next.clone(),
                changes: 1
            }
        );
    }

    #[test]
    fn a_continue_writes_the_number_of_the_pull_request_only_in_an_empty_row() {
        let checked = Checked {
            found: Found::Nothing("no rule holds".into()),
            facts: Some(pr_at("abc1234")),
            head: Head::None,
        };
        let written = Resumed::of(&checked, "review", None);
        assert_eq!((written.pr_written, written.pr_kept), (Some(7), None));
        let same = Resumed::of(&checked, "review", Some(7));
        assert_eq!((same.pr_written, same.pr_kept), (None, None));
        let kept = Resumed::of(&checked, "review", Some(5));
        assert_eq!((kept.pr_written, kept.pr_kept), (None, Some(5)));
        assert!(kept.lines("review").contains(
            &"The ticket row names the pull request #5, and the open pull request of the branch is #7. Vision kept #5 in the row".to_string()
        ));
        // A merged pull request is not open, so its number is not written.
        let merged = Checked {
            found: Found::Nothing("no rule holds".into()),
            facts: Some(facts(json!({"pr": {"number": 7, "merged": true}}))),
            head: Head::None,
        };
        assert_eq!(Resumed::of(&merged, "review", None).pr_written, None);
    }
}
