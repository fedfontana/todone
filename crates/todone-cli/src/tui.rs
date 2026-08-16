//! The interactive ratatui review: one screen per finding plus a confirm
//! screen, driven by the core session state machine.
//!
//! The app is terminal-agnostic: it renders to any `ratatui::Terminal`
//! (crossterm in production, `TestBackend` in tests) and processes
//! crossterm events through [`PortApp::handle_key`].

use std::collections::HashMap;
use std::path::PathBuf;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table};
use todone_core::draft::{ContextSnippet, IssueDraft};
use todone_core::model::Finding;
use todone_core::session::{Decision, Session};

use crate::context::extract_context;
use crate::editor::edit_draft;
use crate::highlight::HighlightEngine;
use crate::run::scan::ScanContext;

/// The screen currently shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Reviewing findings.
    Review,
    /// Shrinking/growing the selected comment range.
    SelectionEdit,
    /// The pre-execution confirmation list.
    Confirm,
}

/// What the user asked the TUI loop to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppAction {
    /// Keep running.
    Continue,
    /// Leave the TUI and execute the session.
    Execute,
    /// Leave the TUI without changing anything.
    Quit,
}

/// Full-screen review state.
pub struct PortApp {
    /// The core session (findings + decisions + cursor).
    pub session: Session,
    /// Context windows per finding index.
    pub contexts: Vec<crate::context::Context>,
    /// The current screen.
    pub mode: Mode,
    /// Transient status message shown in the footer.
    pub message: Option<String>,
    /// Whether the help overlay is open.
    pub show_help: bool,
    /// Whether execution will only print the plan.
    pub dry_run: bool,
    /// When set, quitting the review executes instead of aborting, and the
    /// confirm screen is skipped once everything is decided (`--yes`).
    pub auto_confirm: bool,
    /// Cached sources per repository-relative path.
    sources: HashMap<PathBuf, String>,
    /// Cached highlight spans per repository-relative path.
    spans: HashMap<PathBuf, Vec<crate::highlight::HighlightSpan>>,
    engine: HighlightEngine,
    /// Repository root, for reads and editors.
    pub root: PathBuf,
    /// Context configuration (before/after).
    pub context_lines: (usize, usize),
}

impl PortApp {
    /// Builds the app for a session over the scanned findings.
    pub fn new(session: Session, ctx: &ScanContext) -> Self {
        let (before, after) = (ctx.config.context.before, ctx.config.context.after);
        let mut app = Self {
            session,
            contexts: Vec::new(),
            mode: Mode::Review,
            message: None,
            show_help: false,
            dry_run: false,
            auto_confirm: false,
            sources: HashMap::new(),
            spans: HashMap::new(),
            engine: HighlightEngine::new(),
            root: ctx.repo.root.clone(),
            context_lines: (before, after),
        };
        app.refresh_contexts();
        app
    }

    /// Recomputes context windows for every finding (also picks up
    /// selection edits).
    pub fn refresh_contexts(&mut self) {
        let (before, after) = self.context_lines;
        let findings: Vec<Finding> = self.session.findings.clone();
        self.contexts = findings
            .iter()
            .map(|finding| {
                let source = self.source_of(finding).to_string();
                extract_context(&source, finding, before, after)
            })
            .collect();
    }

    /// The cached source text of a finding's file.
    pub fn source_of(&mut self, finding: &Finding) -> &str {
        let root = self.root.clone();
        let path = finding.path().to_path_buf();
        self.sources
            .entry(path.clone())
            .or_insert_with(|| std::fs::read_to_string(root.join(&path)).unwrap_or_default())
    }

    /// The cached highlight spans of a finding's file.
    pub fn spans_of(&mut self, finding: &Finding) -> Vec<crate::highlight::HighlightSpan> {
        let language = finding.run.comments[finding.primary].language.clone();
        let source = self.source_of(finding).to_string();
        self.spans
            .entry(finding.path().to_path_buf())
            .or_insert_with(|| {
                self.engine
                    .highlight(&language, &source)
                    .unwrap_or_default()
            })
            .clone()
    }

    /// The currently shown finding, if any.
    pub fn current(&self) -> Option<&Finding> {
        self.session.current()
    }

    /// Resolves the finding at the cursor for mutation.
    pub fn current_mut(&mut self) -> Option<&mut Finding> {
        let cursor = self.session.cursor();
        self.session.findings.get_mut(cursor)
    }

    /// Handles one key, mutating state and returning the app action.
    pub fn handle_key(&mut self, key: ratatui::crossterm::event::KeyCode) -> AppAction {
        if self.show_help {
            if key == ratatui::crossterm::event::KeyCode::Esc
                || key == ratatui::crossterm::event::KeyCode::Char('?')
                || key == ratatui::crossterm::event::KeyCode::Char('q')
            {
                self.show_help = false;
            }
            return AppAction::Continue;
        }
        match self.mode {
            Mode::SelectionEdit => self.handle_selection_key(key),
            Mode::Confirm => self.handle_confirm_key(key),
            Mode::Review => self.handle_review_key(key),
        }
    }

    fn handle_review_key(&mut self, key: ratatui::crossterm::event::KeyCode) -> AppAction {
        use ratatui::crossterm::event::KeyCode::{Char, Down, Esc, Up};
        match key {
            Char('p') => AppAction::Continue, // handled by the caller (editor session)
            Char('o') => AppAction::Continue, // handled by the caller (read-only view)
            Char('s') => {
                self.decide(Decision::Skip);
                self.advance()
            }
            Char('d') => {
                self.decide(Decision::Delete);
                self.advance()
            }
            Char('x') => {
                self.session.set_current_decision(Decision::Skip);
                self.message = Some("decision cleared".into());
                self.undecide();
                AppAction::Continue
            }
            Char('e') => {
                self.mode = Mode::SelectionEdit;
                AppAction::Continue
            }
            Char('j') | Down => {
                self.session.navigate(1);
                AppAction::Continue
            }
            Char('k') | Up => {
                self.session.navigate(-1);
                AppAction::Continue
            }
            Char('c') => {
                self.mode = Mode::Confirm;
                AppAction::Continue
            }
            Char('?') => {
                self.show_help = true;
                AppAction::Continue
            }
            Char('q') | Esc => {
                if self.auto_confirm {
                    AppAction::Execute
                } else {
                    AppAction::Quit
                }
            }
            _ => AppAction::Continue,
        }
    }

    fn handle_selection_key(&mut self, key: ratatui::crossterm::event::KeyCode) -> AppAction {
        use ratatui::crossterm::event::KeyCode::{Char, Down, Esc, Left, Right, Up};
        match key {
            Left => {
                if let Some(finding) = self.current_mut() {
                    finding.shrink_selection();
                    self.refresh_contexts();
                }
                AppAction::Continue
            }
            Right => {
                if let Some(finding) = self.current_mut() {
                    finding.grow_selection();
                    self.refresh_contexts();
                }
                AppAction::Continue
            }
            Char('r') => {
                if let Some(finding) = self.current_mut() {
                    finding.reset_selection();
                    self.refresh_contexts();
                }
                AppAction::Continue
            }
            Char('q') | Esc => {
                self.mode = Mode::Review;
                AppAction::Continue
            }
            Char('j') | Down => {
                self.session.navigate(1);
                AppAction::Continue
            }
            Char('k') | Up => {
                self.session.navigate(-1);
                AppAction::Continue
            }
            _ => AppAction::Continue,
        }
    }

    fn handle_confirm_key(&mut self, key: ratatui::crossterm::event::KeyCode) -> AppAction {
        use ratatui::crossterm::event::KeyCode::{Char, Esc};
        match key {
            Char('y') => AppAction::Execute,
            Char('b') | Char('q') | Esc => {
                self.mode = Mode::Review;
                AppAction::Continue
            }
            _ => AppAction::Continue,
        }
    }

    fn decide(&mut self, decision: Decision) {
        self.session.set_current_decision(decision);
    }

    /// Clears the current decision (leaves it undecided).
    fn undecide(&mut self) {
        let cursor = self.session.cursor();
        if let Some(slot) = self.session.decisions.get_mut(cursor) {
            *slot = None;
        }
    }

    /// Moves to the next finding after a decision; when everything is
    /// decided, jumps to the confirm screen (or executes directly with
    /// `--yes`).
    fn advance(&mut self) -> AppAction {
        if self.session.next_undecided() {
            return AppAction::Continue;
        }
        self.message = Some("all findings decided".into());
        if self.auto_confirm {
            AppAction::Execute
        } else {
            self.mode = Mode::Confirm;
            AppAction::Continue
        }
    }

    /// The draft for the current finding, prefilled from the comment text.
    pub fn prefilled_draft(&self) -> Option<IssueDraft> {
        let finding = self.current()?;
        let comment = &finding.run.comments[finding.primary];
        let mut title: String = comment.text.lines().next()?.trim().to_string();
        if title.len() > 72 {
            title.truncate(72);
            title.push_str("...");
        }
        Some(IssueDraft {
            category: finding.category.clone(),
            path: comment.path.clone(),
            commit: self.session.commit.clone(),
            title,
            description: format!(
                "Ported from `{}:{}`\n\n```\n{}\n```\n",
                comment.path.display(),
                comment.line,
                comment.text.trim()
            ),
        })
    }

    /// The snippet for the current finding, as embedded in drafts.
    pub fn current_snippet(&self) -> Option<ContextSnippet> {
        let finding = self.current()?;
        let context = self.contexts.get(self.session.cursor())?;
        let primary = &finding.run.comments[finding.primary];
        Some(ContextSnippet {
            language: primary.language.clone(),
            text: context
                .lines
                .iter()
                .map(|l| l.text.as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        })
    }
}

/// Renders the whole app into a frame.
pub fn render(frame: &mut ratatui::Frame, app: &mut PortApp) {
    if app.show_help {
        render_help(frame);
        return;
    }
    match app.mode {
        Mode::Review | Mode::SelectionEdit => render_review(frame, app),
        Mode::Confirm => render_confirm(frame, app),
    }
}

fn render_help(frame: &mut ratatui::Frame) {
    let help = vec![
        Line::from("p  port the comment to an issue (opens the editor)"),
        Line::from("o  open the file read-only in the editor"),
        Line::from("s  skip the comment"),
        Line::from("d  delete the comment without creating an issue"),
        Line::from("x  clear the decision for this finding"),
        Line::from("e  edit the selected comment range"),
        Line::from("j/k  next / previous finding"),
        Line::from("c  go to the confirmation screen"),
        Line::from("q  quit (no changes)"),
        Line::from(""),
        Line::from("selection editing: ← shrink · → grow · r reset · esc done"),
        Line::from("confirmation:      y execute · b back to review"),
    ];
    let paragraph = Paragraph::new(help).block(
        Block::default()
            .title(" todone help ")
            .borders(Borders::ALL),
    );
    frame.render_widget(paragraph, frame.area());
}

fn render_review(frame: &mut ratatui::Frame, app: &mut PortApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(2),
        ])
        .split(frame.area());

    render_header(frame, chunks[0], app);
    render_context_area(frame, chunks[1], app);
    render_footer(frame, chunks[2], app);
}

fn render_header(frame: &mut ratatui::Frame, area: Rect, app: &PortApp) {
    let Some(finding) = app.current() else {
        return;
    };
    let primary = &finding.run.comments[finding.primary];
    let decision = app.session.current_decision();
    let badge = match decision {
        Some(Decision::Skip) => " [skip]",
        Some(Decision::Delete) => " [delete]",
        Some(Decision::Port(_)) => " [port]",
        None => "",
    };
    let selection = if app.mode == Mode::SelectionEdit {
        " (selecting)"
    } else {
        ""
    };
    let title = format!(
        " {}/{} — {}:{}: {}{}{}",
        app.session.cursor() + 1,
        app.session.len(),
        primary.path.display(),
        finding.line(),
        finding.category,
        badge,
        selection
    );
    let mut line = Line::from(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    line.spans.push(Span::styled(
        format!("  ({} undecided)", app.session.undecided_count()),
        Style::default().fg(Color::DarkGray),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

/// Converts the context window into styled ratatui lines.
fn context_lines(app: &mut PortApp) -> Vec<Line<'static>> {
    let cursor = app.session.cursor();
    let finding = app.session.findings.get(cursor).cloned();
    let Some(finding) = finding else {
        return Vec::new();
    };
    let context = app.contexts.get(cursor).cloned();
    let Some(context) = context else {
        return Vec::new();
    };
    let selection = finding.selected_range();
    let spans = app.spans_of(&finding);
    let width = context
        .lines
        .iter()
        .map(|l| l.line.to_string().len())
        .max()
        .unwrap_or(1);

    let mut out = Vec::new();
    for line in context.lines.iter() {
        let marker =
            if line.byte_range.start < selection.end && line.byte_range.end > selection.start {
                ">"
            } else {
                " "
            };
        let gutter = format!(" {marker} {:>width$} │ ", line.line, width = width);
        let mut spans_line = Vec::new();
        spans_line.push(Span::styled(
            gutter,
            Style::default().fg(Color::Rgb(90, 100, 110)),
        ));

        for (i, ch) in line.text.char_indices() {
            let offset = line.byte_range.start + i;
            let span = spans
                .iter()
                .rev()
                .find(|s| offset >= s.range.start && offset < s.range.end);
            let mut style = match span.and_then(|s| s.fg) {
                Some((r, g, b)) => {
                    let mut style = Style::default().fg(Color::Rgb(r, g, b));
                    if span.is_some_and(|s| s.italic) {
                        style = style.add_modifier(Modifier::ITALIC);
                    }
                    style
                }
                None => Style::default(),
            };
            if offset >= selection.start && offset < selection.end {
                style = style.bg(Color::Rgb(38, 46, 58));
            }
            spans_line.push(Span::styled(ch.to_string(), style));
        }
        out.push(Line::from(spans_line));
    }
    out
}

fn render_context_area(frame: &mut ratatui::Frame, area: Rect, app: &mut PortApp) {
    let lines = context_lines(app);
    let paragraph = Paragraph::new(lines).block(Block::default().borders(Borders::ALL));
    frame.render_widget(paragraph, area);
}

fn render_footer(frame: &mut ratatui::Frame, area: Rect, app: &PortApp) {
    let mode_hint = match app.mode {
        Mode::Review => {
            "p port · s skip · d delete · o view · e select · j/k nav · c confirm · ? help · q quit"
        }
        Mode::SelectionEdit => "← shrink · → grow · r reset · j/k nav · esc done",
        Mode::Confirm => "y execute · b back · q quit",
    };
    let mut text = mode_hint.to_string();
    if let Some(message) = &app.message {
        text = format!("{message}  —  {text}");
    }
    let paragraph = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(paragraph, area);
}

fn render_confirm(frame: &mut ratatui::Frame, app: &mut PortApp) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(frame.area());

    let rows = app
        .session
        .items()
        .iter()
        .map(|item| {
            let finding = &item.finding;
            let primary = &finding.run.comments[finding.primary];
            let (action, title) = match &item.decision {
                Some(Decision::Port(draft)) => ("port", draft.title.clone()),
                Some(Decision::Delete) => ("delete", String::new()),
                Some(Decision::Skip) => ("skip", String::new()),
                None => ("undecided", String::new()),
            };
            Row::new(vec![
                Cell::from((item.index + 1).to_string()),
                Cell::from(primary.path.display().to_string()),
                Cell::from(finding.category.clone()),
                Cell::from(action),
                Cell::from(title),
            ])
        })
        .collect::<Vec<_>>();
    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(30),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Min(10),
        ],
    )
    .header(Row::new(vec![
        Cell::from(Span::styled(
            "#",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Cell::from(Span::styled(
            "path",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Cell::from(Span::styled(
            "category",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Cell::from(Span::styled(
            "action",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Cell::from(Span::styled(
            "title",
            Style::default().add_modifier(Modifier::BOLD),
        )),
    ]))
    .block(Block::default().title(" confirm ").borders(Borders::ALL));
    frame.render_widget(table, chunks[0]);

    let footer = if app.dry_run {
        "dry run: nothing will be created or removed · y show plan · b back"
    } else {
        "y execute · b back · q quit"
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            footer,
            Style::default().fg(Color::DarkGray),
        ))),
        chunks[1],
    );
}

/// Runs the interactive review loop on the real terminal.
///
/// The TUI leaves the alternate screen while an editor session runs, and
/// returns to it afterwards. Ends with [`AppAction::Execute`] or
/// [`AppAction::Quit`].
///
/// # Errors
///
/// Returns an error when the terminal cannot be set up or restored, or when
/// an editor session fails.
pub fn run_interactive(
    app: &mut PortApp,
    editor: &crate::editor::Editor,
) -> anyhow::Result<AppAction> {
    use ratatui::backend::CrosstermBackend;
    use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind};
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };

    enable_raw_mode()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;

    let result = loop {
        terminal.draw(|frame| render(frame, app))?;
        if !event::poll(std::time::Duration::from_millis(200))? {
            continue;
        }
        let Event::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        let action = match key.code {
            KeyCode::Char('p') => {
                let draft = app.prefilled_draft();
                let snippet = app.current_snippet();
                let Some(draft) = draft else {
                    app.message = Some("nothing to port here".into());
                    continue;
                };
                let Some(snippet) = snippet else {
                    continue;
                };
                match suspend(&mut terminal, || edit_draft(editor, &draft, &snippet))? {
                    crate::editor::DraftOutcome::Drafted(draft) => {
                        app.session
                            .set_current_decision(todone_core::session::Decision::Port(draft));
                        app.message = Some("drafted; s/d/x can change this decision".into());
                    }
                    crate::editor::DraftOutcome::Aborted => {
                        app.message = Some("draft aborted; nothing changed".into());
                    }
                }
                continue;
            }
            KeyCode::Char('o') => {
                let Some(finding) = app.session.current() else {
                    continue;
                };
                let path = finding.path().to_path_buf();
                let line = finding.line();
                match suspend(&mut terminal, || {
                    editor.view_readonly(&app.root, &path, line)
                }) {
                    Ok(()) => app.message = Some("read-only view closed".into()),
                    Err(err) => app.message = Some(format!("editor failed: {err}")),
                }
                continue;
            }
            other => app.handle_key(other),
        };
        match action {
            AppAction::Continue => {}
            done @ (AppAction::Execute | AppAction::Quit) => break done,
        }
    };

    terminal.clear()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    disable_raw_mode()?;
    Ok(result)
}

/// Runs `f` outside the TUI: the terminal is restored to normal mode first
/// (so the editor sees the real terminal) and the TUI is re-entered after.
fn suspend<R>(
    terminal: &mut Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
    f: impl FnOnce() -> anyhow::Result<R>,
) -> anyhow::Result<R> {
    use ratatui::crossterm::execute;
    use ratatui::crossterm::terminal::{
        EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
    };

    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    let result = f();
    enable_raw_mode()?;
    execute!(std::io::stdout(), EnterAlternateScreen)?;
    terminal.clear()?;
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use todone_core::model::{Comment, CommentRun, Selection};

    fn finding(line: usize, end_line: usize) -> Finding {
        Finding {
            run: CommentRun {
                comments: vec![Comment {
                    path: "src/a.rs".into(),
                    line,
                    end_line,
                    column: 0,
                    byte_range: 0..13,
                    text: "// TODO: x".into(),
                    language: "rust".into(),
                }],
            },
            category: "TODO".into(),
            primary: 0,
            selection: Selection::full(1),
        }
    }

    fn ctx_for(_finding: Finding) -> (ScanContext, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "// TODO: x\n").unwrap();
        let repo = todone_core::repo::RepoInfo {
            root: dir.path().to_path_buf(),
            commit: Some("abc".into()),
        };
        let ctx = ScanContext {
            config: todone_core::config::Config::defaults(),
            repo,
        };
        (ctx, dir)
    }

    #[test]
    fn review_screen_renders_finding() {
        let (ctx, _dir) = ctx_for(finding(1, 1));
        let session = Session::new(vec![finding(1, 1)], "abc".into());
        let mut app = PortApp::new(session, &ctx);

        // The context window itself must contain the comment text.
        let rendered_lines = context_lines(&mut app);
        assert_eq!(rendered_lines.len(), 1, "expected one context line");
        let text: String = rendered_lines[0]
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect();
        assert!(text.contains("// TODO: x"), "comment missing: {text:?}");

        let backend = TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let buffer = terminal.backend().buffer();
        let content = buffer_to_string(buffer);
        assert!(content.contains("1/1"), "header missing:\n{content}");
        assert!(
            content.contains("src/a.rs:1: TODO"),
            "path missing:\n{content}"
        );
        assert!(
            content.contains("// TODO: x"),
            "comment missing:\n{content}"
        );
    }

    #[test]
    fn keys_make_decisions_and_navigate() {
        let (ctx, _dir) = ctx_for(finding(1, 1));
        let mut app = PortApp::new(
            Session::new(vec![finding(1, 1), finding(2, 2)], "abc".into()),
            &ctx,
        );
        use ratatui::crossterm::event::KeyCode::{Char, Down};
        assert_eq!(app.handle_key(Char('s')), AppAction::Continue);
        assert_eq!(*app.session.decision(0).unwrap(), Decision::Skip);
        assert_eq!(app.session.cursor(), 1);
        assert_eq!(app.handle_key(Down), AppAction::Continue);
        assert_eq!(app.session.cursor(), 1);
        assert_eq!(app.handle_key(Char('q')), AppAction::Quit);
    }

    #[test]
    fn deciding_everything_leads_to_confirm() {
        let (ctx, _dir) = ctx_for(finding(1, 1));
        let mut app = PortApp::new(
            Session::new(vec![finding(1, 1), finding(2, 2)], "abc".into()),
            &ctx,
        );
        use ratatui::crossterm::event::KeyCode::Char;
        app.handle_key(Char('s'));
        app.handle_key(Char('s'));
        assert_eq!(app.mode, Mode::Confirm);
        assert_eq!(app.handle_key(Char('y')), AppAction::Execute);
    }

    #[test]
    fn confirm_screen_lists_decisions() {
        let (ctx, _dir) = ctx_for(finding(1, 1));
        let mut session = Session::new(vec![finding(1, 1), finding(2, 2)], "abc".into());
        session.set_decision(0, Decision::Skip);
        session.set_decision(1, Decision::Delete);
        let mut app = PortApp::new(session, &ctx);
        app.mode = Mode::Confirm;

        let backend = TestBackend::new(100, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let content = buffer_to_string(terminal.backend().buffer());
        assert!(content.contains("confirm"));
        assert!(content.contains("skip"));
        assert!(content.contains("delete"));
    }

    #[test]
    fn selection_editing_updates_the_range() {
        let run = CommentRun {
            comments: vec![
                Comment {
                    path: "a.rs".into(),
                    line: 1,
                    end_line: 1,
                    column: 0,
                    byte_range: 0..9,
                    text: "// TODO: a".into(),
                    language: "rust".into(),
                },
                Comment {
                    path: "a.rs".into(),
                    line: 2,
                    end_line: 2,
                    column: 0,
                    byte_range: 10..24,
                    text: "// note: b".into(),
                    language: "rust".into(),
                },
            ],
        };
        let f = Finding {
            run,
            category: "TODO".into(),
            primary: 0,
            selection: Selection::full(2),
        };
        let (ctx, _dir) = ctx_for(finding(1, 1));
        let mut app = PortApp::new(Session::new(vec![f], "abc".into()), &ctx);
        app.mode = Mode::SelectionEdit;
        use ratatui::crossterm::event::KeyCode::{Char, Left, Right};
        app.handle_key(Left);
        let sel = app.current().unwrap().selection;
        assert_eq!((sel.start, sel.end), (0, 0));
        app.handle_key(Left);
        let sel = app.current().unwrap().selection;
        assert_eq!((sel.start, sel.end), (0, 0));
        app.handle_key(Right);
        let sel = app.current().unwrap().selection;
        assert_eq!((sel.start, sel.end), (0, 1));
        app.handle_key(Char('r'));
        let sel = app.current().unwrap().selection;
        assert_eq!((sel.start, sel.end), (0, 1));
    }

    fn buffer_to_string(buffer: &ratatui::buffer::Buffer) -> String {
        let area = buffer.area;
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buffer[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }
}
