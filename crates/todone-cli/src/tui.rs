//! The interactive ratatui review: one screen per finding plus a confirm
//! screen, driven by the core session state machine.
//!
//! The context pane fills the available terminal height, wraps long lines
//! (toggleable with `w`), and scrolls less/bat-style (`j`/`k`, `h`/`l`,
//! `Ctrl-d/u`, `Ctrl-f/b`, `g`/`G`, `z` to center the comment). Findings
//! are navigated with `Ctrl-n`/`Ctrl-p`; the statusline shows the repo,
//! forge, commit, and session stats.
//!
//! The app is terminal-agnostic: it renders to any `ratatui::Terminal`
//! (crossterm in production, `TestBackend` in tests) and processes
//! crossterm events through [`PortApp::handle_key`].

use std::collections::HashMap;
use std::path::PathBuf;

use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, Wrap};
use todone_core::draft::{ContextSnippet, IssueDraft};
use todone_core::model::Finding;
use todone_core::session::{Decision, Session};
use unicode_width::UnicodeWidthStr;

use crate::context::{Context, LineKind, extract_context};
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
    /// The current screen.
    pub mode: Mode,
    /// Transient status message shown in the hints line.
    pub message: Option<String>,
    /// Whether the help overlay is open.
    pub show_help: bool,
    /// Whether execution will only print the plan.
    pub dry_run: bool,
    /// When set, quitting the review executes instead of aborting, and the
    /// confirm screen is skipped once everything is decided (`--yes`).
    pub auto_confirm: bool,
    /// Word wrap on the context pane.
    pub wrap: bool,
    /// Tab width for expanding tabs in the context pane.
    pub tab_width: usize,
    /// Vertical scroll offset (rendered rows) of the context pane.
    pub v_scroll: usize,
    /// Horizontal scroll offset (characters; only visible without wrap).
    pub h_scroll: usize,
    /// Repo basename for the statusline.
    pub repo_name: String,
    /// Forge kind for the statusline.
    pub forge: String,
    /// Short commit hash for the statusline.
    pub commit: String,
    /// Scroll label for the statusline, computed each frame.
    pub scroll_label: String,
    /// The context pane size from the last frame.
    pub pane_height: usize,
    pub pane_width: usize,
    /// Cached sources per repository-relative path.
    sources: HashMap<PathBuf, String>,
    /// Cached whole-file context lines per repository-relative path.
    contexts: HashMap<PathBuf, Vec<crate::context::ContextLine>>,
    /// Cached highlight spans per repository-relative path.
    spans: HashMap<PathBuf, Vec<crate::highlight::HighlightSpan>>,
    engine: HighlightEngine,
    /// Repository root, for reads and editors.
    pub root: PathBuf,
    /// The config context window (before/after), used for draft snippets.
    pub context_lines: (usize, usize),
}

impl PortApp {
    /// Builds the app for a session over the scanned findings.
    pub fn new(session: Session, ctx: &ScanContext) -> Self {
        let (before, after) = (ctx.config.context.before, ctx.config.context.after);
        let repo_name = ctx
            .repo
            .root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| ctx.repo.root.display().to_string());
        let commit = ctx
            .repo
            .commit
            .as_deref()
            .map(|c| c[..c.len().min(8)].to_string())
            .unwrap_or_else(|| "none".to_string());
        Self {
            session,
            mode: Mode::Review,
            message: None,
            show_help: false,
            dry_run: false,
            auto_confirm: false,
            wrap: ctx.config.context.wrap,
            tab_width: ctx.config.context.tab_width,
            v_scroll: 0,
            h_scroll: 0,
            repo_name,
            forge: ctx.config.forge.kind.clone(),
            commit,
            scroll_label: "TOP".into(),
            pane_height: 0,
            pane_width: 0,
            sources: HashMap::new(),
            contexts: HashMap::new(),
            spans: HashMap::new(),
            engine: HighlightEngine::new(),
            root: ctx.repo.root.clone(),
            context_lines: (before, after),
        }
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

    /// The context window for the finding at `cursor`.
    ///
    /// The whole file is extracted (cached per path) so the pane can scroll
    /// through everything; only the selection is refreshed per frame.
    fn context_for_finding(&mut self, cursor: usize) -> Context {
        let Some(finding) = self.session.findings.get(cursor).cloned() else {
            return Context {
                lines: Vec::new(),
                selection: 0..0,
            };
        };
        let path = finding.path().to_path_buf();
        let source = self.source_of(&finding).to_string();
        let total = source.lines().count().max(1);
        let before = finding.line().saturating_sub(1);
        let after = total.saturating_sub(finding.line());
        let lines = self
            .contexts
            .entry(path)
            .or_insert_with(|| extract_context(&source, &finding, before, after).lines)
            .clone();
        Context {
            lines,
            selection: finding.selected_range(),
        }
    }

    /// The context pane plus the rendered row count of each line and the
    /// total height.
    fn view(&mut self, _pane_height: usize, pane_width: usize) -> (Context, Vec<usize>, usize) {
        let context = self.context_for_finding(self.session.cursor());
        let rows: Vec<usize> = context
            .lines
            .iter()
            .map(|line| rendered_rows(&line.text, pane_width, self.wrap, self.tab_width))
            .collect();
        let total = rows.iter().sum();
        (context, rows, total)
    }

    /// The maximum scroll offset for the current pane.
    fn max_scroll(&mut self) -> usize {
        let (_, _, total) = self.view(self.pane_height, self.pane_width);
        total.saturating_sub(self.pane_height)
    }

    /// Scrolls the context pane by `delta` rendered rows, clamped.
    fn scroll_lines(&mut self, delta: isize) {
        let max = self.max_scroll();
        self.v_scroll = (self.v_scroll as isize + delta).clamp(0, max as isize) as usize;
    }

    /// Moves the cursor by `delta` findings, resetting the scroll offsets.
    fn navigate(&mut self, delta: isize) {
        self.session.navigate(delta);
        self.v_scroll = 0;
        self.h_scroll = 0;
    }

    /// Centers the viewport on the selected comment range (vim `zz`).
    fn center_comment(&mut self) {
        let (context, rows, total) = self.view(self.pane_height, self.pane_width);
        let mut selected_start = None;
        let mut selected_end = 0;
        let mut y = 0;
        for (line, count) in context.lines.iter().zip(&rows) {
            let start = y;
            y += count;
            if line.kind == LineKind::Selected {
                selected_start.get_or_insert(start);
                selected_end = y;
            }
        }
        let Some(start) = selected_start else { return };
        let center = (start + selected_end) / 2;
        let max = total.saturating_sub(self.pane_height);
        self.v_scroll = center.saturating_sub(self.pane_height / 2).min(max);
    }

    /// Toggles word wrap, resetting the horizontal pan.
    fn toggle_wrap(&mut self) {
        self.wrap = !self.wrap;
        if self.wrap {
            self.h_scroll = 0;
        }
        self.message = Some(
            if self.wrap {
                "word wrap on"
            } else {
                "word wrap off"
            }
            .into(),
        );
    }

    /// The scroll indicator for the statusline.
    fn scroll_label(&mut self) -> String {
        let (_, _, total) = self.view(self.pane_height, self.pane_width);
        if self.pane_height == 0 || total <= self.pane_height {
            "ALL".into()
        } else if self.v_scroll == 0 {
            "TOP".into()
        } else {
            let max = total - self.pane_height;
            if self.v_scroll >= max {
                "BOT".into()
            } else {
                format!("{}%", self.v_scroll * 100 / max)
            }
        }
    }

    /// Handles one key, mutating state and returning the app action.
    pub fn handle_key(&mut self, key: ratatui::crossterm::event::KeyEvent) -> AppAction {
        if self.show_help {
            if matches!(
                key.code,
                ratatui::crossterm::event::KeyCode::Esc
                    | ratatui::crossterm::event::KeyCode::Char('?')
                    | ratatui::crossterm::event::KeyCode::Char('q')
            ) {
                self.show_help = false;
            }
            return AppAction::Continue;
        }
        match self.mode {
            Mode::SelectionEdit => self.handle_selection_key(&key),
            Mode::Confirm => self.handle_confirm_key(&key),
            Mode::Review => self.handle_review_key(&key),
        }
    }

    /// The keys shared by the review and selection screens: scrolling,
    /// panning, and finding navigation. Returns `Some` when handled.
    fn handle_nav_key(&mut self, key: &ratatui::crossterm::event::KeyEvent) -> Option<AppAction> {
        use ratatui::crossterm::event::{KeyCode, KeyModifiers};
        match (key.code, key.modifiers) {
            (KeyCode::Char('n'), KeyModifiers::CONTROL) => {
                self.navigate(1);
                Some(AppAction::Continue)
            }
            (KeyCode::Char('p'), KeyModifiers::CONTROL) => {
                self.navigate(-1);
                Some(AppAction::Continue)
            }
            (KeyCode::Char('j'), _) | (KeyCode::Down, _) => {
                self.scroll_lines(1);
                Some(AppAction::Continue)
            }
            (KeyCode::Char('k'), _) | (KeyCode::Up, _) => {
                self.scroll_lines(-1);
                Some(AppAction::Continue)
            }
            (KeyCode::Char('h'), _) | (KeyCode::Left, _) => {
                self.h_scroll = self.h_scroll.saturating_sub(1);
                Some(AppAction::Continue)
            }
            (KeyCode::Char('l'), _) | (KeyCode::Right, _) => {
                self.h_scroll += 1;
                Some(AppAction::Continue)
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) => {
                self.scroll_lines((self.pane_height as isize) / 2);
                Some(AppAction::Continue)
            }
            (KeyCode::Char('u'), KeyModifiers::CONTROL) => {
                self.scroll_lines(-((self.pane_height as isize) / 2));
                Some(AppAction::Continue)
            }
            (KeyCode::Char('f'), KeyModifiers::CONTROL) => {
                self.scroll_lines(self.pane_height as isize);
                Some(AppAction::Continue)
            }
            (KeyCode::Char('b'), KeyModifiers::CONTROL) => {
                self.scroll_lines(-(self.pane_height as isize));
                Some(AppAction::Continue)
            }
            (KeyCode::Char('g'), _) => {
                self.v_scroll = 0;
                Some(AppAction::Continue)
            }
            (KeyCode::Char('G'), _) => {
                self.scroll_lines(isize::MAX);
                Some(AppAction::Continue)
            }
            (KeyCode::Char('z'), _) => {
                self.center_comment();
                Some(AppAction::Continue)
            }
            (KeyCode::Char('w'), _) => {
                self.toggle_wrap();
                Some(AppAction::Continue)
            }
            _ => None,
        }
    }

    fn handle_review_key(&mut self, key: &ratatui::crossterm::event::KeyEvent) -> AppAction {
        use ratatui::crossterm::event::KeyCode::{Char, Esc};
        if let Some(action) = self.handle_nav_key(key) {
            return action;
        }
        match key.code {
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

    fn handle_selection_key(&mut self, key: &ratatui::crossterm::event::KeyEvent) -> AppAction {
        use ratatui::crossterm::event::KeyCode::{Char, Esc};
        if let Some(action) = self.handle_nav_key(key) {
            return action;
        }
        match key.code {
            Char('[') => {
                if let Some(finding) = self.current_mut() {
                    finding.grow_selection_top();
                }
                AppAction::Continue
            }
            Char(']') => {
                if let Some(finding) = self.current_mut() {
                    finding.grow_selection_bottom();
                }
                AppAction::Continue
            }
            Char('{') => {
                if let Some(finding) = self.current_mut() {
                    finding.shrink_selection_top();
                }
                AppAction::Continue
            }
            Char('}') => {
                if let Some(finding) = self.current_mut() {
                    finding.shrink_selection_bottom();
                }
                AppAction::Continue
            }
            Char('r') => {
                if let Some(finding) = self.current_mut() {
                    finding.reset_selection();
                }
                AppAction::Continue
            }
            Char('q') | Esc => {
                self.mode = Mode::Review;
                AppAction::Continue
            }
            _ => AppAction::Continue,
        }
    }

    fn handle_confirm_key(&mut self, key: &ratatui::crossterm::event::KeyEvent) -> AppAction {
        use ratatui::crossterm::event::KeyCode::{Char, Esc};
        match key.code {
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
    ///
    /// Surrounding code only: the selected comment lines do not belong in
    /// the issue.
    pub fn current_snippet(&mut self) -> Option<ContextSnippet> {
        let cursor = self.session.cursor();
        let finding = self.session.findings.get(cursor).cloned()?;
        let (before, after) = self.context_lines;
        let source = self.source_of(&finding).to_string();
        let context = extract_context(&source, &finding, before, after);
        let primary = &finding.run.comments[finding.primary];
        let lines: Vec<&str> = context
            .lines
            .iter()
            .filter(|line| line.kind != LineKind::Selected)
            .map(|line| line.text.as_str())
            .collect();
        let text = if lines.is_empty() {
            "(no surrounding context)".to_string()
        } else {
            lines.join("\n")
        };
        Some(ContextSnippet {
            language: primary.language.clone(),
            text,
        })
    }
}

/// Expands tabs to spaces with `tab_width` tab stops.
fn expand_tabs(text: &str, tab_width: usize) -> String {
    let mut out = String::with_capacity(text.len() + 16);
    let mut col = 0usize;
    for ch in text.chars() {
        if ch == '\t' {
            let spaces = tab_width - col % tab_width;
            out.extend(std::iter::repeat_n(' ', spaces));
            col += spaces;
        } else {
            col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
            out.push(ch);
        }
    }
    out
}

/// The number of display rows a context line occupies at `width`, after tab
/// expansion.
fn rendered_rows(text: &str, width: usize, wrap: bool, tab_width: usize) -> usize {
    if !wrap || width == 0 {
        return 1;
    }
    let text = expand_tabs(text, tab_width);
    let mut rows = 1usize;
    let mut col = 0usize;
    for word in text.split_whitespace() {
        let word_width = UnicodeWidthStr::width(word);
        if word_width >= width {
            // A word wider than the pane spans multiple rows by itself.
            rows += word_width / width;
            col = word_width % width;
            continue;
        }
        if col > 0 && col + 1 + word_width > width {
            rows += 1;
            col = word_width;
        } else {
            col += if col == 0 { word_width } else { 1 + word_width };
        }
    }
    rows
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
        Line::from("Ctrl-n / Ctrl-p  next / previous finding"),
        Line::from("j / k  scroll · h / l  pan · g / G  top / bottom · z  center"),
        Line::from("Ctrl-d/u  half page · Ctrl-f/b  full page · w  toggle wrap"),
        Line::from("c  go to the confirmation screen"),
        Line::from("q  quit (no changes)"),
        Line::from(""),
        Line::from("selection editing: [ grow up · ] grow down · { shrink up · } shrink down"),
        Line::from("                   r reset · esc done"),
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
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_header(frame, chunks[0], app);
    render_context_area(frame, chunks[1], app);
    render_statusline(frame, chunks[2], app);
    render_hints(frame, chunks[3], app);
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
/// Converts the visible part of the context window into styled ratatui
/// lines: only lines intersecting the viewport are built, so the per-frame
/// cost stays proportional to the pane rather than the file.
fn context_to_lines(
    app: &mut PortApp,
    context: &Context,
    rows: &[usize],
    v_scroll: usize,
    pane_height: usize,
) -> Vec<Line<'static>> {
    let Some(finding) = app.session.findings.get(app.session.cursor()).cloned() else {
        return Vec::new();
    };
    if context.lines.is_empty() {
        return Vec::new();
    }
    let selection = context.selection.clone();
    let spans = app.spans_of(&finding);
    let width = context
        .lines
        .iter()
        .map(|line| line.line.to_string().len())
        .max()
        .unwrap_or(1);

    // Cumulative row starts, so the visible line range can be found.
    let mut starts = Vec::with_capacity(rows.len() + 1);
    let mut acc = 0;
    starts.push(0);
    for count in rows {
        acc += count;
        starts.push(acc);
    }
    let view_end = v_scroll + pane_height;
    let first = (0..rows.len()).find(|&i| starts[i] + rows[i] > v_scroll);
    let last = (0..rows.len()).rev().find(|&i| starts[i] < view_end);
    let (Some(first), Some(last)) = (first, last) else {
        return Vec::new();
    };

    let mut out = Vec::new();
    for index in 0..=last - first {
        let line = &context.lines[first + index];
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

        let mut col = 0usize;
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
            // Tabs are control characters: ratatui drops them, so they are
            // expanded to spaces here (style included).
            if ch == '\t' {
                let spaces = app.tab_width - col % app.tab_width;
                col += spaces;
                spans_line.push(Span::styled(" ".repeat(spaces), style));
            } else {
                col += unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
                spans_line.push(Span::styled(ch.to_string(), style));
            }
        }
        out.push(Line::from(spans_line));
    }
    out
}

fn render_context_area(frame: &mut ratatui::Frame, area: Rect, app: &mut PortApp) {
    // The paragraph content sits inside the block's borders.
    let height = area.height.saturating_sub(2) as usize;
    let width = area.width.saturating_sub(2) as usize;
    app.pane_height = height;
    app.pane_width = width;
    let (context, rows, total) = app.view(height, width);

    let max = total.saturating_sub(height);
    if app.v_scroll > max {
        app.v_scroll = max;
    }
    app.scroll_label = app.scroll_label();

    let lines = context_to_lines(app, &context, &rows, app.v_scroll, height);
    let mut paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL))
        .scroll((app.v_scroll as u16, app.h_scroll as u16));
    if app.wrap {
        paragraph = paragraph.wrap(Wrap { trim: false });
    }
    frame.render_widget(paragraph, area);
}

/// The vim-style statusline: repo, forge, commit, and session stats.
fn render_statusline(frame: &mut ratatui::Frame, area: Rect, app: &PortApp) {
    let Some(finding) = app.current() else {
        return;
    };
    let primary = &finding.run.comments[finding.primary];
    let mode = match app.mode {
        Mode::Review => "NORMAL",
        Mode::SelectionEdit => "SELECT",
        Mode::Confirm => "CONFIRM",
    };
    let wrap = if app.wrap { "wrap" } else { "nowrap" };
    let left = format!(
        " {} ({}) · {}:{} · {}",
        app.repo_name,
        app.forge,
        primary.path.display(),
        finding.line(),
        app.commit
    );
    let mut right = format!(
        " {}/{} · {}u · {} · {} · {}",
        app.session.cursor() + 1,
        app.session.len(),
        app.session.undecided_count(),
        mode,
        wrap,
        app.scroll_label
    );
    if app.dry_run {
        right.push_str(" · dry-run");
    }
    let width = area.width as usize;
    let text = fit_status(&left, &right, width);
    let line = Line::from(Span::styled(
        text,
        Style::default().add_modifier(Modifier::REVERSED),
    ));
    frame.render_widget(Paragraph::new(line), area);
}

/// Fits a left/right statusline pair into `width`, truncating the left side
/// when necessary.
fn fit_status(left: &str, right: &str, width: usize) -> String {
    let left_len = left.chars().count();
    let right_len = right.chars().count();
    if left_len + right_len <= width {
        let pad = width - left_len - right_len;
        format!("{left}{:pad$}{right}", "", pad = pad)
    } else {
        let keep = width.saturating_sub(right_len + 1);
        let truncated: String = left.chars().take(keep).collect();
        format!("{truncated}…{right}")
    }
}

fn render_hints(frame: &mut ratatui::Frame, area: Rect, app: &PortApp) {
    let mode_hint = match app.mode {
        Mode::Review => {
            "p port · s skip · d delete · o view · e select · Ctrl-n/p nav · c confirm · ? help · q quit"
        }
        Mode::SelectionEdit => "[ ] grow · { } shrink · r reset · esc done",
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
        // The editor-bound keys only apply on the review screen.
        if app.mode == Mode::Review {
            match key.code {
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
                _ => {}
            }
        }
        let action = app.handle_key(key);
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
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use todone_core::model::{Comment, CommentRun, Selection};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

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
            commit: Some("abcdef1234567890".into()),
            is_repo: true,
            remote: None,
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
    fn statusline_shows_repo_forge_and_stats() {
        let (ctx, _dir) = ctx_for(finding(1, 1));
        let mut app = PortApp::new(Session::new(vec![finding(1, 1)], "abc".into()), &ctx);
        app.dry_run = true;

        let backend = TestBackend::new(120, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let content = buffer_to_string(terminal.backend().buffer());
        assert!(content.contains("NORMAL"), "mode missing:\n{content}");
        assert!(content.contains("github"), "forge missing:\n{content}");
        assert!(content.contains("abcdef12"), "commit missing:\n{content}");
        assert!(content.contains("wrap"), "wrap marker missing:\n{content}");
        assert!(content.contains("dry-run"), "dry-run missing:\n{content}");
        assert!(
            content.contains("1u"),
            "undecided count missing:\n{content}"
        );
    }

    #[test]
    fn ctrl_navigation_and_decisions() {
        let (ctx, _dir) = ctx_for(finding(1, 1));
        let mut app = PortApp::new(
            Session::new(vec![finding(1, 1), finding(2, 2)], "abc".into()),
            &ctx,
        );
        assert_eq!(app.handle_key(key(KeyCode::Char('s'))), AppAction::Continue);
        assert_eq!(*app.session.decision(0).unwrap(), Decision::Skip);
        assert_eq!(app.session.cursor(), 1);
        assert_eq!(
            app.handle_key(ctrl(KeyCode::Char('p'))),
            AppAction::Continue
        );
        assert_eq!(app.session.cursor(), 0);
        assert_eq!(
            app.handle_key(ctrl(KeyCode::Char('n'))),
            AppAction::Continue
        );
        assert_eq!(app.session.cursor(), 1);
        assert_eq!(app.handle_key(key(KeyCode::Char('q'))), AppAction::Quit);
    }

    #[test]
    fn deciding_everything_leads_to_confirm() {
        let (ctx, _dir) = ctx_for(finding(1, 1));
        let mut app = PortApp::new(
            Session::new(vec![finding(1, 1), finding(2, 2)], "abc".into()),
            &ctx,
        );
        app.handle_key(key(KeyCode::Char('s')));
        app.handle_key(key(KeyCode::Char('s')));
        assert_eq!(app.mode, Mode::Confirm);
        assert_eq!(app.handle_key(key(KeyCode::Char('y'))), AppAction::Execute);
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
    fn selection_editing_moves_top_and_bottom() {
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
                Comment {
                    path: "a.rs".into(),
                    line: 3,
                    end_line: 3,
                    column: 0,
                    byte_range: 25..39,
                    text: "// note: c".into(),
                    language: "rust".into(),
                },
            ],
        };
        // Primary is the middle comment, selection covers all three.
        let f = Finding {
            run,
            category: "TODO".into(),
            primary: 1,
            selection: Selection::full(3),
        };
        let (ctx, _dir) = ctx_for(finding(1, 1));
        let mut app = PortApp::new(Session::new(vec![f], "abc".into()), &ctx);
        app.mode = Mode::SelectionEdit;

        // Grow and shrink in both directions.
        app.handle_key(key(KeyCode::Char('[')));
        let sel = app.current().unwrap().selection;
        assert_eq!((sel.start, sel.end), (0, 2), "grow up at the run edge");

        app.handle_key(key(KeyCode::Char('}')));
        let sel = app.current().unwrap().selection;
        assert_eq!((sel.start, sel.end), (0, 1), "shrink down");

        app.handle_key(key(KeyCode::Char('{')));
        let sel = app.current().unwrap().selection;
        assert_eq!((sel.start, sel.end), (1, 1), "shrink up to the primary");

        // The primary never leaves the selection.
        app.handle_key(key(KeyCode::Char('{')));
        let sel = app.current().unwrap().selection;
        assert_eq!((sel.start, sel.end), (1, 1));

        app.handle_key(key(KeyCode::Char(']')));
        let sel = app.current().unwrap().selection;
        assert_eq!((sel.start, sel.end), (1, 2), "grow down");

        app.handle_key(key(KeyCode::Char('r')));
        let sel = app.current().unwrap().selection;
        assert_eq!((sel.start, sel.end), (0, 2), "reset");

        // Back to review.
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.mode, Mode::Review);
    }

    #[test]
    fn wrap_toggle_flips_and_resets_h_scroll() {
        let (ctx, _dir) = ctx_for(finding(1, 1));
        let mut app = PortApp::new(Session::new(vec![finding(1, 1)], "abc".into()), &ctx);
        assert!(app.wrap, "wrap defaults on");
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.h_scroll, 1);
        app.handle_key(key(KeyCode::Char('w')));
        assert!(!app.wrap);
        assert_eq!(app.h_scroll, 1, "turning wrap off keeps the pan");
        app.handle_key(key(KeyCode::Char('w')));
        assert!(app.wrap);
        assert_eq!(app.h_scroll, 0, "turning wrap on resets the pan");
    }

    #[test]
    fn scroll_keys_move_and_clamp() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let mut content = String::new();
        // Long lines so the (fill-sized) window wraps beyond the pane; the
        // comment sits mid-file so the window is not clamped by the edges.
        for i in 1..=99 {
            content.push_str(&format!("line {i} {}\n", "word ".repeat(40)));
        }
        content.push_str("// TODO: x\n");
        for i in 101..=200 {
            content.push_str(&format!("line {i} {}\n", "word ".repeat(40)));
        }
        std::fs::write(dir.path().join("src/a.rs"), content).unwrap();
        let repo = todone_core::repo::RepoInfo {
            root: dir.path().to_path_buf(),
            commit: None,
            is_repo: true,
            remote: None,
        };
        let ctx = ScanContext {
            config: todone_core::config::Config::defaults(),
            repo,
        };
        let mut app = PortApp::new(Session::new(vec![finding(100, 100)], "abc".into()), &ctx);

        // Render once so the pane size is known.
        let backend = TestBackend::new(60, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let (_, _, total) = app.view(app.pane_height, app.pane_width);
        assert!(total > app.pane_height, "wrapped window must overflow");
        let max = total - app.pane_height;

        // g goes to the top; the finding is below the fold, so j scrolls.
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.v_scroll, 0);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.v_scroll, 1);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.v_scroll, 0);

        // G jumps to the bottom and clamps.
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.v_scroll, max);

        // Ctrl-f pages down (clamped), Ctrl-b back to the top.
        app.handle_key(key(KeyCode::Char('g')));
        app.handle_key(ctrl(KeyCode::Char('f')));
        assert_eq!(app.v_scroll, app.pane_height);
        app.handle_key(ctrl(KeyCode::Char('d')));
        assert_eq!(app.v_scroll, app.pane_height + app.pane_height / 2);
        app.handle_key(ctrl(KeyCode::Char('b')));
        assert_eq!(app.v_scroll, app.pane_height / 2);
        app.handle_key(ctrl(KeyCode::Char('b')));
        assert_eq!(app.v_scroll, 0);

        // z centers on the selected comment.
        app.handle_key(ctrl(KeyCode::Char('f')));
        app.handle_key(key(KeyCode::Char('z')));
        assert!(app.v_scroll > 0);
        assert!(app.v_scroll <= max);
    }

    #[test]
    fn context_spans_the_whole_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        let mut content = String::new();
        for i in 1..=200 {
            content.push_str(&format!("before line {i}\n"));
        }
        content.push_str("// TODO: x\n");
        for i in 202..=400 {
            content.push_str(&format!("after line {i}\n"));
        }
        std::fs::write(dir.path().join("src/a.rs"), content).unwrap();
        let repo = todone_core::repo::RepoInfo {
            root: dir.path().to_path_buf(),
            commit: None,
            is_repo: true,
            remote: None,
        };
        let ctx = ScanContext {
            config: todone_core::config::Config::defaults(),
            repo,
        };
        let mut app = PortApp::new(Session::new(vec![finding(201, 201)], "abc".into()), &ctx);

        // The whole file is navigable: line 1 and line 400 are in the window.
        let context = app.context_for_finding(app.session.cursor());
        assert_eq!(context.lines.len(), 400);
        assert_eq!(context.lines.first().unwrap().line, 1);
        assert_eq!(context.lines.last().unwrap().line, 400);

        // Scrolling reaches the very bottom of the file.
        let backend = TestBackend::new(80, 30);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        app.handle_key(key(KeyCode::Char('G')));
        let max = app.max_scroll();
        let (context, _, _) = app.view(app.pane_height, app.pane_width);
        let last = context.lines.last().unwrap().line;
        assert_eq!(last, 400);
        assert_eq!(app.v_scroll, max);
    }

    #[test]
    fn tabs_are_expanded_in_the_pane() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/a.rs"), "\tlet x = 1;\n// TODO: x\n").unwrap();
        let repo = todone_core::repo::RepoInfo {
            root: dir.path().to_path_buf(),
            commit: None,
            is_repo: true,
            remote: None,
        };
        let ctx = ScanContext {
            config: todone_core::config::Config::defaults(),
            repo,
        };
        let mut app = PortApp::new(Session::new(vec![finding(2, 2)], "abc".into()), &ctx);
        assert_eq!(app.tab_width, 4, "tab width defaults to 4");

        let backend = TestBackend::new(80, 20);
        let mut terminal = ratatui::Terminal::new(backend).unwrap();
        terminal.draw(|frame| render(frame, &mut app)).unwrap();
        let content = buffer_to_string(terminal.backend().buffer());
        assert!(
            content.contains("    let x = 1;"),
            "tab must expand to four spaces:\n{content}"
        );
        assert!(!content.contains('\t'), "no raw tabs in the buffer");
    }

    #[test]
    fn tab_width_comes_from_config() {
        let (mut ctx, _dir) = ctx_for(finding(1, 1));
        ctx.config.context.tab_width = 8;
        let app = PortApp::new(Session::new(vec![finding(1, 1)], "abc".into()), &ctx);
        assert_eq!(app.tab_width, 8);
    }

    #[test]
    fn snippet_excludes_the_comment_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(
            dir.path().join("src/a.rs"),
            "fn before() {}\n// TODO: x\nfn after() {}\n",
        )
        .unwrap();
        let repo = todone_core::repo::RepoInfo {
            root: dir.path().to_path_buf(),
            commit: Some("abc".into()),
            is_repo: true,
            remote: None,
        };
        let ctx = ScanContext {
            config: todone_core::config::Config::defaults(),
            repo,
        };
        let run = CommentRun {
            comments: vec![Comment {
                path: "src/a.rs".into(),
                line: 2,
                end_line: 2,
                column: 0,
                byte_range: 15..25,
                text: "// TODO: x".into(),
                language: "rust".into(),
            }],
        };
        let finding = Finding {
            run,
            category: "TODO".into(),
            primary: 0,
            selection: Selection::full(1),
        };
        let mut app = PortApp::new(Session::new(vec![finding], "abc".into()), &ctx);
        let snippet = app.current_snippet().unwrap();
        assert!(snippet.text.contains("fn before() {}"));
        assert!(snippet.text.contains("fn after() {}"));
        assert!(!snippet.text.contains("// TODO: x"));
    }

    #[test]
    fn rendered_rows_estimates_wrapping() {
        assert_eq!(rendered_rows("short line", 80, true, 4), 1);
        assert_eq!(rendered_rows("short line", 80, false, 4), 1);
        // Two words that do not fit on one line.
        assert!(rendered_rows("aaaaaaaaaa bbbbbbbbbb", 10, true, 4) > 1);
        // A word longer than the pane spans multiple rows.
        assert!(rendered_rows("aaaaaaaaaaaaaaaaaaaa", 8, true, 4) >= 3);
        // Empty text still occupies a row.
        assert_eq!(rendered_rows("", 80, true, 4), 1);
        // Tabs expand before wrapping: a tab is tab_width columns.
        assert_eq!(rendered_rows("\tword", 80, true, 4), 1);
        assert!(rendered_rows("\taaaaaaaaaa", 8, true, 4) > 1);
        assert_eq!(rendered_rows("\tword", 80, true, 2), 1);
    }

    #[test]
    fn fit_status_truncates_the_left_side() {
        assert_eq!(fit_status("ab", "cd", 6), "ab  cd");
        let fitted = fit_status("a very long left side", "right", 12);
        assert_eq!(fitted.chars().count(), 12);
        assert!(fitted.ends_with("right"));
        assert!(fitted.contains('…'));
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
