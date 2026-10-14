//! Slidev for den, after Slidev's own extension for VS Code: a deck's slides
//! as an outline in a tab of their own, a preview in a browser tab that
//! follows the editor's cursor, and the dev server started on demand.
//!
//! den draws the outline from data (`den_extension::view`). The deck is
//! split into slides here (`slides.rs`), and the server is found, started
//! and waited for here too (`server.rs`). The preview is a browser tab this
//! extension owns (`open_browser` with a tab name), moved from slide to slide
//! with a `navigate` message Slidev's page listens for, so it never reloads.
//! A deck's tab also offers Slide beside Source and Preview: a file view
//! (`set_file_views`) whose page shows the slide under the cursor.
//! A worker thread owns each folder's state and takes den's events over a
//! channel; between them it waits for starting servers to answer.

mod server;
mod slides;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use den_extension::view::{Column, Content, Line, Row, SelectOption, Span, ToolbarItem, actions};
use den_extension::{Context, Extension, FileView, Host, events, register};
use serde::Deserialize;
use serde_json::Value;

use crate::slides::{Deck, Entry, same_file};

/// The view's id in the manifest.
const VIEW: &str = "slides";
/// The name of the preview's browser tab.
const PREVIEW: &str = "preview";
/// The id of the Slide file view.
const SLIDE: &str = "slide";
/// How often a starting server is asked whether it is up.
const TICK: Duration = Duration::from_millis(500);
/// How long a server may take to start before it is given up on.
const START_TIMEOUT: Duration = Duration::from_secs(90);

// -- Settings ------------------------------------------------------------------

#[derive(Clone, Debug, Deserialize)]
#[serde(default)]
struct Config {
    port: f64,
    command: String,
    sync: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config { port: 3030., command: String::new(), sync: true }
    }
}

impl Config {
    fn port(&self) -> u16 {
        self.port.clamp(1024., 65000.) as u16
    }
}

fn config(settings: serde_json::Map<String, Value>) -> Config {
    serde_json::from_value(Value::Object(settings)).unwrap_or_default()
}

// -- The extension ---------------------------------------------------------------

enum Job {
    Event(String, Value),
    Stop,
}

struct Slidev {
    jobs: Sender<Job>,
    thread: Option<JoinHandle<()>>,
}

impl Extension for Slidev {
    fn activate(host: Host, context: Context) -> Self {
        let (jobs, rx) = mpsc::channel();
        let config = config(context.settings);
        let thread = std::thread::Builder::new()
            .name("slidev".into())
            .spawn(move || Worker { host, config, folders: HashMap::new() }.run(rx))
            .ok();
        Slidev { jobs, thread }
    }

    fn event(&mut self, name: &str, data: Value) {
        _ = self.jobs.send(Job::Event(name.to_string(), data));
    }

    fn deactivate(&mut self) {
        _ = self.jobs.send(Job::Stop);
        if let Some(thread) = self.thread.take() {
            _ = thread.join();
        }
    }
}

register!(Slidev);

// -- The worker ----------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    /// Started in a terminal, not answering yet.
    Starting(Instant),
    Ready,
}

/// Where to show the deck once its server answers.
#[derive(Clone, Debug, PartialEq)]
enum Show {
    Tab,
    Browser,
    /// The Slide view of this file's tab.
    File(PathBuf),
}

/// A dev server for one deck.
struct Server {
    deck: PathBuf,
    port: u16,
    state: State,
    /// Shown there once it answers.
    then: Option<Show>,
}

/// The decks of the window on `root`, and what shows of them.
struct Folder {
    /// The folder as den named it.
    root: String,
    decks: Vec<PathBuf>,
    deck: Option<PathBuf>,
    slides: Deck,
    /// The slide the cursor or the outline is on, by its index.
    current: Option<usize>,
    view_open: bool,
    /// The preview follows the cursor.
    sync: bool,
    servers: Vec<Server>,
    /// The deck the preview tab shows.
    previewing: Option<PathBuf>,
    /// The pattern of the Slide file view, as last set.
    file_pattern: String,
}

impl Folder {
    fn new(root: &str, sync: bool) -> Self {
        let decks = server::find_decks(Path::new(root));
        let deck = decks.first().cloned();
        let slides = deck.as_deref().map(Deck::load).unwrap_or_default();
        Folder {
            root: root.to_string(),
            decks,
            deck,
            slides,
            current: None,
            view_open: false,
            sync,
            servers: Vec::new(),
            previewing: None,
            file_pattern: String::new(),
        }
    }

    fn server(&self) -> Option<&Server> {
        let deck = self.deck.as_deref()?;
        self.servers.iter().find(|s| same_file(&s.deck, deck))
    }

    fn server_mut(&mut self) -> Option<&mut Server> {
        let deck = self.deck.clone()?;
        self.servers.iter_mut().find(|s| same_file(&s.deck, &deck))
    }

    /// Make `deck` the one shown, read afresh.
    fn set_deck(&mut self, deck: PathBuf) {
        if !self.decks.iter().any(|d| same_file(d, &deck)) {
            self.decks.push(deck.clone());
        }
        self.slides = Deck::load(&deck);
        self.deck = Some(deck);
        self.current = None;
    }

    /// The page the preview shows for the current slide.
    fn page(&self) -> usize {
        self.current.map_or(1, |index| self.slides.page(index))
    }

    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

struct Worker {
    host: Host,
    config: Config,
    folders: HashMap<String, Folder>,
}

impl Worker {
    fn run(mut self, rx: Receiver<Job>) {
        loop {
            match rx.recv_timeout(TICK) {
                Ok(Job::Stop) | Err(RecvTimeoutError::Disconnected) => return,
                Ok(Job::Event(name, data)) => self.event(&name, data),
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.tick();
        }
    }

    /// The window on `root`'s state, read the first time it is asked for.
    fn folder(&mut self, root: &str) -> &mut Folder {
        let sync = self.config.sync;
        self.folders
            .entry(root.to_string())
            .or_insert_with(|| Folder::new(root, sync))
    }

    fn event(&mut self, name: &str, mut data: Value) {
        let root = data["root"].as_str().unwrap_or_default().to_string();
        let text = |key: &str| data[key].as_str().map(str::to_string);
        let path = text("path").map(PathBuf::from);
        match name {
            events::WORKSPACE_OPENED => {
                self.set_file_view(&root);
            }
            events::ACTIVE_FILE_CHANGED => {
                if let Some(path) = path {
                    self.file_shown(&root, path);
                }
            }
            events::FILE_SAVED => {
                if let Some(path) = path {
                    self.saved(&root, path);
                }
            }
            events::CURSOR_MOVED => {
                let line = data["line"].as_u64().unwrap_or(1) as usize;
                if let Some(path) = path {
                    self.cursor(&root, &path, line);
                }
            }
            events::COMMAND => match text("id").as_deref() {
                Some("show") => self.show(&root),
                Some("preview") => self.preview(&root, Show::Tab),
                Some("open-external") => self.preview(&root, Show::Browser),
                Some("export") => self.export(&root),
                _ => {}
            },
            events::FILE_VIEW_SHOWN if text("view").as_deref() == Some(SLIDE) => {
                let line = data["line"].as_u64().unwrap_or(1) as usize;
                if let Some(path) = path {
                    self.slide_shown(&root, path, line, text("url"));
                }
            }
            events::VIEW_OPENED if text("view").as_deref() == Some(VIEW) => {
                self.folder(&root).view_open = true;
                self.draw(&root);
            }
            events::VIEW_CLOSED if text("view").as_deref() == Some(VIEW) => {
                self.folder(&root).view_open = false;
            }
            events::VIEW_ACTION if text("view").as_deref() == Some(VIEW) => {
                let action = text("action").unwrap_or_default();
                self.action(&root, &action, text("row"), text("data"), text("value"));
            }
            events::SETTINGS_CHANGED => {
                if let Value::Object(settings) = data["settings"].take() {
                    self.config = config(settings);
                    let roots: Vec<String> = self.folders.keys().cloned().collect();
                    for root in roots {
                        self.folder(&root).sync = self.config.sync;
                        self.draw(&root);
                    }
                }
            }
            _ => {}
        }
    }

    /// Slide beside Source and Preview, above the decks and the files the
    /// one shown imports.
    fn set_file_view(&mut self, root: &str) {
        let host = self.host;
        let folder = self.folder(root);
        let mut files: Vec<&Path> = folder.decks.iter().map(PathBuf::as_path).collect();
        files.extend(folder.slides.files.iter().map(PathBuf::as_path));
        let pattern = files_pattern(&files);
        if pattern == folder.file_pattern {
            return;
        }
        folder.file_pattern = pattern.clone();
        let views = match pattern.is_empty() {
            true => Vec::new(),
            false => vec![FileView {
                id: SLIDE.into(),
                label: "Slide".into(),
                tooltip: "The slide the cursor is on, from the dev server".into(),
                file_pattern: pattern,
            }],
        };
        // A den without file views says so; the rest works there all the same.
        if let Err(err) = host.set_file_views(root, &views) {
            host.log(format!("set_file_views: {err}"));
        }
    }

    /// A file's tab came to the front: a deck other than the one shown takes its place.
    fn file_shown(&mut self, root: &str, path: PathBuf) {
        let folder = self.folder(root);
        if folder.slides.has_file(&path) || !server::is_deck(&path) {
            return;
        }
        folder.set_deck(path);
        self.set_file_view(root);
        self.draw(root);
    }

    fn saved(&mut self, root: &str, path: PathBuf) {
        let folder = self.folder(root);
        if let Some(deck) = folder.deck.clone().filter(|_| folder.slides.has_file(&path)) {
            let current = folder.current;
            folder.slides = Deck::load(&deck);
            folder.current = current.filter(|&index| index <= folder.slides.entries.len());
        } else if server::is_deck(&path) && !folder.decks.iter().any(|d| same_file(d, &path)) {
            folder.decks.push(path);
        } else {
            return;
        }
        self.set_file_view(root);
        self.draw(root);
    }

    /// The editor's cursor moved: the outline, and the preview when it follows, go along.
    fn cursor(&mut self, root: &str, path: &Path, line: usize) {
        let folder = self.folder(root);
        if !folder.slides.has_file(path) {
            return;
        }
        let index = folder.slides.slide_at(path, line);
        if index.is_none() || index == folder.current {
            return;
        }
        folder.current = index;
        let sync = folder.sync;
        self.select(root);
        if sync {
            self.navigate(root);
        }
    }

    /// A tab switched to Slide: its page goes to the slide under the
    /// file's cursor, moved there when it already shows the deck's server.
    fn slide_shown(&mut self, root: &str, path: PathBuf, line: usize, url: Option<String>) {
        let host = self.host;
        self.file_shown(root, path.clone());
        let folder = self.folder(root);
        if !folder.slides.has_file(&path) {
            return;
        }
        if let Some(index) = folder.slides.slide_at(&path, line) {
            folder.current = Some(index);
        }
        let page = folder.page();
        let serving = folder.server().filter(|s| s.state == State::Ready).map(|s| s.port);
        self.select(root);
        if let Some(port) = serving
            && url.is_some_and(|url| url.starts_with(&format!("http://localhost:{port}/")))
            && server::ping(port)
        {
            _ = host.file_view_script(root, &path.to_string_lossy(), SLIDE, &navigate_script(page));
            return;
        }
        self.preview(root, Show::File(path));
    }

    fn show(&mut self, root: &str) {
        if let Err(err) = self.host.open_view(root, VIEW) {
            self.host.log(format!("open_view: {err}"));
        }
    }

    /// Show the deck in the preview tab, or the system's browser: at once
    /// when its server answers, else once the server started for it does.
    fn preview(&mut self, root: &str, show: Show) {
        let (host, config) = (self.host, self.config.clone());
        let folder = self.folder(root);
        let Some(deck) = folder.deck.clone() else {
            return host.toast("There's no Slidev deck in this folder.");
        };
        let state = folder.server().map(|s| (s.state, s.port));
        match state {
            Some((State::Ready, port)) if server::ping(port) => return open(host, folder, &show),
            Some((State::Starting(_), _)) => {
                if let Some(server) = folder.server_mut() {
                    server.then = Some(show);
                }
                return host.toast("Slidev is still starting…");
            }
            _ => {}
        }
        folder.servers.retain(|s| !same_file(&s.deck, &deck));
        // A server already on the configured port (one started by hand, or
        // before den restarted) is used when it serves this deck.
        let port = config.port();
        let serves = server::probe(port).is_some_and(|entry| entry.is_empty() || same_file(Path::new(&entry), &deck));
        if serves && !folder.servers.iter().any(|s| s.port == port) {
            folder.servers.push(Server { deck, port, state: State::Ready, then: None });
            return open(host, folder, &show);
        }
        let port = server::free_port(port);
        let dir = server::project_dir(&deck);
        let command = server::serve_command(&config.command, &dir, &deck, port);
        if let Err(err) = host.run_in_terminal(root, &command, Some(&dir.to_string_lossy())) {
            return host.toast(format!("Couldn't start Slidev: {err}"));
        }
        folder.servers.push(Server { deck, port, state: State::Starting(Instant::now()), then: Some(show) });
        host.toast(format!("Starting Slidev on port {port}…"));
    }

    fn export(&mut self, root: &str) {
        let (host, template) = (self.host, self.config.command.clone());
        let Some(deck) = self.folder(root).deck.clone() else {
            return host.toast("There's no Slidev deck in this folder.");
        };
        let dir = server::project_dir(&deck);
        let command = server::export_command(&template, &dir, &deck);
        if let Err(err) = host.run_in_terminal(root, &command, Some(&dir.to_string_lossy())) {
            host.toast(format!("Couldn't export: {err}"));
        }
    }

    /// Ask the starting servers whether they are up; show them where they were wanted.
    fn tick(&mut self) {
        let host = self.host;
        for folder in self.folders.values_mut() {
            let mut ready = Vec::new();
            for server in &mut folder.servers {
                let State::Starting(since) = server.state else { continue };
                if server::ping(server.port) {
                    server.state = State::Ready;
                    if let Some(show) = server.then.take() {
                        ready.push((server.deck.clone(), show));
                    }
                } else if since.elapsed() > START_TIMEOUT {
                    host.toast(format!("Slidev didn't answer on port {}; see its terminal.", server.port));
                    server.then = None;
                }
            }
            folder.servers.retain(|s| !matches!(s.state, State::Starting(since) if since.elapsed() > START_TIMEOUT));
            for (deck, show) in ready {
                // Unless another deck took its place meanwhile.
                if folder.deck.as_deref().is_some_and(|d| same_file(d, &deck)) {
                    open(host, folder, &show);
                }
            }
        }
    }

    /// Move the preview to the current slide, without reloading it.
    fn navigate(&mut self, root: &str) {
        let host = self.host;
        let folder = self.folder(root);
        let showing = match (&folder.previewing, &folder.deck) {
            (Some(previewing), Some(deck)) => same_file(previewing, deck),
            _ => false,
        };
        if !showing || folder.server().is_none_or(|s| s.state != State::Ready) {
            return;
        }
        _ = host.browser_script(root, PREVIEW, &navigate_script(folder.page()));
    }

    fn action(&mut self, root: &str, action: &str, row: Option<String>, data: Option<String>, value: Option<String>) {
        let host = self.host;
        match action {
            actions::SELECT | actions::OPEN => {
                let Some(index) = row.and_then(|r| r.parse().ok()) else { return };
                self.folder(root).current = Some(index);
                self.select(root);
                self.navigate(root);
                if action == actions::OPEN {
                    self.go_to(root, index);
                }
            }
            "goto" => {
                if let Some(index) = data.and_then(|d| d.parse().ok()) {
                    self.go_to(root, index);
                }
            }
            "preview" => self.preview(root, Show::Tab),
            "external" => self.preview(root, Show::Browser),
            "sync" => {
                let folder = self.folder(root);
                folder.sync = !folder.sync;
                let sync = folder.sync;
                let content = Content { toolbar: Some(toolbar(self.folder(root))), ..Default::default() };
                _ = host.set_view(root, VIEW, &content);
                if sync {
                    self.navigate(root);
                }
            }
            "refresh" => {
                let folder = self.folder(root);
                folder.decks = server::find_decks(Path::new(root));
                match folder.deck.clone() {
                    Some(deck) => folder.set_deck(deck),
                    None => {
                        if let Some(first) = folder.decks.first().cloned() {
                            folder.set_deck(first);
                        }
                    }
                }
                self.set_file_view(root);
                self.draw(root);
            }
            "deck" => {
                if let Some(deck) = value.map(PathBuf::from) {
                    self.folder(root).set_deck(deck);
                    self.set_file_view(root);
                    self.draw(root);
                }
            }
            _ => {}
        }
    }

    /// Put the editor's cursor on slide `index`.
    fn go_to(&mut self, root: &str, index: usize) {
        let host = self.host;
        let Some(entry) = self.folder(root).slides.entry(index) else { return };
        _ = host.open_file(root, &entry.file.to_string_lossy(), Some(entry.slide.line as u32));
    }

    /// The outline's selection and details, after the current slide changed.
    fn select(&mut self, root: &str) {
        let host = self.host;
        let folder = self.folder(root);
        if !folder.view_open {
            return;
        }
        let content = Content {
            selected: Some(folder.current.map(|i| i.to_string()).unwrap_or_default()),
            detail: Some(detail(folder)),
            ..Default::default()
        };
        _ = host.set_view(root, VIEW, &content);
    }

    /// All of the outline.
    fn draw(&mut self, root: &str) {
        let host = self.host;
        let folder = self.folder(root);
        if !folder.view_open {
            return;
        }
        let message = match &folder.deck {
            None => "There's no Slidev deck here. Open one, or create slides.md.".to_string(),
            Some(deck) if folder.slides.entries.is_empty() => format!("{} has no slides.", folder.relative(deck)),
            Some(_) => String::new(),
        };
        let content = Content {
            toolbar: Some(toolbar(folder)),
            columns: Some(vec![
                Column { title: "#".into(), width: Some(44.) },
                Column { title: "Title".into(), width: None },
                Column { title: "Layout".into(), width: Some(110.) },
            ]),
            rows: Some(folder.slides.entries.iter().map(|e| row(folder, e)).collect()),
            selected: Some(folder.current.map(|i| i.to_string()).unwrap_or_default()),
            detail: Some(detail(folder)),
            message: Some(message),
            ..Default::default()
        };
        _ = host.set_view(root, VIEW, &content);
    }
}

/// The script that moves Slidev's page to `page`, without reloading it.
fn navigate_script(page: usize) -> String {
    format!("window.postMessage({{ target: 'slidev', type: 'navigate', no: {page}, clicks: 0 }}, '*')")
}

/// A pattern that matches `files`' paths and nothing else, whichever way
/// their separators go (and, on Windows, in any case); empty for none.
fn files_pattern(files: &[&Path]) -> String {
    let mut alternatives: Vec<String> = Vec::new();
    for file in files {
        let alternative = slides::normalize(file)
            .to_string_lossy()
            .split(['\\', '/'])
            .map(escape)
            .collect::<Vec<_>>()
            .join(r"[\\/]");
        if !alternatives.contains(&alternative) {
            alternatives.push(alternative);
        }
    }
    if alternatives.is_empty() {
        return String::new();
    }
    let case = if cfg!(windows) { "(?i)" } else { "" };
    format!("{case}^(?:{})$", alternatives.join("|"))
}

/// `text` with the characters that mean something in a pattern escaped.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if r"\.+*?()|[]{}^$#&-~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Show `folder`'s deck, whose server answers: in the preview tab, at the
/// current slide, in a tab's Slide view, or in the system's browser.
fn open(host: Host, folder: &mut Folder, show: &Show) {
    let Some(port) = folder.server().map(|s| s.port) else { return };
    let url = format!("http://localhost:{port}/{}", folder.page());
    match show {
        Show::Tab => {
            _ = host.open_browser(&folder.root, &url, Some(PREVIEW));
            folder.previewing = folder.deck.clone();
        }
        Show::Browser => {
            if let Err(err) = server::open_external(&url) {
                host.toast(format!("Couldn't open the browser: {err}"));
            }
        }
        Show::File(path) => _ = host.load_file_view(&folder.root, &path.to_string_lossy(), SLIDE, &url),
    }
}

fn toolbar(folder: &Folder) -> Vec<ToolbarItem> {
    let mut items = Vec::new();
    match &folder.deck {
        Some(deck) if folder.decks.len() > 1 => {
            let options = folder
                .decks
                .iter()
                .map(|d| SelectOption { value: d.to_string_lossy().into_owned(), label: folder.relative(d) })
                .collect();
            items.push(ToolbarItem::select("deck", deck.to_string_lossy(), options).tooltip("The deck"));
        }
        Some(deck) => items.push(ToolbarItem::label(folder.relative(deck))),
        None => {}
    }
    items.extend([
        ToolbarItem::spacer(),
        ToolbarItem::button("preview", "Preview", "play").tooltip("Show the slides beside the editor, starting the dev server"),
        ToolbarItem::button("sync", "", "link").tooltip("The preview follows the cursor").active(folder.sync),
        ToolbarItem::button("external", "", "external-link").tooltip("Open the slides in the browser"),
        ToolbarItem::button("refresh", "", "refresh-cw").tooltip("Read the deck again"),
    ]);
    items
}

fn row(folder: &Folder, entry: &Entry) -> Row {
    let mut title = vec![Span::text(entry.slide.title())];
    if folder.deck.as_deref().is_some_and(|deck| !same_file(deck, &entry.file)) {
        let name = entry.file.file_name().unwrap_or_default().to_string_lossy();
        title.push(Span::text(format!("  {name}")).dim().tooltip(folder.relative(&entry.file)));
    }
    let layout = entry.slide.layout();
    let layout = if layout.is_empty() { Vec::new() } else { vec![Span::text(layout).background("#475569")] };
    let no = match entry.no {
        Some(no) => Span::text(no.to_string()),
        None => Span::text("–").tooltip("Hidden"),
    };
    Row {
        id: entry.index.to_string(),
        cells: vec![no.dim().into(), title.into(), layout.into()],
        dim: entry.no.is_none(),
        ..Default::default()
    }
}

/// The current slide's file, frontmatter and notes.
fn detail(folder: &Folder) -> Vec<Line> {
    let Some(entry) = folder.current.and_then(|i| folder.slides.entry(i)) else {
        return Vec::new();
    };
    let slide = &entry.slide;
    let heading = match entry.no {
        Some(no) => format!("Slide {no}: {}", slide.title()),
        None => format!("Hidden slide: {}", slide.title()),
    };
    let mut lines = vec![
        Line::heading(heading),
        Line::new(vec![
            Span::text(format!("{}:{}", folder.relative(&entry.file), slide.line))
                .icon("file-text")
                .tooltip("Go to the slide"),
        ])
        .action("goto", entry.index.to_string()),
    ];
    if !slide.frontmatter.is_empty() {
        lines.push(Line::heading("Frontmatter"));
        lines.extend(
            slide
                .frontmatter
                .iter()
                .map(|(key, value)| Line::new(vec![Span::text(format!("{key}:")).dim().mono(), Span::text(format!(" {value}")).mono()])),
        );
    }
    if !slide.notes.is_empty() {
        lines.push(Line::heading("Notes"));
        lines.extend(slide.notes.lines().map(|line| Line::new(vec![Span::text(line.trim())])));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_agree_with_the_manifest() {
        let manifest = den_extension::Manifest::parse(include_str!("../extension.json")).unwrap();
        let config = config(manifest.settings_values(&Default::default()));
        assert_eq!(config.port(), 3030);
        assert!(config.command.is_empty() && config.sync);
        assert_eq!(manifest.views[0].id, VIEW);
    }

    #[test]
    fn the_slide_view_matches_the_decks_files_only() {
        let deck = Path::new(r"C:\talks\a+b\slides.md");
        let pattern = files_pattern(&[deck, Path::new(r"C:\talks\a+b\pages\intro.md"), deck]);
        let re = regex::Regex::new(&pattern).unwrap();
        assert!(re.is_match(r"C:\talks\a+b\slides.md"));
        assert!(re.is_match("C:/talks/a+b/pages/intro.md"));
        assert!(!re.is_match(r"C:\talks\aab\slides.md"));
        assert!(!re.is_match(r"C:\talks\a+b\slides.md.bak"));
        if cfg!(windows) {
            assert!(re.is_match(r"c:\Talks\A+B\Slides.md"));
        }
        assert_eq!(pattern.matches('|').count(), 1);
        assert_eq!(files_pattern(&[]), "");
    }
}
