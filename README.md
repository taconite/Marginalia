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

### Local PDF verification

PDF questions include the readable local file path, including without a selection.
Resolution failures leave the draft and selected text intact; no prompt is sent.
For an isolated session, create and enter `/tmp/marginalia-local-verification`, then run:
```bash
pi --no-extensions --extension ~/Tools/pi-nvim/extension.ts --session /tmp/marginalia-local-verification/session.jsonl --name marginalia-local-verification --tools read,bash
```
Select that working directory and PID in the reader. Test a selection, a question
requiring another page, page rendering plus an image-read tool call, and a follow-up.
Check the transcript's path against the open PDF and confirm replies in the reader.
Run adapter checks with `cd app && corepack pnpm exec vitest run src/services/pi/bridge.test.ts`.
Run frontend validation with `make lint/frontend`. Unit tests do not replace reader tests.

## SSH from a Mac reader to Ubuntu Pi

Configure an SSH host alias on the Mac; first confirm `ssh your-alias true` works
without a password or host-key prompt. Marginalia uses system `ssh` in batch mode.
Ubuntu needs OpenSSH with Unix-socket forwarding and a running Pi session with the existing pi.nvim bridge.
Pi messages travel through native SSH forwarding; no Python helper is installed or executed.
In **Ask Pi**, choose **SSH**, enter the alias, then leave the field to connect.
Select the remote session by working directory and PID. Local mode uses the original bridge.

For each PDF question, Marginalia hashes the local file and checks the Ubuntu cache
at `~/.cache/marginalia/pdfs/<sha256>.pdf`. It streams missing files over SSH using standard Ubuntu file commands and verifies
their hash before publishing them. Cached PDFs are not retransferred; Pi receives
the Ubuntu path. Closing the reader stops its SSH connections and leaves Pi running.
**Cancel** stops document preparation and retains the draft. Failed sends are never
automatically retried; check Pi before retrying because delivery may have occurred.

Validate with `cd app/src-tauri && cargo test pi_bridge --lib`; also run the frontend checks above.
On the actual Mac reader, repeat selection, cross-page lookup, Figure 2 image access,
and follow-up tests. Verify the cached SHA-256, no second upload, interrupted uploads,
SSH disconnection, and reader-close cleanup. Repeat local Ubuntu tests for regressions.
Unit and loopback tests do not establish actual MacBook acceptance.

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
