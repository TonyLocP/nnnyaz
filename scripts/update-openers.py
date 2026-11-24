#!/usr/bin/env python3
"""Generate nnnyaz/Yazi openers from installed desktop applications.

The generated blocks are delimited by comments, so rerunning the script only
replaces its own output and leaves hand-written configuration alone.
"""

from __future__ import annotations

import argparse
import fnmatch
import json
import os
import re
import shlex
import sys
import tomllib
from pathlib import Path

OPEN_START = "# >>> generated desktop openers >>>"
OPEN_END = "# <<< generated desktop openers <<<"
RULES_START = "# >>> generated desktop rules >>>"
RULES_END = "# <<< generated desktop rules <<<"

TRUE = {"1", "true", "yes"}
GENERIC_LAUNCHERS = {
    "env",
    "gtk-launch",
    "sh",
    "bash",
    "flatpak",
    "snap",
    "xdg-open",
}
DEFAULT_RULE_USES = [
    ("folder/*", ["edit", "open", "reveal"]),
    ("text/*", ["edit", "reveal"]),
    ("image/*", ["open", "reveal"]),
    ("{audio,video}/*", ["play", "reveal"]),
    (
        "application/{json,ndjson,javascript,wine-extension-ini}",
        ["edit", "reveal"],
    ),
    (
        "application/{zip,rar,7z*,tar,gzip,xz,zstd,bzip*,lzma,compress,archive,cpio,arj,xar,ms-cab*}",
        ["extract", "reveal"],
    ),
    ("inode/empty", ["edit", "reveal"]),
    ("vfs/{absent,stale}", ["download"]),
    ("trash/**", ["open", "trash"]),
]


def log(message: str) -> None:
    print(message, file=sys.stderr)


def toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def toml_key(value: str) -> str:
    return value if re.fullmatch(r"[A-Za-z0-9_-]+", value) else toml_string(value)


def desktop_dirs(extra: list[Path]) -> list[Path]:
    dirs: list[Path] = []
    home = Path(os.environ["HOME"]) if os.environ.get("HOME") else None

    def add(path: Path) -> None:
        if path not in dirs:
            dirs.append(path)

    if os.environ.get("XDG_DATA_HOME"):
        add(Path(os.environ["XDG_DATA_HOME"]) / "applications")
    elif home:
        add(home / ".local/share/applications")
    for base in os.environ.get("XDG_DATA_DIRS", "/usr/local/share:/usr/share").split(":"):
        if base:
            add(Path(base) / "applications")
    add(Path("/var/lib/flatpak/exports/share/applications"))
    if home:
        add(home / ".local/share/flatpak/exports/share/applications")
    add(Path("/var/lib/snapd/desktop/applications"))
    for path in extra:
        add(path)
    return dirs


def desktop_entries(dirs: list[Path]) -> list[dict[str, str | list[str] | bool]]:
    apps: list[dict[str, str | list[str] | bool]] = []
    seen: set[str] = set()
    for directory in dirs:
        if not directory.is_dir():
            continue
        for path in sorted(directory.glob("*.desktop")):
            app = parse_desktop(path)
            if app and app["id"] not in seen:
                seen.add(app["id"])
                apps.append(app)
    return apps


def parse_desktop(path: Path) -> dict[str, str | list[str] | bool] | None:
    try:
        text = path.read_text(encoding="utf-8")
    except (OSError, UnicodeDecodeError):
        return None

    values: dict[str, str] = {}
    localized_name: str | None = None
    in_main = False
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("["):
            in_main = line == "[Desktop Entry]"
            continue
        if not in_main or "=" not in line:
            continue
        key, value = line.split("=", 1)
        key, value = key.strip(), value.strip()
        if key == "Name" and not values.get("Name"):
            values[key] = value
        elif key.startswith("Name[") and localized_name is None and value:
            localized_name = value
        elif key.startswith("Name[") or key in values:
            continue
        else:
            values[key] = value

    exec_line = values.get("Exec", "")
    if not exec_line:
        return None
    name = values.get("Name") or localized_name or path.stem
    hidden = values.get("Hidden", "").lower() in TRUE
    no_display = values.get("NoDisplay", "").lower() in TRUE
    if hidden or no_display or values.get("Type", "Application") != "Application":
        return None

    mimes = [m for m in values.get("MimeType", "").split(";") if m]
    categories = [c for c in values.get("Categories", "").split(";") if c]
    if not mimes:
        if "MimeType" in values or "TextEditor" not in categories:
            return None
        if not any(code in exec_line for code in ("%f", "%F", "%u", "%U")):
            return None
        mimes = ["text/*"]

    return {
        "id": path.name,
        "name": name,
        "exec": exec_line,
        "try_exec": values.get("TryExec", ""),
        "mimes": mimes,
        "terminal": values.get("Terminal", "").lower() in TRUE,
    }


def first_program(command: str) -> str | None:
    words = shlex.split(command, posix=True)
    if not words:
        return None
    if words[0] == "env":
        for word in words[1:]:
            if word == "--":
                continue
            if word.startswith("-") or "=" in word:
                continue
            return word
        return None
    return words[0]


def executable_exists(spec: str) -> bool:
    path = Path(spec).expanduser()
    if path.parent != Path("."):
        return path.is_file() and os.access(path, os.X_OK)
    return shutil_which(spec) is not None


def shutil_which(spec: str) -> str | None:
    # Imported lazily only to keep the module header small.
    import shutil

    return shutil.which(spec)


def convert_exec(exec_line: str, name: str, app_id: str) -> str:
    out: list[str] = []
    chars = iter(exec_line)
    for char in chars:
        if char != "%":
            out.append(char)
            continue
        code = next(chars, "")
        if not code:
            out.append("%")
        elif code in ("f", "u"):
            out.append("%s1")
        elif code in ("F", "U"):
            out.append("%s")
        elif code == "d":
            out.append("%d1")
        elif code == "D":
            out.append("%d")
        elif code == "c":
            out.append(shlex.quote(name))
        elif code == "k":
            out.append(shlex.quote(app_id))
        elif code == "%":
            out.append("%")
    return "".join(out).strip()


def usable_apps(apps: list[dict[str, str | list[str] | bool]]) -> list[dict]:
    out: list[dict] = []
    for app in apps:
        program = first_program(str(app["exec"]))
        if not program:
            continue
        check = str(app["try_exec"]) or program
        if not executable_exists(check):
            continue
        command = convert_exec(str(app["exec"]), str(app["name"]), str(app["id"]))
        if not command:
            continue
        key = "desktop_" + re.sub(r"[^A-Za-z0-9_]", "_", str(app["id"]).removesuffix(".desktop"))
        out.append({**app, "key": key, "command": command})
    out.sort(key=lambda app: (str(app["name"]).lower(), str(app["id"])))
    return out


def binary_key(spec: str) -> str:
    name = Path(spec).name.lower()
    return re.sub(r"[^a-z0-9]", "", name)


def resolve_names(apps: list[dict], existing: dict) -> tuple[list[dict], dict[str, str]]:
    """Reuse existing openers when they already launch the same program."""
    opener_cfg = existing.get("opener", {})
    if not isinstance(opener_cfg, dict):
        opener_cfg = {}
    by_binary: dict[str, str] = {}
    by_id: dict[str, str] = {}
    for name, value in opener_cfg.items():
        if str(name).startswith("desktop_"):
            continue
        runs = []
        if isinstance(value, list):
            runs = [item.get("run", "") for item in value if isinstance(item, dict)]
        elif isinstance(value, dict):
            runs = [value.get("run", "")]
        for run in runs:
            if not isinstance(run, str):
                continue
            program = first_program(run)
            if program and Path(program).name not in GENERIC_LAUNCHERS:
                by_binary.setdefault(binary_key(program), name)
            for app in apps:
                if str(app["id"]) in run:
                    by_id.setdefault(str(app["id"]), name)

    existing_keys = set(opener_cfg)
    fresh: list[dict] = []
    names: dict[str, str] = {}
    for app in apps:
        key = str(app["key"])
        program = first_program(str(app["command"])) or ""
        name = by_id.get(str(app["id"])) or by_binary.get(binary_key(program))
        if name and name != key:
            names[key] = name
        elif key in existing_keys:
            names[key] = key
        else:
            names[key] = key
            fresh.append(app)
    return fresh, names


def opener_block(apps: list[dict]) -> list[str]:
    lines = [OPEN_START]
    used: set[str] = set()
    for app in apps:
        key = str(app["key"])
        if key in used:
            continue
        used.add(key)
        flags = 'block = true' if app["terminal"] else 'orphan = true, for = "unix"'
        lines.extend(
            [
                f"{toml_key(key)} = [",
                f"    {{ run = {toml_string(str(app['command']))}, "
                f"desc = {toml_string(str(app['name']))}, {flags} }},",
                "]",
            ]
        )
    lines.append(OPEN_END)
    return lines


def expand_braces(pattern: str) -> list[str]:
    start = pattern.find("{")
    if start < 0:
        return [pattern]
    close = pattern.find("}", start + 1)
    if close < 0:
        return [pattern]
    out: list[str] = []
    for alt in pattern[start + 1 : close].split(","):
        out.extend(expand_braces(pattern[:start] + alt + pattern[close + 1 :]))
    return out


def mime_overlaps(pattern: str, mime: str) -> bool:
    """Whether a configured MIME pattern and a generated MIME type can match."""
    return any(
        fnmatch.fnmatchcase(mime, expanded) or fnmatch.fnmatchcase(expanded, mime)
        for expanded in expand_braces(pattern)
    )


def rule_uses(existing: dict, mime: str, generated: list[str]) -> list[str]:
    open_cfg = existing.get("open", {})
    if not isinstance(open_cfg, dict):
        open_cfg = {}
    rules: list = []
    for key in ("prepend_rules", "rules", "append_rules"):
        value = open_cfg.get(key, [])
        if isinstance(value, list):
            rules.extend(value)

    uses: list[str] = []
    for rule in rules:
        if not isinstance(rule, dict):
            continue
        pattern = rule.get("mime")
        if not isinstance(pattern, str) or not mime_overlaps(pattern, mime):
            continue
        value = rule.get("use", [])
        if isinstance(value, str):
            uses.append(value)
        elif isinstance(value, list):
            uses.extend(item for item in value if isinstance(item, str))
    if "rules" not in open_cfg:
        for pattern, defaults in DEFAULT_RULE_USES:
            if mime_overlaps(pattern, mime):
                uses.extend(defaults)

    result: list[str] = []
    for item in [*uses, *generated, "open", "reveal"]:
        if item not in result:
            result.append(item)
    return result


def rules_block(apps: list[dict], existing: dict, names: dict[str, str]) -> list[str]:
    by_mime: dict[str, list[str]] = {}
    for app in apps:
        name = names.get(str(app["key"]), str(app["key"]))
        for mime in app["mimes"]:
            by_mime.setdefault(str(mime), [])
            if name not in by_mime[str(mime)]:
                by_mime[str(mime)].append(name)
    lines = [RULES_START]
    # Specific MIME types must come before wildcard patterns: Yazi uses the
    # first matching rule, so a broad `text/*` rule would hide `text/plain`.
    for mime in sorted(by_mime, key=lambda m: (m.count("*") + m.count("?"), m)):
        matching_apps: list[str] = []
        for other, others in by_mime.items():
            if other == mime or mime_overlaps(mime, other):
                for name in others:
                    if name not in matching_apps:
                        matching_apps.append(name)
        uses = rule_uses(existing, mime, matching_apps)
        use_list = ", ".join(toml_string(item) for item in uses)
        lines.append(f"    {{ mime = {toml_string(mime)}, use = [ {use_list} ] }},")
    lines.append(RULES_END)
    return lines


def replace_markers(lines: list[str], start: str, end: str, block: list[str]) -> bool:
    try:
        first = next(i for i, line in enumerate(lines) if line.strip() == start)
        last = next(i for i in range(first + 1, len(lines)) if lines[i].strip() == end)
    except StopIteration:
        return False
    lines[first : last + 1] = block
    return True


def section_end(lines: list[str], start: int) -> int:
    for index in range(start + 1, len(lines)):
        if lines[index].lstrip().startswith("["):
            return index
    return len(lines)


def insert_in_section(lines: list[str], section: str, block: list[str]) -> None:
    header = f"[{section}]"
    try:
        start = next(i for i, line in enumerate(lines) if line.strip() == header)
    except StopIteration:
        first_table = next(
            (i for i, line in enumerate(lines) if line.lstrip().startswith("[")), None
        )
        if first_table is None:
            lines.extend([header, *block])
        else:
            lines[first_table:first_table] = [header, *block, ""]
        return
    end = section_end(lines, start)
    while end > start + 1 and not lines[end - 1].strip():
        end -= 1
    lines[end:end] = ["", *block, ""]


def array_open(lines: list[str], key: str) -> tuple[int, int] | None:
    pattern = re.compile(rf"^\s*{re.escape(key)}\s*=\s*\[")
    for line_no, line in enumerate(lines):
        match = pattern.match(line)
        if match:
            return line_no, line.index("[", match.start())
    return None


def insert_rules(lines: list[str], block: list[str]) -> None:
    opener = array_open(lines, "prepend_rules")
    if opener is None:
        insert_in_section(lines, "open", ["prepend_rules = [", *block, "]"])
        return
    line_no, pos = opener
    line = lines[line_no]
    after = line[pos + 1 :]
    if after.strip():
        lines[line_no : line_no + 1] = [line[: pos + 1].rstrip(), *block, after]
    else:
        lines[line_no + 1 : line_no + 1] = block


def strip_markers(text: str) -> str:
    """Return config text without previously generated blocks."""
    out: list[str] = []
    skipping = False
    for line in text.splitlines():
        stripped = line.strip()
        if stripped in (OPEN_START, RULES_START):
            skipping = True
        elif stripped in (OPEN_END, RULES_END):
            skipping = False
        elif not skipping:
            out.append(line)
    return "\n".join(out)


def update_config(path: Path, apps: list[dict], dry_run: bool) -> None:
    try:
        text = path.read_text(encoding="utf-8")
    except FileNotFoundError:
        text = ""
    except OSError as error:
        log(f"error: cannot read {path}: {error}")
        raise SystemExit(1) from error

    try:
        existing = tomllib.loads(strip_markers(text))
    except tomllib.TOMLDecodeError as error:
        log(f"error: {path} is not valid TOML: {error}")
        raise SystemExit(1) from error

    fresh, names = resolve_names(apps, existing)
    openers = opener_block(fresh)
    rules = rules_block(apps, existing, names)
    lines = text.splitlines()
    if not replace_markers(lines, OPEN_START, OPEN_END, openers):
        insert_in_section(lines, "opener", openers)
    if not replace_markers(lines, RULES_START, RULES_END, rules):
        insert_rules(lines, rules)
    updated = "\n".join(lines)
    if text.endswith("\n") or updated:
        updated += "\n"

    if dry_run:
        print(f"# {path}")
        print(updated)
        return
    if updated == text:
        log(f"unchanged: {path}")
        return
    path.parent.mkdir(parents=True, exist_ok=True)
    backup = path.with_suffix(path.suffix + ".bak")
    if path.exists() and not backup.exists():
        backup.write_text(text, encoding="utf-8")
        suffix = f" (backup: {backup})"
    else:
        suffix = ""
    path.write_text(updated, encoding="utf-8")
    log(f"updated: {path}{suffix}")


def default_config_paths(explicit: str | None, home_var: str, xdg_name: str) -> Path:
    if explicit:
        return Path(explicit).expanduser()
    if os.environ.get(home_var):
        return Path(os.environ[home_var]).expanduser() / Path(xdg_name).name
    xdg = os.environ.get("XDG_CONFIG_HOME")
    if xdg:
        return Path(xdg).expanduser() / xdg_name
    home = Path(os.environ["HOME"]) if os.environ.get("HOME") else Path.home()
    return home / ".config" / xdg_name


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--nnnyaz-config", help="path to nnnyaz config.toml")
    parser.add_argument("--yazi-config", help="path to yazi.toml")
    parser.add_argument(
        "--scan-dir",
        action="append",
        default=[],
        type=Path,
        help="extra applications directory (repeatable)",
    )
    parser.add_argument("--dry-run", action="store_true", help="print configs instead of writing")
    args = parser.parse_args()

    apps = usable_apps(desktop_entries(desktop_dirs(args.scan_dir)))
    if not apps:
        log("error: no usable desktop applications found")
        return 1

    paths = [
        default_config_paths(args.nnnyaz_config, "NNNYAZ_CONFIG_HOME", "nnnyaz/config.toml"),
        default_config_paths(args.yazi_config, "YAZI_CONFIG_HOME", "yazi/yazi.toml"),
    ]
    for path in paths:
        update_config(path, apps, args.dry_run)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
