use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    Tabs,
};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, Clone)]
struct ClusterHealth {
    cluster_name: String,
    status: String,
}

#[derive(Debug, Deserialize, Clone)]
struct IndexEntry {
    health: String,
    #[serde(rename = "index")]
    name: String,
    #[serde(rename = "docs.count")]
    docs_count: String,
}

#[derive(Debug, Deserialize, Clone)]
struct AliasEntry {
    alias: String,
    #[serde(rename = "index")]
    index_name: String,
}

#[derive(Debug, Deserialize, Clone)]
struct DataStreamResponse {
    #[serde(default)]
    data_streams: Vec<DataStreamEntry>,
}

#[derive(Debug, Deserialize, Clone)]
struct DataStreamEntry {
    name: String,
    status: Option<String>,
    generation: Option<u64>,
    indices: Option<Vec<DataStreamIndex>>,
}

#[derive(Debug, Deserialize, Clone)]
struct DataStreamIndex {
    #[serde(rename = "index_name")]
    #[allow(dead_code)]
    name: String,
}

#[derive(Debug, Clone)]
struct SavedView {
    name: String,
    scope: String,
    query: String,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    took: Option<u64>,
    timed_out: Option<bool>,
    #[serde(rename = "_shards")]
    shards: Option<SearchShards>,
    hits: SearchHits,
}

#[derive(Debug, Deserialize)]
struct SearchShards {
    failed: u64,
}

#[derive(Debug, Deserialize)]
struct SearchHits {
    total: Option<SearchTotal>,
    hits: Vec<SearchHit>,
}

#[derive(Debug, Deserialize)]
struct SearchTotal {
    value: u64,
}

#[derive(Debug, Deserialize)]
struct SearchHit {
    #[serde(rename = "_id")]
    id: String,
    #[serde(rename = "_source")]
    source: Value,
}

#[derive(Debug, Clone)]
struct DocEntry {
    id: String,
    source: Value,
}

#[derive(Debug, Clone)]
struct SearchSummary {
    total: Option<u64>,
    took: Option<u64>,
    shards_failed: Option<u64>,
    timed_out: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Focus {
    LeftNav,
    Results,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum InputMode {
    Normal,
    Query,
    ScopeFilter,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum ScopeKind {
    Indices,
    Aliases,
    DataStreams,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DocViewMode {
    Pretty,
    Raw,
    Flatten,
}

struct App {
    es_url: String,
    client: reqwest::blocking::Client,
    health: Option<ClusterHealth>,
    indices: Vec<IndexEntry>,
    aliases: Vec<AliasEntry>,
    datastreams: Vec<DataStreamEntry>,
    favorites: Vec<String>,
    saved_views: Vec<SavedView>,
    documents: Vec<DocEntry>,
    docs_total: Option<u64>,
    docs_from: u64,
    docs_size: u64,
    indices_state: ListState,
    aliases_state: ListState,
    datastreams_state: ListState,
    docs_state: TableState,
    focus: Focus,
    input_mode: InputMode,
    scope_kind: ScopeKind,
    scope_filter: String,
    scope_filter_edit: String,
    query: String,
    query_edit: String,
    show_doc_drawer: bool,
    doc_view_mode: DocViewMode,
    search_took_ms: Option<u64>,
    search_shards_failed: Option<u64>,
    search_timed_out: Option<bool>,
    last_error: Option<String>,
    last_fetch: Option<Instant>,
}

impl App {
    fn new(es_url: String) -> Self {
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(3))
            .build()
            .expect("failed to build http client");
        let mut indices_state = ListState::default();
        indices_state.select(None);
        let mut aliases_state = ListState::default();
        aliases_state.select(None);
        let mut datastreams_state = ListState::default();
        datastreams_state.select(None);
        let mut docs_state = TableState::default();
        docs_state.select(None);
        Self {
            es_url,
            client,
            health: None,
            indices: Vec::new(),
            aliases: Vec::new(),
            datastreams: Vec::new(),
            favorites: Vec::new(),
            saved_views: Vec::new(),
            documents: Vec::new(),
            docs_total: None,
            docs_from: 0,
            docs_size: 5,
            indices_state,
            aliases_state,
            datastreams_state,
            docs_state,
            focus: Focus::LeftNav,
            input_mode: InputMode::Normal,
            scope_kind: ScopeKind::Indices,
            scope_filter: String::new(),
            scope_filter_edit: String::new(),
            query: String::new(),
            query_edit: String::new(),
            show_doc_drawer: false,
            doc_view_mode: DocViewMode::Pretty,
            search_took_ms: None,
            search_shards_failed: None,
            search_timed_out: None,
            last_error: None,
            last_fetch: None,
        }
    }

    fn selected_scope_name(&self) -> Option<&str> {
        match self.scope_kind {
            ScopeKind::Indices => self
                .indices_state
                .selected()
                .and_then(|idx| self.indices.get(idx))
                .map(|entry| entry.name.as_str()),
            ScopeKind::Aliases => self
                .aliases_state
                .selected()
                .and_then(|idx| self.aliases.get(idx))
                .map(|entry| entry.alias.as_str()),
            ScopeKind::DataStreams => self
                .datastreams_state
                .selected()
                .and_then(|idx| self.datastreams.get(idx))
                .map(|entry| entry.name.as_str()),
        }
    }

    fn set_scope_kind(&mut self, scope: ScopeKind) {
        if self.scope_kind == scope {
            return;
        }
        self.scope_kind = scope;
        self.reset_docs_paging();
    }

    fn select_next_scope_item(&mut self) {
        self.shift_scope_selection(1);
    }

    fn select_prev_scope_item(&mut self) {
        self.shift_scope_selection(-1);
    }

    fn shift_scope_selection(&mut self, delta: isize) {
        let filtered = self.filtered_scope_indices();
        if filtered.is_empty() {
            self.set_scope_selected(None);
            return;
        }
        let current = self.scope_selected();
        let current_pos = current
            .and_then(|idx| filtered.iter().position(|value| *value == idx))
            .unwrap_or(0);
        let next_pos = if delta >= 0 {
            (current_pos + 1) % filtered.len()
        } else if current_pos == 0 {
            filtered.len() - 1
        } else {
            current_pos - 1
        };
        self.set_scope_selected(Some(filtered[next_pos]));
        self.reset_docs_paging();
    }

    fn select_next_doc(&mut self) {
        if self.documents.is_empty() {
            self.docs_state.select(None);
            return;
        }
        let next = match self.docs_state.selected() {
            Some(idx) if idx + 1 < self.documents.len() => idx + 1,
            _ => 0,
        };
        self.docs_state.select(Some(next));
    }

    fn select_prev_doc(&mut self) {
        if self.documents.is_empty() {
            self.docs_state.select(None);
            return;
        }
        let prev = match self.docs_state.selected() {
            Some(0) | None => self.documents.len() - 1,
            Some(idx) => idx - 1,
        };
        self.docs_state.select(Some(prev));
    }

    fn reset_docs_paging(&mut self) {
        self.docs_from = 0;
        self.docs_total = None;
        self.docs_state.select(None);
    }

    fn next_docs_page(&mut self) {
        if let Some(total) = self.docs_total {
            if self.docs_from + self.docs_size < total {
                self.docs_from += self.docs_size;
            }
        } else {
            self.docs_from += self.docs_size;
        }
    }

    fn prev_docs_page(&mut self) {
        if self.docs_from >= self.docs_size {
            self.docs_from -= self.docs_size;
        } else {
            self.docs_from = 0;
        }
    }

    fn scope_selected(&self) -> Option<usize> {
        match self.scope_kind {
            ScopeKind::Indices => self.indices_state.selected(),
            ScopeKind::Aliases => self.aliases_state.selected(),
            ScopeKind::DataStreams => self.datastreams_state.selected(),
        }
    }

    fn set_scope_selected(&mut self, idx: Option<usize>) {
        match self.scope_kind {
            ScopeKind::Indices => self.indices_state.select(idx),
            ScopeKind::Aliases => self.aliases_state.select(idx),
            ScopeKind::DataStreams => self.datastreams_state.select(idx),
        }
    }

    fn filtered_scope_indices(&self) -> Vec<usize> {
        let needle = self.scope_filter.trim().to_lowercase();
        match self.scope_kind {
            ScopeKind::Indices => filter_indices_by(&self.indices, &needle, |entry| &entry.name),
            ScopeKind::Aliases => filter_indices_by(&self.aliases, &needle, |entry| &entry.alias),
            ScopeKind::DataStreams => {
                filter_indices_by(&self.datastreams, &needle, |entry| &entry.name)
            }
        }
    }

    fn ensure_scope_selection_visible(&mut self) -> bool {
        let filtered = self.filtered_scope_indices();
        if filtered.is_empty() {
            if self.scope_selected().is_some() {
                self.set_scope_selected(None);
                return true;
            }
            return false;
        }
        if let Some(current) = self.scope_selected() {
            if filtered.iter().any(|idx| *idx == current) {
                return false;
            }
        }
        self.set_scope_selected(Some(filtered[0]));
        true
    }
}

fn main() -> Result<()> {
    let es_url = std::env::var("ES_URL").unwrap_or_else(|_| "http://localhost:9200".to_string());
    enable_raw_mode().context("failed to enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("failed to enter alternate screen")?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("failed to create terminal")?;

    let res = run_app(&mut terminal, App::new(es_url));

    disable_raw_mode().ok();
    execute!(terminal.backend_mut(), LeaveAlternateScreen).ok();
    terminal.show_cursor().ok();

    res
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, mut app: App) -> Result<()> {
    let tick_rate = Duration::from_millis(200);
    let refresh_interval = Duration::from_secs(10);
    let mut last_tick = Instant::now();
    refresh_all(&mut app);
    let mut last_refresh = Instant::now();

    loop {
        terminal.draw(|frame| ui(frame, &mut app))?;

        let timeout = tick_rate
            .checked_sub(last_tick.elapsed())
            .unwrap_or_else(|| Duration::from_secs(0));

        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                match app.input_mode {
                    InputMode::Normal => match key.code {
                        KeyCode::Char('q') => return Ok(()),
                        KeyCode::Char('r') => {
                            refresh_all(&mut app);
                            last_refresh = Instant::now();
                        }
                        KeyCode::Char('/') | KeyCode::Char('?') => {
                            app.input_mode = InputMode::Query;
                            app.query_edit = app.query.clone();
                        }
                        KeyCode::Char('f') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                            app.input_mode = InputMode::ScopeFilter;
                            app.scope_filter_edit = app.scope_filter.clone();
                        }
                        KeyCode::Tab => {
                            app.focus = match app.focus {
                                Focus::LeftNav => Focus::Results,
                                Focus::Results => Focus::LeftNav,
                            };
                        }
                        KeyCode::Char('1') => {
                            app.set_scope_kind(ScopeKind::Indices);
                            handle_scope_change(&mut app);
                        }
                        KeyCode::Char('2') => {
                            app.set_scope_kind(ScopeKind::Aliases);
                            handle_scope_change(&mut app);
                        }
                        KeyCode::Char('3') => {
                            app.set_scope_kind(ScopeKind::DataStreams);
                            handle_scope_change(&mut app);
                        }
                        KeyCode::Up => match app.focus {
                            Focus::LeftNav => {
                                app.select_prev_scope_item();
                                handle_scope_change(&mut app);
                            }
                            Focus::Results => app.select_prev_doc(),
                        },
                        KeyCode::Down => match app.focus {
                            Focus::LeftNav => {
                                app.select_next_scope_item();
                                handle_scope_change(&mut app);
                            }
                            Focus::Results => app.select_next_doc(),
                        },
                        KeyCode::Enter | KeyCode::Char('o') => {
                            if app.focus == Focus::Results {
                                app.show_doc_drawer = !app.show_doc_drawer;
                            }
                        }
                        KeyCode::Esc => {
                            if app.show_doc_drawer {
                                app.show_doc_drawer = false;
                            }
                        }
                        KeyCode::Char('d') => handle_docs_refresh(&mut app),
                        KeyCode::Char('n') => {
                            app.next_docs_page();
                            handle_docs_refresh(&mut app);
                        }
                        KeyCode::Char('p') => {
                            app.prev_docs_page();
                            handle_docs_refresh(&mut app);
                        }
                        KeyCode::Char('v') => {
                            if app.show_doc_drawer {
                                app.doc_view_mode = match app.doc_view_mode {
                                    DocViewMode::Pretty => DocViewMode::Raw,
                                    DocViewMode::Raw => DocViewMode::Flatten,
                                    DocViewMode::Flatten => DocViewMode::Pretty,
                                };
                            }
                        }
                        _ => {}
                    },
                    InputMode::Query => match key.code {
                        KeyCode::Esc => {
                            app.input_mode = InputMode::Normal;
                            app.query_edit.clear();
                        }
                        KeyCode::Enter => {
                            app.query = app.query_edit.trim().to_string();
                            app.input_mode = InputMode::Normal;
                            app.reset_docs_paging();
                            handle_docs_refresh(&mut app);
                        }
                        KeyCode::Backspace => {
                            app.query_edit.pop();
                        }
                        KeyCode::Char(ch) => {
                            app.query_edit.push(ch);
                        }
                        _ => {}
                    },
                    InputMode::ScopeFilter => match key.code {
                        KeyCode::Esc => {
                            app.scope_filter_edit.clear();
                            app.scope_filter.clear();
                            app.input_mode = InputMode::Normal;
                            if app.ensure_scope_selection_visible() {
                                handle_scope_change(&mut app);
                            }
                        }
                        KeyCode::Enter => {
                            app.scope_filter = app.scope_filter_edit.trim().to_string();
                            app.input_mode = InputMode::Normal;
                            if app.ensure_scope_selection_visible() {
                                handle_scope_change(&mut app);
                            }
                        }
                        KeyCode::Backspace => {
                            app.scope_filter_edit.pop();
                            app.scope_filter = app.scope_filter_edit.clone();
                        }
                        KeyCode::Char(ch) => {
                            app.scope_filter_edit.push(ch);
                            app.scope_filter = app.scope_filter_edit.clone();
                        }
                        _ => {}
                    },
                }
            }
        }

        if last_refresh.elapsed() >= refresh_interval {
            refresh_all(&mut app);
            last_refresh = Instant::now();
        }

        if last_tick.elapsed() >= tick_rate {
            last_tick = Instant::now();
        }
    }
}

fn refresh_all(app: &mut App) {
    let mut errors = Vec::new();

    if let Err(err) = refresh_health(app) {
        errors.push(format!("health: {err:#}"));
    }
    if let Err(err) = refresh_indices(app) {
        errors.push(format!("indices: {err:#}"));
    }
    if let Err(err) = refresh_aliases(app) {
        errors.push(format!("aliases: {err:#}"));
    }
    if let Err(err) = refresh_datastreams(app) {
        errors.push(format!("datastreams: {err:#}"));
    }
    if let Err(err) = refresh_docs(app) {
        errors.push(format!("docs: {err:#}"));
    }

    app.last_fetch = Some(Instant::now());
    if errors.is_empty() {
        app.last_error = None;
    } else {
        app.last_error = Some(errors.join(" | "));
    }
}

fn refresh_health(app: &mut App) -> Result<()> {
    let health = fetch_cluster_health(&app.client, &app.es_url)?;
    app.health = Some(health);
    Ok(())
}

fn refresh_indices(app: &mut App) -> Result<()> {
    let selected_name = app
        .indices_state
        .selected()
        .and_then(|idx| app.indices.get(idx))
        .map(|entry| entry.name.to_string());
    let indices = fetch_indices(&app.client, &app.es_url)?;
    app.indices = indices;

    let next_selected = if let Some(name) = selected_name {
        app.indices.iter().position(|entry| entry.name == name)
    } else {
        None
    };
    if app.indices.is_empty() {
        app.indices_state.select(None);
    } else if let Some(idx) = next_selected {
        app.indices_state.select(Some(idx));
    } else {
        app.indices_state.select(Some(0));
    }
    Ok(())
}

fn refresh_aliases(app: &mut App) -> Result<()> {
    let selected_name = app
        .aliases_state
        .selected()
        .and_then(|idx| app.aliases.get(idx))
        .map(|entry| entry.alias.to_string());
    let aliases = fetch_aliases(&app.client, &app.es_url)?;
    app.aliases = aliases;

    let next_selected = if let Some(name) = selected_name {
        app.aliases.iter().position(|entry| entry.alias == name)
    } else {
        None
    };
    if app.aliases.is_empty() {
        app.aliases_state.select(None);
    } else if let Some(idx) = next_selected {
        app.aliases_state.select(Some(idx));
    } else {
        app.aliases_state.select(Some(0));
    }
    Ok(())
}

fn refresh_datastreams(app: &mut App) -> Result<()> {
    let selected_name = app
        .datastreams_state
        .selected()
        .and_then(|idx| app.datastreams.get(idx))
        .map(|entry| entry.name.to_string());
    let datastreams = fetch_datastreams(&app.client, &app.es_url)?;
    app.datastreams = datastreams;

    let next_selected = if let Some(name) = selected_name {
        app.datastreams.iter().position(|entry| entry.name == name)
    } else {
        None
    };
    if app.datastreams.is_empty() {
        app.datastreams_state.select(None);
    } else if let Some(idx) = next_selected {
        app.datastreams_state.select(Some(idx));
    } else {
        app.datastreams_state.select(Some(0));
    }
    Ok(())
}

fn refresh_docs(app: &mut App) -> Result<()> {
    let Some(scope) = app.selected_scope_name().map(|name| name.to_string()) else {
        app.documents.clear();
        app.docs_total = None;
        app.search_took_ms = None;
        app.search_shards_failed = None;
        app.search_timed_out = None;
        app.docs_state.select(None);
        return Ok(());
    };
    let (docs, summary) = fetch_documents(
        &app.client,
        &app.es_url,
        &scope,
        app.docs_from,
        app.docs_size,
        &app.query,
    )?;
    app.documents = docs;
    app.docs_total = summary.total;
    app.search_took_ms = summary.took;
    app.search_shards_failed = summary.shards_failed;
    app.search_timed_out = summary.timed_out;
    if app.documents.is_empty() {
        app.docs_state.select(None);
    } else {
        let selected = app.docs_state.selected().unwrap_or(0);
        let bounded = selected.min(app.documents.len() - 1);
        app.docs_state.select(Some(bounded));
    }
    Ok(())
}

fn handle_docs_refresh(app: &mut App) {
    if let Err(err) = refresh_docs(app) {
        app.last_error = Some(format!("docs: {err:#}"));
    }
}

fn handle_scope_change(app: &mut App) {
    handle_docs_refresh(app);
}

fn fetch_cluster_health(client: &reqwest::blocking::Client, es_url: &str) -> Result<ClusterHealth> {
    let base = es_url.trim_end_matches('/');
    let url = format!("{base}/_cluster/health");
    let response = client
        .get(url)
        .send()
        .context("request failed")?
        .error_for_status()
        .context("http error")?;
    let health: ClusterHealth = response.json().context("invalid response json")?;
    Ok(health)
}

fn fetch_indices(client: &reqwest::blocking::Client, es_url: &str) -> Result<Vec<IndexEntry>> {
    let base = es_url.trim_end_matches('/');
    let url = format!("{base}/_cat/indices?format=json");
    let response = client
        .get(url)
        .send()
        .context("request failed")?
        .error_for_status()
        .context("http error")?;
    let indices: Vec<IndexEntry> = response.json().context("invalid response json")?;
    Ok(indices)
}

fn fetch_aliases(client: &reqwest::blocking::Client, es_url: &str) -> Result<Vec<AliasEntry>> {
    let base = es_url.trim_end_matches('/');
    let url = format!("{base}/_cat/aliases?format=json");
    let response = client
        .get(url)
        .send()
        .context("request failed")?
        .error_for_status()
        .context("http error")?;
    let aliases: Vec<AliasEntry> = response.json().context("invalid response json")?;
    Ok(aliases)
}

fn fetch_datastreams(
    client: &reqwest::blocking::Client,
    es_url: &str,
) -> Result<Vec<DataStreamEntry>> {
    let base = es_url.trim_end_matches('/');
    let url = format!("{base}/_data_stream");
    let response = client
        .get(url)
        .send()
        .context("request failed")?
        .error_for_status()
        .context("http error")?;
    let payload: DataStreamResponse = response.json().context("invalid response json")?;
    Ok(payload.data_streams)
}

fn fetch_documents(
    client: &reqwest::blocking::Client,
    es_url: &str,
    index: &str,
    from: u64,
    size: u64,
    query: &str,
) -> Result<(Vec<DocEntry>, SearchSummary)> {
    let base = es_url.trim_end_matches('/');
    let url = format!("{base}/{index}/_search?from={from}&size={size}");
    let query = query.trim();
    let body = if query.is_empty() {
        serde_json::json!({ "query": { "match_all": {} } })
    } else {
        serde_json::json!({
            "query": {
                "query_string": {
                    "query": query,
                    "default_operator": "AND"
                }
            }
        })
    };
    let response = client
        .post(url)
        .json(&body)
        .send()
        .context("request failed")?
        .error_for_status()
        .context("http error")?;
    let payload: SearchResponse = response.json().context("invalid response json")?;
    let total = payload.hits.total.map(|value| value.value);
    let shards_failed = payload.shards.map(|shards| shards.failed);
    let summary = SearchSummary {
        total,
        took: payload.took,
        shards_failed,
        timed_out: payload.timed_out,
    };
    let docs = payload
        .hits
        .hits
        .into_iter()
        .map(|hit| DocEntry {
            id: hit.id,
            source: hit.source,
        })
        .collect();
    Ok((docs, summary))
}


fn ui(frame: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(frame.size());

    render_top_bar(frame, chunks[0], app);

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(20), Constraint::Percentage(80)])
        .split(chunks[1]);

    render_left_nav(frame, body_chunks[0], app);
    render_right_main(frame, body_chunks[1], app);

    if app.show_doc_drawer {
        render_doc_drawer(frame, chunks[0].height, app);
    }
}

fn render_top_bar(frame: &mut ratatui::Frame, area: Rect, app: &App) {
    let label_style = Style::default().fg(Color::Gray);
    let cluster_name = app
        .health
        .as_ref()
        .map(|health| health.cluster_name.as_str())
        .unwrap_or("-");
    let cluster_style = app
        .health
        .as_ref()
        .map(|health| status_style(&health.status))
        .unwrap_or_else(|| Style::default().fg(Color::Gray));
    let auth = auth_label(&app.es_url);
    let scope = scope_label(app);
    let mode = "QueryString";
    let (status_text, status_style) = status_summary(app);

    let mut spans = Vec::new();
    spans.push(Span::styled("cluster:", label_style));
    spans.push(Span::raw(" "));
    spans.push(Span::styled(cluster_name, cluster_style));
    spans.push(Span::raw("  "));
    spans.push(Span::styled("auth:", label_style));
    spans.push(Span::raw(" "));
    spans.push(Span::raw(auth));
    spans.push(Span::raw("  "));
    spans.push(Span::styled("scope:", label_style));
    spans.push(Span::raw(" "));
    spans.push(Span::raw(scope));
    spans.push(Span::raw("  "));
    spans.push(Span::styled("mode:", label_style));
    spans.push(Span::raw(" "));
    spans.push(Span::raw(mode));
    spans.push(Span::raw("  "));
    spans.push(Span::styled(status_text, status_style));

    let header = Paragraph::new(Line::from(spans))
        .block(Block::default().borders(Borders::ALL).title("TopBar"));
    frame.render_widget(header, area);
}

fn render_left_nav(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(7),
            Constraint::Length(5),
            Constraint::Length(7),
        ])
        .split(area);

    let tabs = Tabs::new(vec![
        Line::from("Indices"),
        Line::from("Aliases"),
        Line::from("DataStreams"),
    ])
    .select(scope_tab_index(app.scope_kind))
    .highlight_style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD))
    .block(Block::default().borders(Borders::ALL).title("Scope"));
    frame.render_widget(tabs, chunks[0]);

    let filter_text = match app.input_mode {
        InputMode::ScopeFilter => app.scope_filter_edit.as_str(),
        _ => app.scope_filter.as_str(),
    };
    let filter_line = Line::from(vec![
        Span::styled("Filter", Style::default().fg(Color::Gray)),
        Span::raw(": "),
        Span::raw(if filter_text.is_empty() { "-" } else { filter_text }),
    ]);
    let filter_block =
        Paragraph::new(filter_line).block(Block::default().borders(Borders::ALL).title("Search"));
    frame.render_widget(filter_block, chunks[1]);

    let (scope_items, mut scope_state) = build_scope_items(app);
    let scope_list = List::new(scope_items)
        .block(Block::default().borders(Borders::ALL).title(scope_title(app.scope_kind)))
        .highlight_style(list_focus_style(app.focus == Focus::LeftNav))
        .highlight_symbol("> ");
    frame.render_stateful_widget(scope_list, chunks[2], &mut scope_state);

    let favorites_items: Vec<ListItem> = if app.favorites.is_empty() {
        vec![ListItem::new(Line::from("No favorites"))]
    } else {
        app.favorites
            .iter()
            .map(|name| ListItem::new(Line::from(name.as_str())))
            .collect()
    };
    let favorites = List::new(favorites_items)
        .block(Block::default().borders(Borders::ALL).title("Favorites"));
    frame.render_widget(favorites, chunks[3]);

    let saved_view_items: Vec<ListItem> = if app.saved_views.is_empty() {
        vec![ListItem::new(Line::from("No saved views"))]
    } else {
        app.saved_views
            .iter()
            .map(|view| {
                let summary = format!("{}  {}  {}", view.name, view.scope, view.query);
                ListItem::new(Line::from(truncate_string(&summary, 60)))
            })
            .collect()
    };
    let saved_views = List::new(saved_view_items)
        .block(Block::default().borders(Borders::ALL).title("Saved Views"));
    frame.render_widget(saved_views, chunks[4]);
}

fn render_right_main(frame: &mut ratatui::Frame, area: Rect, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(5), Constraint::Min(0)])
        .split(area);

    let query_line = query_line(app);
    let filter_line = filter_chips_line(app);
    let results_line = results_summary_line(app);
    let query_block = Paragraph::new(vec![query_line, filter_line, results_line])
        .block(Block::default().borders(Borders::ALL).title("Query"));
    frame.render_widget(query_block, chunks[0]);

    let title = results_title(app.docs_from, app.docs_size, app.docs_total);
    let id_width = result_id_width(chunks[1].width);
    let summary_width = chunks[1].width.saturating_sub(id_width + 5);

    let rows: Vec<Row> = if app.documents.is_empty() {
        vec![Row::new(vec![Cell::from("No documents"), Cell::from("")])]
    } else {
        app.documents
            .iter()
            .map(|doc| {
                let id = truncate_string(&doc.id, id_width as usize);
                let preview = doc_summary(doc, summary_width as usize);
                Row::new(vec![Cell::from(id), Cell::from(preview)])
            })
            .collect()
    };
    let header = Row::new(vec![Cell::from("id"), Cell::from("preview")])
        .style(Style::default().fg(Color::Gray).add_modifier(Modifier::BOLD));
    let table = Table::new(rows, [Constraint::Length(id_width), Constraint::Min(10)])
        .header(header)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(list_focus_style(app.focus == Focus::Results));
    frame.render_stateful_widget(table, chunks[1], &mut app.docs_state);
}

fn render_doc_drawer(frame: &mut ratatui::Frame, top_offset: u16, app: &App) {
    let size = frame.size();
    let height = size.height.saturating_sub(top_offset);
    if height < 5 {
        return;
    }
    let drawer_width = drawer_width(size.width);
    let drawer_area = Rect {
        x: size.width.saturating_sub(drawer_width),
        y: top_offset,
        width: drawer_width,
        height,
    };
    frame.render_widget(Clear, drawer_area);
    let lines = doc_drawer_lines(app, drawer_area.height.saturating_sub(2) as usize);
    let drawer = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title("Doc"));
    frame.render_widget(drawer, drawer_area);
}

fn scope_tab_index(scope: ScopeKind) -> usize {
    match scope {
        ScopeKind::Indices => 0,
        ScopeKind::Aliases => 1,
        ScopeKind::DataStreams => 2,
    }
}

fn scope_title(scope: ScopeKind) -> &'static str {
    match scope {
        ScopeKind::Indices => "Indices",
        ScopeKind::Aliases => "Aliases",
        ScopeKind::DataStreams => "DataStreams",
    }
}

fn query_line<'a>(app: &'a App) -> Line<'a> {
    let label_style = Style::default().fg(Color::Gray);
    let value = match app.input_mode {
        InputMode::Query => app.query_edit.as_str(),
        _ => app.query.as_str(),
    };
    let value = if value.is_empty() { "-" } else { value };
    let suffix = if app.input_mode == InputMode::Query {
        "*"
    } else {
        ""
    };
    Line::from(vec![
        Span::styled(format!("Query{suffix}"), label_style),
        Span::raw(": "),
        Span::raw(value),
    ])
}

fn filter_chips_line<'a>(app: &'a App) -> Line<'a> {
    let label_style = Style::default().fg(Color::Gray);
    let mut spans = vec![Span::styled("Filters", label_style), Span::raw(": ")];
    if app.query.trim().is_empty() {
        spans.push(Span::raw("(none)"));
    } else {
        let chip = truncate_string(app.query.trim(), 40);
        spans.push(Span::styled(
            format!(" {} ", chip),
            Style::default().bg(Color::DarkGray).fg(Color::Black),
        ));
    }
    Line::from(spans)
}

fn results_summary_line<'a>(app: &'a App) -> Line<'a> {
    let label_style = Style::default().fg(Color::Gray);
    let hits = app
        .docs_total
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let took = app
        .search_took_ms
        .map(|value| format!("{value}ms"))
        .unwrap_or_else(|| "-".to_string());
    let failed = app.search_shards_failed.unwrap_or(0);
    let timed_out = app.search_timed_out.unwrap_or(false);
    let mut parts = vec![format!("hits {hits}"), format!("took {took}")];
    if failed > 0 {
        parts.push(format!("shard_fail {failed}"));
    }
    if timed_out {
        parts.push("timeout".to_string());
    }
    let status = parts.join(" | ");
    let mut spans = vec![Span::styled("Results", label_style), Span::raw(": ")];
    let status_style = if failed > 0 || timed_out {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    spans.push(Span::styled(status, status_style));
    Line::from(spans)
}

fn results_title(from: u64, size: u64, total: Option<u64>) -> String {
    if let Some(total) = total {
        if total == 0 {
            return "Results (0)".to_string();
        }
        let page = from / size + 1;
        let pages = (total + size - 1) / size;
        format!("Results (page {page}/{pages}, total {total})")
    } else {
        format!("Results (from {from}, size {size})")
    }
}

fn result_id_width(total_width: u16) -> u16 {
    let min = 12;
    let max = 28;
    let target = total_width / 3;
    target.clamp(min, max)
}

fn doc_drawer_lines(app: &App, max_lines: usize) -> Vec<Line<'_>> {
    let mut lines = Vec::new();
    let Some(idx) = app.docs_state.selected() else {
        return vec![Line::from("No document selected")];
    };
    let Some(doc) = app.documents.get(idx) else {
        return vec![Line::from("No document selected")];
    };

    lines.push(Line::from(vec![
        Span::styled("ID: ", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(&doc.id),
    ]));
    lines.push(doc_view_line(app.doc_view_mode));
    lines.push(Line::from(vec![
        Span::styled("Actions", Style::default().fg(Color::Gray)),
        Span::raw(": include  exclude  copy  search"),
    ]));
    lines.push(Line::from(""));
    if max_lines > 0 && lines.len() >= max_lines {
        lines.truncate(max_lines);
        return lines;
    }

    let token = highlight_token(&app.query);
    let body_lines = match app.doc_view_mode {
        DocViewMode::Pretty => json_lines_pretty(&doc.source),
        DocViewMode::Raw => json_lines_raw(&doc.source),
        DocViewMode::Flatten => json_lines_flatten(&doc.source),
    };
    let mut truncated = false;
    for line in body_lines {
        if lines.len() >= max_lines {
            truncated = true;
            break;
        }
        if let Some(ref token) = token {
            lines.push(highlight_line(&line, token));
        } else {
            lines.push(Line::from(line));
        }
    }

    if truncated && max_lines > 0 {
        lines.truncate(max_lines);
        if let Some(last) = lines.last_mut() {
            *last = Line::from("...");
        }
    }
    lines
}

fn doc_view_line(mode: DocViewMode) -> Line<'static> {
    let active = Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD);
    let inactive = Style::default().fg(Color::Gray);
    let pretty = if mode == DocViewMode::Pretty {
        active
    } else {
        inactive
    };
    let raw = if mode == DocViewMode::Raw {
        active
    } else {
        inactive
    };
    let flat = if mode == DocViewMode::Flatten {
        active
    } else {
        inactive
    };
    Line::from(vec![
        Span::styled("View: ", Style::default().fg(Color::Gray)),
        Span::styled("Pretty", pretty),
        Span::raw(" | "),
        Span::styled("Raw", raw),
        Span::raw(" | "),
        Span::styled("Flatten", flat),
    ])
}

fn json_lines_pretty(value: &Value) -> Vec<String> {
    serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| "<invalid json>".to_string())
        .lines()
        .map(|line| line.to_string())
        .collect()
}

fn json_lines_raw(value: &Value) -> Vec<String> {
    vec![serde_json::to_string(value).unwrap_or_else(|_| "<invalid json>".to_string())]
}

fn json_lines_flatten(value: &Value) -> Vec<String> {
    let mut out = Vec::new();
    flatten_json_value(value, "", &mut out);
    if out.is_empty() {
        out.push("<empty>".to_string());
    }
    out
}

fn flatten_json_value(value: &Value, prefix: &str, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                let next = if prefix.is_empty() {
                    key.to_string()
                } else {
                    format!("{prefix}.{key}")
                };
                flatten_json_value(value, &next, out);
            }
        }
        Value::Array(values) => {
            for (idx, value) in values.iter().enumerate() {
                let next = format!("{prefix}[{idx}]");
                flatten_json_value(value, &next, out);
            }
        }
        _ => {
            let label = if prefix.is_empty() { "<root>" } else { prefix };
            out.push(format!("{label} = {}", json_value_inline(value)));
        }
    }
}

fn json_value_inline(value: &Value) -> String {
    match value {
        Value::String(text) => format!("\"{text}\""),
        Value::Number(num) => num.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => "null".to_string(),
        _ => "<complex>".to_string(),
    }
}

fn highlight_token(query: &str) -> Option<String> {
    let token = query
        .split_whitespace()
        .find(|part| !part.is_empty())
        .map(|part| part.trim_matches('"').trim_matches('\'').to_string());
    token.filter(|value| !value.is_empty())
}

fn highlight_line(line: &str, token: &str) -> Line<'static> {
    if token.is_empty() || !line.contains(token) {
        return Line::from(line.to_string());
    }
    let mut spans = Vec::new();
    let mut rest = line;
    while let Some(pos) = rest.find(token) {
        let (before, after) = rest.split_at(pos);
        if !before.is_empty() {
            spans.push(Span::raw(before.to_string()));
        }
        spans.push(Span::styled(
            token.to_string(),
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
        rest = &after[token.len()..];
    }
    if !rest.is_empty() {
        spans.push(Span::raw(rest.to_string()));
    }
    Line::from(spans)
}

fn build_scope_items(app: &App) -> (Vec<ListItem<'_>>, ListState) {
    let filtered = app.filtered_scope_indices();
    let mut state = ListState::default();
    let selected = app.scope_selected();
    let selected_pos = selected.and_then(|idx| filtered.iter().position(|value| *value == idx));
    state.select(selected_pos);

    let items: Vec<ListItem> = if filtered.is_empty() {
        vec![ListItem::new(Line::from("No items"))]
    } else {
        filtered
            .iter()
            .map(|idx| match app.scope_kind {
                ScopeKind::Indices => scope_line_index(&app.indices[*idx]),
                ScopeKind::Aliases => scope_line_alias(&app.aliases[*idx]),
                ScopeKind::DataStreams => scope_line_datastream(&app.datastreams[*idx]),
            })
            .collect()
    };
    (items, state)
}

fn scope_line_index(entry: &IndexEntry) -> ListItem<'_> {
    let status = match entry.health.as_str() {
        "green" => Span::styled("green", Style::default().fg(Color::Green)),
        "yellow" => Span::styled("yellow", Style::default().fg(Color::Yellow)),
        "red" => Span::styled("red", Style::default().fg(Color::Red)),
        _ => Span::styled(entry.health.as_str(), Style::default().fg(Color::Gray)),
    };
    ListItem::new(Line::from(vec![
        Span::styled(&entry.name, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        status,
        Span::raw(format!(" docs={}", entry.docs_count)),
    ]))
}

fn scope_line_alias(entry: &AliasEntry) -> ListItem<'_> {
    ListItem::new(Line::from(vec![
        Span::styled(&entry.alias, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" -> "),
        Span::raw(&entry.index_name),
    ]))
}

fn scope_line_datastream(entry: &DataStreamEntry) -> ListItem<'_> {
    let status = entry.status.as_deref().unwrap_or("-");
    let status_span = match status.to_lowercase().as_str() {
        "green" => Span::styled(status, Style::default().fg(Color::Green)),
        "yellow" => Span::styled(status, Style::default().fg(Color::Yellow)),
        "red" => Span::styled(status, Style::default().fg(Color::Red)),
        _ => Span::styled(status, Style::default().fg(Color::Gray)),
    };
    let backing = entry
        .indices
        .as_ref()
        .map(|values| values.len())
        .unwrap_or(0);
    let generation = entry
        .generation
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    ListItem::new(Line::from(vec![
        Span::styled(&entry.name, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(" "),
        status_span,
        Span::raw(format!(" gen={generation} backing={backing}")),
    ]))
}

fn auth_label(es_url: &str) -> &'static str {
    if es_url.contains('@') {
        "basic"
    } else {
        "none"
    }
}

fn scope_label(app: &App) -> String {
    let kind = match app.scope_kind {
        ScopeKind::Indices => "index",
        ScopeKind::Aliases => "alias",
        ScopeKind::DataStreams => "datastream",
    };
    let name = app.selected_scope_name().unwrap_or("-");
    format!("{kind}/{name}")
}

fn status_summary(app: &App) -> (String, Style) {
    let hits = app
        .docs_total
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let took = app
        .search_took_ms
        .map(|value| format!("{value}ms"))
        .unwrap_or_else(|| "-".to_string());
    let failed = app.search_shards_failed.unwrap_or(0);
    let timed_out = app.search_timed_out.unwrap_or(false);
    let mut parts = vec![format!("hits {hits}"), format!("took {took}")];
    if failed > 0 {
        parts.push(format!("shard_fail {failed}"));
    }
    if timed_out {
        parts.push("timeout".to_string());
    }
    if app.last_error.is_some() {
        parts.push("error".to_string());
    }
    let text = format!("status: {}", parts.join(" | "));
    let style = if failed > 0 || timed_out || app.last_error.is_some() {
        Style::default().fg(Color::Red).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::Gray)
    };
    (text, style)
}

fn list_focus_style(active: bool) -> Style {
    if active {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().add_modifier(Modifier::BOLD)
    }
}

fn doc_summary(doc: &DocEntry, max_len: usize) -> String {
    let source = serde_json::to_string(&doc.source).unwrap_or_else(|_| "<invalid>".into());
    truncate_string(&source, max_len)
}

fn filter_indices_by<T, F>(items: &[T], needle: &str, extract: F) -> Vec<usize>
where
    F: Fn(&T) -> &str,
{
    if needle.is_empty() {
        return (0..items.len()).collect();
    }
    items
        .iter()
        .enumerate()
        .filter_map(|(idx, entry)| {
            let hay = extract(entry).to_lowercase();
            if hay.contains(needle) {
                Some(idx)
            } else {
                None
            }
        })
        .collect()
}

fn drawer_width(total_width: u16) -> u16 {
    let min = 30;
    let max = total_width.saturating_sub(2).max(min);
    let target = total_width.saturating_mul(55) / 100;
    target.clamp(min, max)
}

fn truncate_string(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_string();
    }
    let mut out = String::new();
    for (idx, ch) in value.chars().enumerate() {
        if idx >= max_len {
            break;
        }
        out.push(ch);
    }
    out.push_str("...");
    out
}

fn status_style(status: &str) -> Style {
    match status {
        "green" => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        "yellow" => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        "red" => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        _ => Style::default().fg(Color::Gray),
    }
}
