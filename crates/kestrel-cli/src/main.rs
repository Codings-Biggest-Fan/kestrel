//! `kestrel status | scenario <name> | boost on|off | profile <name>`: talks to the service
//! over the same pipe as the tray, so every safety check still applies.

use kestrel_core::ipc::Request;
use kestrel_core::profiles;
use kestrel_core::scenario::Scenario;
use std::process::ExitCode;

const USAGE: &str = "usage: kestrel status
       kestrel scenario <extreme|balanced|silent|superbattery>
       kestrel boost <on|off>
       kestrel profile <name>";

fn key(s: &str) -> String {
    s.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_ascii_lowercase()
}

fn scenario(name: &str) -> Option<Scenario> {
    Scenario::ALL.into_iter().find(|s| key(s.label()) == key(name))
}

fn run(args: &[String]) -> Result<String, String> {
    if !matches!(args.first().map(String::as_str), Some("status" | "scenario" | "boost" | "profile")) {
        return Err(USAGE.into());
    }
    let mut c = kestrel_core::transport::client().map_err(|e| format!("service not running: {e}"))?;
    let send = |c: &mut kestrel_core::ipc::Client<_>, r: &Request| c.command(r).map_err(|e| e.to_string());
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["status"] => {
            let s = c.status().map_err(|e| e.to_string())?;
            let opt = |v: Option<u8>, unit: &str| v.map_or("-".to_string(), |v| format!("{v}{unit}"));
            Ok(format!(
                "model     {} ({}){}\nscenario  {}\nboost     {}\ncpu       {} fan {}\ngpu       {} fan {}\nbattery   {}{}",
                s.model.name,
                s.model.firmware,
                if s.model.read_only { " read-only" } else { "" },
                s.scenario.map_or("-", |x| x.label()),
                s.cooler_boost.map_or("-", |b| if b { "on" } else { "off" }),
                opt(s.cpu_temp, "°C"),
                opt(s.cpu_fan_pct, "%"),
                opt(s.gpu_temp, "°C"),
                opt(s.gpu_fan_pct, "%"),
                opt(s.battery_pct, "%"),
                match s.on_ac {
                    Some(true) => " (plugged in)",
                    Some(false) => " (on battery)",
                    None => "",
                },
            ))
        }
        ["scenario", name] => {
            let s = scenario(name).ok_or_else(|| format!("unknown scenario {name:?}\n{USAGE}"))?;
            send(&mut c, &Request::SetScenario(s))?;
            Ok(format!("scenario: {}", s.label()))
        }
        ["boost", v @ ("on" | "off")] => {
            send(&mut c, &Request::SetCoolerBoost(*v == "on"))?;
            Ok(format!("Cooler Boost: {v}"))
        }
        ["profile", name @ ..] if !name.is_empty() => {
            let name = name.join(" ");
            let caps = c.status().map_err(|e| e.to_string())?.caps;
            let all: Vec<_> =
                profiles::load().into_iter().chain(profiles::presets(&caps.scenarios, caps.curve_points as usize, false)).collect();
            let p = pick_profile(&all, &name)?.clone();
            for r in profiles::requests(&p) {
                send(&mut c, &r)?;
            }
            Ok(format!("profile: {}", p.name))
        }
        _ => Err(USAGE.into()),
    }
}

/// An exact (case-insensitive) name wins; otherwise a punctuation-insensitive match must be unique.
fn pick_profile<'a>(all: &'a [profiles::Profile], name: &str) -> Result<&'a profiles::Profile, String> {
    if let Some(p) = all.iter().find(|p| p.name.eq_ignore_ascii_case(name)) {
        return Ok(p);
    }
    let loose: Vec<_> = all.iter().filter(|p| key(&p.name) == key(name)).collect();
    match loose.as_slice() {
        [p] => Ok(p),
        [] => Err(format!("no profile named {name:?}")),
        _ => Err(format!("{name:?} matches several profiles; use the exact name")),
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match run(&args) {
        Ok(out) => {
            println!("{out}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scenario_names() {
        assert_eq!(scenario("Silent"), Some(Scenario::Silent));
        assert_eq!(scenario("super-battery"), Some(Scenario::SuperBattery));
        assert_eq!(scenario("superbattery"), Some(Scenario::SuperBattery));
        assert_eq!(scenario("turbo"), None);
    }
}
