//! The hook of the tick. `tick.rs` calls [`tick`] right after the turn module collected the
//! ended turns.

use anyhow::{bail, Result};

use super::{intake, judge, record, resume};
use crate::memory::Memory;
use crate::tick::Pass;

/// In this order: commits the hand edits of the memory, so that the Judge reads a committed
/// memory; takes in the cards of each work turn that ended with the kind `questions` and has
/// no rows yet; ends the recordings that a dead process left; applies the result of each Judge
/// turn that ended; sends each card whose Judge gave no outcome in `caps.judge_minutes` to the
/// person; starts one Judge turn for the new cards of each asking turn; and resumes each asking
/// session whose cards all have a final answer.
///
/// Each step is safe to run two times, and a crash between two steps creates no second Judge
/// turn, no second decision file, and no second resume. A failed step does not stop the later
/// steps, but the call then fails and names the steps.
pub fn tick(pass: &mut Pass) -> Result<()> {
    let ctx = pass.ctx;
    let now = pass.now;
    let mut failed: Vec<&str> = Vec::new();
    let mut fail = |step: &'static str, result: Result<()>| {
        if let Err(e) = result {
            ctx.log(&format!("questions: the step {step} failed: {e:#}"));
            failed.push(step);
        }
    };

    let memory = match Memory::from_ctx(ctx) {
        Ok(memory) => {
            match memory.commit_hand_edits() {
                Ok(edits) => {
                    for problem in &edits.left {
                        ctx.log(&format!(
                            "questions: a hand edit of the memory is not valid, so it stays out: {problem}"
                        ));
                    }
                }
                Err(e) => fail("hand-edits", Err(e)),
            }
            Some(memory)
        }
        Err(e) => {
            fail("memory", Err(e));
            None
        }
    };

    fail("intake", intake::take_in(ctx, &mut pass.ledger, now));
    if let Some(memory) = &memory {
        fail("recover", record::recover(ctx, &pass.ledger, memory, now));
        fail("apply", judge::apply_all(ctx, &pass.ledger, memory, now));
    }
    fail("timeouts", judge::timeouts(ctx, &pass.ledger, now));
    if let Some(memory) = &memory {
        fail("judge", judge::start_all(ctx, &pass.ledger, memory.dir()));
    }
    fail("resume", resume::resume_all(ctx, &pass.ledger));

    if !failed.is_empty() {
        bail!(
            "the questions step(s) {} failed. See {}",
            failed.join(", "),
            ctx.paths.log_file().display()
        );
    }
    Ok(())
}
