# Slidev

[Slidev](https://sli.dev) decks in den, after Slidev's own extension for VS Code. Open the outline with the **Slides** button in the title bar (after den's Explorer, Search and Source Control), *Slidev: Show Slides* in den's menu, or <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd>.

- **The outline:** in the sidebar, beside the Explorer. Every slide of the deck with its number, title and layout, as Slidev numbers them. Hidden slides (`hide: true`) are faded and unnumbered, and slides a `src:` imports show the file they come from. Click a slide for its file, frontmatter and notes; double-click it (or Enter) to put the editor's cursor on it.
- **The preview:** the **Preview** button (▷) starts the dev server in a terminal (or uses the one already serving this deck) and shows the slides in your browser. Every page of the deck there goes to the slide you click in the outline and to the slide the editor's cursor is in, without reloading.
- **The slide under the cursor:** a deck's tab has **Slide** beside Source and Preview. It opens the slide the cursor is on in your browser, starting the dev server if it isn't running.
- *Export Slides to PDF* runs `slidev export` in a terminal.
- **Decks** are files named `slides.md`, and the Markdown files of a package that depends on `@slidev/cli`. The deck of the active tab is the one shown; with several in the folder, the toolbar picks between them.

The outline follows the deck as saved: save to see an edit there. Slidev itself updates the preview as you save.

## Settings

| Setting | |
| --- | --- |
| **Port** | Where the dev server starts (3030), or the next free port |
| **Dev server command** | How to start Slidev, with `{args}` for the deck and port, such as `pnpm slidev {args} --remote`. Empty picks `pnpm exec slidev`, `yarn slidev`, `bunx slidev` or `npx slidev` by the project's lockfile |
| **Preview follows the cursor** | Whether the preview goes to the slide the cursor is in (on) |

## How it works

den draws the outline from data (`den_extension::view`), in the sidebar since the manifest puts the view there (`"location": "sidebar"`). This extension splits the deck into slides by `@slidev/parser`'s rules (`src/slides.rs`), and finds, starts and waits for the server (`src/server.rs`). A server is recognised by its page, which names the deck it serves. The preview is Slidev's page in the system's browser. As `cursor_moved` events arrive, the extension moves it the way Slidev's presenter mode does: it patches the `nav` state the server shares with its pages (a POST to `/@server-reactive/nav`), and the pages follow. Slide is a file view (`set_file_views`): den sends `file_view_shown` with the cursor's line, and the extension answers with the slide's address (`load_file_view`), which den 0.32 and later open in the browser. A den older than 0.20 shows no Slide button, and the outline as a tab, opened from den's menu or the keys.

```sh
cargo test
cargo build --release
```

To build against a local den checkout, add a `.cargo/config.toml` (it is ignored) with:

```toml
[patch."https://github.com/patrickiel/den"]
den-extension = { path = "../den/crates/den-extension" }
```

Then run `node ../den/scripts/sideload.ts --dev .` from this folder.
