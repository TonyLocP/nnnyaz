//! Actions that keybindings can invoke.
//!
//! Keymaps stay declarative: a binding stores an [`Action`] parsed from a
//! command string such as `"sort size --reverse"`. Arbitrary closures are not
//! allowed here; Lua can register named actions instead.

use crate::app::{CopyKind, Linemode, SearchKind, Sort};

#[derive(Clone, Debug, PartialEq)]
pub enum Action {
    // application / modes
    Quit,
    QuitCwd,
    QuitOrClose,
    Close,
    Escape,
    Help,
    Tasks,
    Info,
    Nop,

    // navigation
    Move(isize),
    HalfPage(i8),
    Page(i8),
    Top,
    Bottom,
    Parent,
    Open,
    OpenWith,
    History(i8),
    Follow,
    Goto(String),
    GotoTrash,
    Cd,

    // view
    Hidden,
    TogglePreview,
    Zoom(i8),
    Seek(i16),

    // selection
    Toggle,
    ToggleSelect,
    SelectAll,
    InvertSelection,
    VisualSelect,
    VisualUnset,

    // clipboard / files
    Yank,
    Cut,
    Paste(bool),
    Unyank,
    ClipboardCopy,
    ClipboardPaste,
    Trash,
    Delete,
    Rename,
    Create,
    BulkCreate,
    Filter,
    Find(bool),
    FindNext(bool),
    Search(SearchKind),
    CancelSearch,
    Shell,
    ShellBlock,
    Fzf,
    Zoxide,
    Suspend,
    Editor,
    Pager,
    CopyPath(CopyKind),
    SetLinemode(Linemode),
    SetSort(Sort, bool),
    Link(bool),
    Hardlink,

    // tabs
    NewTab(bool),
    RenameTab,
    CloseTab,
    SwitchTab(usize),
    NextTab(isize),
    SwapTab(isize),

    // tasks / info
    TaskMove(isize),
    TaskTop,
    TaskBottom,
    TaskInspect,
    TaskCancel,
    InfoMove(isize),
    InfoSwipe(isize),
    InfoCopyCell,

    /// Several actions run in order for one binding, as in Yazi.
    Chain(Vec<Action>),

    /// A named action registered from `init.lua`.
    Custom(String),
}

impl Action {
    /// Human label used by the which-key menu when a binding has no `desc`.
    pub fn label(&self) -> String {
        match self {
            Action::Move(n) => format!("move {n}"),
            Action::HalfPage(n) => format!("half page {n}"),
            Action::Page(n) => format!("page {n}"),
            Action::History(n) => format!("history {n}"),
            Action::Goto(p) => format!("go to {p}"),
            Action::Zoom(n) => match n {
                0 => "zoom fit".into(),
                n if *n > 0 => "zoom in".into(),
                _ => "zoom out".into(),
            },
            Action::Seek(n) => format!("seek {n}"),
            Action::Search(k) => match k {
                SearchKind::Fd => "search fd".into(),
                SearchKind::Rg => "search rg".into(),
            },
            Action::CopyPath(k) => format!("copy {k:?}"),
            Action::SetLinemode(m) => format!("linemode {m:?}"),
            Action::SetSort(s, reverse) => {
                format!("sort {s:?}{}", if *reverse { " ↓" } else { " ↑" })
            }
            Action::Link(relative) => {
                if *relative {
                    "symlink relative".into()
                } else {
                    "symlink absolute".into()
                }
            }
            Action::NewTab(home) => {
                if *home {
                    "new tab at home".into()
                } else {
                    "new tab here".into()
                }
            }
            Action::SwitchTab(i) => format!("tab {}", i + 1),
            Action::NextTab(n) => format!("tab {n:+}"),
            Action::SwapTab(n) => format!("swap tab {n:+}"),
            Action::TaskMove(n) => format!("task move {n}"),
            Action::InfoMove(n) => format!("info move {n}"),
            Action::InfoSwipe(n) => format!("info swipe {n}"),
            Action::Paste(force) => {
                if *force {
                    "paste force".into()
                } else {
                    "paste".into()
                }
            }
            Action::Find(fwd) => {
                if *fwd {
                    "find next".into()
                } else {
                    "find previous".into()
                }
            }
            Action::FindNext(fwd) => {
                if *fwd {
                    "next found".into()
                } else {
                    "previous found".into()
                }
            }
            Action::Chain(actions) => actions
                .iter()
                .map(Action::label)
                .collect::<Vec<_>>()
                .join(", "),
            Action::Custom(name) => name.clone(),
            other => other.name().to_string(),
        }
    }

    /// The canonical command name (without arguments).
    pub fn name(&self) -> &'static str {
        match self {
            Action::Quit => "quit",
            Action::QuitCwd => "quit_cwd",
            Action::QuitOrClose => "quit_or_close",
            Action::Close => "close",
            Action::Escape => "escape",
            Action::Help => "help",
            Action::Tasks => "tasks",
            Action::Info => "info",
            Action::Nop => "nop",
            Action::Move(_) => "move",
            Action::HalfPage(_) => "half_page",
            Action::Page(_) => "page",
            Action::Top => "top",
            Action::Bottom => "bottom",
            Action::Parent => "parent",
            Action::Open => "open",
            Action::OpenWith => "open_with",
            Action::History(_) => "history",
            Action::Follow => "follow",
            Action::Goto(_) => "goto",
            Action::GotoTrash => "goto_trash",
            Action::Cd => "cd",
            Action::Hidden => "hidden",
            Action::TogglePreview => "toggle_preview",
            Action::Zoom(_) => "zoom",
            Action::Seek(_) => "seek",
            Action::Toggle => "toggle",
            Action::ToggleSelect => "toggle_select",
            Action::SelectAll => "select_all",
            Action::InvertSelection => "invert_selection",
            Action::VisualSelect => "visual_select",
            Action::VisualUnset => "visual_unset",
            Action::Yank => "yank",
            Action::Cut => "cut",
            Action::Paste(_) => "paste",
            Action::Unyank => "unyank",
            Action::ClipboardCopy => "clipboard_copy",
            Action::ClipboardPaste => "clipboard_paste",
            Action::Trash => "trash",
            Action::Delete => "delete",
            Action::Rename => "rename",
            Action::Create => "create",
            Action::BulkCreate => "bulk_create",
            Action::Filter => "filter",
            Action::Find(_) => "find",
            Action::FindNext(_) => "find_next",
            Action::Search(_) => "search",
            Action::CancelSearch => "cancel_search",
            Action::Shell => "shell",
            Action::ShellBlock => "shell_block",
            Action::Fzf => "fzf",
            Action::Zoxide => "zoxide",
            Action::Suspend => "suspend",
            Action::Editor => "editor",
            Action::Pager => "pager",
            Action::CopyPath(_) => "copy",
            Action::SetLinemode(_) => "linemode",
            Action::SetSort(_, _) => "sort",
            Action::Link(_) => "link",
            Action::Hardlink => "hardlink",
            Action::NewTab(_) => "new_tab",
            Action::RenameTab => "rename_tab",
            Action::CloseTab => "close_tab",
            Action::SwitchTab(_) => "switch_tab",
            Action::NextTab(_) => "next_tab",
            Action::SwapTab(_) => "swap_tab",
            Action::TaskMove(_) => "task_move",
            Action::TaskTop => "task_top",
            Action::TaskBottom => "task_bottom",
            Action::TaskInspect => "task_inspect",
            Action::TaskCancel => "task_cancel",
            Action::InfoMove(_) => "info_move",
            Action::InfoSwipe(_) => "info_swipe",
            Action::InfoCopyCell => "info_copy_cell",
            Action::Chain(_) => "chain",
            Action::Custom(_) => "custom",
        }
    }

    /// Parse a command string such as `"sort size --reverse"`.
    pub fn parse(input: &str) -> Result<Action, String> {
        let mut parts = input.split_whitespace();
        let Some(name) = parts.next() else {
            return Err("empty action".into());
        };
        let args: Vec<&str> = parts.collect();
        let arg = |i: usize| -> Result<&str, String> {
            args.get(i)
                .copied()
                .ok_or_else(|| format!("{name}: missing argument {}", i + 1))
        };
        let num = |i: usize| -> Result<isize, String> {
            arg(i)?
                .parse::<isize>()
                .map_err(|_| format!("{name}: invalid number '{}'", args[i]))
        };
        let reverse = args
            .iter()
            .any(|a| matches!(*a, "--reverse" | "-r" | "--reverse=yes" | "--reverse=true"));
        let force = args.iter().any(|a| *a == "--force" || *a == "-f");
        let previous = args
            .iter()
            .any(|a| matches!(*a, "--previous" | "backward" | "prev" | "previous"));
        let interactive = args.contains(&"--interactive");
        let cut = args.contains(&"--cut");
        let permanently = args.contains(&"--permanently");

        Ok(match name {
            "quit" => Action::Quit,
            "quit_cwd" => Action::QuitCwd,
            "quit_or_close" => Action::QuitOrClose,
            "close" => Action::Close,
            "escape" => Action::Escape,
            "help" => Action::Help,
            "tasks" | "tasks:show" => Action::Tasks,
            "info" | "spot" => Action::Info,
            "nop" => Action::Nop,
            "move" => Action::Move(num(0)?),
            "arrow" => match args.first().copied() {
                Some("prev" | "previous") => Action::Move(-1),
                Some("next") => Action::Move(1),
                Some("top") => Action::Top,
                Some("bot" | "bottom") => Action::Bottom,
                Some("-50%") => Action::HalfPage(-1),
                Some("50%") => Action::HalfPage(1),
                Some("-100%") => Action::Page(-1),
                Some("100%") => Action::Page(1),
                Some(n) => Action::Move(
                    n.parse::<isize>()
                        .map_err(|_| format!("arrow: invalid number '{n}'"))?,
                ),
                None => return Err("arrow: missing argument".into()),
            },
            "half_page" => Action::HalfPage(num(0)? as i8),
            "page" => Action::Page(num(0)? as i8),
            "top" => Action::Top,
            "bottom" => Action::Bottom,
            "parent" | "leave" => Action::Parent,
            "open" | "enter" => {
                if interactive {
                    Action::OpenWith
                } else {
                    Action::Open
                }
            }
            "open_with" => Action::OpenWith,
            "history" => Action::History(num(0)? as i8),
            "back" => Action::History(-1),
            "forward" => Action::History(1),
            "follow" => Action::Follow,
            "goto" => Action::Goto(arg(0)?.to_string()),
            "goto_trash" => Action::GotoTrash,
            "cd" => match args.first() {
                None | Some(&"--interactive") => Action::Cd,
                Some(_) => Action::Goto(arg(0)?.to_string()),
            },
            "hidden" => Action::Hidden,
            "toggle_preview" => Action::TogglePreview,
            "zoom" => match arg(0)? {
                "in" | "+" => Action::Zoom(1),
                "out" | "-" => Action::Zoom(-1),
                "reset" | "fit" | "0" => Action::Zoom(0),
                other => return Err(format!("zoom: unknown direction '{other}'")),
            },
            "seek" => Action::Seek(num(0)? as i16),
            "toggle" => Action::Toggle,
            "toggle_select" => Action::ToggleSelect,
            "select_all" => Action::SelectAll,
            "invert_selection" => Action::InvertSelection,
            "toggle_all" => {
                if args.contains(&"--state=on") {
                    Action::SelectAll
                } else {
                    Action::InvertSelection
                }
            }
            "visual_select" => Action::VisualSelect,
            "visual_unset" => Action::VisualUnset,
            "visual_mode" => {
                if args.contains(&"--unset") {
                    Action::VisualUnset
                } else {
                    Action::VisualSelect
                }
            }
            "yank" => {
                if cut {
                    Action::Cut
                } else {
                    Action::Yank
                }
            }
            "copy" if args.is_empty() => Action::Yank,
            "cut" => Action::Cut,
            "paste" => Action::Paste(force),
            "unyank" => Action::Unyank,
            "clipboard_copy" => Action::ClipboardCopy,
            "clipboard_paste" => Action::ClipboardPaste,
            "trash" => Action::Trash,
            "remove" => {
                if permanently {
                    Action::Delete
                } else {
                    Action::Trash
                }
            }
            "delete" => Action::Delete,
            "rename" => Action::Rename,
            "create" => Action::Create,
            "bulk_create" => Action::BulkCreate,
            "filter" => Action::Filter,
            "find" => Action::Find(!previous),
            "find_next" | "find_arrow" => Action::FindNext(!previous),
            "search" => Action::Search(match arg(0)? {
                "fd" => SearchKind::Fd,
                "rg" => SearchKind::Rg,
                other => return Err(format!("search: unknown tool '{other}'")),
            }),
            "plugin" => match args.first().copied() {
                Some("fd") => Action::Search(SearchKind::Fd),
                Some("rg") => Action::Search(SearchKind::Rg),
                Some("fzf") => Action::Fzf,
                Some("zoxide") => Action::Zoxide,
                Some("trash") => Action::GotoTrash,
                Some(other) => return Err(format!("plugin: unsupported plugin '{other}'")),
                None => return Err("plugin: missing plugin name".into()),
            },
            "shell" => {
                if args.contains(&"--block") {
                    Action::ShellBlock
                } else {
                    Action::Shell
                }
            }
            "cancel_search" => Action::CancelSearch,
            "fzf" => Action::Fzf,
            "zoxide" => Action::Zoxide,
            "suspend" => Action::Suspend,
            "editor" => Action::Editor,
            "pager" => Action::Pager,
            "copy" if args.first() == Some(&"cell") => Action::InfoCopyCell,
            "copy" => Action::CopyPath(parse_copy(arg(0)?)?),
            "linemode" => Action::SetLinemode(parse_linemode(arg(0)?)?),
            "sort" => Action::SetSort(parse_sort(arg(0)?)?, reverse),
            "link" => {
                let relative = args.contains(&"--relative")
                    || matches!(args.first(), Some(&"relative") | Some(&"rel"));
                if !relative
                    && !args.is_empty()
                    && !matches!(args.first(), Some(&"absolute") | Some(&"abs"))
                {
                    return Err(format!("link: unknown mode '{}'", args[0]));
                }
                Action::Link(relative)
            }
            "hardlink" => Action::Hardlink,
            "new_tab" | "tab_create" => Action::NewTab(matches!(args.first(), Some(&"home"))),
            "rename_tab" | "tab_rename" => Action::RenameTab,
            "close_tab" | "tab_close" => Action::CloseTab,
            "switch_tab" => {
                let n = num(0)?;
                if n < 1 {
                    return Err("switch_tab: tab numbers start at 1".into());
                }
                Action::SwitchTab(n as usize - 1)
            }
            "tab_switch" => {
                let n = num(0)?;
                if args.contains(&"--relative") {
                    Action::NextTab(n)
                } else {
                    if n < 0 {
                        return Err("tab_switch: tab numbers start at 0".into());
                    }
                    Action::SwitchTab(n as usize)
                }
            }
            "next_tab" => Action::NextTab(num(0)?),
            "swap_tab" | "tab_swap" => Action::SwapTab(num(0)?),
            "task_move" => Action::TaskMove(num(0)?),
            "task_top" => Action::TaskTop,
            "task_bottom" => Action::TaskBottom,
            "task_inspect" | "inspect" => Action::TaskInspect,
            "task_cancel" | "cancel" => Action::TaskCancel,
            "info_move" => Action::InfoMove(num(0)?),
            "info_swipe" | "swipe" => match args.first().copied() {
                Some("prev" | "previous") => Action::InfoSwipe(-1),
                Some("next") => Action::InfoSwipe(1),
                _ => Action::InfoSwipe(num(0)?),
            },
            "info_copy_cell" => Action::InfoCopyCell,
            other => Action::Custom(other.to_string()),
        })
    }
}

fn parse_copy(name: &str) -> Result<CopyKind, String> {
    Ok(match name {
        "path" => CopyKind::Path,
        "url" => CopyKind::Url,
        "dir" | "dir_path" | "dirpath" => CopyKind::DirPath,
        "dir_url" | "dirurl" => CopyKind::DirUrl,
        "filename" | "name" => CopyKind::FileName,
        "stem" | "no_ext" | "name_no_ext" | "name_without_ext" => CopyKind::NameNoExt,
        other => return Err(format!("copy: unknown kind '{other}'")),
    })
}

pub fn parse_linemode(name: &str) -> Result<Linemode, String> {
    Ok(match name {
        "size" => Linemode::Size,
        "permissions" | "perms" => Linemode::Permissions,
        "btime" | "created" => Linemode::Btime,
        "mtime" | "modified" | "time" => Linemode::Mtime,
        "owner" => Linemode::Owner,
        "none" => Linemode::None,
        other => return Err(format!("linemode: unknown mode '{other}'")),
    })
}

pub fn parse_sort(name: &str) -> Result<Sort, String> {
    Ok(match name {
        "name" | "alphabetical" => Sort::Name,
        "natural" => Sort::Natural,
        "size" => Sort::Size,
        "mtime" | "modified" | "time" => Sort::Time,
        "btime" | "created" => Sort::Btime,
        "extension" | "ext" => Sort::Extension,
        "random" => Sort::Random,
        other => return Err(format!("sort: unknown key '{other}'")),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quit_cwd_roundtrips() {
        assert_eq!(Action::parse("quit_cwd").unwrap(), Action::QuitCwd);
        assert_eq!(Action::QuitCwd.name(), "quit_cwd");
        assert_eq!(Action::QuitCwd.label(), "quit_cwd");
    }

    #[test]
    fn parses_yazi_action_names() {
        let parse = Action::parse;

        assert_eq!(parse("arrow prev").unwrap(), Action::Move(-1));
        assert_eq!(parse("arrow next").unwrap(), Action::Move(1));
        assert_eq!(parse("arrow top").unwrap(), Action::Top);
        assert_eq!(parse("arrow bot").unwrap(), Action::Bottom);
        assert_eq!(parse("arrow -50%").unwrap(), Action::HalfPage(-1));
        assert_eq!(parse("arrow 100%").unwrap(), Action::Page(1));
        assert_eq!(parse("arrow 7").unwrap(), Action::Move(7));
        assert_eq!(parse("back").unwrap(), Action::History(-1));
        assert_eq!(parse("forward").unwrap(), Action::History(1));

        assert_eq!(parse("open --interactive").unwrap(), Action::OpenWith);
        assert_eq!(parse("yank --cut").unwrap(), Action::Cut);
        assert_eq!(parse("remove").unwrap(), Action::Trash);
        assert_eq!(parse("remove --permanently").unwrap(), Action::Delete);
        assert_eq!(parse("plugin fd").unwrap(), Action::Search(SearchKind::Fd));
        assert_eq!(parse("plugin rg").unwrap(), Action::Search(SearchKind::Rg));
        assert_eq!(parse("plugin fzf").unwrap(), Action::Fzf);
        assert_eq!(parse("plugin zoxide").unwrap(), Action::Zoxide);
        assert_eq!(parse("plugin trash").unwrap(), Action::GotoTrash);
        assert!(parse("plugin unknown").is_err());

        assert_eq!(parse("shell").unwrap(), Action::Shell);
        assert_eq!(parse("shell --interactive").unwrap(), Action::Shell);
        assert_eq!(
            parse("shell --block --interactive").unwrap(),
            Action::ShellBlock
        );
        assert_eq!(Action::ShellBlock.name(), "shell_block");

        assert_eq!(
            parse("copy dirpath").unwrap(),
            Action::CopyPath(CopyKind::DirPath)
        );
        assert_eq!(
            parse("copy dirurl").unwrap(),
            Action::CopyPath(CopyKind::DirUrl)
        );
        assert_eq!(
            parse("copy name_without_ext").unwrap(),
            Action::CopyPath(CopyKind::NameNoExt)
        );
        assert_eq!(parse("copy cell").unwrap(), Action::InfoCopyCell);

        assert_eq!(
            parse("find --previous --smart").unwrap(),
            Action::Find(false)
        );
        assert_eq!(parse("find_arrow").unwrap(), Action::FindNext(true));
        assert_eq!(
            parse("sort alphabetical --reverse=yes").unwrap(),
            Action::SetSort(Sort::Name, true)
        );
        assert_eq!(
            parse("sort mtime --reverse=no").unwrap(),
            Action::SetSort(Sort::Time, false)
        );

        assert_eq!(parse("cd ~").unwrap(), Action::Goto("~".into()));
        assert_eq!(parse("cd --interactive").unwrap(), Action::Cd);
        assert_eq!(
            parse("tab_create --current").unwrap(),
            Action::NewTab(false)
        );
        assert_eq!(parse("tab_switch 0").unwrap(), Action::SwitchTab(0));
        assert_eq!(
            parse("tab_switch 1 --relative").unwrap(),
            Action::NextTab(1)
        );
        assert_eq!(parse("tab_swap -1").unwrap(), Action::SwapTab(-1));
        assert_eq!(parse("tasks:show").unwrap(), Action::Tasks);
        assert_eq!(parse("inspect").unwrap(), Action::TaskInspect);
        assert_eq!(parse("swipe -1").unwrap(), Action::InfoSwipe(-1));
        assert_eq!(parse("swipe prev").unwrap(), Action::InfoSwipe(-1));
        assert_eq!(parse("swipe next").unwrap(), Action::InfoSwipe(1));

        assert_eq!(parse("toggle").unwrap(), Action::Toggle);
        assert_eq!(parse("toggle_select").unwrap(), Action::ToggleSelect);
        assert_eq!(parse("toggle_all --state=on").unwrap(), Action::SelectAll);
        assert_eq!(parse("toggle_all").unwrap(), Action::InvertSelection);
        assert_eq!(parse("visual_mode --unset").unwrap(), Action::VisualUnset);
        assert_eq!(parse("link").unwrap(), Action::Link(false));
        assert_eq!(parse("link --relative").unwrap(), Action::Link(true));
        assert_eq!(parse("link absolute").unwrap(), Action::Link(false));
    }
}
