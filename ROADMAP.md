# rnn roadmap

A running list of parity and UX work. Checked items are done; unchecked items
are candidates, roughly in priority order.

## Recently completed

- [x] nnn-style status line: cursor position (`3/12`), local mtime, mode,
      size, and extension or symlink target.
- [x] Find match counter in the `/` and `?` prompt and while cycling with
      `n`/`N`.
- [x] Picker mode: `--chooser-file <path>` (Yazi) and `-p <path>` (nnn),
      plus `-0` for NUL-separated output. `Space` selects entries; `Enter` on a
      file, or quitting with a selection, writes the paths. `-` writes to
      stdout.
- [x] Yazi default keymap loads without warnings; tabs, trash, tasks, zoxide,
      and the open-with menu.
- [x] Browser-associated files (PDF, XML, …) open in the system default
      browser instead of whichever browser owns the MIME type.
- [x] Docs refresh: picker mode, the status line, and browser routing are
      documented; the stale “zoxide is not implemented” note is gone.
- [x] **Rename cursor UX** — the rename prompt starts with the stem selected
      and the cursor before the extension, like nnn/Yazi. Dotfiles select the
      whole name. Prompts now have a real editing buffer: arrows, Home/End,
      Shift-selection, Backspace/Delete, and Ctrl+A/E/U/K/W. The selection is
      highlighted and the terminal cursor follows the caret.

## Next

- [ ] **Bulk rename** — edit the selected names in `$EDITOR` and apply the
      changes in one batch.
- [ ] **Shell parity** — align `:`/`;` blocking semantics with Yazi/nnn.

## Local setup (outside this repo)

- `~/.local/bin/set-default-browser` chooses the default browser, moves
  browser-owned document types to it, and keeps browser-owned audio/video on
  `mpv`. `--dry-run` previews the changes.

## Known divergences

- `Q` quits and writes the cwd file (Yazi uses `Q` for `quit --no-cwd-file`);
  this is intentional so the fish wrapper can `cd` on quit.
- Yazi `pick` context is not supported; picker behavior is configured with
  `--chooser-file`/`-p` instead.
