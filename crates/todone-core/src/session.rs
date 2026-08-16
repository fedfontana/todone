//! The interactive session state machine.
//!
//! A [`Session`] holds the findings of a scan plus a decision for each one.
//! It is a pure data structure: the TUI, the Ghostty GUI, and tests all
//! drive it the same way, and it knows nothing about editors or forges.
//!
//! Decisions are only recorded here. Nothing is written to disk or to any
//! forge until the caller (the CLI) executes the session after the user
//! confirms.

use crate::draft::IssueDraft;
use crate::model::Finding;

/// What the user chose to do with a finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Leave the comment in place.
    Skip,
    /// Create an issue and remove the comment.
    Port(IssueDraft),
    /// Remove the comment without creating an issue.
    Delete,
}

impl Decision {
    /// A short human-readable label for menus and summaries.
    pub fn label(&self) -> &'static str {
        match self {
            Decision::Skip => "skip",
            Decision::Port(_) => "port",
            Decision::Delete => "delete",
        }
    }
}

/// One row of the confirm summary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionItem {
    /// Index into `Session::findings`.
    pub index: usize,
    /// The finding itself.
    pub finding: Finding,
    /// The decision made, if any.
    pub decision: Option<Decision>,
}

/// Interactive state over a set of findings.
#[derive(Debug, Clone)]
pub struct Session {
    /// The findings, in scan order.
    pub findings: Vec<Finding>,
    /// Decisions by finding index; `None` means undecided.
    pub decisions: Vec<Option<Decision>>,
    /// Index of the finding currently shown.
    pub cursor: usize,
    /// The commit hash the scan ran against, embedded in new drafts.
    pub commit: String,
}

impl Session {
    /// Creates a session over `findings` with no decisions.
    pub fn new(findings: Vec<Finding>, commit: String) -> Self {
        let decisions = vec![None; findings.len()];
        Self {
            findings,
            decisions,
            cursor: 0,
            commit,
        }
    }

    /// The finding currently shown, or `None` when the session is empty.
    pub fn current(&self) -> Option<&Finding> {
        self.findings.get(self.cursor)
    }

    /// The current cursor position.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The number of findings in the session.
    pub fn len(&self) -> usize {
        self.findings.len()
    }

    /// Whether the session has no findings.
    pub fn is_empty(&self) -> bool {
        self.findings.is_empty()
    }

    /// The decision for the finding at `index`, if any.
    pub fn decision(&self, index: usize) -> Option<&Decision> {
        self.decisions.get(index).and_then(Option::as_ref)
    }

    /// The decision for the current finding, if any.
    pub fn current_decision(&self) -> Option<&Decision> {
        self.decision(self.cursor)
    }

    /// Records the decision for the finding at `index`.
    pub fn set_decision(&mut self, index: usize, decision: Decision) {
        if let Some(slot) = self.decisions.get_mut(index) {
            *slot = Some(decision);
        }
    }

    /// Records the decision for the current finding.
    pub fn set_current_decision(&mut self, decision: Decision) {
        self.set_decision(self.cursor, decision);
    }

    /// Moves the cursor by `delta` findings, clamping at the edges.
    ///
    /// # Examples
    ///
    /// ```
    /// use todone_core::session::Session;
    ///
    /// let mut session = Session::new(vec![], "abc".into());
    /// session.navigate(5);
    /// assert_eq!(session.cursor(), 0);
    /// ```
    pub fn navigate(&mut self, delta: isize) {
        if self.findings.is_empty() {
            self.cursor = 0;
            return;
        }
        let target = self.cursor as isize + delta;
        self.cursor = target.clamp(0, self.findings.len() as isize - 1) as usize;
    }

    /// Jumps to the finding at `index`, clamped.
    pub fn goto(&mut self, index: usize) {
        if self.findings.is_empty() {
            self.cursor = 0;
            return;
        }
        self.cursor = index.min(self.findings.len() - 1);
    }

    /// Whether every finding has a decision.
    pub fn all_decided(&self) -> bool {
        self.decisions.iter().all(Option::is_some)
    }

    /// The number of findings without a decision.
    pub fn undecided_count(&self) -> usize {
        self.decisions.iter().filter(|d| d.is_none()).count()
    }

    /// Moves the cursor to the next finding without a decision and returns
    /// whether one was found.
    pub fn next_undecided(&mut self) -> bool {
        for offset in 1..=self.decisions.len() {
            let index = (self.cursor + offset) % self.decisions.len();
            if self.decisions[index].is_none() {
                self.cursor = index;
                return true;
            }
        }
        false
    }

    /// The confirm summary: every finding paired with its decision.
    pub fn items(&self) -> Vec<SessionItem> {
        self.findings
            .iter()
            .cloned()
            .enumerate()
            .map(|(index, finding)| SessionItem {
                index,
                finding,
                decision: self.decisions[index].clone(),
            })
            .collect()
    }

    /// The indices of findings the user wants to port (in scan order).
    pub fn ports(&self) -> Vec<usize> {
        self.decisions
            .iter()
            .enumerate()
            .filter_map(|(i, d)| matches!(d, Some(Decision::Port(_))).then_some(i))
            .collect()
    }

    /// The indices of findings the user wants to delete (in scan order).
    pub fn deletes(&self) -> Vec<usize> {
        self.decisions
            .iter()
            .enumerate()
            .filter_map(|(i, d)| matches!(d, Some(Decision::Delete)).then_some(i))
            .collect()
    }

    /// The drafts to create, paired with their finding index (in scan
    /// order).
    pub fn draft_tasks(&self) -> Vec<(usize, &IssueDraft)> {
        self.decisions
            .iter()
            .enumerate()
            .filter_map(|(i, d)| match d {
                Some(Decision::Port(draft)) => Some((i, draft)),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Comment, CommentRun, Selection};
    use std::ops::Range;

    fn finding(line: usize) -> Finding {
        Finding {
            run: CommentRun {
                comments: vec![Comment {
                    path: "a.rs".into(),
                    line,
                    end_line: line,
                    column: 0,
                    byte_range: Range { start: 0, end: 10 },
                    text: "// TODO: x".into(),
                    language: "rust".into(),
                }],
            },
            category: "TODO".into(),
            primary: 0,
            selection: Selection::full(1),
        }
    }

    fn draft(title: &str) -> IssueDraft {
        IssueDraft {
            category: "TODO".into(),
            path: "a.rs".into(),
            commit: "abc".into(),
            title: title.into(),
            description: "d".into(),
        }
    }

    #[test]
    fn navigation_clamps() {
        let mut session = Session::new(vec![finding(1), finding(2), finding(3)], "abc".into());
        assert_eq!(session.cursor(), 0);
        session.navigate(1);
        assert_eq!(session.cursor(), 1);
        session.navigate(10);
        assert_eq!(session.cursor(), 2);
        session.navigate(-10);
        assert_eq!(session.cursor(), 0);
        session.goto(99);
        assert_eq!(session.cursor(), 2);
    }

    #[test]
    fn empty_session_navigation_is_safe() {
        let mut session = Session::new(vec![], "abc".into());
        assert!(session.is_empty());
        assert!(session.current().is_none());
        session.navigate(3);
        assert_eq!(session.cursor(), 0);
        assert!(!session.next_undecided());
    }

    #[test]
    fn decisions_are_recorded_and_listed() {
        let mut session = Session::new(vec![finding(1), finding(2), finding(3)], "abc".into());
        assert!(!session.all_decided());
        assert_eq!(session.undecided_count(), 3);

        session.set_decision(0, Decision::Skip);
        session.set_decision(1, Decision::Delete);
        session.set_decision(2, Decision::Port(draft("t")));
        assert!(session.all_decided());
        assert_eq!(session.ports(), vec![2]);
        assert_eq!(session.deletes(), vec![1]);
        assert_eq!(session.draft_tasks().len(), 1);
        assert_eq!(session.decision(0).unwrap().label(), "skip");
        assert_eq!(session.decision(1).unwrap().label(), "delete");
        assert_eq!(session.decision(2).unwrap().label(), "port");
    }

    #[test]
    fn replacing_a_decision() {
        let mut session = Session::new(vec![finding(1)], "abc".into());
        session.set_decision(0, Decision::Skip);
        session.set_decision(0, Decision::Delete);
        assert_eq!(session.decision(0).unwrap(), &Decision::Delete);
    }

    #[test]
    fn next_undecided_skips_decided() {
        let mut session = Session::new(vec![finding(1), finding(2), finding(3)], "abc".into());
        session.set_decision(0, Decision::Skip);
        session.set_decision(2, Decision::Skip);
        assert!(session.next_undecided());
        assert_eq!(session.cursor(), 1);
        session.set_decision(1, Decision::Skip);
        assert!(!session.next_undecided());
    }

    #[test]
    fn items_pair_findings_with_decisions() {
        let mut session = Session::new(vec![finding(1), finding(2)], "abc".into());
        session.set_decision(1, Decision::Skip);
        let items = session.items();
        assert_eq!(items.len(), 2);
        assert!(items[0].decision.is_none());
        assert_eq!(items[1].decision.as_ref().unwrap().label(), "skip");
    }
}
