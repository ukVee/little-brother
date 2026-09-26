//! Fault-rate bookkeeping for the capture path (slice 007).
//!
//! Slice 007 demotes the `ov7251` rebind from a routine pre-auth step to a
//! fault handler. A handler that fires silently is how "works most of the
//! time" survived a month, so the demotion comes with a counter: every wedge
//! the daemon sees and every rebind that actually runs is counted, and
//! `cement-ir doctor` reports both.
//!
//! # Why one plain file in `/run`
//!
//! - `/run` is **tmpfs**: no SSD/eMMC wear on this device (a standing
//!   preference here), and "since boot" comes for free — the file cannot
//!   outlive the boot it describes.
//! - It is deliberately **not** under `/run/cement-ir/`: that directory is
//!   `RuntimeDirectory=` of `irfaced.socket`, so systemd deletes it when the
//!   socket unit stops. A fault count that resets when the socket is
//!   restarted would quietly under-report.
//! - `/run` is root-writable only, so only root bumps (the daemon and
//!   `sensor-recover.sh` both run as root) while anyone can read — which is
//!   what lets `cement-ir doctor` report the fault rate without root.
//!
//! Format, one counter per line, so the shell half can read it with `awk` and
//! a human can read it with `cat`:
//!
//! ```text
//! wedge 0
//! recover 0
//! ```
//!
//! Both halves honour `CEMENT_IR_FAULTS` as an override (`lib/common.sh` reads
//! the same variable). That exists so the round trip can actually be tested —
//! the shell writes the file and this code reads it, and a format that is only
//! ever written by one of the two would drift.

use std::io;

/// The default counter file. Absent = nothing has faulted since boot.
pub const DEFAULT_PATH: &str = "/run/cement-ir-faults";

/// The counter file in use (`CEMENT_IR_FAULTS`, else [`DEFAULT_PATH`]).
pub fn path() -> String {
    std::env::var("CEMENT_IR_FAULTS").unwrap_or_else(|_| DEFAULT_PATH.to_string())
}

/// Captures that failed with the wedge signature (bumped by the daemon).
pub const WEDGE: &str = "wedge";
/// Rebinds that actually fired (bumped by `lib/sensor-recover.sh`).
pub const RECOVER: &str = "recover";

fn parse(text: &str) -> Vec<(String, u64)> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let k = it.next()?;
            let v = it.next()?.parse().ok()?;
            Some((k.to_string(), v))
        })
        .collect()
}

fn load() -> Vec<(String, u64)> {
    match std::fs::read_to_string(path()) {
        Ok(t) => parse(&t),
        Err(_) => Vec::new(),
    }
}

/// Read one counter. A missing file or key reads 0 — "nothing has happened"
/// and "nobody has written yet" are the same statement here.
pub fn get(key: &str) -> u64 {
    load()
        .into_iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v)
        .unwrap_or(0)
}

/// Increment one counter and return the new value. Root-only (`/run`).
///
/// Written through a temp file + `rename` so a reader never sees a truncated
/// file, and so a crash mid-write cannot lose the other counters.
pub fn bump(key: &str) -> io::Result<u64> {
    let mut counters = load();
    let mut new = 1;
    match counters.iter_mut().find(|(k, _)| k == key) {
        Some((_, v)) => {
            *v += 1;
            new = *v;
        }
        None => counters.push((key.to_string(), 1)),
    }
    let body: String = counters
        .iter()
        .map(|(k, v)| format!("{k} {v}\n"))
        .collect();
    let p = path();
    let tmp = format!("{p}.{}", std::process::id());
    std::fs::write(&tmp, body)?;
    // 0644: root writes, anyone reads — that is what lets `cement-ir doctor`
    // report the fault rate unprivileged.
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644));
    match std::fs::rename(&tmp, &p) {
        Ok(()) => Ok(new),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

/// Bump a counter and say so on stderr (the journal, for the daemon) — a
/// fault is an **event** now, never a silent step. Failing to record it is
/// itself only a warning: the fault report matters more than the count.
pub fn record(key: &str, reason: &str) {
    match bump(key) {
        Ok(n) => eprintln!("cement-ir: FAULT {key} #{n} since boot: {reason}"),
        Err(e) => eprintln!(
            "cement-ir: FAULT {key}: {reason} (counter {}: {e})",
            path()
        ),
    }
}

/// Does this error carry the wedge signature?
///
/// The two mechanisms, both from `hardware/notes/005`:
/// - the sensor stops answering I2C (`EREMOTEIO`), so the driver cannot even
///   write its reset register, and the subdev/entity goes away (`ENODEV`,
///   `ENXIO`);
/// - the CIO2 pipeline is left in a state where `STREAMON` returns `EINVAL`.
///
/// `EINVAL` is ambiguous by itself (a genuinely wrong format returns it too),
/// which is exactly why the count is reported as a **rate** rather than
/// treated as proof: a rising wedge count against a working format is the
/// signal. Callers that know the step they were on classify at the call site.
pub fn looks_wedged(e: &io::Error) -> bool {
    matches!(
        e.raw_os_error(),
        Some(libc::EREMOTEIO) | Some(libc::ENODEV) | Some(libc::ENXIO) | Some(libc::EINVAL)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_counters_and_ignores_junk() {
        let c = parse("wedge 3\nrecover 1\n\ngarbage\nnoval \nbad x\n");
        assert_eq!(c, vec![("wedge".to_string(), 3), ("recover".to_string(), 1)]);
    }

    /// The format is written by `lib/common.sh`'s `fault_bump` and read here;
    /// this pins the shape both halves agree on (see also the shell round-trip
    /// in the slice's verification).
    #[test]
    fn bump_creates_increments_and_preserves_other_counters() {
        let tmp = std::env::temp_dir().join(format!("cement-ir-faults-test.{}", std::process::id()));
        // SAFETY: single-threaded test process; no other thread reads the env.
        unsafe { std::env::set_var("CEMENT_IR_FAULTS", &tmp) };
        let _ = std::fs::remove_file(&tmp);
        assert_eq!(get(WEDGE), 0, "a missing file reads 0");
        assert_eq!(bump(WEDGE).unwrap(), 1);
        assert_eq!(bump(WEDGE).unwrap(), 2);
        assert_eq!(bump(RECOVER).unwrap(), 1);
        assert_eq!(get(WEDGE), 2, "the other counter survived");
        assert_eq!(get(RECOVER), 1);
        let text = std::fs::read_to_string(&tmp).unwrap();
        assert!(text.contains("wedge 2"), "shell-readable: {text:?}");
        assert!(text.contains("recover 1"), "shell-readable: {text:?}");
        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn wedge_signature_covers_the_two_mechanisms() {
        assert!(looks_wedged(&io::Error::from_raw_os_error(libc::EREMOTEIO)));
        assert!(looks_wedged(&io::Error::from_raw_os_error(libc::EINVAL)));
        assert!(!looks_wedged(&io::Error::from_raw_os_error(libc::EACCES)));
        assert!(!looks_wedged(&io::Error::other("no errno at all")));
    }
}
