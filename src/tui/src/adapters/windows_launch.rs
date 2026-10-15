//! Windows launch boundary: one encoded PowerShell command starts an exact
//! executable with an argument vector and an explicit, cleared environment.
//! The terminal's current shell never interprets user arguments or env values.
use crate::core::checkpoints::LaunchPlan;
use base64::{Engine, engine::general_purpose::STANDARD};

fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}
/// Windows CRT command-line encoding, used by ProcessStartInfo on .NET Framework
/// where ArgumentList is unavailable. This text is never passed to cmd.exe.
fn argument(value: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for c in value.chars() {
        if c == '\\' {
            slashes += 1;
            continue;
        }
        if c == '"' {
            out.extend(std::iter::repeat_n('\\', slashes * 2 + 1));
        } else {
            out.extend(std::iter::repeat_n('\\', slashes));
        }
        out.push(c);
        slashes = 0;
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}
fn script(plan: &LaunchPlan) -> Result<String, String> {
    if !plan
        .binary
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("exe"))
    {
        return Err("Native Windows launches require a .exe; select the underlying executable instead of a .cmd/.bat shim".into());
    }
    let mut script = String::from(
        "$ErrorActionPreference='Stop'; $p=New-Object System.Diagnostics.ProcessStartInfo; $p.UseShellExecute=$false; $p.EnvironmentVariables.Clear(); ",
    );
    script.push_str(&format!(
        "$p.FileName={}; $p.WorkingDirectory={}; $p.Arguments={}; ",
        literal(&plan.binary.to_string_lossy()),
        literal(&plan.cwd.to_string_lossy()),
        literal(
            &plan
                .args
                .iter()
                .map(|a| argument(a))
                .collect::<Vec<_>>()
                .join(" ")
        )
    ));
    for (key, value) in &plan.environment {
        script.push_str(&format!(
            "$p.EnvironmentVariables[{}]={}; ",
            literal(key),
            literal(value)
        ));
    }
    script.push_str("$child=[System.Diagnostics.Process]::Start($p); if ($null -eq $child) { throw 'Native process did not start' }; $child.WaitForExit(); $code=$child.ExitCode; $child.Dispose(); exit $code");
    Ok(script)
}
pub(crate) fn argv(plan: &LaunchPlan) -> Result<Vec<String>, String> {
    let script = script(plan)?;
    let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    Ok(vec![
        "powershell.exe".into(),
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-EncodedCommand".into(),
        STANDARD.encode(bytes),
    ])
}
pub(crate) fn command(plan: &LaunchPlan) -> Result<String, String> {
    Ok(argv(plan)?.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crt_arguments_preserve_empty_quotes_and_trailing_slashes() {
        assert_eq!(argument(""), "\"\"");
        assert_eq!(argument("a\"b"), "\"a\\\"b\"");
        assert_eq!(argument("C:\\space dir\\"), "\"C:\\space dir\\\\\"");
    }
    #[cfg(windows)]
    #[test]
    fn quoted_arguments_roundtrip_through_windows_parser() {
        use windows_sys::Win32::{Foundation::LocalFree, UI::Shell::CommandLineToArgvW};
        let values = [
            "",
            "plain",
            "two words",
            "quote\"here",
            "C:\\space path\\",
            "a\\\"b",
            "%PATH% & $(x); ` λ 🚀",
        ];
        let text = format!(
            "fixture.exe {}",
            values
                .iter()
                .map(|s| argument(s))
                .collect::<Vec<_>>()
                .join(" ")
        );
        let encoded: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let mut count = 0;
        let words = unsafe { CommandLineToArgvW(encoded.as_ptr(), &mut count) };
        assert!(!words.is_null());
        let decoded = unsafe { std::slice::from_raw_parts(words, count as usize) }
            .iter()
            .skip(1)
            .map(|pointer| {
                let mut length = 0;
                unsafe {
                    while *pointer.add(length) != 0 {
                        length += 1;
                    }
                    String::from_utf16(std::slice::from_raw_parts(*pointer, length)).unwrap()
                }
            })
            .collect::<Vec<_>>();
        unsafe {
            LocalFree(words.cast());
        }
        assert_eq!(decoded, values);
    }
    #[test]
    fn encoded_launch_keeps_metacharacters_out_of_terminal_shell() {
        let plan = LaunchPlan {
            operation: crate::core::checkpoints::Operation::Resume,
            binary: "C:/Tools/agent.exe".into(),
            args: vec!["x'; & calc.exe; #\"\\".into()],
            cwd: "C:/project".into(),
            environment: std::collections::BTreeMap::from([
                ("HOME".into(), "C:/user's space".into()),
                ("PATH".into(), "C:/Tools".into()),
            ]),
        };
        let argv = argv(&plan).unwrap();
        assert!(
            argv[5]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"+/=".contains(&b))
        );
        let bytes = STANDARD.decode(&argv[5]).unwrap();
        let decoded = String::from_utf16(
            &bytes
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(decoded.contains("EnvironmentVariables.Clear()"));
        assert!(decoded.contains("C:/user''s space"));
        assert!(decoded.contains("UseShellExecute=$false"));
        assert!(!command(&plan).unwrap().contains("calc.exe"));
        let mut shim = plan;
        shim.binary = "C:/agent.cmd".into();
        assert!(self::argv(&shim).is_err());
    }
}
