// error types for cpu affinity operations

use std::{error::Error, fmt, io};

#[derive(Debug)]
#[non_exhaustive]
pub enum CpuAffinityError {
    // i/o or system call error
    Io(io::Error),

    // operation not supported on this platform
    NotSupported,

    // invalid cpu id
    InvalidCpu { cpu: usize, max: usize },

    // cpu list is empty
    EmptyCpuList,
}

impl fmt::Display for CpuAffinityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CpuAffinityError::Io(err) => write!(f, "I/O error: {}", err),
            CpuAffinityError::NotSupported => {
                write!(f, "CPU affinity operations are not supported on this platform")
            }
            CpuAffinityError::InvalidCpu { cpu, max } => {
                write!(f, "CPU {} is invalid (max CPU is {})", cpu, max)
            }
            CpuAffinityError::EmptyCpuList => write!(f, "CPU list cannot be empty"),
        }
    }
}

impl Error for CpuAffinityError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            CpuAffinityError::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for CpuAffinityError {
    fn from(err: io::Error) -> Self {
        CpuAffinityError::Io(err)
    }
}
