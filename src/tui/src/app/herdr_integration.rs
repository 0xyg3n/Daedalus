//! Install a native Herdr popup binding without replacing unrelated configuration.
use std::{io::Write, path::Path};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};
const DESCRIPTION: &str = "Daedalus workspace map";
pub const HELP: &str = "daedalus herdr configure --file ABS [--key f12|prefix+LETTER] [--apply]\nPreview a Herdr popup binding available in every pane. --apply merges it into config.toml and saves a private backup. Existing conflicting bindings are refused. Run herdr config check and reload configuration after installation.\nThe popup runs daedalus popup; F12 closes it and returns to the original pane.\n";
pub fn merge(text: &str, key: &str) -> Result<String, String> {
    if text.len() > 1024 * 1024 {
        return Err("Herdr config exceeds 1 MiB".into());
    }
    if !(key
        .strip_prefix('f')
        .and_then(|v| v.parse::<u8>().ok())
        .is_some_and(|v| (1..=24).contains(&v))
        || key
            .strip_prefix("prefix+")
            .is_some_and(|s| s.len() == 1 && s.bytes().all(|c| c.is_ascii_lowercase())))
    {
        return Err("Use f1 through f24 or prefix+LETTER".into());
    }
    let mut doc = text
        .parse::<DocumentMut>()
        .map_err(|_| "Invalid Herdr TOML configuration")?;
    if doc.get("keys").is_none() {
        doc["keys"] = Item::Table(Table::new());
    }
    let keys = doc["keys"]
        .as_table_mut()
        .ok_or("Herdr keys must be a table")?;
    for (name, item) in keys.iter() {
        if name != "command" && contains_key(item, key) {
            return Err("Shortcut is already assigned in Herdr; choose another --key".into());
        }
    }
    if keys.get("command").is_none() {
        keys["command"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    let commands = keys["command"]
        .as_array_of_tables_mut()
        .ok_or("Herdr keys.command must use [[keys.command]] tables")?;
    for command in commands.iter() {
        if !managed(command)
            && command
                .get("key")
                .is_some_and(|item| contains_key(item, key))
        {
            return Err(
                "Shortcut is already assigned to another command; choose another --key".into(),
            );
        }
    }
    commands.retain(|t| !managed(t));
    let mut command = Table::new();
    command["key"] = value(key);
    command["command"] = value("daedalus popup");
    command["type"] = value("popup");
    command["description"] = value(DESCRIPTION);
    command["width"] = value("95%");
    command["height"] = value("95%");
    commands.push(command);
    Ok(doc.to_string())
}
fn managed(table: &Table) -> bool {
    table.get("description").and_then(Item::as_str) == Some(DESCRIPTION)
        && table.get("command").and_then(Item::as_str) == Some("daedalus popup")
        && table.get("type").and_then(Item::as_str) == Some("popup")
}

fn contains_key(item: &Item, key: &str) -> bool {
    if let Some(text) = item.as_str() {
        return text.eq_ignore_ascii_case(key);
    }
    if let Some(array) = item.as_array() {
        return array
            .iter()
            .any(|v| v.as_str().is_some_and(|s| s.eq_ignore_ascii_case(key)));
    }
    item.as_table()
        .is_some_and(|t| t.iter().any(|(_, i)| contains_key(i, key)))
}
pub fn run(args: &[String], out: &mut impl Write) -> Result<(), String> {
    if args.is_empty() || args == ["--help"] || args == ["-h"] {
        return out
            .write_all(HELP.as_bytes())
            .map_err(|_| "Cannot write integration help".into());
    }
    if args[0] != "configure" {
        return Err("Use daedalus herdr --help".into());
    }
    let (mut file, mut key, mut apply) = (None, "f12".to_owned(), false);
    let mut options = args[1..].iter();
    while let Some(option) = options.next() {
        match option.as_str() {
            "--file" if file.is_none() => file = Some(options.next().ok_or("Missing --file")?),
            "--key" => key = options.next().ok_or("Missing --key")?.clone(),
            "--apply" if !apply => apply = true,
            _ => return Err("Invalid integration option; use --help".into()),
        }
    }
    let path = Path::new(file.ok_or("Missing --file ABS")?);
    if !path.is_absolute() {
        return Err("Herdr configuration path must be absolute".into());
    }
    let before = crate::core::layouts::read_private_limit(path, 1024 * 1024)?.unwrap_or_default();
    let text = std::str::from_utf8(&before).map_err(|_| "Herdr config is not UTF-8")?;
    let after = merge(text, &key)?;
    if apply && before != after.as_bytes() {
        let backup = path.with_extension("toml.daedalus-backup");
        // Never overwrite a previous recovery copy.
        if !before.is_empty() && !backup.exists() {
            crate::core::layouts::write_private(&backup, &before)?;
        }
        if crate::core::layouts::read_private_limit(path, 1024 * 1024)?.unwrap_or_default()
            != before
        {
            return Err("Herdr config changed; preview again".into());
        }
        crate::core::layouts::write_private(path, after.as_bytes())?;
    }
    writeln!(
        out,
        "{}: {key} opens Daedalus as a Herdr popup in every pane. F12 closes the popup.{}",
        if apply { "Configured" } else { "Preview" },
        if apply {
            " Run herdr config check and reload configuration."
        } else {
            " Add --apply to install."
        }
    )
    .map_err(|_| "Cannot write integration result".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn merge_preserves_comments_and_commands_is_idempotent_and_refuses_collisions() {
        let original = "# user's config\n[theme]\nname = 'dark'\n[[keys.command]]\nkey = 'prefix+y'\ncommand = 'lazygit'\n";
        let once = merge(original, "f12").unwrap();
        assert!(once.contains("# user's config"));
        assert!(once.contains("lazygit"));
        let twice = merge(&once, "f12").unwrap();
        assert_eq!(once, twice);
        assert!(merge("[keys]\nzoom = 'f12'", "f12").is_err());
        assert!(merge("[[keys.command]]\nkey = 'f12'\ncommand = 'other'", "f12").is_err());
        assert!(merge("[broken", "f12").is_err());
    }
}
