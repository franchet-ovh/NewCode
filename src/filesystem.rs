//! Cheap disk versions and independently jittered polling deadlines.
use std::{
    fs,
    path::Path,
    time::{Duration, SystemTime},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DiskVersion {
    Present {
        len: u64,
        modified: Option<SystemTime>,
        identity: (u64, u64, i64, i64),
    },
    Missing,
    Error(std::io::ErrorKind),
}
impl DiskVersion {
    pub(crate) fn read(path: &Path) -> Self {
        match fs::metadata(path) {
            Ok(m) => {
                #[cfg(unix)]
                let identity = {
                    use std::os::unix::fs::MetadataExt;
                    (m.dev(), m.ino(), m.ctime(), m.ctime_nsec())
                };
                #[cfg(not(unix))]
                let identity = (0, 0, 0, 0);
                Self::Present {
                    len: m.len(),
                    modified: m.modified().ok(),
                    identity,
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::Missing,
            Err(e) => Self::Error(e.kind()),
        }
    }
}

pub(crate) fn poll_interval() -> Duration {
    let mut bytes = [0; 2];
    let _ = getrandom::fill(&mut bytes);
    Duration::from_millis(800 + u16::from_ne_bytes(bytes) as u64 % 401)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jitter_stays_within_one_second_bounds_and_varies() {
        let intervals: Vec<_> = (0..32).map(|_| poll_interval()).collect();
        assert!(
            intervals
                .iter()
                .all(|d| (Duration::from_millis(800)..=Duration::from_millis(1200)).contains(d))
        );
        assert!(intervals.iter().any(|d| d != &intervals[0]));
    }
}
