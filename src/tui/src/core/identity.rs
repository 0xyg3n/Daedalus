//! Terminal identity is independent of display names, views and conversations.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Backend {
    Tmux,
    Herdr,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
pub struct TerminalId {
    pub backend: Backend,
    /// Exact tmux socket path or Herdr server name; never a display alias.
    pub endpoint: String,
    pub pane: String,
    /// Process generation for tmux, terminal UUID for Herdr.
    pub generation: String,
}

impl TerminalId {
    pub fn validate(&self) -> Result<(), &'static str> {
        for field in [&self.endpoint, &self.pane, &self.generation] {
            if field.is_empty() || field.len() > 4096 || field.chars().any(char::is_control) {
                return Err("Incomplete or invalid terminal identity");
            }
        }
        match self.backend {
            Backend::Tmux => {
                if !std::path::Path::new(&self.endpoint).is_absolute()
                    || !self
                        .pane
                        .strip_prefix('%')
                        .is_some_and(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
                {
                    return Err("Invalid tmux endpoint or pane ID");
                }
            }
            Backend::Herdr => {
                if self.endpoint.len() > 80
                    || matches!(self.endpoint.as_str(), "." | "..")
                    || !self
                        .endpoint
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
                {
                    return Err("Invalid Herdr server name");
                }
            }
        }
        Ok(())
    }

    /// Require exactly one live match immediately before a side effect.
    /// Backends must deduplicate linked views into physical terminals first.
    pub fn resolve<'a>(&self, live: &'a [Self]) -> Result<&'a Self, &'static str> {
        self.validate()?;
        let mut matches = live.iter().filter(|candidate| *candidate == self);
        let actual = matches
            .next()
            .ok_or("Terminal disappeared or changed; refresh and select again")?;
        if matches.next().is_some() {
            return Err("Ambiguous terminal identity");
        }
        Ok(actual)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn terminal() -> TerminalId {
        TerminalId {
            backend: Backend::Tmux,
            endpoint: crate::test_support::path("/tmp/example.sock").into(),
            pane: "%1".into(),
            generation: "v2:10:100:20:200".into(),
        }
    }
    #[test]
    fn restart_or_other_server_cannot_reuse_binding() {
        let target = terminal();
        let mut changed = target.clone();
        changed.generation = "v2:10:101:20:201".into();
        assert!(target.resolve(&[changed]).is_err());
        let mut changed = target.clone();
        changed.endpoint = crate::test_support::path("/tmp/other.sock").into();
        assert!(target.resolve(&[changed]).is_err());
        assert!(target.resolve(&[target.clone(), target.clone()]).is_err());
        assert!(target.resolve(std::slice::from_ref(&target)).is_ok());
    }
    #[test]
    fn invalid_identifiers_are_rejected_before_execution() {
        let mut target = terminal();
        target.pane = "-a".into();
        assert!(target.validate().is_err());
        target.backend = Backend::Herdr;
        target.endpoint = "../server".into();
        assert!(target.validate().is_err());
        target.endpoint = "project-1".into();
        target.generation.clear();
        assert!(target.validate().is_err());
    }
}
