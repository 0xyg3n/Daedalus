//! Explicit per-launch modes; unsupported providers retain their native policy.
pub fn choices(tool: &str) -> Vec<(&'static str, &'static str, &'static str)> {
    let mut choices = vec![(
        "native",
        "Native profile default",
        "Keep the selected account's existing permission policy",
    )];
    if matches!(tool, "codex" | "claude") {
        choices.extend([
            (
                "supervised",
                "Supervised",
                "Ask before commands; keep workspace restrictions",
            ),
            (
                "edits",
                "Auto-accept edits",
                "Allow workspace edits; commands can still ask",
            ),
            (
                "auto",
                "Automatic review",
                "Use the provider's own automatic approval review",
            ),
            (
                "full",
                "Full access",
                "Commands and file changes can run without approval or sandbox restrictions",
            ),
        ]);
    }
    choices
}
pub fn parse(selection: &str) -> Result<(&str, &str), String> {
    let (tool, mode) = selection.split_once('@').unwrap_or((selection, "native"));
    if mode.contains('@') || !choices(tool).iter().any(|c| c.0 == mode) {
        return Err("This permission mode is unavailable for the selected provider".into());
    }
    Ok((tool, mode))
}
pub fn apply(selection: &str, args: &mut Vec<String>) -> Result<(), String> {
    let (tool, mode) = parse(selection)?;
    if mode == "native" {
        return Ok(());
    }
    let mut clean = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let arg = &args[i];
        if matches!(
            arg.as_str(),
            "--sandbox" | "-s" | "--ask-for-approval" | "-a" | "--permission-mode"
        ) {
            i += 2;
            continue;
        }
        if matches!(
            arg.as_str(),
            "--dangerously-bypass-approvals-and-sandbox"
                | "--dangerously-skip-permissions"
                | "--approve-for-me"
                | "--full-auto"
        ) || ["--sandbox=", "--ask-for-approval=", "--permission-mode="]
            .iter()
            .any(|p| arg.starts_with(p))
        {
            i += 1;
            continue;
        }
        clean.push(arg.clone());
        i += 1;
    }
    let values: &[&str] = match (tool, mode) {
        ("codex", "supervised") => &[
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "untrusted",
        ],
        ("codex", "edits") => &[
            "--sandbox",
            "workspace-write",
            "--ask-for-approval",
            "on-request",
        ],
        ("codex", "auto") => &["--approve-for-me"],
        ("codex", "full") => &["--dangerously-bypass-approvals-and-sandbox"],
        ("claude", "supervised") => &["--permission-mode", "default"],
        ("claude", "edits") => &["--permission-mode", "acceptEdits"],
        ("claude", "auto") => &["--permission-mode", "auto"],
        ("claude", "full") => &["--permission-mode", "bypassPermissions"],
        _ => return Err("Unsupported permission contract".into()),
    };
    clean.extend(values.iter().map(|s| s.to_string()));
    *args = clean;
    Ok(())
}
pub fn label(selection: &str) -> String {
    match parse(selection) {
        Ok((tool, mode)) => format!(
            "{tool} · {}",
            choices(tool)
                .iter()
                .find(|c| c.0 == mode)
                .map(|c| c.1)
                .unwrap_or("Native default")
        ),
        Err(_) => selection.into(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn explicit_supervision_removes_inherited_bypass_but_keeps_model_and_profile() {
        let mut args = vec![
            "--model".into(),
            "chosen-model".into(),
            "--dangerously-bypass-approvals-and-sandbox".into(),
            "--sandbox=danger-full-access".into(),
        ];
        apply("codex@supervised", &mut args).unwrap();
        assert_eq!(
            args,
            [
                "--model",
                "chosen-model",
                "--sandbox",
                "workspace-write",
                "--ask-for-approval",
                "untrusted"
            ]
        );
        assert!(parse("grok@edits").is_err());
        assert!(parse("codex@full@supervised").is_err());
        apply("codex@full", &mut args).unwrap();
        assert!(!args.iter().any(|a| a == "workspace-write"));
        assert_eq!(
            args.last().unwrap(),
            "--dangerously-bypass-approvals-and-sandbox"
        );
    }
}
