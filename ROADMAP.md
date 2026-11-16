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

## Next

- [ ] **Rename cursor UX** — start the rename prompt with the cursor before the
      extension and the stem selected, like nnn/Yazi, instead of selecting the
      whole name.
- [ ] **Bulk rename** — edit the selected names in `$EDITOR` and apply the
      changes in one batch.
- [ ] **Shell parity** — align `:`/`;` blocking semantics with Yazi/nnn.
- [ ] **Docs refresh** — the README still says zoxide is unimplemented and
      should document picker mode and the status line.

## Known divergences

- `Q` quits and writes the cwd file (Yazi uses `Q` for `quit --no-cwd-file`);
  this is intentional so the fish wrapper can `cd` on quit.
- Yazi `pick` context is not supported; picker behavior is configured with
  `--chooser-file`/`-p` instead.
