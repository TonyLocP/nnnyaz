//! Optional `init.lua` overrides and named custom actions.
//!
//! The chunk may return a table using the same schema as the TOML files, and
//! it may call the small `rnn` API while it runs:
//!
//! ```lua
//! rnn.keymap.set("mgr", "<C-s>", "search fd", "search")
//! return { mgr = { show_hidden = true }, theme = { directory = "red" } }
//! ```

use std::{fs, path::Path};

use mlua::{Lua, Table, Value};

use super::{keymap::Context, Config};

const BOOTSTRAP: &str = r#"
rnn = {
  _sets = {},
  _notify = {},
  _actions = {},
  keymap = {},
}
function rnn.notify(msg)
  table.insert(rnn._notify, tostring(msg))
end
function rnn.keymap.set(ctx, on, run, desc)
  table.insert(rnn._sets, { ctx = ctx, on = on, run = run, desc = desc })
end
function rnn.register_action(name, fn)
  rnn._actions[name] = fn
end
"#;

pub fn apply(config: &mut Config, dir: &Path, warnings: &mut Vec<String>) {
    let path = dir.join("init.lua");
    let src = match fs::read_to_string(&path) {
        Ok(src) => src,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return,
        Err(e) => {
            warnings.push(format!("{}: {e}", path.display()));
            return;
        }
    };

    let lua = Lua::new();
    if let Err(e) = lua.load(BOOTSTRAP).set_name("@rnn-bootstrap").exec() {
        warnings.push(format!("init.lua: cannot initialize Lua: {e}"));
        return;
    }
    set_snapshot(&lua, config);

    let returned: Value = match lua.load(&src).set_name("@init.lua").eval() {
        Ok(value) => value,
        Err(e) => {
            warnings.push(format!("{}: {e}", path.display()));
            return;
        }
    };

    apply_calls(&lua, config, warnings);
    collect_notices(&lua, warnings);

    if let Value::Table(table) = returned {
        apply_returned(table, config, warnings);
    }

    config.lua = Some(lua);
}

fn set_snapshot(lua: &Lua, config: &Config) {
    let Ok(rnn) = lua.globals().get::<Table>("rnn") else {
        return;
    };
    let _ = rnn.set("version", env!("CARGO_PKG_VERSION"));
    let Ok(snapshot) = lua.create_table() else {
        return;
    };
    let Ok(mgr) = lua.create_table() else {
        return;
    };
    let _ = mgr.set("ratio", vec![config.mgr.ratio.0, config.mgr.ratio.1]);
    let _ = mgr.set("show_hidden", config.mgr.show_hidden);
    let _ = mgr.set("show_preview", config.mgr.show_preview);
    let _ = mgr.set("scrolloff", config.mgr.scrolloff as i64);
    let _ = snapshot.set("mgr", mgr);
    let _ = rnn.set("config", snapshot);
}

fn apply_calls(lua: &Lua, config: &mut Config, warnings: &mut Vec<String>) {
    let Ok(rnn) = lua.globals().get::<Table>("rnn") else {
        return;
    };
    let Ok(sets) = rnn.get::<Table>("_sets") else {
        return;
    };
    for pair in sets.pairs::<Value, Value>() {
        let Ok((_, value)) = pair else {
            continue;
        };
        let Value::Table(entry) = value else {
            continue;
        };
        let ctx_name = entry.get::<String>("ctx").ok();
        let on = entry.get::<String>("on").ok();
        let run = entry.get::<String>("run").ok();
        let desc = entry.get::<String>("desc").ok();
        match (ctx_name, on, run) {
            (Some(name), Some(on), Some(run)) => match context_by_name(&name) {
                Some(ctx) => config.keymap.set_binding(ctx, &on, &run, desc, warnings),
                None => warnings.push(format!("init.lua: unknown keymap context '{name}'")),
            },
            _ => warnings
                .push("init.lua: rnn.keymap.set(ctx, on, run) expects string arguments".into()),
        }
    }
}

fn context_by_name(name: &str) -> Option<Context> {
    match name {
        "mgr" => Some(Context::Mgr),
        "tasks" => Some(Context::Tasks),
        "info" | "spot" => Some(Context::Info),
        _ => None,
    }
}

fn collect_notices(lua: &Lua, warnings: &mut Vec<String>) {
    let Ok(rnn) = lua.globals().get::<Table>("rnn") else {
        return;
    };
    let Ok(notices) = rnn.get::<Table>("_notify") else {
        return;
    };
    for notice in notices.sequence_values::<String>().flatten() {
        warnings.push(notice);
    }
}

fn apply_returned(table: Table, config: &mut Config, warnings: &mut Vec<String>) {
    if let Ok(value) = table.get::<Value>("keymap") {
        if !matches!(value, Value::Nil) {
            match to_toml(&value) {
                Ok(value) => config.keymap.apply(&value, warnings),
                Err(e) => warnings.push(format!("init.lua: keymap: {e}")),
            }
        }
        let _ = table.set("keymap", Value::Nil);
    }
    if let Ok(value) = table.get::<Value>("theme") {
        if !matches!(value, Value::Nil) {
            match to_toml(&value) {
                Ok(value) => config.theme.apply(&value, warnings),
                Err(e) => warnings.push(format!("init.lua: theme: {e}")),
            }
        }
        let _ = table.set("theme", Value::Nil);
    }

    match to_toml(&Value::Table(table)) {
        Ok(value) => config.apply_value(value, warnings),
        Err(e) => warnings.push(format!("init.lua: {e}")),
    }
}

fn to_toml(value: &Value) -> Result<toml::Value, String> {
    Ok(match value {
        Value::Nil => return Err("nil cannot be a config value".into()),
        Value::Boolean(b) => toml::Value::Boolean(*b),
        Value::Integer(i) => toml::Value::Integer(*i),
        Value::Number(n) => toml::Value::Float(*n),
        Value::String(s) => toml::Value::String(s.to_string_lossy()),
        Value::Table(table) => {
            let len = table.raw_len();
            let mut keys = Vec::new();
            for pair in table.clone().pairs::<Value, Value>() {
                let (key, _) = pair.map_err(|e| e.to_string())?;
                keys.push(key);
            }
            let is_array = len > 0
                && keys.len() == len
                && keys.iter().all(|key| match key {
                    Value::Integer(i) => *i >= 1 && (*i as usize) <= len,
                    _ => false,
                });
            if is_array {
                let mut out = Vec::with_capacity(len);
                for i in 1..=len {
                    let item: Value = table.raw_get(i).map_err(|e| e.to_string())?;
                    out.push(to_toml(&item)?);
                }
                toml::Value::Array(out)
            } else {
                let mut out = toml::value::Table::new();
                for pair in table.clone().pairs::<Value, Value>() {
                    let (key, item) = pair.map_err(|e| e.to_string())?;
                    let key = match key {
                        Value::String(s) => s.to_string_lossy(),
                        other => {
                            return Err(format!(
                                "table key must be a string, got {}",
                                other.type_name()
                            ))
                        }
                    };
                    out.insert(key, to_toml(&item)?);
                }
                toml::Value::Table(out)
            }
        }
        other => return Err(format!("unsupported value type {}", other.type_name())),
    })
}

/// Names registered with `rnn.register_action`, for keymap validation.
pub fn registered_actions(lua: &Lua) -> Vec<String> {
    let Ok(rnn) = lua.globals().get::<Table>("rnn") else {
        return Vec::new();
    };
    let Ok(actions) = rnn.get::<Table>("_actions") else {
        return Vec::new();
    };
    actions
        .pairs::<String, Value>()
        .flatten()
        .map(|(name, _)| name)
        .collect()
}

/// Run a named action registered from Lua. Returns the last `rnn.notify`
/// message, if any.
pub fn run_action(lua: &Lua, name: &str) -> Result<Option<String>, String> {
    let rnn = lua
        .globals()
        .get::<Table>("rnn")
        .map_err(|e| e.to_string())?;
    let actions = rnn.get::<Table>("_actions").map_err(|e| e.to_string())?;
    let function = actions
        .get::<mlua::Function>(name)
        .map_err(|_| format!("unknown action '{name}'"))?;
    function.call::<()>(()).map_err(|e| e.to_string())?;

    let notices = rnn.get::<Table>("_notify").map_err(|e| e.to_string())?;
    let last = notices.sequence_values::<String>().flatten().last();
    let _ = notices.clear();
    Ok(last)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "rnn-lua-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn lua_returns_overrides() {
        let dir = temp_dir("overrides");
        fs::write(
            dir.join("init.lua"),
            r#"
            rnn.keymap.set("mgr", "<C-s>", "search fd")
            return {
              mgr = { show_hidden = true, ratio = { 30, 70 } },
              theme = { directory = "red" },
            }
            "#,
        )
        .unwrap();
        let mut config = Config::default();
        let mut warnings = Vec::new();
        apply(&mut config, &dir, &mut warnings);
        assert!(warnings.is_empty(), "{warnings:?}");
        assert!(config.mgr.show_hidden);
        assert_eq!(config.mgr.ratio, (30, 70));
        assert!(config.lua.is_some());
        let _ = fs::remove_dir_all(dir);
    }

    #[test]
    fn lua_errors_warn_and_keep_defaults() {
        let dir = temp_dir("error");
        fs::write(dir.join("init.lua"), "this is not lua").unwrap();
        let mut config = Config::default();
        let mut warnings = Vec::new();
        apply(&mut config, &dir, &mut warnings);
        assert!(!warnings.is_empty());
        assert!(!config.mgr.show_hidden);
        let _ = fs::remove_dir_all(dir);
    }
}
