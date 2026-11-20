//! Custom profiles: a scenario plus optional fan curves and Cooler Boost, applied in one click.
//! Built-in presets are generated per model; user profiles live in `profiles.json`.

use crate::automation::user_dir;
use crate::ipc::Request;
use crate::scenario::Scenario;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Profile {
    pub name: String,
    pub scenario: Scenario,
    #[serde(default)]
    pub cooler_boost: bool,
    #[serde(default)]
    pub cpu_curve: Option<Vec<u8>>,
    #[serde(default)]
    pub gpu_curve: Option<Vec<u8>>,
}

/// `n` speeds rising linearly from `lo` to `hi` %, rounded to 5 %.
pub fn ramp(lo: u8, hi: u8, n: usize) -> Vec<u8> {
    (0..n)
        .map(|i| {
            let t = if n > 1 { i as f32 / (n - 1) as f32 } else { 1.0 };
            let v = lo as f32 + (hi as f32 - lo as f32) * t;
            ((v / 5.0).round() * 5.0).clamp(0.0, 100.0) as u8
        })
        .collect()
}

/// Presets for a model that offers `scenarios` and has `curve_points` fan-curve points (0 = none).
pub fn presets(scenarios: &[Scenario], curve_points: usize) -> Vec<Profile> {
    let p = |name: &str, scenario, cooler_boost, curve: Option<Vec<u8>>| Profile {
        name: name.into(),
        scenario,
        cooler_boost,
        cpu_curve: curve.clone(),
        gpu_curve: curve,
    };
    let n = curve_points;
    let mut out = vec![
        p("Quiet", Scenario::Silent, false, None),
        p("Everyday", Scenario::Balanced, false, None),
        p("Performance", Scenario::Extreme, false, None),
    ];
    if n > 0 {
        out.push(p("Performance, quiet fans", Scenario::Extreme, false, Some(ramp(0, 75, n))));
        out.push(p("Gaming, strong fans", Scenario::Extreme, false, Some(ramp(40, 100, n))));
    }
    out.push(p("Max cooling", Scenario::Extreme, true, None));
    out.retain(|x| scenarios.contains(&x.scenario));
    out
}

pub fn load() -> Vec<Profile> {
    std::fs::read_to_string(user_dir().join("profiles.json"))
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

pub fn save(list: &[Profile]) -> std::io::Result<()> {
    let dir = user_dir();
    std::fs::create_dir_all(&dir)?;
    let json = serde_json::to_string_pretty(list).map_err(std::io::Error::other)?;
    // Write then rename, so a failed write never leaves a truncated list behind.
    let tmp = dir.join("profiles.json.tmp");
    std::fs::write(&tmp, json)?;
    std::fs::rename(tmp, dir.join("profiles.json"))
}

/// Requests in apply order. The scenario goes first; a curve may then switch the EC to
/// Extreme on models that only follow curves there, and Cooler Boost is set last.
pub fn requests(p: &Profile) -> Vec<Request> {
    let mut r = vec![Request::SetScenario(p.scenario)];
    if let Some(c) = &p.cpu_curve {
        r.push(Request::SetFanCurve { gpu: false, speeds: c.clone() });
    }
    if let Some(c) = &p.gpu_curve {
        r.push(Request::SetFanCurve { gpu: true, speeds: c.clone() });
    }
    r.push(Request::SetCoolerBoost(p.cooler_boost));
    r
}

/// One-line summary for the UI.
pub fn describe(p: &Profile) -> String {
    let mut parts = vec![p.scenario.label().to_string()];
    if p.cpu_curve.is_some() || p.gpu_curve.is_some() {
        parts.push("custom fan curve".into());
    }
    if p.cooler_boost {
        parts.push("Cooler Boost".into());
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramps_rise_in_5_percent_steps() {
        assert_eq!(ramp(40, 100, 6), vec![40, 50, 65, 75, 90, 100]);
        assert_eq!(ramp(0, 75, 6), vec![0, 15, 30, 45, 60, 75]);
    }

    #[test]
    fn presets_follow_the_model() {
        let vector = [Scenario::Extreme, Scenario::Balanced, Scenario::Silent];
        let names: Vec<String> = presets(&vector, 6).into_iter().map(|p| p.name).collect();
        assert_eq!(names.len(), 6);
        assert!(presets(&vector, 0).iter().all(|p| p.cpu_curve.is_none()));
        assert!(presets(&[], 6).is_empty());
    }

    #[test]
    fn apply_order() {
        let p = &presets(&[Scenario::Extreme], 6)[1];
        let r = requests(p);
        assert_eq!(r.first(), Some(&Request::SetScenario(Scenario::Extreme)));
        assert!(matches!(r[1], Request::SetFanCurve { gpu: false, .. }));
        assert_eq!(r.last(), Some(&Request::SetCoolerBoost(false)));
    }
}
