# nnnyaz

[![CI](https://github.com/TonyLocP/nnnyaz/actions/workflows/ci.yml/badge.svg)](https://github.com/TonyLocP/nnnyaz/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/TonyLocP/nnnyaz)](https://github.com/TonyLocP/nnnyaz/releases)

A small nnn/Yazi-style terminal file manager with a native preview pane.

nnnyaz keeps a Yazi-like feel: modal `mgr` navigation, prefix keymaps
(`g`, `,`, `c`, `m`, `t`), tabs, a trash bin, a background task manager, and
an `open with` menu. Configuration is declarative TOML, with an optional
`init.lua` for overrides and small custom actions.

> **AI-built project:** essentially all of the code, tests, and docs were
> written by AI. I set the direction and test the result — see
> [AI usage](#ai-usage).

## Features

- Yazi-style modal navigation and prefix menus, with nnn's startup flags
- Tabs, marks, nnn bookmarks, trash, bulk rename, and a task manager
- Preview pane for text, images (Kitty/Sixel), PDFs, fonts, archives, and
  media, with a shared on-disk cache
- Yazi-compatible `keymap.toml`, `[opener]`/`[open]` rules, sessions, and
  `[help]`/`[which]` configuration
- Picker mode (`--chooser-file` / `-p`, `-0`) for shell pipelines
- TOML configuration plus an optional `init.lua` API

## Install

Prebuilt binaries for Linux (x86_64 and aarch64) and macOS (Intel and Apple
Silicon) are attached to each
[release](https://github.com/TonyLocP/nnnyaz/releases). Each archive contains
the `nnnyaz` binary, the license, and the changelog.

From source with Cargo:

```sh
cargo install --git https://github.com/TonyLocP/nnnyaz
```

Or build a local checkout:

```sh
cargo build --release
install -Dm755 target/release/nnnyaz ~/.local/bin/nnnyaz
```

Shell completions, a man page, and a fish wrapper are included:

```sh
# fish
cp completions/nnnyaz.fish ~/.config/fish/completions/

# bash (from the repo root)
source completions/nnnyaz.bash

# zsh: put completions/_nnnyaz on your $fpath, then re-run compinit
cp completions/_nnnyaz ~/.zsh/completions/

# man page
cp man/nnnyaz.1 ~/.local/share/man/man1/

# fish wrapper for cd-on-quit (also defines the `f` alias)
mkdir -p ~/.config/fish/functions
cp wrappers/nnnyaz.fish ~/.config/fish/functions/nnnyaz.fish
```

Run it in a directory:

```sh
nnnyaz [path]
```

`nnnyaz --help` lists the command-line options, `nnnyaz --version` prints the
version, and `nnnyaz --generate-config` prints a commented default `config.toml`
to stdout (redirect it into your config directory to get started).
`nnnyaz --check-config` reports config warnings without starting the TUI and
exits 1 if any were found. The main options are `--cwd-file` (see below) and the
picker flags `--chooser-file` / `-p` and `-0` / `--null`.

nnn's startup and display flags are also supported:

- `-H` — show hidden files.
- `-T <key>` — set the sort order (`a`/`d`/`s` size, `e` extension, `t` time,
  `v` natural; uppercase reverses).
- `-l <n>` — how many rows the mouse wheel moves.
- `-i` — append the hovered file's `file -b` description to the status line.
- `-d` / `-U` — start in the detail linemode (time, permissions, owner, size)
  or the owner linemode.
- `-J` — keep the cursor on an entry after toggling its selection.
- `-b <key>` — start in a bookmark (an `NNN_BMS` key or a name in nnn's
  bookmarks directory); a path argument takes precedence.
- `-Q` — skip the warning about running tasks on quit.
- `-o` — make `l`/`→` open only directories, so files open with Enter alone.
- `-n` — nnn's type-to-nav mode (persistent filtering; Enter opens the match).
- `-A` — don't auto-enter a lone matching directory.
- `-R` — accepted for nnn compatibility; nnnyaz never rolls over at the list
  edges.
- `-g` / `-z` — regex / fuzzy filtering instead of the default smart-case
  substring match.
- `-e` — open text files in `$VISUAL`/`$EDITOR`/`vi` instead of the external
  opener (empty files are left alone).
- `-u` — accepted for compatibility; nnnyaz already acts on the selection
  without prompting.
- `-f` — keep a persistent shell-command history at `<config>/.history`
  (Up/Down in the `;`/`:` prompt recall the last 16 commands).
- `-K` — report keymap collisions and exit: a binding that can never fire
  because another binding claims the same key, or because a longer sequence
  (such as `g g`) shadows it.
- `-E` — use `$EDITOR` rather than `$VISUAL` for internal undetached edits
  (bulk rename/create); `r` on a single entry edits its name in that editor
  instead of the inline prompt.
- `-c` — treat `NNN_OPENER` as a terminal program: run the opener attached and
  wait for it (so TUI openers work) and disable `-e`.
- `-t <secs>` — lock the terminal after that many idle seconds with
  `$NNN_LOCKER` (default `vlock`); `0` disables locking.
- `-s <name>` / `-S` — load a saved session / save the current tabs on exit
  (`@` is the last session; saving defaults to `@`). Loading a named session
  first stores the previous state in `@`. Sessions live under
  `<config>/sessions`.

While the filter prompt is open, `/` cycles string → fuzzy → regex (matching
nnn) and `:` cycles smart case → case-insensitive → case-sensitive; the active
mode is shown next to the filter text.

Inside nnnyaz, `t S` opens nnn's session menu (`s` save, `l` load, `r` restore
`@`). `l` lists the saved sessions and filters them as you type: `j`/`k` always
edit the filter text, while `J`/`K`, `Ctrl+j`/`Ctrl+k`, or `↑`/`↓` move through
the list. Enter loads the highlighted session, and `Ctrl+d` deletes it after a
`y`/Enter confirmation.

## Quitting and the current directory

`q` quits and leaves the shell where it was. `Q` quits and records nnnyaz's
current directory so a shell wrapper can `cd` there. A child process cannot
change its parent shell's directory, so this needs a small wrapper.

Pass a file with `--cwd-file <path>` (or set `NNNYAZ_CWD_FILE`); `Q` writes the
current directory to it. The file is only written by `Q`, never by `q`, so the
wrapper should remove it before starting nnnyaz. A ready-to-copy fish function
lives in `wrappers/nnnyaz.fish`. It defines both
`nnnyaz` and the short alias `f`; keep `y` for yazi:

```sh
mkdir -p ~/.config/fish/functions
cp wrappers/nnnyaz.fish ~/.config/fish/functions/nnnyaz.fish
```

Bash/zsh wrappers use the same idea:

```sh
nnnyaz() {
  local cwd_file
  cwd_file=$(mktemp) || return
  rm -f "$cwd_file"
  command nnnyaz --cwd-file "$cwd_file" "$@"
  local status=$?
  [ -s "$cwd_file" ] && cd "$(cat "$cwd_file")"
  rm -f "$cwd_file"
  return $status
}
```

`quit_cwd` is also available as a normal action, so `Q` can be rebound in
`keymap.toml`.

## Marks and bookmarks

`m` followed by a key saves the current directory as a mark; `'` followed by
the same key jumps back to it. Marks are stored in `<config>/marks` as one
`key<TAB>path` line per entry, so they survive restarts. Because `m` is also
the linemode prefix, the keys already used by linemode (`s`, `p`, `b`, `m`,
`o`, `n`) keep their linemode meaning; use any other key for a mark.

`b` opens a bookmark prompt. Type an `NNN_BMS` key (for example `d`) or the
name of a symlinked bookmark and press `Enter`; an empty prompt opens nnn's
bookmarks directory. `B` bookmarks the current directory by creating a symlink
under `${XDG_CONFIG_HOME:-~/.config}/nnn/bookmarks`, named after the directory
by default. The directory is shared with nnn, so bookmarks created in either
file manager work in both. `NNN_BMS` uses nnn's `key:path;key:path` format:

```sh
export NNN_BMS='d:~/Documents;u:/home/user/Cam Uploads;D:~/Downloads/'
```

## Picker mode

nnnyaz can act as a file picker, like nnn's `-p` or Yazi's `--chooser-file`.
Pass a file with `--chooser-file <path>`, `-p <path>`, or set
`NNNYAZ_CHOOSER_FILE`. Select entries with `Space`; press `Enter` on any entry
(files and directories alike) to pick the selection, or the hovered entry when
nothing is selected, and quit. Quitting normally with `q` or `Q` also writes an
existing selection.

`-p -` writes the paths to stdout instead of a file, which is useful in
command substitution. `-0` (or `--null`) separates paths with NUL instead of a
newline; like Yazi and nnn, nnnyaz puts separators only between paths and does
not append a trailing one. If nothing is selected on a plain quit, the chooser
file is left untouched.

```fish
# Pick files and print them one per line.
set -l files (nnnyaz -p - ~/Downloads)
for f in $files
    echo $f
end
```

```sh
# Pick files and pass them to a command with NUL separators.
nnnyaz -p - -0 ~/Downloads | xargs -0 some-command
```

The status line shows `picker` while picker mode is active.

## Terminal support

Image and video thumbnails are drawn with the kitty graphics protocol in kitty
and ghostty, and with sixel in terminals that support it (foot, WezTerm, mlterm,
xterm with sixel enabled, …). The protocol is detected automatically; set
`NNNYAZ_IMAGE_PROTOCOL=kitty|sixel|none` to override it.

Inside tmux, graphics must be allowed to pass through to the client. Prefer
enabling it only for the pane that runs nnnyaz:

```sh
tmux set -p allow-passthrough on
nnnyaz
```

The shipped fish wrapper does this automatically and restores the previous pane
setting when nnnyaz exits.

`set -g allow-passthrough on` also works, but it enables passthrough for every
pane that does not override it. `allow-passthrough all` is broader still: it
permits passthrough even from hidden panes. Passthrough lets programs in that
pane send escape sequences straight to the client terminal, so keep it as
narrow as practical.

tmux hides the client terminal from the pane (`TERM` and DA1 describe tmux
itself, while inherited Kitty/Ghostty variables describe where the server
started), so nnnyaz asks tmux for the attached client's terminal name. This lets
foot's Sixel previews work inside tmux. If detection still fails, set
`NNNYAZ_IMAGE_PROTOCOL=sixel`; `NNNYAZ_DEBUG=1` prints the detected protocol and
client terminal.

Video thumbnails are generated with `ffmpeg`. PDFs are rendered a page at a
time with `pdftoppm` (poppler), and `J`/`K` or scrolling over the preview
switches pages. Page counts come from `pdfinfo` when it is installed.
Archives are listed as an indented tree with sizes using `7z`/`7zz`.
Compressed tarballs (`.tar.gz`, `.tgz`, …) are expanded one level so the tar's
contents are shown instead of the inner `.tar` file. Fonts are rendered as a
glyph sheet with ImageMagick (`magick`, or `convert` on ImageMagick 6). JSON is
pretty-printed with `jq`. Tabs in text previews expand to `preview.tab_size`
spaces (2 by default), so indented source lines keep their shape.

Decoded images are cached on disk (shared with Yazi when its cache is
available), and image/video/PDF neighbours are preloaded in the background, so
navigating back over a folder is instant. The first view of a very large photo
is decoded and downscaled once; writing it to the cache happens in the
background rather than delaying the image. EXIF orientation is applied, so
photos from phones and cameras are shown upright. Embedded non-sRGB ICC
profiles (Display P3, Adobe RGB, grayscale gamma curves, …) are converted to
sRGB before display; CMYK profiles are left to the decoder, matching Yazi.

`F` opens a Yazi-style spot: the preview fills the screen, `j`/`k` scroll it,
`h`/`l` swipe to the previous/next file, `c c` copies the hovered path, and
`Esc`, `Tab`, or `Ctrl+c` closes it. `<Tab>` still opens the metadata info
overlay.

## Configuration

nnnyaz reads configuration from the first of:

1. `$NNNYAZ_CONFIG_HOME`
2. `$XDG_CONFIG_HOME/nnnyaz`
3. `~/.config/nnnyaz`

The files are all optional:

- `config.toml` — settings and open-with rules
- `keymap.toml` — keybindings
- `theme.toml` — colors and styles
- `init.lua` — programmatic overrides and named actions

Load order is built-in defaults, then `config.toml`, `keymap.toml`,
`theme.toml`, and finally `init.lua`. A broken file only produces a warning
and is skipped; the rest of the configuration still applies.

### `config.toml`

```toml
[mgr]
# Relative weights for the list and preview panes. A Yazi-style
# [parent, current, preview] triple is also accepted; the last two are used.
ratio = [40, 60]
show_hidden = false
show_preview = true
sort_by = "name"        # name | natural | size | mtime | btime | extension | random
sort_reverse = false
sort_sensitive = false  # compare names case-sensitively
sort_dir_first = true   # keep directories above files
sort_fallback = "alphabetical"  # tie-break: alphabetical | natural
sort_translit = false   # strip accents before natural sorting (é -> e)
show_symlink = true     # append " -> target" to symlinks in the list
linemode = "size"       # size | permissions | btime | mtime | owner | detail | none
scrolloff = 5           # rows of context kept above/below the cursor (0 disables)
mouse_events = ["click", "scroll", "drag"]  # click | scroll | touch | move | drag; [] disables

[preview]
cache_size = 16
cache_dir = ""          # nnnyaz's preview cache; empty uses the system temp dir
max_width = 1920        # cap on decoded preview width, in pixels
max_height = 1080       # cap on decoded preview height, in pixels
image_filter = "box"    # box | nearest | triangle | catmull-rom | gaussian | lanczos3
image_quality = 90      # JPEG quality for cached previews (1-100)
image_delay = 30        # ms to wait before decoding a newly selected image
tab_size = 2            # spaces per tab in text previews (1-16)
wrap = "no"             # "yes" wraps long lines in text previews

[tasks]
show_progress = true
suppress_preload = false  # do not preload neighbouring image/video/PDF previews

[confirm]
quit_with_tasks = true
trash_title = "Trash {n} selected file{s}?"   # {n} count, {s} plural "s"
delete_title = "Permanently delete {n} selected file{s}?"

# Prompt titles use Yazi's [input] keys. A trailing colon is optional; the
# *_origin/*_offset/cursor_blink keys are accepted but do not apply because
# nnnyaz prompts on the status line.
[input]
cd_title = "go to"
create_title = [ "create", "create dir" ]
rename_title = "rename"
filter_title = "filter"
find_title = [ "find→", "find←" ]
shell_title = [ "shell", "shell (block)" ]

# Ordering for the which-key prefix menu (Yazi's [which]).
[which]
sort_by = "none"        # none | key | desc
sort_sensitive = false  # compare keys/descriptions case-sensitively
sort_reverse = false
sort_translit = false   # strip accents before comparing (é -> e)

# Open-with rules use Yazi's [opener] and [open] schema.
[opener]
myedit = { run = "nvim %s", desc = "Edit with Neovim", block = true }

[open]
rules = [
  { mime = "text/*", use = "myedit" },
  { mime = "*", use = "open" },
]
```

If `[opener]` or `[open]` is omitted, nnnyaz falls back to your Yazi config and
then to built-in defaults.

`Enter`/`o` open the whole selection when there is one, or the hovered entry
otherwise; `l`/`→` enter the hovered directory (or act on the hovered file).
`O` (or `<S-Enter>`) lists the openers that apply to the selection. The built-in
`open` opener uses the system handler for the file's MIME type, but
browser-associated files follow the browser you selected with
`xdg-settings set default-web-browser`. For example, if PDFs are registered to
Brave while Zen is your default browser, nnnyaz opens the PDF in Zen. Set `OPENER`
(or nnn's `NNN_OPENER`) to override this and use your own launcher for
everything; `OPENER` wins when both are set. With `-c`, the opener runs
attached to the terminal and nnnyaz waits for it.

Opener commands support Yazi's placeholders: `%s`/`%S` expand to the target
files, `%d`/`%D` to their parent directories, a 1-based suffix picks one target
(`%s1`, `%d2`), and `%%` is a literal `%`. A command with an unnumbered `%s` or
`%S` runs once with every target (Yazi's `spread`); otherwise it runs once per
file. Hovered, yanked, and tab placeholders expand to empty in an opener's
context, matching Yazi.

### Updating openers from installed apps

`scripts/update-openers.py` scans XDG `.desktop` entries and writes managed
blocks into both nnnyaz's `config.toml` and Yazi's `yazi.toml`. It adds one
opener per available application and one prepend rule per MIME type, reusing
existing openers when they already launch the same program. Hidden, unavailable,
and non-file entries are skipped, as are URL-scheme handlers and shell wrappers;
text editors without `MimeType=` are treated as `text/*`, terminal wrappers such
as `alacritty -e nvim` are unwrapped into blocking editor commands, and commands
without a file placeholder get `%s` appended. MIME types with identical option
lists are collapsed into one brace-expanded rule.

```sh
python3 scripts/update-openers.py --dry-run   # print the generated configs
python3 scripts/update-openers.py             # write both configs
```

Only the marker-delimited generated blocks are replaced, hand-written config is
left alone, and the first pre-generation version is kept in a `.bak` file. Use
`--nnnyaz-config`, `--yazi-config`, or repeated `--scan-dir` to override paths.

### `keymap.toml`

Bindings map a key sequence to an action command. Contexts are `mgr`, `tasks`,
`info`, and `help` (Yazi's `spot` context is accepted as an alias for `info`,
so Yazi's spot bindings also drive the `F` full-screen view). The `spot` action
opens that view.

```toml
[mgr]
prepend_keymap = [
  { on = "<C-s>", run = "search fd", desc = "search files" },
  { on = "g g", run = "top", desc = "top of list" },
]
append_keymap = [
  { on = "Q", run = "quit" },
]

[tasks]
prepend_keymap = [
  { on = "x", run = "task_cancel" },
]
```

`[help]` accepts Yazi's help-menu actions: `close`, `escape`, `arrow prev` /
`arrow next`, `arrow N`, `arrow top` / `arrow bot`, and Yazi's page steps
(`arrow -50%`, `arrow 50%`, `arrow -100%`, `arrow 100%`). Keys you
leave unbound keep nnnyaz's built-in help keys, including `/` search and
`n`/`N`, half/full-page jumps, and `q`/`~` to close — so a Yazi `[help]` block
that only binds `j`/`k` still gets the extra navigation.

Once `/` search is open, the query uses the same editing keys as the rename
prompt: `Ctrl+H`/`Ctrl+L` move, `Shift+←/→` (or `Ctrl+Shift+H/L`) select,
`Ctrl+W` deletes the previous word, `Ctrl+U`/`Ctrl+K` kill to the start/end,
`Ctrl+Z` undoes, `Ctrl+A`/`Ctrl+E` jump to the start/end, and `Ctrl+Shift+S`
selects all. `Enter` keeps the query, `Esc` cancels it.

`prepend_keymap` overrides existing bindings, while `append_keymap` adds new
ones. A `keymap` array replaces the whole context:

```toml
[mgr]
keymap = [
  { on = "j", run = "move 1" },
  { on = "k", run = "move -1" },
  { on = "q", run = "quit" },
]
```

A shorthand table form is also accepted:

```toml
[mgr.keymap]
"g g" = "top"
```

Key sequences are written like `<C-d>`, `<A-->`, `<S-Enter>`, `<Space>`, `g g`.
Unsupported Yazi contexts (`input`, `confirm`, `pick`, `cmp`, `which`) are
ignored.

Run `~` inside nnnyaz for the built-in key list.

Yazi's action names are accepted too, so most of a Yazi `keymap.toml` can be
pasted in unchanged. For example: `arrow prev`, `enter`, `leave`,
`remove --permanently`, `plugin fd`, `plugin zoxide`, `copy dirpath`,
`copy name_without_ext`, `sort alphabetical --reverse=yes`, `cd ~`,
`tab_switch 0`, `tab_swap 1`, and `tasks:show`. In the `tasks` and `info`
contexts, `arrow` moves the task/info cursor.

`Z` jumps to a directory from zoxide's database when `zoxide` is installed.

### Mouse

Mouse support mirrors Yazi and is controlled by `mgr.mouse_events` (default
`["click", "scroll", "drag"]`). An empty list disables mouse capture entirely.

- Left click a file to select it; right click selects and opens it.
- Scroll over the list to move the cursor by one row.
- Left click the preview to open the current entry; scroll over the preview to
  seek text previews by one row or turn PDF pages.
- Click a tab in the tab bar to switch to it.
- Mouse input is ignored in prompts and overlays, matching Yazi's `mgr` layer.

### Prompt editing

Prompts share a single-line editing buffer. Rename starts with the stem selected
and the cursor before the extension, so typing replaces the stem; dotfiles
select the whole name.

| Keys | Action |
| --- | --- |
| `Ctrl+H` / `Ctrl+L` (or `←` / `→`) | Move left / right |
| `Ctrl+Shift+H` / `Ctrl+Shift+L` (or `Shift+←` / `Shift+→`) | Extend the selection |
| `Home` / `End`, `Ctrl+A` / `Ctrl+E` | Jump to start / end |
| `Ctrl+Shift+S` | Select the whole name |
| `Ctrl+U` / `Ctrl+K` | Delete to start / end |
| `Ctrl+W` | Delete the previous word |
| `Ctrl+Z` | Undo the last edit (a run of typing undoes as one step) |
| `Backspace` / `Delete` | Delete before / at the caret |
| `Enter` / `Esc` | Save / cancel |

While renaming, a dim hint on the right of the status line lists the binds and
shortens itself on narrow terminals. nnnyaz requests a keyboard protocol that
distinguishes modified keys: tmux's modifyOtherKeys inside tmux, kitty's
protocol elsewhere. Terminals without support fall back to the legacy encoding.

### Bulk rename

With two or more entries selected, `r` opens their names — one per line — in
`$VISUAL`/`$EDITOR`. Save and quit to apply the whole batch. Names can be
swapped (`a`/`b`), because nnnyaz moves them through temporary names. If the line
count changes, a name is empty, duplicated, or contains `/`, or a destination
already exists outside the batch, nnnyaz refuses the rename and leaves everything
untouched.

### Shell commands

`;` opens a command prompt and runs the command in the background as a task, so
the file manager stays usable. Open the task manager (`w`), then `Enter` on the
task to inspect captured stdout/stderr. `:` blocks and hands the terminal to the
command, which is what interactive programs need.

### `theme.toml`

Themes use nnnyaz's semantic roles rather than Yazi's theme schema. A role can be
a color name, a `#rrggbb`/`#rgb` value, a 0–255 palette index, or a style table.

```toml
[theme]
directory = "blue"
selection = { reversed = true }
status_error = "#ff5555"
status_info = { fg = "yellow", bold = true }
border = "darkgray"
```

Roles: `fg`, `dim`, `selection`, `directory`, `tab_active`, `tab_inactive`,
`prefix_key`, `prefix_desc`, `info_section`, `info_label`, `status`,
`status_error`, `status_info`, `task_running`, `task_done`, `task_failed`,
`task_cancelled`, `task_percent`, `preview_title`, `border`.

### `init.lua`

`init.lua` runs after the TOML files. It may call a small API and return a
table using the same schema as the TOML files.

```lua
nnnyaz.keymap.set("mgr", "<C-s>", "search fd", "search")
nnnyaz.notify("nnnyaz " .. nnnyaz.version)

nnnyaz.register_action("hello", function()
  nnnyaz.notify("hello from Lua")
end)

nnnyaz.keymap.set("mgr", "H", "hello")

return {
  mgr = { show_hidden = true, linemode = "mtime" },
  theme = { directory = "red" },
}
```

Available API:

- `nnnyaz.version` — nnnyaz version string
- `nnnyaz.config` — read-only snapshot of the loaded settings
- `nnnyaz.notify(message)` — set the status message
- `nnnyaz.keymap.set(context, keys, action, desc)` — bind a key
- `nnnyaz.register_action(name, fn)` — register a named action

Keybindings always reference action names or commands, not arbitrary Lua
closures. Custom actions are referenced by name:

```toml
[mgr]
prepend_keymap = [ { on = "H", run = "hello" } ]
```

## Performance

Large directories collect entry metadata with a bounded number of worker
threads (up to 16, only above 512 entries). Results are concatenated in
readdir order, so sorting, linemodes, symlink targets, and the status line see
exactly the same data as before. On this machine, a 20k-file release reload
dropped from about 24 ms to about 14 ms; the remaining time is almost entirely
filesystem metadata calls.

To measure your own machine:

```sh
cargo build --release
python3 scripts/bench-reload.py 20000 target/release/nnnyaz
```

`NNNYAZ_DEBUG=1` also prints `collect` and `sort` timings for each reload.

## Development

Run the unit and CLI tests with `cargo test`. For a quick end-to-end check of
the help overlay, picker, and delete confirmation against a real terminal,
build first and then run the manual pty smoke test:

```sh
cargo build
python3 scripts/pty-smoke.py
```

It uses only the Python standard library. It is deliberately not part of CI
because pty timing varies between platforms.

## Yazi compatibility

nnnyaz deliberately mirrors Yazi where it has an equivalent feature: default
keybindings, prefix menus, open-with rules, trash, tasks, and zoxide. It does
not try to be a drop-in Yazi replacement: config lives under `nnnyaz`, the theme
schema is nnnyaz-specific, and the plugin API is intentionally minimal.

## AI usage

Almost everything in this repository — the code, tests, and documentation —
was written by **Big Pickle** in OpenCode, because I'm broke. <3

My role was setting the direction and testing the result. Please read this as
an AI-coded project with a human at the wheel, not as hand-written software.
If there's a problem, an issue is very welcome.

## Acknowledgements

nnnyaz stands on the shoulders of [nnn](https://github.com/jarun/nnn) (whose
keymap and command line it emulates) and
[Yazi](https://github.com/sxyazi/yazi) (whose configuration and UX it
follows). It is built with [ratatui](https://github.com/ratatui/ratatui),
[image](https://github.com/image-rs/image), and the other crates listed in
`Cargo.toml`.

## License

nnnyaz is released under the [MIT license](LICENSE). The transliteration
table in `src/translit.rs` is derived from Yazi and carries Yazi's MIT notice;
see [`THIRD_PARTY_LICENSES.md`](THIRD_PARTY_LICENSES.md).

