//! NVIDIA GPU status through `nvidia-smi`, which ships with every NVIDIA driver. Called on
//! demand by the UI only, so nothing stays resident.

#[derive(Debug, Clone, Default, PartialEq)]
pub struct GpuInfo {
    pub name: String,
    pub driver: String,
    pub temp_c: Option<u32>,
    pub power_w: Option<f32>,
    pub core_mhz: Option<u32>,
    pub mem_mhz: Option<u32>,
    pub load_pct: Option<u32>,
    pub pstate: String,
}

pub const QUERY: &str =
    "name,driver_version,temperature.gpu,power.draw,clocks.gr,clocks.mem,utilization.gpu,pstate";
pub const DRIVER_PAGE: &str = "https://www.nvidia.com/en-us/drivers/";

/// Parses one line of `--query-gpu=QUERY --format=csv,noheader,nounits`. "[N/A]" fields
/// (a sleeping laptop GPU reports some) become `None`.
pub fn parse(line: &str) -> Option<GpuInfo> {
    let f: Vec<&str> = line.split(',').map(str::trim).collect();
    if f.len() < 8 || f[0].is_empty() {
        return None;
    }
    let num = |s: &str| s.parse::<f32>().ok();
    Some(GpuInfo {
        name: f[0].into(),
        driver: f[1].into(),
        temp_c: num(f[2]).map(|v| v as u32),
        power_w: num(f[3]),
        core_mhz: num(f[4]).map(|v| v as u32),
        mem_mhz: num(f[5]).map(|v| v as u32),
        load_pct: num(f[6]).map(|v| v as u32),
        pstate: f[7].into(),
    })
}

pub fn query() -> Result<GpuInfo, String> {
    let mut cmd = std::process::Command::new("nvidia-smi");
    cmd.args([
        &format!("--query-gpu={QUERY}"),
        "--format=csv,noheader,nounits",
    ]);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0800_0000);
    }
    let out = cmd
        .output()
        .map_err(|_| "No NVIDIA driver found (nvidia-smi is missing).".to_string())?;
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .find_map(parse)
        .ok_or_else(|| "nvidia-smi returned no GPU.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_nvidia_smi() {
        let g = parse("NVIDIA GeForce RTX 5080 Laptop GPU, 581.57, 48, 12.34, 210, 405, 3, P8")
            .unwrap();
        assert_eq!(g.name, "NVIDIA GeForce RTX 5080 Laptop GPU");
        assert_eq!(g.driver, "581.57");
        assert_eq!(
            (g.temp_c, g.core_mhz, g.load_pct),
            (Some(48), Some(210), Some(3))
        );
        assert_eq!(g.pstate, "P8");
        let idle = parse(
            "NVIDIA GeForce RTX 4060 Laptop GPU, 566.03, [N/A], [N/A], [N/A], [N/A], [N/A], P8",
        )
        .unwrap();
        assert_eq!(idle.temp_c, None);
        assert!(parse("").is_none());
    }
}
