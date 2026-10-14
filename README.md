# Slidev

[Slidev](https://sli.dev) decks in den, after Slidev's own extension for VS Code. Open the outline with the **Slides** button in the title bar, *Slidev: Show Slides* in den's menu, or <kbd>Ctrl</kbd>+<kbd>Alt</kbd>+<kbd>Shift</kbd>+<kbd>S</kbd>.

- **The outline:** every slide of the deck with its number, title and layout, as Slidev numbers them. Hidden slides (`hide: true`) are faded and unnumbered, and slides a `src:` imports show the file they come from. Click a slide for its file, frontmatter and notes; double-click it (or Enter) to put the editor's cursor on it.
- **The preview:** **Preview** starts the dev server in a terminal (or uses the one already serving this deck) and shows the slides in a browser tab, where den puts browsers. The preview goes to the slide you click in the outline, and, with **Sync** on, to the slide the editor's cursor is in, without reloading.
- **Open in the browser** shows the slides in your system's browser; *Export Slides to PDF* runs `slidev export` in a terminal.
- **Decks** are files named `slides.md`, and the Markdown files of a package that depends on `@slidev/cli`. The deck of the active tab is the one shown; with several in the folder, the toolbar picks between them.

The outline follows the deck as saved: save to see an edit there. Slidev itself updates the preview as you save.

## Settings

| Setting | |
| --- | --- |
| **Port** | Where the dev server starts (3030), or the next free port |
| **Dev server command** | How to start Slidev, with `{args}` for the deck and port, such as `pnpm slidev {args} --remote`. Empty picks `pnpm exec slidev`, `yarn slidev`, `bunx slidev` or `npx slidev` by the project's lockfile |
| **Preview follows the cursor** | Whether Sync starts on (the toolbar's Sync button switches it per window) |
| **Title bar button** | The Slides button, on folders with a deck (after a restart) |

## How it works

den draws the outline from data (`den_extension::view`). This extension splits the deck into slides by `@slidev/parser`'s rules (`src/slides.rs`), and finds, starts and waits for the server (`src/server.rs`). A server is recognised by its page, which names the deck it serves. The preview is a browser tab the extension owns (`open_browser` with a tab name). It moves from slide to slide by posting Slidev's `navigate` message into the page (`browser_script`) as `cursor_moved` events arrive. So it needs a den newer than 0.19, which has those.

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
