// cpu topology detection and physical core management

use {
    crate::{
        affinity::{cpu_count, max_cpu_id},
        error::CpuAffinityError,
    },
    std::{collections::HashSet, fs},
};

// get number of physical cpu cores (excluding hyperthreads)
#[cfg(target_os = "linux")]
pub fn physical_core_count() -> Result<usize, CpuAffinityError> {
    let max_cpu = max_cpu_id()?;
    let mut seen_cores = HashSet::new();

    for cpu in 0..=max_cpu {
        let core_id_path = format!("/sys/devices/system/cpu/cpu{cpu}/topology/core_id");

        if let Ok(content) = fs::read_to_string(&core_id_path) {
            if let Ok(core_id) = content.trim().parse::<usize>() {
                // sanity check: use max_cpu * 2 as upper bound
                if core_id <= max_cpu.saturating_mul(2) {
                    seen_cores.insert(core_id);
                }
            }
        }
    }

    if seen_cores.is_empty() {
        // fallback: assume no hyperthreading
        cpu_count()
    } else {
        Ok(seen_cores.len())
    }
}

#[cfg(not(target_os = "linux"))]
pub fn physical_core_count() -> Result<usize, CpuAffinityError> {
    Err(CpuAffinityError::NotSupported)
}
