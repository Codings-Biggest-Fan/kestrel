//! Fan curves: temperature -> fan duty, with a hard thermal failsafe.

use serde::{Deserialize, Serialize};

/// Above this temperature Kestrel ignores the curve and forces full fans.
pub const FAILSAFE_TEMP_C: u8 = 95;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FanCurve {
    /// (temperature °C, duty %) points, sorted by temperature.
    pub points: Vec<(u8, u8)>,
}

impl FanCurve {
    pub fn new(points: Vec<(u8, u8)>) -> Result<Self, String> {
        if points.len() < 2 {
            return Err("a fan curve needs at least 2 points".into());
        }
        for w in points.windows(2) {
            if w[1].0 <= w[0].0 {
                return Err("temperatures must increase".into());
            }
            if w[1].1 < w[0].1 {
                return Err("fan speed must not drop as temperature rises".into());
            }
        }
        if points.iter().any(|p| p.1 > 100) {
            return Err("fan speed is a percentage (0-100)".into());
        }
        Ok(Self { points })
    }

    pub fn balanced() -> Self {
        Self {
            points: vec![(40, 0), (55, 30), (65, 45), (75, 65), (85, 85), (90, 100)],
        }
    }

    pub fn silent() -> Self {
        Self {
            points: vec![(50, 0), (65, 25), (75, 40), (85, 70), (90, 100)],
        }
    }

    /// Duty for `temp`, linearly interpolated; 100 % at or above the failsafe.
    pub fn duty(&self, temp: u8) -> u8 {
        if temp >= FAILSAFE_TEMP_C {
            return 100;
        }
        let pts = &self.points;
        if temp <= pts[0].0 {
            return pts[0].1;
        }
        for w in pts.windows(2) {
            let ((t0, d0), (t1, d1)) = (w[0], w[1]);
            if temp <= t1 {
                let f = (temp - t0) as u32 * (d1 - d0) as u32 / (t1 - t0) as u32;
                return d0 + f as u8;
            }
        }
        pts[pts.len() - 1].1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolates() {
        let c = FanCurve::new(vec![(40, 20), (80, 100)]).unwrap();
        assert_eq!(c.duty(30), 20);
        assert_eq!(c.duty(60), 60);
        assert_eq!(c.duty(90), 100);
    }

    #[test]
    fn failsafe_overrides_curve() {
        let c = FanCurve::new(vec![(40, 0), (99, 10)]).unwrap();
        assert_eq!(c.duty(FAILSAFE_TEMP_C), 100);
    }

    #[test]
    fn rejects_bad_curves() {
        assert!(FanCurve::new(vec![(50, 50)]).is_err());
        assert!(FanCurve::new(vec![(50, 50), (40, 60)]).is_err());
        assert!(FanCurve::new(vec![(40, 60), (50, 50)]).is_err());
        assert!(FanCurve::new(vec![(40, 60), (50, 150)]).is_err());
    }
}
