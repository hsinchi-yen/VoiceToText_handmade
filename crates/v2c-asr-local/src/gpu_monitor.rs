//! Reads NVIDIA VRAM through NVML (T1.10b).
//!
//! `nvml-wrapper` loads `nvml.dll` (Windows) / `libnvidia-ml.so.1` (Linux) at runtime with
//! `libloading`, so the binary starts and runs on machines without an NVIDIA driver: there
//! [`GpuMonitor::new`] simply ends up unavailable and [`GpuMonitor::snapshot`] returns `None`.
//! On Windows the NVIDIA driver installs `nvml.dll` into `System32`.

use nvml_wrapper::enums::device::UsedGpuMemory;
use nvml_wrapper::Nvml;

use crate::gpu_guard::GpuSnapshot;

const MIB: u64 = 1024 * 1024;

/// VRAM monitor for one NVIDIA device. Cheap to poll (a few NVML calls per snapshot).
pub struct GpuMonitor {
    nvml: Option<Nvml>,
    device_index: u32,
    pid: u32,
}

impl std::fmt::Debug for GpuMonitor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuMonitor")
            .field("available", &self.nvml.is_some())
            .field("device_index", &self.device_index)
            .finish()
    }
}

impl Default for GpuMonitor {
    fn default() -> Self {
        Self::new()
    }
}

impl GpuMonitor {
    /// Initialises NVML for device 0. Never fails; see [`GpuMonitor::available`].
    ///
    /// NVML numbers devices in PCI bus order, while CUDA (and so whisper.cpp) by default puts
    /// the fastest device first (`CUDA_DEVICE_ORDER=FASTEST_FIRST`). On a single-NVIDIA-GPU
    /// machine both are 0; with several NVIDIA GPUs the indices may differ, so match by PCI bus
    /// id or UUID and use [`GpuMonitor::with_device`].
    pub fn new() -> Self {
        Self::with_device(0)
    }

    /// Like [`GpuMonitor::new`] for another NVML device index.
    pub fn with_device(device_index: u32) -> Self {
        let nvml = Nvml::init()
            .ok()
            .filter(|n| n.device_by_index(device_index).is_ok());
        Self {
            nvml,
            device_index,
            pid: std::process::id(),
        }
    }

    /// NVML loaded and the device exists.
    pub fn available(&self) -> bool {
        self.nvml.is_some()
    }

    /// Device name, e.g. `NVIDIA GeForce RTX 2060`, for the settings page.
    pub fn device_name(&self) -> Option<String> {
        let nvml = self.nvml.as_ref()?;
        nvml.device_by_index(self.device_index).ok()?.name().ok()
    }

    /// Current free / total VRAM, or `None` without NVML / on a read error. One NVML call;
    /// this is what the guard polls. `used_by_us_mb` is always `None` here.
    pub fn snapshot(&self) -> Option<GpuSnapshot> {
        let nvml = self.nvml.as_ref()?;
        let device = nvml.device_by_index(self.device_index).ok()?;
        let mem = device.memory_info().ok()?;
        Some(GpuSnapshot {
            free_mb: mem.free / MIB,
            total_mb: mem.total / MIB,
            used_by_us_mb: None,
        })
    }

    /// Like [`GpuMonitor::snapshot`] plus this process's own usage, for the settings page only
    /// (it enumerates every GPU process, so do not call it on every guard poll).
    pub fn detailed_snapshot(&self) -> Option<GpuSnapshot> {
        let mut snap = self.snapshot()?;
        snap.used_by_us_mb = self.our_usage_mb();
        Some(snap)
    }

    /// VRAM used by this process, MiB. WDDM (Windows) reports `Unavailable` -> `None`;
    /// Linux / TCC report bytes.
    fn our_usage_mb(&self) -> Option<u64> {
        let nvml = self.nvml.as_ref()?;
        let device = nvml.device_by_index(self.device_index).ok()?;
        [
            device.running_compute_processes(),
            device.running_graphics_processes(),
        ]
        .into_iter()
        .filter_map(Result::ok)
        .flatten()
        .filter(|p| p.pid == self.pid)
        .filter_map(|p| match p.used_gpu_memory {
            UsedGpuMemory::Used(bytes) => Some(bytes / MIB),
            UsedGpuMemory::Unavailable => None,
        })
        .max()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_device_degrades_to_none() {
        // No machine has 1000 NVIDIA GPUs: must be "unavailable", never a panic.
        let m = GpuMonitor::with_device(1000);
        assert!(!m.available());
        assert_eq!(m.snapshot(), None);
        assert_eq!(m.device_name(), None);
        let _ = format!("{m:?}");
    }

    #[test]
    fn default_monitor_never_panics() {
        let m = GpuMonitor::default();
        if let Some(s) = m.snapshot() {
            assert!(s.free_mb <= s.total_mb);
        }
    }

    /// Real read on a machine with an NVIDIA GPU:
    /// `cargo test -p v2c-asr-local gpu_monitor::tests::nvml_smoke_read -- --ignored --nocapture`
    #[test]
    #[ignore = "needs an NVIDIA GPU and driver"]
    fn nvml_smoke_read() {
        let m = GpuMonitor::new();
        assert!(m.available(), "NVML not available");
        let s = m.detailed_snapshot().expect("snapshot");
        println!(
            "device={:?} free={} MiB total={} MiB used={} MiB used_by_us={:?}",
            m.device_name(),
            s.free_mb,
            s.total_mb,
            s.used_mb(),
            s.used_by_us_mb
        );
        assert!(s.total_mb > 0 && s.free_mb <= s.total_mb);
    }
}
