//! Finding decks and running Slidev's dev server: the project a deck is in,
//! the command that starts it, and whether a port answers as Slidev.

use std::io::{Read, Write};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Markdown that is never a deck.
const NOT_DECKS: [&str; 4] = ["readme.md", "changelog.md", "license.md", "contributing.md"];
/// Folders `find_decks` doesn't look in: dependencies, builds, and a deck's
/// own parts (`pages/` holds the files decks import).
const SKIP: [&str; 6] = ["node_modules", "dist", "target", "pages", "components", "snippets"];

/// The nearest folder from `file` up that has a `package.json`.
pub fn package_dir(file: &Path) -> Option<PathBuf> {
    file.ancestors()
        .skip(1)
        .find(|dir| dir.join("package.json").is_file())
        .map(Path::to_path_buf)
}

/// Where the dev server runs for the deck `entry`: its package's folder, else its own.
pub fn project_dir(entry: &Path) -> PathBuf {
    package_dir(entry).unwrap_or_else(|| entry.parent().unwrap_or(Path::new("")).to_path_buf())
}

fn uses_slidev(package_dir: &Path) -> bool {
    std::fs::read_to_string(package_dir.join("package.json")).is_ok_and(|text| text.contains("@slidev/cli"))
}

/// Whether `path` is a Slidev deck: a Markdown file named `slides.md`, or
/// one in a package that depends on `@slidev/cli`.
pub fn is_deck(path: &Path) -> bool {
    let Some(name) = path.file_name().map(|n| n.to_string_lossy().to_lowercase()) else {
        return false;
    };
    if !name.ends_with(".md") || NOT_DECKS.contains(&name.as_str()) {
        return false;
    }
    let parts = path.components().map(|c| c.as_os_str().to_string_lossy().to_lowercase());
    if parts.collect::<Vec<_>>().iter().any(|part| SKIP.contains(&part.as_str())) {
        return false;
    }
    name == "slides.md" || package_dir(path).is_some_and(|dir| uses_slidev(&dir))
}

/// The decks in `root` and up to two folders down, `slides.md` first.
pub fn find_decks(root: &Path) -> Vec<PathBuf> {
    let mut decks = Vec::new();
    let mut dirs = vec![(root.to_path_buf(), 0)];
    while let Some((dir, depth)) = dirs.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        for entry in read.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_lowercase();
            let Ok(kind) = entry.file_type() else { continue };
            if kind.is_dir() {
                if depth < 2 && !name.starts_with('.') && !SKIP.contains(&name.as_str()) {
                    dirs.push((path, depth + 1));
                }
            } else if is_deck(&path) {
                decks.push(path);
            }
        }
    }
    decks.sort_by_key(|p| {
        let name = p.file_name().map(|n| n.to_string_lossy().to_lowercase());
        (name.as_deref() != Some("slides.md"), p.components().count(), p.clone())
    });
    decks
}

/// How the project in `dir` runs its packages, by the lockfile from there up.
pub fn runner(dir: &Path) -> &'static str {
    for dir in dir.ancestors() {
        if dir.join("pnpm-lock.yaml").is_file() {
            return "pnpm exec slidev";
        }
        if dir.join("yarn.lock").is_file() {
            return "yarn slidev";
        }
        if dir.join("bun.lock").is_file() || dir.join("bun.lockb").is_file() {
            return "bunx slidev";
        }
        if dir.join("package-lock.json").is_file() {
            break;
        }
    }
    "npx slidev"
}

/// `path` as a shell word: forward slashes, quoted when it has spaces.
fn shell_path(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.contains(char::is_whitespace) { format!("\"{text}\"") } else { text }
}

/// The deck `entry` relative to `dir`, as a shell word.
pub fn entry_arg(entry: &Path, dir: &Path) -> String {
    shell_path(entry.strip_prefix(dir).unwrap_or(entry))
}

/// The command that serves `entry` on `port`, run in `dir`: `template` with
/// `{args}` filled in, or the project's runner when it is empty.
pub fn serve_command(template: &str, dir: &Path, entry: &Path, port: u16) -> String {
    let args = format!("{} --port {port}", entry_arg(entry, dir));
    match template.trim() {
        "" => format!("{} {args}", runner(dir)),
        template if template.contains("{args}") => template.replace("{args}", &args),
        template => format!("{template} {args}"),
    }
}

/// The command that exports `entry` to PDF, run in `dir`.
pub fn export_command(template: &str, dir: &Path, entry: &Path) -> String {
    let args = format!("export {}", entry_arg(entry, dir));
    match template.trim() {
        "" => format!("{} {args}", runner(dir)),
        template if template.contains("{args}") => template.replace("{args}", &args),
        template => format!("{template} {args}"),
    }
}

fn loopbacks(port: u16) -> [SocketAddr; 2] {
    [
        SocketAddr::from((Ipv4Addr::LOCALHOST, port)),
        // Vite may listen on `localhost` as ::1 only.
        SocketAddr::from((Ipv6Addr::LOCALHOST, port)),
    ]
}

/// What answers on `port`, when it is Slidev: the deck it serves, from its
/// page's `slidev:entry`, or "" when the page doesn't say.
pub fn probe(port: u16) -> Option<String> {
    loopbacks(port).iter().find_map(|addr| {
        let mut stream = TcpStream::connect_timeout(addr, Duration::from_millis(300)).ok()?;
        _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let request = format!("GET / HTTP/1.0\r\nHost: localhost:{port}\r\nAccept: text/html\r\n\r\n");
        stream.write_all(request.as_bytes()).ok()?;
        let mut page = Vec::new();
        _ = stream.take(64 * 1024).read_to_end(&mut page);
        let page = String::from_utf8_lossy(&page);
        page.to_lowercase().contains("slidev").then(|| entry_of(&page))
    })
}

/// Whether Slidev answers on `port`.
pub fn ping(port: u16) -> bool {
    probe(port).is_some()
}

/// Move the pages of the Slidev on `port` to `page`, as its presenter does:
/// a patch of the `nav` state the server shares with its pages, which follow
/// a presenter without reloading. Whether the server took it.
pub fn navigate(port: u16, page: usize) -> bool {
    let time = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_millis() as u64);
    let body = serde_json::json!({
        "source": "den",
        "patch": { "page": page, "clicks": 0, "lastUpdate": { "id": "den", "type": "presenter", "time": time } },
        "timestamp": time,
    })
    .to_string();
    let request = format!(
        "POST /@server-reactive/nav HTTP/1.0\r\nHost: localhost:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    loopbacks(port).iter().any(|addr| {
        let Ok(mut stream) = TcpStream::connect_timeout(addr, Duration::from_millis(300)) else { return false };
        _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
        let mut status = [0; 12];
        stream.write_all(request.as_bytes()).is_ok() && stream.read_exact(&mut status).is_ok() && status.ends_with(b" 200")
    })
}

/// The deck a Slidev page says it serves, or "".
fn entry_of(page: &str) -> String {
    let Some(at) = page.find("property=\"slidev:entry\"") else {
        return String::new();
    };
    let tag = &page[at..page[at..].find('>').map_or(page.len(), |end| at + end)];
    tag.split_once("content=\"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(entry, _)| entry.replace("&amp;", "&"))
        .unwrap_or_default()
}

/// Whether nothing listens on `port`.
fn free(port: u16) -> bool {
    // Nothing answers on either loopback (a server on `::` or `localhost`
    // may not stop a bind on 127.0.0.1), and it can be bound.
    loopbacks(port)
        .iter()
        .all(|addr| TcpStream::connect_timeout(addr, Duration::from_millis(100)).is_err())
        && TcpListener::bind((Ipv4Addr::LOCALHOST, port)).is_ok()
}

/// The first free port from `from` on.
pub fn free_port(from: u16) -> u16 {
    (from..from.saturating_add(100)).find(|&port| free(port)).unwrap_or(from)
}

/// Open `url` in the system's browser.
pub fn open_external(url: &str) -> std::io::Result<()> {
    let mut command;
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command = std::process::Command::new("rundll32");
        command.args(["url.dll,FileProtocolHandler", url]);
        // No console window for it.
        command.creation_flags(0x0800_0000);
    }
    #[cfg(target_os = "macos")]
    {
        command = std::process::Command::new("open");
        command.arg(url);
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        command = std::process::Command::new("xdg-open");
        command.arg(url);
    }
    command.spawn().map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("den-slidev-{name}-{}", std::process::id()));
        _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn decks_are_slides_md_or_in_a_slidev_package() {
        let dir = temp("decks");
        std::fs::create_dir_all(dir.join("talk/pages")).unwrap();
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("talk/package.json"), r#"{"dependencies":{"@slidev/cli":"^52"}}"#).unwrap();
        for file in ["talk/deck.md", "talk/README.md", "talk/pages/part.md", "docs/notes.md", "docs/slides.md"] {
            std::fs::write(dir.join(file), "# x").unwrap();
        }
        assert!(is_deck(&dir.join("talk/deck.md")));
        assert!(!is_deck(&dir.join("talk/README.md")));
        assert!(!is_deck(&dir.join("talk/pages/part.md")));
        assert!(!is_deck(&dir.join("docs/notes.md")));
        let decks = find_decks(&dir);
        assert_eq!(decks, [dir.join("docs").join("slides.md"), dir.join("talk").join("deck.md")]);
        assert_eq!(project_dir(&dir.join("talk/deck.md")), dir.join("talk"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commands_use_the_projects_runner() {
        let dir = temp("runner");
        let entry = dir.join("my talk").join("slides.md");
        assert_eq!(serve_command("", &dir, &entry, 3030), "npx slidev \"my talk/slides.md\" --port 3030");
        std::fs::write(dir.join("pnpm-lock.yaml"), "").unwrap();
        assert_eq!(serve_command("", &dir, &dir.join("slides.md"), 3031), "pnpm exec slidev slides.md --port 3031");
        assert_eq!(export_command("", &dir, &dir.join("slides.md")), "pnpm exec slidev export slides.md");
        assert_eq!(serve_command("pnpm dev -- {args} --remote", &dir, &dir.join("a.md"), 1), "pnpm dev -- a.md --port 1 --remote");
        assert_eq!(serve_command("slidev", &dir, &dir.join("a.md"), 1), "slidev a.md --port 1");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_page_names_its_deck() {
        let page = r#"<head><meta property="slidev:version" content="52.20.1">
<meta property="slidev:entry" content="C:/talks/a&amp;b/slides.md"><title>x</title>"#;
        assert_eq!(entry_of(page), "C:/talks/a&b/slides.md");
        assert_eq!(entry_of("<title>Slidev</title>"), "");
    }

    #[test]
    fn navigating_patches_the_servers_nav_state() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0; 1024];
            while !request.ends_with(b"}") {
                let read = stream.read(&mut chunk).unwrap();
                request.extend_from_slice(&chunk[..read]);
            }
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n").unwrap();
            String::from_utf8(request).unwrap()
        });
        assert!(navigate(port, 7));
        let request = server.join().unwrap();
        assert!(request.starts_with("POST /@server-reactive/nav HTTP/1.0\r\n"));
        let body: serde_json::Value = serde_json::from_str(request.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["patch"]["page"], 7);
        assert_eq!(body["patch"]["lastUpdate"]["type"], "presenter");
    }

    #[test]
    fn a_taken_port_is_skipped_and_not_slidev() {
        let taken = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = taken.local_addr().unwrap().port();
        assert!(!free(port));
        assert_ne!(free_port(port), port);
        // It listens but never answers: not Slidev.
        assert!(!ping(port));
    }
}
