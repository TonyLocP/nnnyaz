# rnn

A small nnn/Yazi-style terminal file manager with a native preview pane.

rnn keeps a Yazi-like feel: modal `mgr` navigation, prefix keymaps
(`g`, `,`, `c`, `m`, `t`), tabs, a trash bin, a background task manager, and
an `open with` menu. Configuration is declarative TOML, with an optional
`init.lua` for overrides and small custom actions.

## Build

```sh
cargo build --release
install -Dm755 target/release/rnn ~/.local/bin/rnn
```

Run it in a directory:

```sh
rnn [path]
```

## Quitting and the current directory

`q` quits and leaves the shell where it was. `Q` quits and records rnn's
current directory so a shell wrapper can `cd` there. A child process cannot
change its parent shell's directory, so this needs a small wrapper.

Pass a file with `--cwd-file <path>` (or set `RNN_CWD_FILE`); `Q` writes the
current directory to it. The file is only written by `Q`, never by `q`, so the
wrapper should remove it before starting rnn:

```fish
function rnn --description 'rnn with cd-on-quit'
    set -l cwd_file (mktemp)
    or return
    rm -f $cwd_file
    command rnn --cwd-file $cwd_file $argv
    set -l status $status
    if test -s $cwd_file
        cd (cat $cwd_file)
    end
    rm -f $cwd_file
    return $status
end
```

Save it as `~/.config/fish/functions/rnn.fish`. Bash/zsh wrappers use the same
idea:

```sh
rnn() {
  local cwd_file
  cwd_file=$(mktemp) || return
  rm -f "$cwd_file"
  command rnn --cwd-file "$cwd_file" "$@"
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

rnn can act as a file picker, like nnn's `-p` or Yazi's `--chooser-file`.
Pass a file with `--chooser-file <path>`, `-p <path>`, or set
`RNN_CHOOSER_FILE`. Select entries with `Space`; press `Enter` on any entry
(files and directories alike) to pick the selection, or the hovered entry when
nothing is selected, and quit. Quitting normally with `q` or `Q` also writes an
existing selection.

`-p -` writes the paths to stdout instead of a file, which is useful in
command substitution. `-0` (or `--null`) separates paths with NUL instead of a
newline. If nothing is selected on a plain quit, the chooser file is left
untouched.

```fish
# Pick files and print them one per line.
set -l files (rnn -p - ~/Downloads)
for f in $files
    echo $f
end
```

```sh
# Pick files and pass them to a command with NUL separators.
rnn -p - -0 ~/Downloads | xargs -0 some-command
```

The status line shows `picker` while picker mode is active.

## Terminal support

Image and video thumbnails are drawn with the kitty graphics protocol in kitty
and ghostty, and with sixel in terminals that support it (foot, WezTerm, mlterm,
xterm with sixel enabled, …). The protocol is detected automatically; set
`RNN_IMAGE_PROTOCOL=kitty|sixel|none` to override it.

Inside tmux, enable graphics passthrough:

```tmux
set -g allow-passthrough on
```

Video thumbnails are generated with `ffmpeg`. PDFs are rendered a page at a
time with `pdftoppm` (poppler), and `J`/`K` or scrolling over the preview
switches pages. Page counts come from `pdfinfo` when it is installed.
Archives are listed as an indented tree with sizes using `7z`/`7zz`. Fonts are
rendered as a glyph sheet with ImageMagick (`magick`, or `convert` on
ImageMagick 6). JSON is pretty-printed with `jq`.

Decoded images are cached on disk (shared with Yazi when its cache is
available), and image/video/PDF neighbours are preloaded in the background, so
navigating back over a folder is instant. The first view of a very large photo
is decoded and downscaled once; writing it to the cache happens in the
background rather than delaying the image. EXIF orientation is applied, so
photos from phones and cameras are shown upright.

## Configuration

rnn reads configuration from the first of:

1. `$RNN_CONFIG_HOME`
2. `$XDG_CONFIG_HOME/rnn`
3. `~/.config/rnn`

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
linemode = "size"       # size | permissions | btime | mtime | owner | none
scrolloff = 5           # rows of context kept above/below the cursor (0 disables)
mouse_events = ["click", "scroll", "drag"]  # click | scroll | touch | move | drag; [] disables

[preview]
cache_size = 16
max_width = 1920        # cap on decoded preview width, in pixels
max_height = 1080       # cap on decoded preview height, in pixels
image_filter = "box"    # box | nearest | triangle | catmull-rom | gaussian | lanczos3
image_quality = 90      # JPEG quality for cached previews (1-100)
image_delay = 30        # ms to wait before decoding a newly selected image

[tasks]
show_progress = true

[confirm]
quit_with_tasks = true

# Open-with rules use Yazi's [opener] and [open] schema.
[opener]
myedit = { run = "nvim %s", desc = "Edit with Neovim", block = true }

[open]
rules = [
  { mime = "text/*", use = "myedit" },
  { mime = "*", use = "open" },
]
```

If `[opener]` or `[open]` is omitted, rnn falls back to your Yazi config and
then to built-in defaults.

`Enter`/`o` open the whole selection when there is one, or the hovered entry
otherwise; `l`/`→` enter the hovered directory (or act on the hovered file).
`O` (or `<S-Enter>`) lists the openers that apply to the selection. The built-in
`open` opener uses the system handler for the file's MIME type, but
browser-associated files follow the browser you selected with
`xdg-settings set default-web-browser`. For example, if PDFs are registered to
Brave while Zen is your default browser, rnn opens the PDF in Zen. Set `OPENER`
to override this and use your own launcher for everything.

Opener commands support Yazi's placeholders: `%s`/`%S` expand to the target
files, `%d`/`%D` to their parent directories, a 1-based suffix picks one target
(`%s1`, `%d2`), and `%%` is a literal `%`. A command with an unnumbered `%s` or
`%S` runs once with every target (Yazi's `spread`); otherwise it runs once per
file. Hovered, yanked, and tab placeholders expand to empty in an opener's
context, matching Yazi.

### `keymap.toml`

Bindings map a key sequence to an action command. Contexts are `mgr`, `tasks`,
and `info` (`spot` is accepted as an alias for `info`).

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
Unsupported Yazi contexts (`input`, `confirm`, `pick`, `cmp`, `help`, `which`)
are ignored.

Run `~` inside rnn for the built-in key list.

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
shortens itself on narrow terminals. rnn requests a keyboard protocol that
distinguishes modified keys: tmux's modifyOtherKeys inside tmux, kitty's
protocol elsewhere. Terminals without support fall back to the legacy encoding.

### Bulk rename

With two or more entries selected, `r` opens their names — one per line — in
`$VISUAL`/`$EDITOR`. Save and quit to apply the whole batch. Names can be
swapped (`a`/`b`), because rnn moves them through temporary names. If the line
count changes, a name is empty, duplicated, or contains `/`, or a destination
already exists outside the batch, rnn refuses the rename and leaves everything
untouched.

### Shell commands

`;` opens a command prompt and runs the command in the background as a task, so
the file manager stays usable. Open the task manager (`w`), then `Enter` on the
task to inspect captured stdout/stderr. `:` blocks and hands the terminal to the
command, which is what interactive programs need.

### `theme.toml`

Themes use rnn's semantic roles rather than Yazi's theme schema. A role can be
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
rnn.keymap.set("mgr", "<C-s>", "search fd", "search")
rnn.notify("rnn " .. rnn.version)

rnn.register_action("hello", function()
  rnn.notify("hello from Lua")
end)

rnn.keymap.set("mgr", "H", "hello")

return {
  mgr = { show_hidden = true, linemode = "mtime" },
  theme = { directory = "red" },
}
```

Available API:

- `rnn.version` — rnn version string
- `rnn.config` — read-only snapshot of the loaded settings
- `rnn.notify(message)` — set the status message
- `rnn.keymap.set(context, keys, action, desc)` — bind a key
- `rnn.register_action(name, fn)` — register a named action

Keybindings always reference action names or commands, not arbitrary Lua
closures. Custom actions are referenced by name:

```toml
[mgr]
prepend_keymap = [ { on = "H", run = "hello" } ]
```

## Yazi compatibility

rnn deliberately mirrors Yazi where it has an equivalent feature: default
keybindings, prefix menus, open-with rules, trash, tasks, and zoxide. It does
not try to be a drop-in Yazi replacement: config lives under `rnn`, the theme
schema is rnn-specific, and the plugin API is intentionally minimal.
