<p align="center">
  <img src="docs/logo.png" width="256" height="256" alt="Marginalia">
</p>

# Marginalia

A desktop ebook and PDF reader connected to an existing Pi session in Neovim.

> _Marginalia (n.) — notes, comments, and annotations made in the margins of a
> book; from Latin marginalis, "of the margin."_

This fork keeps Marginalia's document reader and annotation interface while
moving agent conversations into Pi. Marginalia sends selected text and bounded
reading context over pi.nvim's local Unix socket; Pi owns the conversation,
model, tools, extensions, and responses.

## Features

Reading:

- EPUB, PDF, MOBI, AZW/AZW3, FB2, CBZ, TXT, and Markdown
- Page and scroll modes
- Highlights, bookmarks, annotations, and Markdown export
- Full-text search and configurable presentation

Pi integration:

- Select text and choose **Ask Pi**
- Explicitly select a live Pi session by working directory and PID
- Send the document title, author, section, page, selected text, surrounding
  text, and question
- Mirror Pi's assistant replies in the reading panel
- Continue the same conversation in the existing Pi terminal in Neovim
- Keep provider credentials and agent configuration out of Marginalia

## Requirements

- Linux or macOS
- The socket-bridge build in `~/Tools/pi-nvim`, with a live Pi session

Start Pi with `:PiSession` in Neovim. Alternatively, start Pi manually with
pi.nvim's bridge extension:

```bash
pi --extension /path/to/pi-nvim/extension.ts
```

Open the **Ask Pi** panel in Marginalia, refresh the session list, and select the
intended session. Marginalia never falls back to a different session if that
selection disconnects.

## Development

Requires Rust, Node.js 20+, and pnpm.

```bash
make install
make dev
make build
make lint
make fmt
```

The Pi adapter is deliberately separated by process and protocol:

- Marginalia owns document rendering and context extraction.
- Its Tauri backend discovers and connects to pi.nvim's private Unix sockets.
- Prompt delivery uses pi.nvim's existing short-lived `prompt` request.
- A separate subscription mirrors every completed assistant message from the
  selected live Pi session, including replies to prompts entered in Neovim.

## Built with

- [Readest](https://github.com/readest/readest) — ebook reader
- [Foliate](https://github.com/johnfactotum/foliate-js) and PDF.js — document rendering
- [Pi](https://github.com/badlogic/pi-mono) and pi.nvim — agent conversation
- [Tauri v2](https://tauri.app) — native desktop runtime

## License

AGPL-3.0 — same as Readest and upstream Marginalia.
