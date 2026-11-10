//! `vision needs` (section 8.4): with a terminal, an interactive pass over each card that waits
//! for the person, one key at a time; then the other needs with the command for each one.
//! Without a terminal, or with `--json`, it prints the list and exits.
//!
//! A key is read in the raw mode of the terminal through `libc`, so that one key acts at once.
//! When the terminal has no raw mode, the view reads one line for each key instead.

use std::io::{self, BufRead, IsTerminal, Read, Write};
use std::time::Instant;

use anyhow::{Context, Result};
use serde::Serialize;

use super::card::{index_of_letter, letter};
use super::commands::{answer_card, answered_line, mark_needless, PersonAnswer};
use super::{needless_text, Agents, Question, Reply};
use crate::cli::Needless;
use crate::ctx::Ctx;
use crate::ledger::questions::{STATE_ASKED, STATE_PROPOSED};
use crate::ledger::Ledger;
use crate::memory::store::short;
use crate::memory::Memory;
use crate::snapshot::{Need, NeedKind};
use crate::{executor, pipeline};

/// One key of the view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Key {
    Enter,
    Char(char),
    /// Ctrl-C, Ctrl-D, or the end of the input.
    Quit,
}

/// Where the view reads keys and lines.
pub(crate) trait Keys {
    /// The next key.
    fn key(&mut self) -> Result<Key>;
    /// One line of text, without its newline. `None` at the end of the input.
    fn line(&mut self) -> Result<Option<String>>;
}

/// Reads each key as one line: an empty line is Enter, else its first character.
pub(crate) struct LineKeys<R: BufRead> {
    input: R,
}

impl<R: BufRead> LineKeys<R> {
    pub(crate) fn new(input: R) -> LineKeys<R> {
        LineKeys { input }
    }
}

impl<R: BufRead> Keys for LineKeys<R> {
    fn key(&mut self) -> Result<Key> {
        match self.line()? {
            None => Ok(Key::Quit),
            Some(line) => Ok(match line.trim().chars().next() {
                None => Key::Enter,
                Some(c) => Key::Char(c),
            }),
        }
    }

    fn line(&mut self) -> Result<Option<String>> {
        let mut line = String::new();
        if self.input.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        Ok(Some(line.trim_end_matches(['\n', '\r']).to_string()))
    }
}

/// The keys of the terminal: one key at a time in raw mode, and lines in the normal mode.
struct TerminalKeys {
    stdin: io::Stdin,
}

/// The raw mode of the terminal while it lives. Drop restores the old mode.
struct RawMode {
    old: libc::termios,
}

impl RawMode {
    fn enter() -> Option<RawMode> {
        // SAFETY: tcgetattr and tcsetattr read and write a termios struct of standard input;
        // the struct is plain data, and a failure only means that there is no raw mode.
        unsafe {
            let mut old: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut old) != 0 {
                return None;
            }
            let mut raw = old;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO | libc::ISIG);
            raw.c_cc[libc::VMIN] = 1;
            raw.c_cc[libc::VTIME] = 0;
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                return None;
            }
            Some(RawMode { old })
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        // SAFETY: restores the struct that tcgetattr gave.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.old);
        }
    }
}

impl Keys for TerminalKeys {
    fn key(&mut self) -> Result<Key> {
        let Some(_raw) = RawMode::enter() else {
            return LineKeys::new(self.stdin.lock()).key();
        };
        let mut lock = self.stdin.lock();
        loop {
            let mut byte = [0u8; 1];
            if lock.read(&mut byte)? == 0 {
                return Ok(Key::Quit);
            }
            match byte[0] {
                b'\r' | b'\n' => return Ok(Key::Enter),
                3 | 4 => return Ok(Key::Quit),
                b if b.is_ascii_lowercase() || b == b'-' => return Ok(Key::Char(char::from(b))),
                // Escape sequences, capitals, and other bytes do nothing.
                _ => continue,
            }
        }
    }

    fn line(&mut self) -> Result<Option<String>> {
        LineKeys::new(self.stdin.lock()).line()
    }
}

/// One card of the list of `vision needs --json`.
#[derive(Debug, Serialize)]
struct CardView {
    id: String,
    ticket: String,
    agent: String,
    step: String,
    state: String,
    question: String,
    context: String,
    example: String,
    choices: Vec<ChoiceView>,
    /// The number of the recommended choice, from 1.
    recommended: Option<usize>,
    why: String,
    evidence: String,
    proposal: Option<ProposalView>,
    /// Why the Judge sent the card to the person.
    judge_reason: Option<String>,
    command: String,
}

#[derive(Debug, Serialize)]
struct ChoiceView {
    n: usize,
    letter: Option<char>,
    label: String,
    means: String,
}

#[derive(Debug, Serialize)]
struct ProposalView {
    answer: String,
    /// The number of the choice, from 1.
    choice: Option<usize>,
    reason: String,
    sources: Vec<String>,
}

#[derive(Debug, Serialize)]
struct OtherView {
    kind: NeedKind,
    ticket: Option<String>,
    id: String,
    text: String,
    command: String,
}

#[derive(Debug, Serialize)]
struct NeedsView {
    cards: Vec<CardView>,
    others: Vec<OtherView>,
}

fn waiting_cards(ledger: &Ledger) -> Result<Vec<Question>> {
    let mut agents = Agents::default();
    ledger
        .questions_in_states(&[STATE_PROPOSED, STATE_ASKED])?
        .into_iter()
        .map(|row| Question::load(ledger, row, &mut agents))
        .collect()
}

fn others(ctx: &Ctx, ledger: &Ledger) -> Result<Vec<Need>> {
    let mut needs = pipeline::needs(ctx, ledger)?;
    needs.extend(executor::needs(ctx, ledger)?);
    needs.extend(crate::curator::needs(ctx, ledger)?);
    Ok(needs)
}

/// The command that the person runs for a need of another module.
pub(crate) fn command_for(need: &Need) -> String {
    let ticket = need.ticket.as_deref().unwrap_or("KEY");
    match need.kind {
        NeedKind::Card => format!("vision answer {} --choice N", need.id),
        NeedKind::Plan => format!(
            "vision plan {ticket}, then vision approve-plan {ticket} {}",
            need.id
        ),
        NeedKind::Operation => format!("vision approve {ticket} {}", need.id),
        NeedKind::Flag => format!("vision log {ticket}, then vision retry {ticket}"),
        NeedKind::Proposal => format!("vision memory review --proposal {}", need.id),
    }
}

fn kind_name(kind: NeedKind) -> &'static str {
    match kind {
        NeedKind::Card => "card",
        NeedKind::Plan => "plan",
        NeedKind::Operation => "operation",
        NeedKind::Flag => "flag",
        NeedKind::Proposal => "proposal",
    }
}

fn card_command(question: &Question) -> String {
    match question.proposal().and_then(|p| p.choice(&question.card)) {
        Some(i) => format!("vision answer {} --choice {}", question.row.id, i + 1),
        None => format!(
            "vision answer {} --choice N | --text \"T\" | --needless code|ticket|memory|no-matter",
            question.row.id
        ),
    }
}

fn card_view(question: &Question) -> CardView {
    let card = &question.card;
    CardView {
        id: question.row.id.clone(),
        ticket: question.row.ticket.clone(),
        agent: question.agent.clone(),
        step: question.step.clone(),
        state: question.row.state.clone(),
        question: card.text.trim().to_string(),
        context: card.context.trim().to_string(),
        example: card.example.trim().to_string(),
        choices: card
            .options
            .iter()
            .enumerate()
            .map(|(i, o)| ChoiceView {
                n: i + 1,
                letter: letter(i),
                label: o.label.trim().to_string(),
                means: o.means.trim().to_string(),
            })
            .collect(),
        recommended: card.recommended_index().map(|i| i + 1),
        why: card.why.trim().to_string(),
        evidence: card.evidence.trim().to_string(),
        proposal: question.proposal().map(|p| ProposalView {
            answer: p.text(card),
            choice: p.choice(card).map(|i| i + 1),
            reason: question
                .view
                .judge
                .as_ref()
                .map(|e| e.reason.trim().to_string())
                .unwrap_or_default(),
            sources: question.sources(),
        }),
        judge_reason: question.judge_reason(),
        command: card_command(question),
    }
}

/// `vision needs [--json]`.
pub fn needs_command(ctx: &Ctx, json: bool) -> Result<()> {
    ctx.require_setup()?;
    let ledger = ctx.ledger()?;
    let interactive = !json && io::stdin().is_terminal() && io::stdout().is_terminal();
    if interactive {
        let memory = Memory::from_ctx(ctx)?;
        let mut keys = TerminalKeys { stdin: io::stdin() };
        let mut out = io::stdout();
        review(ctx, &ledger, &memory, &mut keys, &mut out)?;
        return Ok(());
    }
    let cards = waiting_cards(&ledger)?;
    let others = others(ctx, &ledger)?;
    if json {
        let view = NeedsView {
            cards: cards.iter().map(card_view).collect(),
            others: others
                .iter()
                .map(|n| OtherView {
                    kind: n.kind,
                    ticket: n.ticket.clone(),
                    id: n.id.clone(),
                    text: n.text.clone(),
                    command: command_for(n),
                })
                .collect(),
        };
        println!("{}", serde_json::to_string_pretty(&view)?);
        return Ok(());
    }
    print!("{}", list_text(&cards, &others));
    Ok(())
}

/// The list of `vision needs` without a terminal.
fn list_text(cards: &[Question], others: &[Need]) -> String {
    if cards.is_empty() && others.is_empty() {
        return "Nothing waits for you.\n".into();
    }
    let mut out = String::new();
    if !cards.is_empty() {
        out.push_str(&format!("{} card(s) wait for you:\n", cards.len()));
        for q in cards {
            out.push_str(&format!(
                "\n{}  {}  {} asks{}:\n  {}\n",
                q.row.id,
                q.row.ticket,
                q.agent,
                if q.step.is_empty() {
                    String::new()
                } else {
                    format!(" in the step {}", q.step)
                },
                q.card.text.trim()
            ));
            for (i, o) in q.card.options.iter().enumerate() {
                out.push_str(&format!(
                    "  {}) {}: {}\n",
                    i + 1,
                    o.label.trim(),
                    o.means.trim()
                ));
            }
            if let Some(p) = q.proposal() {
                out.push_str(&format!("  The Judge proposes: {}\n", p.text(&q.card)));
            }
            out.push_str(&format!("  Answer: {}\n", card_command(q)));
        }
    }
    if !others.is_empty() {
        out.push_str("\nOther things wait for you:\n");
        for need in others {
            out.push_str(&format!(
                "  {}  {}  {}\n    run: {}\n",
                kind_name(need.kind),
                need.ticket.as_deref().unwrap_or("-"),
                need.text,
                command_for(need)
            ));
        }
    }
    out
}

/// The text of one card in the view.
fn card_text(question: &Question, n: usize, of: usize) -> String {
    let card = &question.card;
    let mut out = format!(
        "\n{}  {}  {} asks{}  ({n} of {of})\n\n{}\n",
        question.row.id,
        question.row.ticket,
        question.agent,
        if question.step.is_empty() {
            String::new()
        } else {
            format!(" in the step {}", question.step)
        },
        card.text.trim()
    );
    if !card.context.trim().is_empty() {
        out.push_str(&format!("\nWhy it is asked: {}\n", card.context.trim()));
    }
    if !card.example.trim().is_empty() {
        out.push_str(&format!("Example: {}\n", card.example.trim()));
    }
    out.push('\n');
    for (i, o) in card.options.iter().enumerate() {
        let mark = letter(i).map_or_else(|| format!("{}.", i + 1), |l| format!("{l})"));
        out.push_str(&format!(
            "  {mark} {}: {}\n",
            o.label.trim(),
            o.means.trim()
        ));
    }
    if let Some(i) = card.recommended_index() {
        let mark = letter(i).map_or_else(|| format!("{}.", i + 1), |l| format!("{l})"));
        out.push_str(&format!(
            "\nThe agent recommends {mark} {}{}\n",
            card.options[i].label.trim(),
            if card.why.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", card.why.trim())
            }
        ));
    }
    if !card.evidence.trim().is_empty() {
        out.push_str(&format!("Evidence: {}\n", card.evidence.trim()));
    }
    match question.proposal() {
        Some(p) => {
            let mark = p
                .choice(card)
                .and_then(letter)
                .map(|l| format!("{l}) "))
                .unwrap_or_default();
            out.push_str(&format!("\nThe Judge proposes {mark}{}", p.text(card)));
            if let Some(reason) = question.judge_reason() {
                out.push_str(&format!(": {reason}"));
            }
            out.push('\n');
            let sources = question.sources();
            if !sources.is_empty() {
                out.push_str(&format!("Its sources: {}\n", sources.join(", ")));
            }
        }
        None => {
            if let Some(reason) = question.judge_reason() {
                out.push_str(&format!("\nThe Judge sent it to you: {reason}\n"));
            }
        }
    }
    let keys =
        "a letter picks a choice, t types an answer, - marks it as needless, s skips, q leaves";
    if question.proposal().is_some() {
        out.push_str(&format!("\nEnter accepts the proposal, {keys}.\n"));
    } else {
        out.push_str(&format!("\nKeys: {keys}.\n"));
    }
    out
}

/// What the person did with one card.
enum Act {
    Reply(Reply),
    Needless(Needless),
    Skip,
    Leave,
}

fn read_act(question: &Question, keys: &mut dyn Keys, out: &mut dyn Write) -> Result<Act> {
    loop {
        write!(out, "> ")?;
        out.flush()?;
        let key = keys.key()?;
        match key {
            Key::Quit | Key::Char('q') => {
                writeln!(out, "q")?;
                return Ok(Act::Leave);
            }
            Key::Enter => match question.proposal() {
                Some(p) => {
                    writeln!(out, "the proposal of the Judge")?;
                    return Ok(Act::Reply(p));
                }
                None => {
                    writeln!(out)?;
                    writeln!(out, "There is no proposal. Press a letter, t, -, s, or q.")?;
                }
            },
            Key::Char('s') => {
                writeln!(out, "s")?;
                return Ok(Act::Skip);
            }
            Key::Char('t') => {
                writeln!(out, "t")?;
                write!(out, "Your answer (Enter alone goes back): ")?;
                out.flush()?;
                match keys.line()? {
                    None => return Ok(Act::Leave),
                    Some(text) if text.trim().is_empty() => continue,
                    Some(text) => return Ok(Act::Reply(Reply::Text(text.trim().to_string()))),
                }
            }
            Key::Char('-') => {
                writeln!(out, "-")?;
                writeln!(
                    out,
                    "Why is it needless? c) the code has the answer, t) the ticket has it, m) the memory has it, n) each choice is fine. Enter goes back."
                )?;
                write!(out, "> ")?;
                out.flush()?;
                let reason = match keys.key()? {
                    Key::Char('c') => Needless::Code,
                    Key::Char('t') => Needless::Ticket,
                    Key::Char('m') => Needless::Memory,
                    Key::Char('n') => Needless::NoMatter,
                    Key::Quit => return Ok(Act::Leave),
                    _ => {
                        writeln!(out)?;
                        continue;
                    }
                };
                writeln!(out, "{}", needless_text(super::needless_name(reason)))?;
                return Ok(Act::Needless(reason));
            }
            Key::Char(c) => {
                match index_of_letter(c).filter(|i| *i < question.card.options.len()) {
                    Some(i) => {
                        writeln!(out, "{c}) {}", question.card.options[i].label.trim())?;
                        return Ok(Act::Reply(Reply::Choice(i)));
                    }
                    None => {
                        writeln!(out)?;
                        writeln!(out, "No choice has the letter {c}. Press a letter of a choice, t, -, s, or q.")?;
                    }
                }
            }
        }
    }
}

fn read_text(prompt: &str, keys: &mut dyn Keys, out: &mut dyn Write) -> Result<String> {
    write!(out, "{prompt}")?;
    out.flush()?;
    Ok(keys.line()?.unwrap_or_default().trim().to_string())
}

/// The interactive pass over each card that waits, and then the other needs.
pub(crate) fn review(
    ctx: &Ctx,
    ledger: &Ledger,
    memory: &Memory,
    keys: &mut dyn Keys,
    out: &mut dyn Write,
) -> Result<()> {
    let cards = waiting_cards(ledger)?;
    if cards.is_empty() {
        writeln!(out, "No card waits for you.")?;
    }
    let of = cards.len();
    let (mut answered, mut skipped) = (0, 0);
    let mut left = false;
    for (n, listed) in cards.iter().enumerate() {
        // A different command may have answered it meanwhile.
        let question = match Question::find(ledger, &listed.row.id) {
            Ok(q) if q.row.state == STATE_PROPOSED || q.row.state == STATE_ASKED => q,
            _ => continue,
        };
        ledger.set_question_shown(&question.row.id, &crate::util::time::now_rfc3339())?;
        write!(out, "{}", card_text(&question, n + 1, of))?;
        let shown = Instant::now();
        let act = read_act(&question, keys, out)?;
        let seconds = i64::try_from(shown.elapsed().as_secs()).ok();
        let result = match act {
            Act::Leave => {
                left = true;
                break;
            }
            Act::Skip => {
                skipped += 1;
                continue;
            }
            Act::Needless(reason) => mark_needless(ctx, ledger, &question.row.id, reason, seconds),
            Act::Reply(reply) => {
                let why = read_text("Why (Enter to skip): ", keys, out)?;
                let when = read_text("Applies when (Enter to skip): ", keys, out)?;
                let given = PersonAnswer {
                    reply,
                    why,
                    when,
                    seconds,
                };
                answer_card(ctx, ledger, memory, &question.row.id, &given)
            }
        };
        match result {
            Ok(done) => {
                answered += 1;
                writeln!(out, "{}", answered_line(&done))?;
            }
            Err(e) => writeln!(out, "Not saved: {e:#}")?,
        }
    }
    if of > 0 {
        writeln!(
            out,
            "\n{answered} answered, {skipped} skipped{}.",
            if left { ", and you left the rest" } else { "" }
        )?;
    }
    let others = others(ctx, ledger).context("could not read the other needs")?;
    if !others.is_empty() {
        writeln!(out, "\nOther things wait for you:")?;
        for need in &others {
            writeln!(
                out,
                "  {}  {}  {}\n    run: {}",
                kind_name(need.kind),
                need.ticket.as_deref().unwrap_or("-"),
                short(&need.text, 100),
                command_for(need)
            )?;
        }
    }
    out.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::{Kind, NewPrinciple, Writer};
    use crate::paths::Paths;
    use crate::questions::Answer;
    use serde_json::json;

    const ASKING: &str = "20261110T100000Z-DEMO-12-plan-1";

    fn card(n: usize) -> String {
        json!({"text": format!("Question {n}?"), "context": "The export button.",
               "example": "The March report.", "evidence": "src/export.rs:12", "kind": "product",
               "area": ["exports"], "recommended": 0, "why": "Finance imports CSV.",
               "options": [{"label": "CSV", "means": "plain"}, {"label": "XLSX", "means": "sheet"}]})
        .to_string()
    }

    fn setup() -> (tempfile::TempDir, Ctx, Ledger, Memory) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = Ctx::new(Paths::new(dir.path().join("home")));
        crate::init::run(&ctx).unwrap();
        let ledger = ctx.ledger().unwrap();
        let memory = Memory::from_ctx(&ctx).unwrap();
        memory
            .add_principle(&NewPrinciple {
                agent: "judge".into(),
                kind: Some(Kind::Product),
                area: vec!["exports".into()],
                by: Writer::Person("user".into()),
                evidence: vec![],
                rule: "When a report is exported, make sure that it is CSV.".into(),
                because: String::new(),
                not_when: String::new(),
            })
            .unwrap();
        ledger
            .conn()
            .execute(
                "INSERT INTO turns (id, ticket, agent, step, status) VALUES (?1, 'DEMO-12', 'builder', 'plan', 'done')",
                [ASKING],
            )
            .unwrap();
        let proposal = json!({"judge": {"card": "Q-0001", "outcome": "propose", "choice": 0,
            "basis": "principle", "sources": ["P-001"], "sure": false, "reason": "P-001 fits."},
            "rule": null})
        .to_string();
        let asked =
            json!({"judge": null, "rule": "rule 1: the topic `money` is in judge.always_ask"})
                .to_string();
        for n in 1..=6 {
            let (state, outcome, view, sources) = if n == 1 {
                (
                    "proposed",
                    "propose",
                    proposal.as_str(),
                    Some(r#"["P-001"]"#),
                )
            } else {
                ("asked", "ask", asked.as_str(), None)
            };
            ledger
                .conn()
                .execute(
                    "INSERT INTO questions (id, ticket, turn, card, state, outcome, basis, proposal, sources, created)
                     VALUES (?1, 'DEMO-12', ?2, ?3, ?4, ?5, 'none', ?6, ?7, '2026-11-10T10:00:00Z')",
                    rusqlite::params![format!("Q-000{n}"), ASKING, card(n), state, outcome, view, sources],
                )
                .unwrap();
        }
        (dir, ctx, ledger, memory)
    }

    #[test]
    fn the_view_takes_each_kind_of_answer_one_key_at_a_time() {
        let (_dir, ctx, ledger, memory) = setup();
        let script = "\nBecause.\n\nx\nb\n\nAlways.\n-\nt\nt\nUse JSON\n\n\ns\nq\n";
        let mut keys = LineKeys::new(script.as_bytes());
        let mut out = Vec::new();
        review(&ctx, &ledger, &memory, &mut keys, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        for part in [
            "Q-0001  DEMO-12  builder asks in the step plan  (1 of 6)\n\nQuestion 1?\n",
            "Why it is asked: The export button.\nExample: The March report.\n",
            "  a) CSV: plain\n  b) XLSX: sheet\n",
            "The agent recommends a) CSV: Finance imports CSV.\nEvidence: src/export.rs:12\n",
            "The Judge proposes a) CSV: P-001 fits.\nIts sources: P-001\n",
            "Enter accepts the proposal, a letter picks a choice",
            "The Judge sent it to you: rule 1: the topic `money` is in judge.always_ask",
            "No choice has the letter x.",
            "Saved the answer to Q-0001 as D-0001.",
            "Marked Q-0003 as needless.",
            "4 answered, 1 skipped, and you left the rest.",
        ] {
            assert!(text.contains(part), "{part:?} in {text}");
        }
        let row = |id: &str| ledger.question(id).unwrap().unwrap();
        let q1 = row("Q-0001");
        assert_eq!(q1.state, "answered");
        assert_eq!(q1.took_proposal, Some(true));
        assert!(q1.shown.is_some());
        let answer: Answer = serde_json::from_str(q1.answer.as_deref().unwrap()).unwrap();
        assert_eq!(
            (answer.text.as_str(), answer.why.as_str()),
            ("CSV", "Because.")
        );
        let q2: Answer = serde_json::from_str(row("Q-0002").answer.as_deref().unwrap()).unwrap();
        assert_eq!((q2.choice, q2.when.as_str()), (Some(1), "Always."));
        assert_eq!(row("Q-0003").flag.as_deref(), Some("needless:ticket"));
        let q4: Answer = serde_json::from_str(row("Q-0004").answer.as_deref().unwrap()).unwrap();
        assert_eq!((q4.choice, q4.text.as_str()), (None, "Use JSON"));
        assert_eq!(row("Q-0005").state, "asked");
        assert!(row("Q-0005").shown.is_some());
        assert_eq!(row("Q-0006").state, "asked");
        let seconds: Vec<Option<i64>> = ledger
            .conn()
            .prepare("SELECT seconds FROM human ORDER BY id")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect();
        assert_eq!(seconds.len(), 4);
        assert!(seconds.iter().all(|s| s.is_some()), "{seconds:?}");
        let p1 = std::fs::read_to_string(ctx.paths.principles_dir().join("P-001.md")).unwrap();
        assert!(
            p1.contains("uses: {used: 0, agreed: 1, corrected: 0}"),
            "{p1}"
        );
    }

    #[test]
    fn enter_with_no_proposal_does_nothing_and_the_end_of_input_leaves() {
        let (_dir, ctx, ledger, memory) = setup();
        ledger
            .conn()
            .execute_batch("UPDATE questions SET state = 'needless' WHERE id <> 'Q-0002'")
            .unwrap();
        let mut keys = LineKeys::new("\n".as_bytes());
        let mut out = Vec::new();
        review(&ctx, &ledger, &memory, &mut keys, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("There is no proposal."), "{text}");
        assert!(!text.contains("Enter accepts the proposal"), "{text}");
        assert!(
            text.contains("0 answered, 0 skipped, and you left the rest."),
            "{text}"
        );
        assert_eq!(ledger.question("Q-0002").unwrap().unwrap().state, "asked");
    }

    #[test]
    fn each_other_need_names_its_command() {
        let need = |kind| Need {
            kind,
            ticket: Some("DEMO-12".into()),
            id: "ab12".into(),
            text: "x".into(),
        };
        assert_eq!(
            command_for(&need(NeedKind::Plan)),
            "vision plan DEMO-12, then vision approve-plan DEMO-12 ab12"
        );
        assert_eq!(
            command_for(&need(NeedKind::Flag)),
            "vision log DEMO-12, then vision retry DEMO-12"
        );
        assert_eq!(
            command_for(&need(NeedKind::Operation)),
            "vision approve DEMO-12 ab12"
        );
    }
}
