//! 集成测试共用的小工具：临时目录、写策略文件、以及一个"够真实"的硬件画像。
//!
//! 刻意不引入 `tempfile` 等依赖：策略引擎的依赖面只有 serde / toml / gopt-hal，
//! 测试也不该例外。

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use gopt_hal::{CoreLayout, GpuInfo, GpuVendor, HardwareInfo, ProcessorGroup};
use gopt_policy::EvalInput;

static COUNTER: AtomicU32 = AtomicU32::new(0);

/// 测试用的临时目录，Drop 时自动删除。
#[derive(Debug)]
pub struct Scratch {
    path: PathBuf,
}

impl Scratch {
    /// 新建一个唯一目录（进程 id + 序号，避免并行测试互相踩）。
    pub fn new(name: &str) -> Self {
        let unique = format!(
            "gopt-policy-test-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        let path = std::env::temp_dir().join(unique);
        std::fs::create_dir_all(&path).expect("create scratch directory");
        Self { path }
    }

    /// 目录路径。
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// 写入一个策略文件并返回完整路径。
    pub fn write(&self, file_name: &str, text: &str) -> PathBuf {
        let path = self.path.join(file_name);
        std::fs::write(&path, text).expect("write policy file");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// 仓库里的内置策略目录（`rust/policies`）。
pub fn policies_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("policies")
}

/// 构造一份硬件画像：`physical` 个物理核、每核 `smt` 个逻辑处理器、指定内存与显卡。
pub fn hardware(physical: u32, smt: u32, ram_mb: u64, gpu: Option<GpuVendor>) -> HardwareInfo {
    let logical = physical * smt;
    let mut layout = Vec::new();
    for core in 0..physical {
        let mut mask = 0u64;
        for sibling in 0..smt {
            let index = core * smt + sibling;
            if index < 64 {
                mask |= 1u64 << index;
            }
        }
        layout.push(CoreLayout::new(core, 0, mask));
    }
    HardwareInfo {
        cpu_model: format!("Test CPU {physical}C/{logical}T"),
        physical_cores: physical,
        logical_cores: logical,
        supports_hyper_threading: smt > 1,
        cpu_base_freq_mhz: 3600,
        core_layout: layout,
        processor_groups: vec![ProcessorGroup::new(0, logical.min(64))],
        gpu: gpu.map(|vendor| GpuInfo {
            vendor,
            vendor_id: match vendor {
                GpuVendor::Nvidia => 0x10de,
                GpuVendor::Amd => 0x1002,
                GpuVendor::Intel => 0x8086,
                GpuVendor::Microsoft => 0x1414,
                GpuVendor::Unknown => 0xffff,
            },
            device_id: 0x1234,
            model: "Test GPU".to_string(),
            vram_mb: 8192,
            driver_version: Some("31.0.15.3742".to_string()),
            is_hardware: !matches!(vendor, GpuVendor::Microsoft),
            is_software_adapter: false,
        }),
        system_ram_mb: ram_mb,
        available_ram_mb: ram_mb / 2,
        large_pages_available: false,
        warnings: Vec::new(),
    }
}

/// 8 核 16 线程 / 32GB / NVIDIA / 未提权的求值输入。
pub fn eval_input(ram_mb: u64, elevated: bool) -> EvalInput {
    EvalInput::new(hardware(8, 2, ram_mb, Some(GpuVendor::Nvidia)), elevated)
}
