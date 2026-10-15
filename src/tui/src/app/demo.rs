//! In-memory demonstration data, carried forward from the existing dashboard.
//! No discovery, external commands, private configuration or persistent writes.
use super::{
    backend::{self, Backend, Observations, Source},
    config::ControlConfig,
};
use crate::core::layouts::Layouts;
use serde_json::Value;
use std::{
    io::{BufRead, Write},
    time::{SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, String>;
pub struct Demo {
    fixture: Value,
    layouts: Layouts,
}
impl Demo {
    fn new() -> Result<Self> {
        let fixture: Value = serde_json::from_str(include_str!("../../assets/demo.json"))
            .map_err(|_| "Invalid built-in demonstration")?;
        #[cfg(windows)]
        let fixture = {
            fn native_paths(value: &mut Value, root: &std::path::Path) {
                match value {
                    Value::Object(fields) => {
                        for (key, value) in fields {
                            if [
                                "socket",
                                "cwd",
                                "directory",
                                "working_directory",
                                "path",
                                "transcript",
                                "endpoint",
                            ]
                            .contains(&key.as_str())
                                && let Some(path) = value
                                    .as_str()
                                    .filter(|path| path.starts_with('/') && !path.starts_with("//"))
                            {
                                *value = root
                                    .join(path.trim_start_matches('/'))
                                    .to_string_lossy()
                                    .into_owned()
                                    .into();
                            } else {
                                native_paths(value, root);
                            }
                        }
                    }
                    Value::Array(items) => {
                        for item in items {
                            native_paths(item, root);
                        }
                    }
                    _ => (),
                }
            }
            let mut fixture = fixture;
            native_paths(&mut fixture, &std::env::temp_dir().join("daedalus-demo"));
            fixture
        };
        Ok(Self {
            fixture,
            layouts: Layouts::default(),
        })
    }
}
impl Source for Demo {
    fn is_demo(&self) -> bool {
        true
    }
    fn refresh(&mut self, _: &ControlConfig) -> Result<Observations> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "Clock unavailable")?
            .as_secs_f64();
        let mut snapshot = self.fixture["snapshot"].clone();
        if let Some(panes) = snapshot["panes"].as_array_mut() {
            for pane in panes {
                if let Some(stamp) = pane["activity_at"].as_f64() {
                    pane["activity_at"] = (now - (1_000_000.0 - stamp)).into();
                }
            }
        }
        Ok(Observations {
            snapshot,
            layouts: self.layouts.clone(),
            now,
            ..Observations::default()
        })
    }
    fn preview(&mut self, terminal: &Value, rows: usize) -> Result<String> {
        let id = terminal["terminal_id"]
            .as_str()
            .ok_or("Unknown demonstration terminal")?;
        let text = self.fixture["previews"][id]
            .as_str()
            .ok_or("Unknown demonstration terminal")?;
        Ok(text.lines().take(rows).collect::<Vec<_>>().join("\n"))
    }
    fn save_layouts(&mut self, layouts: &Layouts) -> Result<()> {
        self.layouts = layouts.clone();
        Ok(())
    }
}
pub fn run(reader: &mut impl BufRead, writer: &mut impl Write) -> Result<()> {
    #[cfg(unix)]
    let directory = std::path::PathBuf::from("/daedalus-demo");
    #[cfg(windows)]
    let directory = std::env::temp_dir().join("daedalus-demo");
    let config = ControlConfig::fresh(&directory)?;
    backend::run_with(
        reader,
        writer,
        &mut Backend::new(directory, config, Demo::new()?)?,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    #[test]
    fn demo_preserves_ten_terminal_previews_and_never_uses_native_source() {
        let mut source = Demo::new().unwrap();
        let config =
            ControlConfig::fresh(Path::new(crate::test_support::path("/daedalus-demo"))).unwrap();
        let observations = source.refresh(&config).unwrap();
        let panes = observations.snapshot["panes"].as_array().unwrap();
        assert_eq!(panes.len(), 10);
        for pane in panes {
            assert!(!source.preview(pane, 200).unwrap().is_empty());
        }
        let mut bytes = Vec::new();
        run(
            &mut std::io::Cursor::new(b"{\"version\":2,\"op\":\"quit\"}\n"),
            &mut bytes,
        )
        .unwrap();
        let frame: Value =
            serde_json::from_slice(bytes.split(|b| *b == b'\n').next().unwrap()).unwrap();
        assert_eq!(frame["demo"], true);
        assert_eq!(frame["terminals"], 10);
        assert_eq!(frame["sections"].as_array().unwrap().len(), 11);
    }
}
