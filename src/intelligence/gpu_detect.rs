// GPU detection for model recommendation.
//
// Detects the best available GPU and its VRAM so the wizard can pre-select
// a suitable LLM.  All detection is purely local (no network calls).

#[derive(Debug, Clone, PartialEq)]
pub enum GpuVendor {
    Nvidia,
    Amd,
    Intel,
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
/// The thresholds are conservative — we want the model to run comfortably,
/// not just barely fit.
pub fn suggested_model(gpu: Option<&GpuInfo>) -> &'static str {
    let vram = gpu.and_then(|g| g.vram_mib);
    match vram {
        Some(v) if v >= 8192 => "mistral:7b",     // 8 GB+: mistral 7B fits well
        Some(v) if v >= 4096 => "qwen2.5:3b",  // 4–8 GB: qwen2.5 3B is the sweet spot
        _ => "qwen2.5:3b",                      // CPU/iGPU: qwen2.5 3B best quality/size tradeoff
    }
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
            // Intel vendor ID.  LMEM (local memory) is present on Arc discrete GPUs.
            let vram_path = path.join("device/prelim_lmem_total_bytes");
            let vram_mib = std::fs::read_to_string(&vram_path)
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|bytes| bytes / 1024 / 1024);
            // Only report Intel if it has local memory (Arc discrete) — skip
            // integrated graphics which can't run an LLM usefully.
            if vram_mib.is_some() {
                return Some(GpuInfo { vendor: GpuVendor::Intel, vram_mib });
            }
        }
    }
    None
}
