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
- [x] **EXIF orientation and configurable preview** — source images apply their
      EXIF orientation, so phone and camera photos are shown upright (cache
      entries are written already rotated). `[preview]` now accepts Yazi's
      `max_width`, `max_height`, `image_filter`, `image_quality`, and
      `image_delay` in addition to `cache_size`; invalid values warn and keep
      the default. rnn keeps its faster defaults (1920×1080, box filter,
      quality 90) rather than Yazi's 600×900/triangle/75.
- [x] **Font and archive previews** — font files (`ttf`, `otf`, `ttc`, `woff`,
      …) render a glyph sheet through ImageMagick (`magick`/`convert`) and go
      through the same image cache and preload path as photos. Archive listings
      now parse `7z l -slt` and show an indented tree with human-readable sizes,
      synthesizing parent directories and skipping `__MACOSX` metadata.
- [x] **Selection-aware open and Yazi opener placeholders** — `Enter`/`o` open
      the whole selection (or the hovered entry), while `l`/`→` keep entering
      the hovered directory. `O` intersects the `[open]` rules across every
      target. Opener commands expand `%s`/`%S`, `%d`/`%D`, `%%`, and 1-based
      numbered forms; commands with an unnumbered `%s`/`%S` run once with all
      targets (Yazi's `spread`), and the rest run once per file. Hovered,
      yanked, and tab placeholders expand to empty in an opener's context, as
      in Yazi. Picker `Enter` now picks directories instead of entering them.
- [x] **Marks and bookmarks** — `m` + key saves a directory mark and `'` + key
      jumps to it; marks persist in `<config>/marks` and stale or missing
      marks report a status message instead of changing directory. Keys that
      are linemode bindings (`m s`, `m p`, `m b`, `m m`, `m o`, `m n`) keep
      their linemode meaning. `b` opens a bookmark prompt that accepts
      `NNN_BMS` keys and symlinked bookmark names, with an empty prompt opening
      nnn's bookmarks directory; `B` creates a symlink bookmark under
      `${XDG_CONFIG_HOME:-~/.config}/nnn/bookmarks`, shared with nnn.
- [x] **Picker edge cases** — chooser output now places the newline/NUL only
      between paths, with no trailing separator, matching Yazi and nnn. Empty
      `--chooser-file`/`-p` values are rejected up front instead of failing at
      exit. Picker `Enter`/`o` still writes the selection (or hovered entry)
      and quits, `O` still opens the interactive menu, and a plain quit writes
      an existing selection like nnn while leaving the file untouched when
      nothing is selected.

## Next

- [ ] ICC color management for image previews (currently only EXIF orientation
      is applied).
- [ ] List the contents of compressed tarballs (for example `.tar.gz`) in the
      archive preview instead of treating the archive as a single entry.

## Local setup (outside this repo)

- `~/.local/bin/set-default-browser` chooses the default browser, moves
  browser-owned document types to it, and keeps browser-owned audio/video on
  `mpv`. `--dry-run` previews the changes.

## Known divergences

- `Q` quits and writes the cwd file (Yazi uses `Q` for `quit --no-cwd-file`);
  this is intentional so the fish wrapper can `cd` on quit.
- `l`/`→` also open a hovered file; Yazi's `enter` only enters directories.
  `Enter`/`o` follow Yazi's selection-aware `open`.
- Marks use `m` + key, but `m s`, `m p`, `m b`, `m m`, `m o`, and `m n` stay
  bound to linemode, so those six keys cannot name a mark. This keeps the
  default keymap compatible with Yazi's linemode prefix.
- Yazi `pick` context is not supported; picker behavior is configured with
  `--chooser-file`/`-p` instead.
