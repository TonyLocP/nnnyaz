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
- [x] **Bulk rename** — with multiple entries selected, `r` edits their names
      in `$EDITOR` and applies the batch. Swaps go through temporary names;
      mismatched counts, empty/duplicate names, `/`, and collisions outside the
      batch are refused before anything moves.
- [x] **Shell parity** — `;` now runs the command in the background as a task
      (stdout/stderr visible in the task inspector) while `:` keeps Yazi's
      blocking, terminal-handover behavior.
- [x] **Rename cursor UX** — the rename prompt starts with the stem selected
      and the cursor before the extension, like nnn/Yazi. Dotfiles select the
      whole name. Prompts now have a real editing buffer: arrows, Ctrl+H/L,
      Home/End, Ctrl+Shift+H/L and Shift-selection, Backspace/Delete,
      Ctrl+A/E start/end, Ctrl+Shift+S select all, Ctrl+U/K/W, and Ctrl+Z undo
      (typing runs undo as one step). The selection is highlighted and the
      terminal cursor follows the caret. A dim hint on the right of the status
      line lists the rename binds, shortening itself to fit narrow terminals.
      rnn asks tmux for modifyOtherKeys and other terminals for kitty's
      keyboard protocol so Ctrl+Shift+letter is distinguishable from
      Ctrl+letter.
- [x] **PDF preview** — PDFs render as images through `pdftoppm`, the same
      backend Yazi uses, instead of a hex dump. `J`/`K` and preview scrolling
      turn pages, the title and info panel show the page number, and page
      images are cached per page in rnn's own cache (Yazi's PDF cache holds
      only the last page, so it cannot be shared safely).
- [x] **Snappier image preview** — large photos downscale with the box filter
      (several times faster than Triangle) and the disk-cache write now happens
      on a background thread, so showing a first view never waits on the
      re-encode. Preloads run on their own worker, start only after a 150 ms
      idle pause, and drop their batch if the target moves again, so even a
      25 MB JPEG (~700 ms decode) cannot delay the visible image. Display
      requests are debounced 30 ms like Yazi's `preview.image_delay`, PDFs are
      preloaded alongside image/video neighbours, and the sixel encoder maps
      pixels through a 32³ palette lookup table instead of a per-pixel search.

## Next

- [ ] Review the remaining Yazi/nnn differences after PDF preview. Candidates:
      picker `--chooser-file` edge cases, open-with rules, and previewer
      coverage (fonts and archive details).

## Local setup (outside this repo)

- `~/.local/bin/set-default-browser` chooses the default browser, moves
  browser-owned document types to it, and keeps browser-owned audio/video on
  `mpv`. `--dry-run` previews the changes.

## Known divergences

- `Q` quits and writes the cwd file (Yazi uses `Q` for `quit --no-cwd-file`);
  this is intentional so the fish wrapper can `cd` on quit.
- Yazi `pick` context is not supported; picker behavior is configured with
  `--chooser-file`/`-p` instead.
