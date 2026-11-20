//! Lid-close action of the active Windows power plan, via `powercfg` (no admin needed).

/// Labels in powercfg's value order: 0 do nothing, 1 sleep, 2 hibernate, 3 shut down.
pub const LID_ACTIONS: [&str; 4] = ["Do nothing", "Sleep", "Hibernate", "Shut down"];

/// Parses `powercfg /qh SCHEME_CURRENT SUB_BUTTONS LIDACTION`. The labels are localized, so
/// this takes the first two values written as `: 0x…` (AC, then DC).
pub fn parse_lid(out: &str) -> Option<(u8, u8)> {
    let mut vals = out.lines().filter_map(|l| {
        let v = l.rsplit(':').next()?.trim();
        u8::from_str_radix(v.strip_prefix("0x")?, 16).ok().filter(|&v| v < 4)
    });
    Some((vals.next()?, vals.next()?))
}

#[cfg(windows)]
fn powercfg(args: &[&str]) -> Result<String, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = std::process::Command::new("powercfg")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let msg = if err.is_empty() { String::from_utf8_lossy(&out.stdout).trim().to_string() } else { err };
        return Err(format!("powercfg failed ({}): {msg}", out.status));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

#[cfg(not(windows))]
fn powercfg(_args: &[&str]) -> Result<String, String> {
    Err("powercfg is only available on Windows".into())
}

/// (plugged in, on battery)
pub fn lid_actions() -> Result<(u8, u8), String> {
    // `/qh` includes hidden settings; many laptops hide LIDACTION, and `/q` then prints nothing.
    let out = powercfg(&["/qh", "SCHEME_CURRENT", "SUB_BUTTONS", "LIDACTION"])?;
    parse_lid(&out).ok_or_else(|| format!("unexpected powercfg output: {}", out.trim().lines().last().unwrap_or("(empty)")))
}

pub fn set_lid_action(ac: bool, action: u8) -> Result<(), String> {
    if action as usize >= LID_ACTIONS.len() {
        return Err(format!("lid action {action} out of range"));
    }
    let which = if ac { "/setacvalueindex" } else { "/setdcvalueindex" };
    powercfg(&[which, "SCHEME_CURRENT", "SUB_BUTTONS", "LIDACTION", &action.to_string()])?;
    powercfg(&["/setactive", "SCHEME_CURRENT"]).map(drop)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_powercfg_output() {
        let out = "Power Scheme GUID: 381b4222-f694-41f0-9685-ff5bb260df2e  (Balanced)\n  Subgroup GUID: 4f971e89-eebd-4455-a8de-9e59040e7347  (Power buttons and lid)\n    Power Setting GUID: 5ca83367-6e45-459f-a27b-476b1d01c936  (Lid close action)\n      Possible Setting Index: 000\n      Possible Setting Friendly Name: Do nothing\n    Current AC Power Setting Index: 0x00000000\n    Current DC Power Setting Index: 0x00000001\n";
        assert_eq!(parse_lid(out), Some((0, 1)));
        assert_eq!(parse_lid("nothing here"), None);
    }
}
