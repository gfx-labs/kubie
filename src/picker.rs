//! Interactive TUI picker with fuzzy search, tabbed source filtering, preview pane,
//! match highlighting, and background item streaming.

use std::collections::HashSet;
use std::io;
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::{cursor, execute};
use nucleo_matcher::pattern::{AtomKind, CaseMatching, Normalization, Pattern};
use nucleo_matcher::{Config, Matcher};
use ratatui::prelude::*;
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph, Tabs};

use crate::frecency::FrecencyDb;

// ---------------------------------------------------------------------------
// Public item types
// ---------------------------------------------------------------------------

/// A selectable item in the picker.
#[derive(Clone, Debug)]
pub struct PickerItem {
    /// The value returned on selection (e.g. context name).
    pub value: String,
    /// Display text shown in the list (may differ from value).
    pub display: String,
    /// Source/group label for tab filtering and display tag (e.g. "do-prod", "my-rancher", "kubeconfig").
    pub source: String,
    /// Provider type used for color mapping (e.g. "digitalocean", "gke", "rancher", "kubeconfig").
    pub provider_type: String,
    /// Preview lines shown in the right pane when this item is highlighted.
    pub preview: Vec<PreviewLine>,
}

/// A single line in the preview pane. Values may contain `\n` for multiline display.
#[derive(Clone, Debug)]
pub struct PreviewLine {
    pub label: String,
    pub value: String,
}

// ---------------------------------------------------------------------------
// Picker state
// ---------------------------------------------------------------------------

/// A filtered item with its match highlights.
struct FilteredEntry {
    /// Index into `items`.
    item_idx: usize,
    /// Character positions in the display (cluster name) that matched the query.
    match_positions: Vec<u32>,
}

/// Parse the query into an optional `@source` filter and the remaining name query.
/// Examples:
///   "prod"      -> (None, "prod")
///   "@do"       -> (Some("do"), "")
///   "@do prod"  -> (Some("do"), "prod")
///   "@gke test" -> (Some("gke"), "test")
fn parse_query(query: &str) -> (Option<&str>, &str) {
    let trimmed = query.trim_start();
    if let Some(rest) = trimmed.strip_prefix('@') {
        // Split at the first space after the @token.
        if let Some(space_idx) = rest.find(' ') {
            let source_query = rest[..space_idx].trim();
            let name_query = rest[space_idx + 1..].trim_start();
            if source_query.is_empty() {
                (None, name_query)
            } else {
                (Some(source_query), name_query)
            }
        } else {
            // Just "@something" with no space -- entire thing is source filter.
            let source_query = rest.trim();
            if source_query.is_empty() {
                (None, "")
            } else {
                (Some(source_query), "")
            }
        }
    } else {
        (None, trimmed)
    }
}

struct PickerState {
    items: Vec<PickerItem>,
    frecency_scores: Vec<f64>,
    frecency: FrecencyDb,
    query: String,
    cursor: usize,
    filtered: Vec<FilteredEntry>,
    selected: usize,
    tabs: Vec<String>,
    active_tab: usize,
    scroll_offset: usize,
    /// Channel for receiving new items from background threads.
    rx: Option<mpsc::Receiver<Vec<PickerItem>>>,
    /// True while waiting for the first batch of items from the background channel.
    loading: bool,
    /// Animation frame counter for the loading spinner.
    spinner_tick: usize,
    /// Preview pane width percentage (0 = disabled, 1-80).
    preview_width: u16,
    /// Minimum terminal columns to show preview.
    preview_min: u16,
}

impl PickerState {
    fn new(
        items: Vec<PickerItem>,
        frecency: FrecencyDb,
        rx: Option<mpsc::Receiver<Vec<PickerItem>>>,
        picker_settings: &crate::settings::Picker,
    ) -> Self {
        let frecency_scores: Vec<f64> = items.iter().map(|item| frecency.score(&item.value)).collect();
        let tabs = build_tabs(&items);

        let mut indices: Vec<usize> = (0..items.len()).collect();
        indices.sort_by(|&a, &b| {
            frecency_scores[b]
                .partial_cmp(&frecency_scores[a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let filtered = indices
            .into_iter()
            .map(|i| FilteredEntry {
                item_idx: i,
                match_positions: Vec::new(),
            })
            .collect();

        let loading = items.is_empty() && rx.is_some();

        PickerState {
            items,
            frecency_scores,
            frecency,
            query: String::new(),
            cursor: 0,
            filtered,
            selected: 0,
            tabs,
            active_tab: 0,
            scroll_offset: 0,
            rx,
            loading,
            spinner_tick: 0,
            preview_width: picker_settings.preview.width.min(80),
            preview_min: picker_settings.preview.min,
        }
    }

    /// Drain any new items from the background channel. Returns true if items were added.
    fn drain_incoming(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        let mut added = false;
        loop {
            match rx.try_recv() {
                Ok(batch) => {
                    for item in batch {
                        if self.items.iter().any(|existing| existing.value == item.value) {
                            continue;
                        }
                        let score = self.frecency.score(&item.value);
                        self.items.push(item);
                        self.frecency_scores.push(score);
                        added = true;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    // Sender dropped -- loading is done.
                    self.loading = false;
                    self.rx = None;
                    break;
                }
            }
        }
        if added {
            self.loading = false;
            self.tabs = build_tabs(&self.items);
        }
        added
    }

    fn refilter(&mut self, matcher: &mut Matcher) {
        let tab_filter = if self.active_tab == 0 {
            None
        } else {
            self.tabs.get(self.active_tab).map(|s| s.as_str())
        };

        let (source_query, name_query) = parse_query(&self.query);

        let has_name_query = !name_query.is_empty();
        let has_source_query = source_query.is_some();

        if !has_name_query && !has_source_query {
            // No query at all -- show everything filtered by tab, sorted by frecency.
            let mut indices: Vec<usize> = (0..self.items.len())
                .filter(|&i| tab_filter.is_none() || self.items[i].source == tab_filter.unwrap())
                .collect();
            indices.sort_by(|&a, &b| {
                self.frecency_scores[b]
                    .partial_cmp(&self.frecency_scores[a])
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            self.filtered = indices
                .into_iter()
                .map(|i| FilteredEntry {
                    item_idx: i,
                    match_positions: Vec::new(),
                })
                .collect();
        } else {
            let name_pattern = if has_name_query {
                Some(Pattern::new(
                    name_query,
                    CaseMatching::Ignore,
                    Normalization::Smart,
                    AtomKind::Fuzzy,
                ))
            } else {
                None
            };
            let source_pattern =
                source_query.map(|sq| Pattern::new(sq, CaseMatching::Ignore, Normalization::Smart, AtomKind::Fuzzy));

            let mut scored: Vec<(usize, u32, Vec<u32>)> = self
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| tab_filter.is_none() || item.source == tab_filter.unwrap())
                .filter_map(|(i, item)| {
                    let mut total_score: u32 = 0;

                    // Source filtering (if @query provided). Must match to be included.
                    if let Some(ref sp) = source_pattern {
                        let mut buf = Vec::new();
                        let hay = nucleo_matcher::Utf32Str::new(&item.source, &mut buf);
                        let score = sp.score(hay, matcher)?;
                        total_score += score;
                    }

                    // Name matching (default: fuzzy against display name).
                    let mut name_positions = Vec::new();
                    if let Some(ref np) = name_pattern {
                        let mut buf = Vec::new();
                        let hay = nucleo_matcher::Utf32Str::new(&item.display, &mut buf);
                        let score = np.score(hay, matcher)?;
                        total_score += score;

                        np.indices(hay, matcher, &mut name_positions);
                        name_positions.sort_unstable();
                        name_positions.dedup();
                    }

                    Some((i, total_score, name_positions))
                })
                .collect();

            scored.sort_by_key(|b| std::cmp::Reverse(b.1));
            self.filtered = scored
                .into_iter()
                .map(|(i, _, name_pos)| FilteredEntry {
                    item_idx: i,
                    match_positions: name_pos,
                })
                .collect();
        }

        self.selected = 0;
        self.scroll_offset = 0;
    }

    fn move_up(&mut self) {
        if !self.filtered.is_empty() && self.selected > 0 {
            self.selected -= 1;
        }
    }

    fn move_down(&mut self) {
        if !self.filtered.is_empty() && self.selected < self.filtered.len() - 1 {
            self.selected += 1;
        }
    }

    fn tab_left(&mut self) {
        if self.active_tab > 0 {
            self.active_tab -= 1;
        } else {
            self.active_tab = self.tabs.len() - 1;
        }
    }

    fn tab_right(&mut self) {
        if self.active_tab < self.tabs.len() - 1 {
            self.active_tab += 1;
        } else {
            self.active_tab = 0;
        }
    }

    fn selected_entry(&self) -> Option<&FilteredEntry> {
        self.filtered.get(self.selected)
    }

    fn selected_item(&self) -> Option<&PickerItem> {
        self.selected_entry().map(|e| &self.items[e.item_idx])
    }

    fn insert_char(&mut self, c: char) {
        self.query.insert(self.cursor, c);
        self.cursor += c.len_utf8();
    }

    fn delete_char_before(&mut self) {
        if self.cursor > 0 {
            let prev = self.query[..self.cursor]
                .chars()
                .last()
                .map(|c| c.len_utf8())
                .unwrap_or(0);
            self.cursor -= prev;
            self.query.remove(self.cursor);
        }
    }

    fn delete_char_after(&mut self) {
        if self.cursor < self.query.len() {
            self.query.remove(self.cursor);
        }
    }

    fn clear_query(&mut self) {
        self.query.clear();
        self.cursor = 0;
    }
}

fn build_tabs(items: &[PickerItem]) -> Vec<String> {
    let mut sources: Vec<String> = Vec::new();
    for item in items {
        if !item.source.is_empty() && !sources.contains(&item.source) {
            sources.push(item.source.clone());
        }
    }
    let mut tabs = vec!["all".to_string()];
    if sources.len() > 1 {
        tabs.extend(sources);
    }
    tabs
}

const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(frame: &mut Frame, state: &mut PickerState) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let show_tabs = state.tabs.len() > 1;

    let mut constraints = Vec::new();
    if show_tabs {
        constraints.push(Constraint::Length(1)); // tabs
    }
    constraints.push(Constraint::Length(1)); // input
    constraints.push(Constraint::Min(3)); // main

    let vertical = Layout::vertical(constraints);
    let areas = vertical.split(area);
    let mut area_idx = 0;

    // Tab bar (above input).
    if show_tabs {
        let tab_area = areas[area_idx];
        area_idx += 1;
        let tab_titles: Vec<&str> = state.tabs.iter().map(|s| s.as_str()).collect();
        let tabs_widget = Tabs::new(tab_titles)
            .select(state.active_tab)
            .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
            .divider("│")
            .padding(" ", " ");
        frame.render_widget(tabs_widget, tab_area);
    }

    // Input line.
    let input_area = areas[area_idx];
    area_idx += 1;
    let input_line = Line::from(vec![
        Span::styled(" > ", Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)),
        Span::styled(state.query.as_str(), Style::default().fg(Color::White)),
    ]);
    frame.render_widget(Paragraph::new(input_line), input_area);
    let cursor_x = input_area.x + 3 + state.query[..state.cursor].chars().count() as u16;
    frame.set_cursor_position((cursor_x, input_area.y));

    // Main area.
    let main_area = areas[area_idx];
    let has_preview = state.preview_width > 0
        && area.width >= state.preview_min
        && state.selected_item().is_some_and(|item| !item.preview.is_empty());
    if has_preview {
        let list_pct = 100 - state.preview_width;
        let horizontal = Layout::horizontal([
            Constraint::Percentage(list_pct),
            Constraint::Percentage(state.preview_width),
        ]);
        let [list_area, preview_area] = horizontal.areas(main_area);
        render_list(frame, state, list_area);
        render_preview(frame, state, preview_area);
    } else {
        render_list(frame, state, main_area);
    }
}

fn render_list(frame: &mut Frame, state: &mut PickerState, area: Rect) {
    // Show loading indicator when waiting for items.
    if state.loading && state.filtered.is_empty() {
        let spinner = SPINNER_FRAMES[state.spinner_tick % SPINNER_FRAMES.len()];
        let loading_line = Line::from(vec![
            Span::styled(format!("  {spinner} "), Style::default().fg(Color::Cyan)),
            Span::styled("loading...", Style::default().fg(Color::DarkGray)),
        ]);
        frame.render_widget(Paragraph::new(loading_line), area);
        return;
    }

    let visible_height = area.height.saturating_sub(1) as usize; // -1 for the count line at bottom

    if state.selected < state.scroll_offset {
        state.scroll_offset = state.selected;
    } else if state.selected >= state.scroll_offset + visible_height {
        state.scroll_offset = state.selected - visible_height + 1;
    }

    let items: Vec<ListItem> = state
        .filtered
        .iter()
        .skip(state.scroll_offset)
        .take(visible_height)
        .enumerate()
        .map(|(vi, entry)| {
            let item = &state.items[entry.item_idx];
            let is_selected = vi + state.scroll_offset == state.selected;

            let source_clr = source_color(&item.provider_type);
            let has_source = !item.source.is_empty();

            // Build display name spans with match highlighting.
            let name_spans = build_highlighted_spans(&item.display, &entry.match_positions, is_selected);

            let mut spans = Vec::new();
            // Selection indicator.
            if is_selected {
                spans.push(Span::styled("▸ ", Style::default().fg(Color::Cyan)));
            } else {
                spans.push(Span::raw("  "));
            }
            // Source tag.
            if has_source {
                let padding = " ".repeat(14_usize.saturating_sub(item.source.len()));
                spans.push(Span::styled(item.source.as_str(), Style::default().fg(source_clr)));
                spans.push(Span::raw(padding));
            }
            spans.extend(name_spans);

            ListItem::new(Line::from(spans))
        })
        .collect();

    let count_text = format!(" {}/{} ", state.filtered.len(), state.items.len());
    let list_block = Block::default().title_bottom(Line::from(count_text).right_aligned());

    frame.render_widget(List::new(items).block(list_block), area);
}

/// Build spans for a display string with matched character positions highlighted.
fn build_highlighted_spans<'a>(display: &'a str, positions: &[u32], is_selected: bool) -> Vec<Span<'a>> {
    let base_style = if is_selected {
        Style::default().fg(Color::White).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    let highlight_style = Style::default()
        .fg(Color::Yellow)
        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED);

    if positions.is_empty() {
        return vec![Span::styled(display, base_style)];
    }

    let pos_set: HashSet<u32> = positions.iter().copied().collect();
    let mut spans: Vec<Span<'_>> = Vec::new();
    let mut current_run = String::new();
    let mut current_is_match = false;

    for (char_idx, ch) in display.chars().enumerate() {
        let is_match = pos_set.contains(&(char_idx as u32));
        if is_match != current_is_match && !current_run.is_empty() {
            let style = if current_is_match { highlight_style } else { base_style };
            spans.push(Span::styled(std::mem::take(&mut current_run), style));
        }
        current_is_match = is_match;
        current_run.push(ch);
    }
    if !current_run.is_empty() {
        let style = if current_is_match { highlight_style } else { base_style };
        spans.push(Span::styled(current_run, style));
    }

    spans
}

fn render_preview(frame: &mut Frame, state: &PickerState, area: Rect) {
    // Use a thin left-side separator instead of a full border box.
    let preview_block = Block::default()
        .borders(Borders::LEFT)
        .border_style(Style::default().fg(Color::DarkGray));

    let inner = preview_block.inner(area);
    frame.render_widget(preview_block, area);

    let Some(item) = state.selected_item() else { return };

    let label_style = Style::default().fg(Color::DarkGray).add_modifier(Modifier::BOLD);
    let value_style = Style::default().fg(Color::White);

    let mut lines: Vec<Line> = Vec::new();
    for pline in &item.preview {
        let label_text = format!(" {:>9} ", format!("{}:", pline.label.to_lowercase()));
        let indent_width = label_text.len();
        let value_lines: Vec<&str> = pline.value.split('\n').collect();
        if let Some((first, rest)) = value_lines.split_first() {
            lines.push(Line::from(vec![
                Span::styled(label_text, label_style),
                Span::styled(*first, value_style),
            ]));
            let indent = " ".repeat(indent_width);
            for continuation in rest {
                lines.push(Line::from(vec![
                    Span::raw(indent.clone()),
                    Span::styled(*continuation, value_style),
                ]));
            }
        }
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

fn source_color(source: &str) -> Color {
    match source {
        "digitalocean" => Color::Blue,
        "gke" => Color::Cyan,
        "eks" => Color::Yellow,
        "aks" => Color::LightBlue,
        "rancher" => Color::Green,
        "kubeconfig" => Color::DarkGray,
        _ => Color::Magenta,
    }
}

// ---------------------------------------------------------------------------
// Main entry points
// ---------------------------------------------------------------------------

/// Run the interactive picker and return the selected item's value, or None if cancelled.
///
/// Items are displayed immediately. If `rx` is provided, new items arriving on
/// the channel will be merged into the list in real time (for background sync).
pub fn pick(
    items: Vec<PickerItem>,
    rx: Option<mpsc::Receiver<Vec<PickerItem>>>,
    picker_settings: &crate::settings::Picker,
) -> Result<Option<String>> {
    if items.is_empty() && rx.is_none() {
        anyhow::bail!("No items to pick from.");
    }

    // If only one item and no background channel, return immediately.
    if items.len() == 1 && rx.is_none() {
        return Ok(Some(items[0].value.clone()));
    }

    let frecency = FrecencyDb::load();
    let mut state = PickerState::new(items, frecency, rx, picker_settings);
    let mut matcher = Matcher::new(Config::DEFAULT);

    terminal::enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, cursor::Show)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_picker(&mut terminal, &mut state, &mut matcher);

    terminal::disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;

    if let Ok(Some(ref value)) = result {
        let mut frecency = FrecencyDb::load();
        frecency.record(value);
        frecency.save();
    }

    result
}

fn run_picker(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: &mut PickerState,
    matcher: &mut Matcher,
) -> Result<Option<String>> {
    loop {
        // Drain any new items from background threads.
        if state.drain_incoming() {
            state.refilter(matcher);
        }

        // Advance spinner animation.
        if state.loading {
            state.spinner_tick = state.spinner_tick.wrapping_add(1);
        }

        terminal.draw(|frame| render(frame, state))?;

        // Poll with a short timeout so we can check for new items and animate spinner.
        if event::poll(Duration::from_millis(80))? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key {
                    KeyEvent { code: KeyCode::Esc, .. }
                    | KeyEvent {
                        code: KeyCode::Char('c'),
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    } => {
                        return Ok(None);
                    }

                    KeyEvent {
                        code: KeyCode::Enter, ..
                    } => {
                        return Ok(state.selected_item().map(|item| item.value.clone()));
                    }

                    KeyEvent { code: KeyCode::Up, .. }
                    | KeyEvent {
                        code: KeyCode::Char('k'),
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    }
                    | KeyEvent {
                        code: KeyCode::Char('p'),
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    } => {
                        state.move_up();
                    }

                    KeyEvent {
                        code: KeyCode::Down, ..
                    }
                    | KeyEvent {
                        code: KeyCode::Char('j'),
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    }
                    | KeyEvent {
                        code: KeyCode::Char('n'),
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    } => {
                        state.move_down();
                    }

                    KeyEvent {
                        code: KeyCode::Left, ..
                    }
                    | KeyEvent {
                        code: KeyCode::BackTab, ..
                    } => {
                        state.tab_left();
                        state.refilter(matcher);
                    }

                    KeyEvent {
                        code: KeyCode::Right, ..
                    }
                    | KeyEvent { code: KeyCode::Tab, .. } => {
                        state.tab_right();
                        state.refilter(matcher);
                    }

                    KeyEvent {
                        code: KeyCode::Char('u'),
                        modifiers: KeyModifiers::CONTROL,
                        ..
                    } => {
                        state.clear_query();
                        state.refilter(matcher);
                    }

                    KeyEvent {
                        code: KeyCode::Backspace,
                        ..
                    } => {
                        state.delete_char_before();
                        state.refilter(matcher);
                    }

                    KeyEvent {
                        code: KeyCode::Delete, ..
                    } => {
                        state.delete_char_after();
                        state.refilter(matcher);
                    }

                    KeyEvent {
                        code: KeyCode::Char(c),
                        modifiers: KeyModifiers::NONE | KeyModifiers::SHIFT,
                        ..
                    } => {
                        state.insert_char(c);
                        state.refilter(matcher);
                    }

                    _ => {}
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Convenience constructors
// ---------------------------------------------------------------------------

/// Build a PickerItem from a plain string (for simple namespace lists).
pub fn simple_item(name: &str) -> PickerItem {
    PickerItem {
        value: name.to_string(),
        display: name.to_string(),
        source: String::new(),
        provider_type: String::new(),
        preview: Vec::new(),
    }
}

/// Build a PickerItem for a local kubeconfig context.
pub fn local_context_item(
    context_name: &str,
    cluster_name: &str,
    server: &str,
    namespace: Option<&str>,
    source_file: &std::path::Path,
) -> PickerItem {
    let mut preview = vec![
        PreviewLine {
            label: "Source".into(),
            value: "kubeconfig".into(),
        },
        PreviewLine {
            label: "Context".into(),
            value: context_name.into(),
        },
        PreviewLine {
            label: "Cluster".into(),
            value: cluster_name.into(),
        },
    ];
    if !server.is_empty() {
        preview.push(PreviewLine {
            label: "Server".into(),
            value: server.into(),
        });
    }
    if let Some(ns) = namespace {
        preview.push(PreviewLine {
            label: "Namespace".into(),
            value: ns.into(),
        });
    }
    preview.push(PreviewLine {
        label: "File".into(),
        value: source_file.display().to_string(),
    });

    PickerItem {
        value: context_name.to_string(),
        display: context_name.to_string(),
        source: "kubeconfig".to_string(),
        provider_type: "kubeconfig".to_string(),
        preview,
    }
}

/// Build a PickerItem for any provider-discovered cluster/context.
pub fn provider_context_item(cluster: &crate::providers::ClusterInfo) -> PickerItem {
    let mut preview = vec![
        PreviewLine {
            label: "Provider".into(),
            value: cluster.provider.clone(),
        },
        PreviewLine {
            label: "Account".into(),
            value: cluster.account.clone(),
        },
        PreviewLine {
            label: "Cluster".into(),
            value: cluster.name.clone(),
        },
    ];

    for field in &cluster.metadata {
        preview.push(PreviewLine {
            label: field.label.clone(),
            value: field.value.clone(),
        });
    }

    PickerItem {
        value: cluster.context_name.clone(),
        display: cluster.name.clone(),
        source: cluster.account.clone(),
        provider_type: cluster.provider.clone(),
        preview,
    }
}
