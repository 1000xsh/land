// core cpu affinity operations

use {
    crate::error::CpuAffinityError,
    std::{fs, io},
};

// maximum cpu id that can be used with CPU_SET
// standard linux value in glibc - fixed at 1024 across major distros
#[cfg(target_os = "linux")]
const CPU_SETSIZE: usize = 1024;

// set cpu affinity for calling thread
#[cfg(target_os = "linux")]
pub fn set_cpu_affinity(cpus: impl IntoIterator<Item = usize>) -> Result<(), CpuAffinityError> {
    // safety: cpu_set_t is pod type, zero-initialization standard
    let mut cpu_set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    let max_cpu = max_cpu_id()?;
    let mut has_cpus = false;

    // validate, deduplicate via CPU_ISSET, and set cpus
    for cpu in cpus {
        if cpu > max_cpu {
            return Err(CpuAffinityError::InvalidCpu { cpu, max: max_cpu });
        }
        if cpu >= CPU_SETSIZE {
            return Err(CpuAffinityError::InvalidCpu {
                cpu,
                max: CPU_SETSIZE - 1,
            });
        }

        // safety: CPU_ISSET safe after validation
        if unsafe { libc::CPU_ISSET(cpu, &cpu_set) } {
            continue;
        }

        // safety: validated cpu within range
        unsafe {
            libc::CPU_SET(cpu, &mut cpu_set);
        }
        has_cpus = true;
    }

    if !has_cpus {
        return Err(CpuAffinityError::EmptyCpuList);
    }

    // safety: sched_setaffinity safe with valid parameters
    let result = unsafe {
        libc::sched_setaffinity(
            0, // 0 means current thread
            std::mem::size_of::<libc::cpu_set_t>(),
            &cpu_set,
        )
    };

    if result != 0 {
        return Err(CpuAffinityError::Io(io::Error::last_os_error()));
    }

    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn set_cpu_affinity(_cpus: impl IntoIterator<Item = usize>) -> Result<(), CpuAffinityError> {
    Err(CpuAffinityError::NotSupported)
}

// get maximum cpu id on system (online cpus only)
#[cfg(target_os = "linux")]
pub fn max_cpu_id() -> Result<usize, CpuAffinityError> {
    // try sysfs first
    if let Ok(content) = fs::read_to_string("/sys/devices/system/cpu/online") {
        let content = content.trim();

        // parse range (e.g., "0-127" or just "0")
        if let Some(range) = content.split('-').nth(1) {
            if let Ok(max) = range.parse::<usize>() {
                return Ok(max);
            }
        } else if let Ok(max) = content.parse::<usize>() {
            return Ok(max);
        }
    }

    // fallback to sysconf for online processors
    // safety: sysconf safe to call
    let count = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };

    if count <= 0 {
        return Err(CpuAffinityError::Io(io::Error::last_os_error()));
    }

    Ok((count as usize).saturating_sub(1))
}

#[cfg(not(target_os = "linux"))]
pub fn max_cpu_id() -> Result<usize, CpuAffinityError> {
    Err(CpuAffinityError::NotSupported)
}

// get total number of online cpus on system
// returns count of online logical cpus (includes hyperthreads)
// equivalent to max_cpu_id() + 1
pub fn cpu_count() -> Result<usize, CpuAffinityError> {
    Ok(max_cpu_id()?.saturating_add(1))
}
