# nnnyaz roadmap

A running list of parity and UX work. Checked items are done; unchecked items
are candidates, roughly in priority order.

## Recently completed

- [x] **Hardening and smoke tests** — the crate forbids `unsafe` code, state
      files are written atomically, and a CLI integration test covers `--help`
      and `--version`.
- [x] **Shell completions and man page** — bash, zsh, and fish completions and
      `man/nnnyaz.1` ship in the source tree and release archives.
- [x] **`--generate-config`** — prints a commented default `config.toml` that
      round-trips with no warnings; wired into help, completions, the man page,
      and a CLI test.
- [x] **MSRV and dependency audit** — `Cargo.toml` declares Rust 1.89, CI checks
      that floor, and a `cargo audit` job runs on every push.
- [x] **`--check-config`** — reports config warnings without starting the TUI,
      exits 1 on warnings, and is wired into help, completions, the man page,
      and a CLI test.
- [x] **Reload performance** — bounded parallel metadata collection for large
      directories (up to 16 workers above 512 entries) preserves ordering and
      behavior; `scripts/bench-reload.py` measures reload, collect, and sort
      phases. A 20k-file release reload went from ~24 ms to ~14 ms here.
- [x] **Fish wrapper** — `wrappers/nnnyaz.fish` ships with the release archives
      and provides cd-on-quit plus the `f` alias (`y` remains yazi).
- [x] **Delete confirmation Enter fix** — LF/Ctrl+J is accepted as Enter, with
      a pty regression check in `scripts/pty-smoke.py`.
- [x] **CI and releases** — GitHub Actions runs formatting, clippy, tests, and
      a release build on Linux and macOS. Tagged releases publish archives for
      Linux (x86_64/aarch64) and macOS (Intel/Apple Silicon) with SHA-256
      checksums; `v0.1.0` was the first release and `v0.1.1` is the current one.
      `CHANGELOG.md` tracks changes.
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
      nnnyaz asks tmux for modifyOtherKeys and other terminals for kitty's
      keyboard protocol so Ctrl+Shift+letter is distinguishable from
      Ctrl+letter.
- [x] **PDF preview** — PDFs render as images through `pdftoppm`, the same
      backend Yazi uses, instead of a hex dump. `J`/`K` and preview scrolling
      turn pages, the title and info panel show the page number, and page
      images are cached per page in nnnyaz's own cache (Yazi's PDF cache holds
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
      the default. nnnyaz keeps its faster defaults (1920×1080, box filter,
      quality 90) rather than Yazi's 600×900/triangle/75.
- [x] **Sixel previews inside tmux** — protocol detection prefers tmux's client
      terminal name over stale Kitty/Ghostty environment hints, so foot is
      recognized even though the pane's `TERM` and DA1 reply describe tmux.
      `NNNYAZ_DEBUG=1` reports the client terminal, and
      `NNNYAZ_IMAGE_PROTOCOL=sixel` remains an explicit override.
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
      in Yazi. `O` also appends installed desktop applications whose MIME types
      match the selection, after the configured openers; entries without
      `MimeType=` use `text/plain` or `text/*` for text editors. Picker `Enter`
      now picks directories instead of entering them.
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
- [x] **ICC color management** — embedded non-sRGB ICC profiles are converted
      to sRGB through `moxcms` before resizing and caching, so Display P3,
      Adobe RGB, and grayscale gamma-tagged images look like they do in Yazi.
      CMYK profiles are left alone and images already tagged sRGB skip the
      transform, matching Yazi's adapter. Unsupported bit depths fall back to
      the normal decoder path.
- [x] **Compressed tarball previews** — when 7z reports a compressed archive as
      a single inner `.tar` (`.tar.gz`, `.tgz`, `.tar.bz2`, `.tar.xz`,
      `.tar.zst`, …), nnnyaz pipes `7z e -so` into `7z l -ttar -si` and shows the
      tar's contents instead of the inner archive file. If the inner listing
      fails, it falls back to the outer listing.
- [x] **CLI help and version** — `-h`/`--help` prints usage, options, and the
      relevant environment variables; `-V`/`--version` prints the version.
      Previously both were rejected as unknown options.
- [x] **Sort keys set the matching linemode** — `, m`/`, M` (mtime),
      `, b`/`, B` (btime), and `, s`/`, S` (size) now switch the linemode as
      well as sorting, like Yazi's default keymap. `, e` and `, a` still only
      sort, since Yazi does not have a matching linemode for them.
- [x] **nnn startup overrides** — `-H` shows hidden files, `-T <key>` selects
      the startup sort order (`a`/`d`/`s` size, `e` extension, `t` time,
      `v` natural; an uppercase key reverses), and `-l <n>` sets the number of
      lines moved per mouse-wheel tick. `d` maps to apparent size because nnnyaz
      does not track disk blocks.
- [x] **Tab expansion in text previews** — tabs expand to the next multiple of
      `preview.tab_size` (default 2, like Yazi) before syntax highlighting, so
      tab-indented source keeps its shape. Previously each tab collapsed to a
      single space.
- [x] **Wrapping text previews** — `preview.wrap` accepts Yazi's `"no"`/`"yes"`
      (and a plain boolean). When enabled, long lines wrap instead of being
      clipped at the pane edge, and preview scrolling still counts logical
      lines like Yazi's spot.
- [x] **Yazi-style spot** — `F` fills the screen with the preview. `j`/`k`
      scroll it, `h`/`l` swipe to the previous/next file, `c c` copies the
      hovered path, and `Esc`, `Tab`, or `Ctrl+c` closes it. Yazi keymaps that
      bind `spot` now open this view (Yazi's `<Tab>`), while nnnyaz's `<Tab>`
      keeps opening the metadata info overlay. The spot reuses the preview
      worker, so images, video thumbnails, PDFs, and text all work.
- [x] **nnn's `-i` info bar** — `-i`/`--file-info` appends `file -b`'s
      description of the hovered entry to the status line, cached by path and
      mtime so moving around a directory does not spawn a process per redraw.
- [x] **More Yazi `mgr` keys** — `sort_sensitive` toggles case-sensitive name
      and extension comparisons, `sort_dir_first = false` mixes directories in
      with files, and `show_symlink = false` hides the ` -> target` suffix.
- [x] **`preview.cache_dir`** — an absolute path (or empty for the temp
      default) chooses nnnyaz's own preview cache. Setting it disables sharing
      Yazi's cache, and `NNNYAZ_CACHE_DIR` still takes precedence.
- [x] **`mgr.sort_fallback`** — equal size/time/extension keys now break ties
      with an alphabetical or natural name comparison, matching Yazi (the
      fallback is case-sensitive, unlike `sort_sensitive`).
- [x] **nnn display startup flags** — `-d` starts in a new detail linemode
      (time, permissions, owner:group, size), `-U` starts with the owner
      linemode, and `-J` keeps the cursor on an entry after toggling its
      selection (nnn's "no auto-advance"). `detail` is also accepted by
      `mgr.linemode` and `linemode detail`.
- [x] **`mgr.sort_translit`** — natural sorting can strip accents and expand
      ligatures (`é` → `e`, `Æ` → `AE`, `ß` → `ss`) first, using a table
      ported from Yazi.
- [x] **nnn's `-b` bookmark startup** — `-b <key>` (or `-b<key>`) starts in an
      `NNN_BMS` bookmark or a name in nnn's bookmarks directory. Like nnn, an
      explicit path argument wins, and an unknown key exits with an error.
- [x] **`tasks.suppress_preload`** — disables preloading the previews of
      neighbouring image/video/PDF/font files, trading instant navigation for
      less background work.
- [x] **Yazi `[input]` titles** — `cd_title`, `create_title`, `rename_title`,
      `filter_title`, `find_title`, and `shell_title` customize prompt labels.
      The two-element arrays pick by mode (file vs. directory, next vs.
      previous, background vs. blocking shell); `*_origin`, `*_offset`, and
      `cursor_blink` are accepted but do not apply to status-line prompts.
- [x] **nnn's `-Q` and `-o`** — `-Q` skips the running-task quit warning, and
      `-o` stops `l`/`→` from opening files, so only Enter opens them (in
      picker mode, only Enter picks too).
- [x] **Yazi `[confirm]` titles** — `trash_title` and `delete_title` customize
      the delete prompts, with `{n}` replaced by the count and `{s}` by `"s"`
      for plural targets (an empty string for one). `overwrite_title` is
      accepted but unused because nnnyaz has no overwrite prompt.
- [x] **nnn sessions** — `-s <name>` loads a session (`@` is the last session)
      and `-S` saves the current tabs on exit, defaulting to `@`. Tabs, cwd,
      cursor, filter, hidden files, sort, linemode, selections, and history are
      restored. Loading a named session stores the previous state in `@`
      first, matching nnn.
- [x] **Session manager** — `t S` opens nnn's `'s'ave/'l'oad/'r'estore?`
      prompt; `r` reloads `@` without saving the current state over it. `l`
      lists the saved sessions immediately and narrows the list as you type;
      `j`/`k` always type into the filter, while `J`/`K`, `Ctrl+j`/`Ctrl+k`,
      and `↑`/`↓` move. Enter loads, and an exact typed name still wins.
      `Ctrl+d` deletes the highlighted session after a `y/N` confirmation.
- [x] **nnn's `-n`, `-A`, and `-R`** — `-n` starts in type-to-nav (persistent
      filtering, Enter opens the match), `-A` disables auto-entering a lone
      matching directory, and `-R` is accepted for compatibility because nnnyaz
      never rolls over at the list edges (Yazi behavior).
- [x] **nnn's `-g` and `-z` filters** — regex and fuzzy filtering, with
      nnn's `/` mode cycle and `:` case toggle inside the filter prompt.
      Fuzzy matches are ranked by nnn's score (best first); an invalid regex
      leaves the list unfiltered, like nnn.
- [x] **nnn's `-e` and `-u`** — `-e` sends text files to
      `$VISUAL`/`$EDITOR`/`vi` on open (empty files show "empty file" and are
      left alone), while `-u` is a compatibility no-op because nnnyaz already
      uses the selection without prompting.
- [x] **nnn's `-f` history file** — `-f` loads and saves the last 16 shell
      commands at `<config>/.history`, and Up/Down in the `;`/`:` prompt
      recall them with nnn's wrap-around behavior.
- [x] **nnn's `-K` keymap check** — `-K` walks the loaded keymap and prints
      `key collision! [...]` for bindings that can never fire: an action
      shadowed by a longer sequence, or sibling chords that match the same
      key. It exits 0 like nnn.
- [x] **Scrollable, searchable help** — `~`/`F1` opens a sectioned overlay
      with `j/k` or `↑/↓` scrolling, half/full-page and Home/End jumps, and
      a `/` search over keys and descriptions (`n`/`N` cycle matches).

- [x] **Renamed to `nnnyaz`** — the crate, binary, GitHub repo, config
      directory (`~/.config/nnnyaz`), cache paths, and environment variables
      (`NNNYAZ_*`) all use the new name. The earlier `rnn` and `yannni`
      names are gone with no fallbacks.
- [x] **nnn's `-E` internal editor** — `-E` uses `$EDITOR` (instead of
      `$VISUAL`, then `$EDITOR`, then `vi`) for internal undetached edits such
      as bulk rename and bulk create. As an extension, `r` on a single entry
      edits its name in that editor rather than opening the inline prompt.
- [x] **nnn's `-c` CLI opener** — `NNN_OPENER` is honored as an external
      opener (after nnnyaz's own `OPENER`), and `-c` runs it attached to the
      terminal and waits, so TUI openers work. `-c` also disables `-e`, like
      nnn.
- [x] **nnn's `-t` idle lock** — `-t <secs>` locks the terminal after that
      many idle seconds by running `$NNN_LOCKER` (default `vlock`) attached,
      then restores the TUI. Any key or mouse event resets the timer, and `0`
      disables it.
- [x] **Help-search editing keys** — the `/` query in the help overlay now
      supports the rename prompt's `Ctrl+W` word delete, `Ctrl+U`/`Ctrl+K`
      kills, `Ctrl+Z` undo, `Ctrl+A`/`Ctrl+E` movement, and shift-based
      selection, with a compact hint line showing what fits.

- [x] **Yazi `[help]` keymap context** — `keymap.toml`'s `[help]` section is
      honored for the help overlay (`close`, `escape`, `arrow …`), including
      `prepend_keymap` / `append_keymap` / full replacement. Unbound keys keep
      nnnyaz's built-in search and paging keys, and which-key prefixes work
      inside help.
- [x] **Yazi `[which]` sort config** — `sort_by` (`none` / `key` / `desc`),
      `sort_sensitive`, `sort_reverse`, and `sort_translit` order the prefix
      menu using the same natural-sort and transliteration rules as file
      names.

## Next

- [ ] **Deferred metadata** — partially addressed by parallel collection, but
      metadata is still gathered for every entry before first paint. The
      remaining ~2× gap to Yazi at 50k entries needs a lazy-metadata layer with
      cache invalidation; key latency is already sub-millisecond.
- [ ] **More integration coverage** — a manual `scripts/pty-smoke.py` now
      covers help/search and picker; CI-wired pty tests and confirmation flows
      remain (kept out of CI to avoid platform-dependent flakiness).
- [x] **Divergence review** — the "Intentional divergences" list below was
      reviewed and kept on purpose; revisit only when upstream Yazi/nnn changes.

## Local setup (outside this repo)

- `~/.local/bin/set-default-browser` chooses the default browser, moves
  browser-owned document types to it, and keeps browser-owned audio/video on
  `mpv`. `--dry-run` previews the changes. nnnyaz now ships the fish wrapper
  that used to live only in the local setup.

## Intentional divergences

Reviewed and kept on purpose — each one is a deliberate decision, not pending
work. Revisit only if upstream Yazi/nnn behavior changes.

- `Q` quits and writes the cwd file (Yazi uses `Q` for `quit --no-cwd-file`);
  this is intentional so the fish wrapper can `cd` on quit.
- `l`/`→` also open a hovered file; Yazi's `enter` only enters directories.
  `Enter`/`o` follow Yazi's selection-aware `open`.
- Marks use `m` + key, but `m s`, `m p`, `m b`, `m m`, `m o`, and `m n` stay
  bound to linemode, so those six keys cannot name a mark. This keeps the
  default keymap compatible with Yazi's linemode prefix.
- Yazi `pick` context is not supported; picker behavior is configured with
  `--chooser-file`/`-p` instead.
- The default filter is smart-case (Yazi-style) rather than nnn's always
  case-insensitive substring match; `:` in the filter prompt cycles to the
  explicit `ic`/`noic` modes.
- `-E` also makes `r` on a single entry edit its name in `$EDITOR`. nnn's
  `-E` only changes the editor used for internal edits (bulk rename/create).
- `NNN_OPENER` is honored, but nnnyaz's own `OPENER` wins when both are set;
  nnn does not read `OPENER`.
