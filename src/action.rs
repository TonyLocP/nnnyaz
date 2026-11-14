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
    Fzf,
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
            Action::Fzf => "fzf",
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
        let reverse = args.iter().any(|a| *a == "--reverse" || *a == "-r");
        let force = args.iter().any(|a| *a == "--force" || *a == "-f");

        Ok(match name {
            "quit" => Action::Quit,
            "quit_cwd" => Action::QuitCwd,
            "quit_or_close" => Action::QuitOrClose,
            "close" => Action::Close,
            "escape" => Action::Escape,
            "help" => Action::Help,
            "tasks" => Action::Tasks,
            "info" | "spot" => Action::Info,
            "nop" => Action::Nop,
            "move" | "arrow" => Action::Move(num(0)?),
            "half_page" => Action::HalfPage(num(0)? as i8),
            "page" => Action::Page(num(0)? as i8),
            "top" => Action::Top,
            "bottom" => Action::Bottom,
            "parent" | "leave" => Action::Parent,
            "open" | "enter" => Action::Open,
            "open_with" => Action::OpenWith,
            "history" => Action::History(num(0)? as i8),
            "follow" => Action::Follow,
            "goto" => Action::Goto(arg(0)?.to_string()),
            "goto_trash" => Action::GotoTrash,
            "cd" => Action::Cd,
            "hidden" => Action::Hidden,
            "toggle_preview" => Action::TogglePreview,
            "zoom" => match arg(0)? {
                "in" | "+" => Action::Zoom(1),
                "out" | "-" => Action::Zoom(-1),
                "reset" | "fit" | "0" => Action::Zoom(0),
                other => return Err(format!("zoom: unknown direction '{other}'")),
            },
            "seek" => Action::Seek(num(0)? as i16),
            "toggle_select" | "toggle" => Action::ToggleSelect,
            "select_all" => Action::SelectAll,
            "invert_selection" => Action::InvertSelection,
            "visual_select" | "visual_mode" => Action::VisualSelect,
            "visual_unset" => Action::VisualUnset,
            "yank" | "copy" if args.is_empty() => Action::Yank,
            "cut" => Action::Cut,
            "paste" => Action::Paste(force),
            "unyank" => Action::Unyank,
            "clipboard_copy" => Action::ClipboardCopy,
            "clipboard_paste" => Action::ClipboardPaste,
            "trash" | "remove" => Action::Trash,
            "delete" => Action::Delete,
            "rename" => Action::Rename,
            "create" => Action::Create,
            "bulk_create" => Action::BulkCreate,
            "filter" => Action::Filter,
            "find" => Action::Find(!matches!(
                args.first(),
                Some(&"backward") | Some(&"prev") | Some(&"previous")
            )),
            "find_next" => Action::FindNext(!matches!(
                args.first(),
                Some(&"backward") | Some(&"prev") | Some(&"previous")
            )),
            "search" => Action::Search(match arg(0)? {
                "fd" => SearchKind::Fd,
                "rg" => SearchKind::Rg,
                other => return Err(format!("search: unknown tool '{other}'")),
            }),
            "shell" => Action::Shell,
            "cancel_search" => Action::CancelSearch,
            "fzf" => Action::Fzf,
            "suspend" => Action::Suspend,
            "editor" => Action::Editor,
            "pager" => Action::Pager,
            "copy" => Action::CopyPath(parse_copy(arg(0)?)?),
            "linemode" => Action::SetLinemode(parse_linemode(arg(0)?)?),
            "sort" => Action::SetSort(parse_sort(arg(0)?)?, reverse),
            "link" => Action::Link(match arg(0)? {
                "relative" | "rel" => true,
                "absolute" | "abs" => false,
                other => return Err(format!("link: unknown mode '{other}'")),
            }),
            "hardlink" => Action::Hardlink,
            "new_tab" => Action::NewTab(matches!(args.first(), Some(&"home"))),
            "rename_tab" => Action::RenameTab,
            "close_tab" => Action::CloseTab,
            "switch_tab" => {
                let n = num(0)?;
                if n < 1 {
                    return Err("switch_tab: tab numbers start at 1".into());
                }
                Action::SwitchTab(n as usize - 1)
            }
            "next_tab" => Action::NextTab(num(0)?),
            "swap_tab" => Action::SwapTab(num(0)?),
            "task_move" => Action::TaskMove(num(0)?),
            "task_top" => Action::TaskTop,
            "task_bottom" => Action::TaskBottom,
            "task_inspect" => Action::TaskInspect,
            "task_cancel" => Action::TaskCancel,
            "info_move" => Action::InfoMove(num(0)?),
            "info_swipe" => Action::InfoSwipe(num(0)?),
            "info_copy_cell" => Action::InfoCopyCell,
            other => Action::Custom(other.to_string()),
        })
    }
}

fn parse_copy(name: &str) -> Result<CopyKind, String> {
    Ok(match name {
        "path" => CopyKind::Path,
        "url" => CopyKind::Url,
        "dir" | "dir_path" => CopyKind::DirPath,
        "dir_url" => CopyKind::DirUrl,
        "filename" | "name" => CopyKind::FileName,
        "stem" | "no_ext" | "name_no_ext" => CopyKind::NameNoExt,
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
        "name" => Sort::Name,
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
}
