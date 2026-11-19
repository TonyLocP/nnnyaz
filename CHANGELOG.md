# Changelog

All notable changes to nnnyaz are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Help-search queries now use the rename prompt's editing keys: `Ctrl+W` word
  delete, `Ctrl+U`/`Ctrl+K` kills, `Ctrl+Z` undo, `Ctrl+A`/`Ctrl+E` movement,
  and shift-based selection.
- Confirmation prompts accept `Enter` as well as `y`.
- Inline prompts highlight the character under the caret, with a configurable
  `cursor` theme role.

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

[Unreleased]: https://github.com/TonyLocP/nnnyaz/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/TonyLocP/nnnyaz/releases/tag/v0.1.0
