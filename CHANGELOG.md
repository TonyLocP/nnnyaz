# Changelog

All notable changes to nnnyaz are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- The open-with menu shows opener descriptions such as `Doom Emacs` instead of
  internal names like `desktop_doom_emacs`.
- Delete confirmations now accept LF as Enter. Some terminals send LF rather
  than CR, which crossterm reports as Ctrl+J in raw mode; it previously
  canceled the prompt.
- Image previews inside tmux detect foot through tmux's client terminal name.
  The pane's `TERM`, DA1 reply, and inherited Kitty/Ghostty variables describe
  tmux or where the server started, so foot's Sixel support was previously
  missed or overridden and previews stayed disabled.
- The shipped fish wrapper enables tmux graphics passthrough only for the pane
  running nnnyaz and restores the previous setting on exit. A global
  `allow-passthrough` setting is no longer needed.

## [0.1.1] - 2026-11-20

### Added

- Help-search queries now use the rename prompt's editing keys: `Ctrl+W` word
  delete, `Ctrl+U`/`Ctrl+K` kills, `Ctrl+Z` undo, `Ctrl+A`/`Ctrl+E` movement,
  and shift-based selection.
- Confirmation prompts accept `Enter` as well as `y`.
- A CLI smoke test for `--help` and `--version`.
- Bash, zsh, and fish completions plus a man page, included in release
  archives.
- `--generate-config` prints a commented default `config.toml` to stdout.
- CI checks the declared MSRV (1.89) and audits dependencies with `cargo audit`.
- A manual `scripts/pty-smoke.py` checks the help overlay and picker end to end.
- `scripts/bench-reload.py` measures reload time and its `collect`/`sort` phases.
- `--check-config` reports config warnings and exits 1 when any are found.
- A shipped fish wrapper (`wrappers/nnnyaz.fish`) provides cd-on-quit and defines
  the `f` alias while leaving `y` for yazi.

### Changed

- Session, marks, history, and cwd files are written atomically via a sibling
  temp file, so a crash cannot leave a truncated state file.
- The crate forbids `unsafe` code.
- Large directories collect entry metadata in parallel (bounded to 16 workers),
  preserving readdir order and all existing metadata-dependent behavior.
- The crate version is now 0.1.1.

## [0.1.0] - 2026-11-18

Initial release.

### Added

- Yazi-style modal navigation with `mgr`, prefix keymaps, tabs, marks, and
  nnn bookmarks.
- nnn startup and display flags, including sort, linemode, bookmarks,
  sessions, history, filtering, locking, and the picker.
- Native preview pane for text, images (Kitty and Sixel), PDFs, fonts,
  archives, hex dumps, and media, with an on-disk cache.
- Yazi-compatible `keymap.toml`, `[opener]`/`[open]` rules, `[help]`, and
  `[which]` configuration.
- TOML configuration plus an optional `init.lua` API.
- Trash with restore and empty, background tasks with progress and
  cancellation, bulk rename, and file operations.
- Picker mode for shell pipelines (`--chooser-file`, `-p`, `-0`).
- MIT license and third-party notices.

[Unreleased]: https://github.com/TonyLocP/nnnyaz/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/TonyLocP/nnnyaz/releases/tag/v0.1.1
[0.1.0]: https://github.com/TonyLocP/nnnyaz/releases/tag/v0.1.0
