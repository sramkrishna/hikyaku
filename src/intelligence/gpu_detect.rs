// GPU detection for model recommendation.
//
// Detects the best available GPU and its VRAM so the wizard can pre-select
// a suitable LLM.  All detection is purely local (no network calls).

#[derive(Debug, Clone, PartialEq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
    #[allow(dead_code)]
    Unknown,
}

#[derive(Debug, Clone)]
pub struct GpuInfo {
    pub vendor: GpuVendor,
    /// VRAM in MiB, if detectable.
    pub vram_mib: Option<u64>,
}

/// Detect the primary GPU.  Returns `None` if no discrete GPU is found.
pub fn detect_gpu() -> Option<GpuInfo> {
    detect_nvidia()
        .or_else(detect_amd)
        .or_else(detect_intel)
}

/// Recommend a model ID from the wizard's curated list given the detected GPU.
///
/// For discrete GPUs we use VRAM as the constraint. For iGPU and CPU-only
/// machines we fall back to system RAM — Ollama runs the model in RAM on those
/// paths, so a 32 GB machine can comfortably run a 7B model (~4-5 GB).
pub fn suggested_model(gpu: Option<&GpuInfo>) -> &'static str {
    let vram = gpu.and_then(|g| g.vram_mib);
    match vram {
        Some(v) if v >= 4096 => "qwen2.5:7b",  // discrete GPU with ≥4 GB VRAM
        Some(_) => "qwen2.5:3b",                // discrete GPU, tighter VRAM
        None => {
            // iGPU or CPU-only: use system RAM as the proxy.
            if system_ram_mib().unwrap_or(0) >= 16384 {
                "qwen2.5:7b"   // 16 GB+ RAM: 7B fits comfortably
            } else {
                "qwen2.5:3b"   // less than 16 GB: stay with 3B
            }
        }
    }
}

/// Total system RAM in MiB, read from /proc/meminfo.
fn system_ram_mib() -> Option<u64> {
    let info = std::fs::read_to_string("/proc/meminfo").ok()?;
    info.lines()
        .find(|l| l.starts_with("MemTotal:"))?
        .split_whitespace()
        .nth(1)?
        .parse::<u64>()
        .ok()
        .map(|kb| kb / 1024)
}

/// Human-readable reason string shown next to the pre-selected model.
pub fn suggestion_reason(gpu: Option<&GpuInfo>) -> String {
    match gpu {
        None => "Recommended for CPU and integrated graphics".to_string(),
        Some(info) => {
            let vendor = match info.vendor {
                GpuVendor::Nvidia => "NVIDIA",
                GpuVendor::Amd => "AMD",
                GpuVendor::Intel => "Intel",
                GpuVendor::Unknown => "GPU",
            };
            match info.vram_mib {
                Some(v) => format!("Recommended for your {} GPU ({} MB VRAM)", vendor, v),
                None => format!("Recommended for your {} GPU", vendor),
            }
        }
    }
}

// ── NVIDIA ────────────────────────────────────────────────────────────────────

fn detect_nvidia() -> Option<GpuInfo> {
    // Quick presence check — no point running nvidia-smi if the device isn't there.
    if !std::path::Path::new("/dev/nvidia0").exists() {
        return None;
    }

    let vram_mib = std::process::Command::new("nvidia-smi")
        .args(["--query-gpu=memory.total", "--format=csv,noheader,nounits"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.trim().parse::<u64>().ok());

    Some(GpuInfo { vendor: GpuVendor::Nvidia, vram_mib })
}

// ── AMD ───────────────────────────────────────────────────────────────────────

fn detect_amd() -> Option<GpuInfo> {
    // Walk DRM card entries looking for an AMD device.
    let drm = std::fs::read_dir("/sys/class/drm").ok()?;
    for entry in drm.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy().to_string();
        // Skip render nodes (renderD*) and connectors — only card* entries.
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }

        let vendor_path = path.join("device/vendor");
        let vendor = std::fs::read_to_string(&vendor_path)
            .unwrap_or_default()
            .trim()
            .to_lowercase();

        if vendor == "0x1002" {
            // AMD vendor ID.
            let vram_path = path.join("device/mem_info_vram_total");
            let vram_mib = std::fs::read_to_string(&vram_path)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|bytes| bytes / 1024 / 1024);
            return Some(GpuInfo { vendor: GpuVendor::Amd, vram_mib });
        }
    }
    None
}

// ── Intel Arc ────────────────────────────────────────────────────────────────

fn detect_intel() -> Option<GpuInfo> {
    let drm = std::fs::read_dir("/sys/class/drm").ok()?;
    for entry in drm.flatten() {
        let path = entry.path();
        let name = path.file_name()?.to_string_lossy().to_string();
        if !name.starts_with("card") || name.contains('-') {
            continue;
        }

        let vendor_path = path.join("device/vendor");
        let vendor = std::fs::read_to_string(&vendor_path)
            .unwrap_or_default()
            .trim()
            .to_lowercase();

        if vendor == "0x8086" {
            // Intel vendor ID. Arc discrete GPUs have LMEM; iGPUs (including
            // Lunar Lake Arc) share system RAM — report them with vram_mib: None
            // so suggested_model() falls through to the system-RAM path.
            let vram_path = path.join("device/prelim_lmem_total_bytes");
            let vram_mib = std::fs::read_to_string(&vram_path)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .filter(|&b| b > 0)
                .map(|bytes| bytes / 1024 / 1024);
            return Some(GpuInfo { vendor: GpuVendor::Intel, vram_mib });
        }
    }
    None
}
