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

## Terminal support

Image and video thumbnails are drawn with the kitty graphics protocol in kitty
and ghostty, and with sixel in terminals that support it (foot, WezTerm, mlterm,
xterm with sixel enabled, …). The protocol is detected automatically; set
`RNN_IMAGE_PROTOCOL=kitty|sixel|none` to override it.

Inside tmux, enable graphics passthrough:

```tmux
set -g allow-passthrough on
```

Video thumbnails are generated with `ffmpeg`.

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

[preview]
cache_size = 16

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
keybindings, prefix menus, open-with rules, trash, and tasks. It does not try
to be a drop-in Yazi replacement: config lives under `rnn`, the theme schema is
rnn-specific, `Z`/zoxide is not implemented, and the plugin API is intentionally
minimal.
