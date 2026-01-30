use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph};
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Deserialize, Clone)]
struct ClusterHealth {
    cluster_name: String,
    status: String,
    number_of_nodes: u64,
    active_primary_shards: u64,
    active_shards: u64,
    unassigned_shards: u64,
}

#[derive(Debug, Deserialize, Clone)]
struct IndexEntry {
    health: String,
    status: String,
    #[serde(rename = "index")]
    name: String,
    #[serde(rename = "docs.count")]
    docs_count: String,
}

#[derive(Debug, Deserialize)]
struct SearchResponse {
    hits: SearchHits,
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
struct IndexDetails {
    name: String,
    uuid: Option<String>,
    shards: Option<String>,
    replicas: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Focus {
    Indices,
    Docs,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum InputMode {
    Normal,
    Query,
}

struct App {
    es_url: String,
    client: reqwest::blocking::Client,
    health: Option<ClusterHealth>,
    indices: Vec<IndexEntry>,
    documents: Vec<DocEntry>,
    docs_total: Option<u64>,
    docs_from: u64,
    docs_size: u64,
    index_details: Option<IndexDetails>,
    indices_state: ListState,
    docs_state: ListState,
    focus: Focus,
    input_mode: InputMode,
    query: String,
    query_edit: String,
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
        let mut docs_state = ListState::default();
        docs_state.select(None);
        Self {
            es_url,
            client,
            health: None,
            indices: Vec::new(),
            documents: Vec::new(),
            docs_total: None,
            docs_from: 0,
            docs_size: 5,
            index_details: None,
            indices_state,
            docs_state,
            focus: Focus::Indices,
            input_mode: InputMode::Normal,
            query: String::new(),
            query_edit: String::new(),
            last_error: None,
            last_fetch: None,
        }
    }

    fn selected_index_name(&self) -> Option<&str> {
        self.indices_state
            .selected()
            .and_then(|idx| self.indices.get(idx))
            .map(|entry| entry.name.as_str())
    }

    fn select_next(&mut self) {
        if self.indices.is_empty() {
            self.indices_state.select(None);
            return;
        }
        let next = match self.indices_state.selected() {
            Some(idx) if idx + 1 < self.indices.len() => idx + 1,
            _ => 0,
        };
        self.indices_state.select(Some(next));
        self.reset_docs_paging();
    }

    fn select_prev(&mut self) {
        if self.indices.is_empty() {
            self.indices_state.select(None);
            return;
        }
        let prev = match self.indices_state.selected() {
            Some(0) | None => self.indices.len() - 1,
            Some(idx) => idx - 1,
        };
        self.indices_state.select(Some(prev));
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
                        KeyCode::Tab => {
                            app.focus = match app.focus {
                                Focus::Indices => Focus::Docs,
                                Focus::Docs => Focus::Indices,
                            };
                        }
                        KeyCode::Up => match app.focus {
                            Focus::Indices => {
                                app.select_prev();
                                handle_index_change(&mut app);
                            }
                            Focus::Docs => app.select_prev_doc(),
                        },
                        KeyCode::Down => match app.focus {
                            Focus::Indices => {
                                app.select_next();
                                handle_index_change(&mut app);
                            }
                            Focus::Docs => app.select_next_doc(),
                        },
                        KeyCode::Enter | KeyCode::Char('d') => {
                            handle_docs_refresh(&mut app);
                        }
                        KeyCode::Char('n') => {
                            app.next_docs_page();
                            handle_docs_refresh(&mut app);
                        }
                        KeyCode::Char('p') => {
                            app.prev_docs_page();
                            handle_docs_refresh(&mut app);
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
    if let Err(err) = refresh_index_details(app) {
        errors.push(format!("details: {err:#}"));
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
    let selected_name = app.selected_index_name().map(|name| name.to_string());
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

fn refresh_docs(app: &mut App) -> Result<()> {
    let Some(index) = app.selected_index_name().map(|name| name.to_string()) else {
        app.documents.clear();
        app.docs_total = None;
        app.docs_state.select(None);
        return Ok(());
    };
    let (docs, total) = fetch_documents(
        &app.client,
        &app.es_url,
        &index,
        app.docs_from,
        app.docs_size,
        &app.query,
    )?;
    app.documents = docs;
    app.docs_total = total;
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

fn handle_index_change(app: &mut App) {
    if let Err(err) = refresh_index_details(app) {
        app.last_error = Some(format!("details: {err:#}"));
    }
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

fn fetch_documents(
    client: &reqwest::blocking::Client,
    es_url: &str,
    index: &str,
    from: u64,
    size: u64,
    query: &str,
) -> Result<(Vec<DocEntry>, Option<u64>)> {
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
    let docs = payload
        .hits
        .hits
        .into_iter()
        .map(|hit| DocEntry {
            id: hit.id,
            source: hit.source,
        })
        .collect();
    Ok((docs, total))
}

fn refresh_index_details(app: &mut App) -> Result<()> {
    let Some(index) = app.selected_index_name() else {
        app.index_details = None;
        return Ok(());
    };
    let details = fetch_index_details(&app.client, &app.es_url, index)?;
    app.index_details = Some(details);
    Ok(())
}

fn fetch_index_details(
    client: &reqwest::blocking::Client,
    es_url: &str,
    index: &str,
) -> Result<IndexDetails> {
    let base = es_url.trim_end_matches('/');
    let url = format!(
        "{base}/{index}?filter_path=*.settings.index.number_of_shards,*.settings.index.number_of_replicas,*.settings.index.uuid"
    );
    let response = client
        .get(url)
        .send()
        .context("request failed")?
        .error_for_status()
        .context("http error")?;
    let payload: Value = response.json().context("invalid response json")?;

    let obj = payload
        .as_object()
        .context("invalid index details response")?;
    let entry = obj
        .get(index)
        .context("index not found in details response")?;
    let settings = entry
        .get("settings")
        .and_then(|value| value.get("index"))
        .and_then(|value| value.as_object())
        .context("missing index settings")?;

    let uuid = settings.get("uuid").and_then(value_to_string);
    let shards = settings.get("number_of_shards").and_then(value_to_string);
    let replicas = settings.get("number_of_replicas").and_then(value_to_string);

    Ok(IndexDetails {
        name: index.to_string(),
        uuid,
        shards,
        replicas,
    })
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn ui(frame: &mut ratatui::Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(5),
            Constraint::Length(3),
        ])
        .split(frame.size());

    let header = Paragraph::new(Line::from(vec![
        Span::styled(
            "Elasticsearch TUI",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("URL: {}", app.es_url),
            Style::default().fg(Color::Gray),
        ),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(header, chunks[0]);

    let body_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(35), Constraint::Percentage(65)])
        .split(chunks[1]);

    let indices_items: Vec<ListItem> = if app.indices.is_empty() {
        vec![ListItem::new(Line::from("No indices"))]
    } else {
        app.indices
            .iter()
            .map(|entry| {
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
                    Span::raw(format!(" {}", entry.status)),
                    Span::raw(format!(" docs={}", entry.docs_count)),
                ]))
            })
            .collect()
    };

    let indices_list = List::new(indices_items)
        .block(Block::default().borders(Borders::ALL).title("Indices"))
        .highlight_style(list_focus_style(app.focus == Focus::Indices))
        .highlight_symbol("> ");
    frame.render_stateful_widget(indices_list, body_chunks[0], &mut app.indices_state);

    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10),
            Constraint::Min(5),
            Constraint::Length(7),
        ])
        .split(body_chunks[1]);

    let mut info_lines: Vec<Line> = Vec::new();
    if let Some(health) = &app.health {
        let status_style = status_style(&health.status);
        info_lines.push(Line::from(vec![
            Span::raw("Cluster: "),
            Span::styled(
                &health.cluster_name,
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        info_lines.push(Line::from(vec![
            Span::raw("Status : "),
            Span::styled(health.status.clone(), status_style),
        ]));
        info_lines.push(Line::from(format!("Nodes  : {}", health.number_of_nodes)));
        info_lines.push(Line::from(format!(
            "Shards : primary={} total={} unassigned={}",
            health.active_primary_shards, health.active_shards, health.unassigned_shards
        )));
    } else {
        info_lines.push(Line::from("No data yet. Press r to refresh."));
    }

    if let Some(last_fetch) = app.last_fetch {
        info_lines.push(Line::from(format!(
            "Last refresh: {}s ago",
            last_fetch.elapsed().as_secs()
        )));
    } else {
        info_lines.push(Line::from("Last refresh: never"));
    }

    if let Some(err) = &app.last_error {
        info_lines.push(Line::from(vec![
            Span::styled("Error: ", Style::default().fg(Color::Red)),
            Span::raw(err),
        ]));
    }

    if let Some(details) = &app.index_details {
        info_lines.push(Line::from(vec![
            Span::raw("Index : "),
            Span::styled(&details.name, Style::default().add_modifier(Modifier::BOLD)),
        ]));
        info_lines.push(Line::from(format!(
            "Shards: {}",
            details.shards.as_deref().unwrap_or("-")
        )));
        info_lines.push(Line::from(format!(
            "Replicas: {}",
            details.replicas.as_deref().unwrap_or("-")
        )));
        info_lines.push(Line::from(format!(
            "UUID: {}",
            details.uuid.as_deref().unwrap_or("-")
        )));
    }

    let query_text = match app.input_mode {
        InputMode::Query => format!("Query*: {}", app.query_edit),
        InputMode::Normal => format!("Query : {}", app.query),
    };
    info_lines.push(Line::from(vec![
        Span::styled("Filter", Style::default().fg(Color::Gray)),
        Span::raw(" "),
        Span::raw(query_text),
        Span::raw(" (press / or ?)"),
    ]));

    let info_block = Paragraph::new(info_lines).block(
        Block::default()
            .borders(Borders::ALL)
            .title("Cluster / Index"),
    );
    frame.render_widget(info_block, right_chunks[0]);

    let docs_title = docs_title(app.docs_from, app.docs_size, app.docs_total);
    let docs_items: Vec<ListItem> = if app.documents.is_empty() {
        vec![ListItem::new(Line::from("No documents"))]
    } else {
        app.documents
            .iter()
            .map(|doc| ListItem::new(Line::from(doc_summary(doc))))
            .collect()
    };
    let docs_block = List::new(docs_items)
        .block(Block::default().borders(Borders::ALL).title(docs_title))
        .highlight_style(list_focus_style(app.focus == Focus::Docs))
        .highlight_symbol("> ");
    frame.render_stateful_widget(docs_block, right_chunks[1], &mut app.docs_state);

    let mut detail_lines = Vec::new();
    if let Some(idx) = app.docs_state.selected() {
        if let Some(doc) = app.documents.get(idx) {
            detail_lines.push(Line::from(vec![
                Span::styled("ID: ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(&doc.id),
            ]));
            let pretty = serde_json::to_string_pretty(&doc.source)
                .unwrap_or_else(|_| "<invalid json>".to_string());
            for line in pretty.lines() {
                detail_lines.push(Line::from(line.to_string()));
            }
        }
    } else {
        detail_lines.push(Line::from("Select a document to preview"));
    }

    let detail_block =
        Paragraph::new(detail_lines).block(Block::default().borders(Borders::ALL).title("Doc"));
    frame.render_widget(detail_block, right_chunks[2]);

    let footer = Paragraph::new(Line::from(vec![
        Span::styled("q", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": quit  "),
        Span::styled("r", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": refresh  "),
        Span::styled("↑/↓", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": select  "),
        Span::styled("d/Enter", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": load docs  "),
        Span::styled("n/p", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": page  "),
        Span::styled("/ or ?", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": filter  "),
        Span::styled("Tab", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw(": focus"),
    ]))
    .block(Block::default().borders(Borders::ALL).title("Keys"));
    frame.render_widget(footer, chunks[2]);
}

fn docs_title(from: u64, size: u64, total: Option<u64>) -> String {
    if let Some(total) = total {
        if total == 0 {
            return "Documents (0)".to_string();
        }
        let page = from / size + 1;
        let pages = (total + size - 1) / size;
        format!("Documents (page {page}/{pages}, total {total})")
    } else {
        format!("Documents (from {from}, size {size})")
    }
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

fn doc_summary(doc: &DocEntry) -> String {
    let source = serde_json::to_string(&doc.source).unwrap_or_else(|_| "<invalid>".into());
    let truncated = truncate_string(&source, 80);
    format!("{} {}", doc.id, truncated)
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
