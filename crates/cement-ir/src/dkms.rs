//! `cement-ir-rt dkms verify` — is the **patched** `dw9719` what `modprobe`
//! resolved, and did the cameras actually appear? (slice 008)
//!
//! # Why this is a program and not a paragraph
//!
//! linux-surface `6.19.8.arch1-3` ships `drivers/media/i2c/dw9719.c` without
//! upstream `d7fe0d53b2a8` ("media: dw9719: Add back the I2C device id
//! table"). With no `id_table` the VCM cannot bind, `ov8865`'s async
//! sub-notifier never completes, `cio2_notifier_complete()` never runs, and
//! **no sensor gets a pad link** — every camera on the machine disappears,
//! including the IR one this project exists for. The DKMS carry
//! (`dw9719-fix/1.0`) is permanent here by decision (on-device only), so its
//! failure mode is permanent too, and that failure is **silent**: `modprobe`
//! happily loads the stock module and squats the name.
//!
//! It already cost three months: a confidently wrong garden note said the
//! cameras were blocked upstream with no on-device fix.
//!
//! So the five conditions below are one implementation, called by everything
//! that needs them: the pacman hook on a `linux-surface` transaction (a kernel
//! upgrade is exactly when this breaks), the boot-time unit (a DKMS rebuild
//! that failed during a *previous* upgrade shows up at the next boot), and
//! `cement-ir doctor`. The hook and the unit call the **installed** binary, so
//! neither carries a repo path.

use std::fs::File;
use std::process::{Command, ExitCode};

use irctl::media;

/// The srcversion of the patched carry — an MD5 over the source, so it is
/// stable across kernel rebuilds and changes only if the *patch* changes.
/// Anything else running under the name `dw9719` is not the module that was
/// vetted. (If you legitimately re-derive the patch, update this.)
pub const PATCHED_SRCVERSION: &str = "1458708E79A2D2CA601F853";
/// The stock, broken module — recorded because the mismatch **is** the
/// diagnosis, and a prefix is all that was captured in August.
pub const STOCK_SRCVERSION_PREFIX: &str = "7EDAAC55";

/// The VCM whose binding gates every pad link on this machine.
const VCM_DRIVER_LINK: &str = "/sys/bus/i2c/devices/i2c-INT347A:00-VCM/driver";
/// Sensors that must each end up with exactly one data link once the VCM
/// bound. (`ov5693` is deliberately absent: it is firmware-gated for an
/// unrelated reason — garden `hardware/notes/006`.)
const LINKED_SENSORS: [&str; 2] = ["ov7251", "ov8865"];

const MEDIA_DEV: &str = "/dev/media0";

pub struct Check {
    pub name: &'static str,
    pub pass: bool,
    pub detail: String,
}

fn ok(name: &'static str, detail: impl Into<String>) -> Check {
    Check { name, pass: true, detail: detail.into() }
}

fn bad(name: &'static str, detail: impl Into<String>) -> Check {
    Check { name, pass: false, detail: detail.into() }
}

/// `modinfo -F <field> dw9719`, trimmed. `None` = modinfo failed or said
/// nothing (module not found on disk, or kmod missing).
fn modinfo(field: &str) -> Option<String> {
    let out = Command::new("modinfo").arg("-F").arg(field).arg("dw9719").output().ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!s.is_empty()).then_some(s)
}

/// Count the data links touching an entity whose name starts with `prefix`.
/// Interface links (the `/dev/video*` associations) are excluded — only the
/// pad-to-pad links the notifier creates count.
fn sensor_links(topo: &media::Topology, prefix: &str) -> Option<usize> {
    let e = topo.entities.iter().find(|e| e.name.starts_with(prefix))?;
    let pads: Vec<u32> = topo
        .pads
        .iter()
        .filter(|p| p.entity_id == e.id)
        .map(|p| p.id)
        .collect();
    Some(
        topo.links
            .iter()
            .filter(|l| {
                l.flags & media::LNK_FL_LINK_TYPE_MASK == 0
                    && (pads.contains(&l.source_id) || pads.contains(&l.sink_id))
            })
            .count(),
    )
}

/// Run all five conditions. Pure reporting — never loads, rebuilds or rebinds
/// anything (`cement-ir dkms install` is the only thing that mutates, and it
/// is root-gated shell).
pub fn verify() -> Vec<Check> {
    let mut checks = Vec::new();

    // 1. Is a module called dw9719 loaded at all?
    let loaded = std::fs::read_to_string("/sys/module/dw9719/srcversion")
        .ok()
        .map(|s| s.trim().to_string());
    match &loaded {
        Some(v) => checks.push(ok("module loaded", format!("srcversion {v}"))),
        None => checks.push(bad(
            "module loaded",
            "dw9719 is not loaded — no camera can enumerate (sudo modprobe dw9719)",
        )),
    }

    // 2. Does the loaded module match what is on disk? A mismatch means the
    //    stock module squatted the name before DKMS's copy was reached.
    let on_disk = modinfo("srcversion");
    match (&loaded, &on_disk) {
        (Some(l), Some(d)) if l == d => checks.push(ok("srcversion matches", d.clone())),
        (Some(l), Some(d)) => checks.push(bad(
            "srcversion matches",
            format!("loaded {l} != on-disk {d} (the stock module squatted the name)"),
        )),
        (_, None) => checks.push(bad(
            "srcversion matches",
            "modinfo could not read dw9719 (not installed on disk, or kmod is missing)",
        )),
        (None, _) => {} // already reported by check 1
    }

    // 3. Is the loaded build the vetted patch?
    match &loaded {
        Some(v) if v == PATCHED_SRCVERSION => {
            checks.push(ok("patched build", format!("the carry ({PATCHED_SRCVERSION})")))
        }
        Some(v) if v.starts_with(STOCK_SRCVERSION_PREFIX) => checks.push(bad(
            "patched build",
            format!(
                "this is the STOCK module ({v}) — it has no i2c id_table, which is the entire bug; sudo cement-ir dkms install"
            ),
        )),
        Some(v) => checks.push(bad(
            "patched build",
            format!(
                "unrecognised build {v} (expected the carry {PATCHED_SRCVERSION}); if the patch was re-derived, update PATCHED_SRCVERSION in dkms.rs"
            ),
        )),
        None => {}
    }

    // 4. The missing id_table, seen from userspace: no i2c alias.
    match modinfo("alias") {
        Some(aliases) => {
            let n = aliases.lines().filter(|l| l.trim() == "i2c:dw9719").count();
            if n >= 1 {
                checks.push(ok("i2c alias present", format!("{n} × i2c:dw9719")));
            } else {
                checks.push(bad(
                    "i2c alias present",
                    "0 — this is the UNPATCHED module; no sensor will get a pad link",
                ));
            }
        }
        None => checks.push(bad("i2c alias present", "modinfo could not list aliases")),
    }

    // 5. DKMS's copy takes precedence over kernel/ — that precedence is the
    //    whole mechanism, so check where modprobe would actually resolve.
    match modinfo("filename") {
        Some(f) if f.contains("/updates/dkms/") => checks.push(ok("resolved from dkms", f)),
        Some(f) => checks.push(bad(
            "resolved from dkms",
            format!("{f} — not /updates/dkms/, so this is the in-tree build"),
        )),
        None => checks.push(bad("resolved from dkms", "modinfo could not read the filename")),
    }

    // 6. The bind everything downstream waits for.
    match std::fs::read_link(VCM_DRIVER_LINK) {
        Ok(p) => {
            let drv = p.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
            if drv == "dw9719" {
                checks.push(ok("VCM bound", "i2c-INT347A:00-VCM -> dw9719"));
            } else {
                checks.push(bad("VCM bound", format!("bound to {drv}, not dw9719")));
            }
        }
        Err(e) => checks.push(bad(
            "VCM bound",
            format!("i2c-INT347A:00-VCM has no driver ({e}) — the id_table is missing"),
        )),
    }

    // 7. The consequence that matters: links exist. Read straight from the
    //    media topology (no media-ctl dependency on this path).
    match File::open(MEDIA_DEV).and_then(|f| media::get_topology(&f)) {
        Err(e) => checks.push(bad("sensor links", format!("cannot read {MEDIA_DEV}: {e}"))),
        Ok(topo) => {
            for s in LINKED_SENSORS {
                let name: &'static str = if s == "ov7251" { "ov7251 link" } else { "ov8865 link" };
                match sensor_links(&topo, s) {
                    Some(1) => checks.push(ok(name, "1 data link")),
                    Some(n) => checks.push(bad(name, format!("{n} links, expected 1"))),
                    None => checks.push(bad(
                        name,
                        format!("no {s} entity in {MEDIA_DEV} — the sensor did not probe"),
                    )),
                }
            }
        }
    }

    checks
}

/// `cement-ir-rt dkms verify [--porcelain]`.
///
/// Human format mirrors `doctor`'s one-line-per-check layout. `--porcelain`
/// emits `status|name|detail` so the shell half can render the same checks in
/// its own table without a second implementation of any of them.
pub fn verify_run(porcelain: bool) -> ExitCode {
    let checks = verify();
    let failed: Vec<&Check> = checks.iter().filter(|c| !c.pass).collect();
    for c in &checks {
        if porcelain {
            println!("{}|{}|{}", if c.pass { "ok" } else { "FAIL" }, c.name, c.detail);
        } else {
            println!(
                "  {:<5} {:<20} {}",
                if c.pass { "ok" } else { "FAIL" },
                c.name,
                c.detail
            );
        }
    }
    if failed.is_empty() {
        if !porcelain {
            println!("dkms verify: the dw9719 carry is intact ({} checks)", checks.len());
        }
        return ExitCode::SUCCESS;
    }
    if !porcelain {
        eprintln!(
            "dkms verify: {} of {} checks FAILED; first: {} — {}",
            failed.len(),
            checks.len(),
            failed[0].name,
            failed[0].detail
        );
        eprintln!(
            "dkms verify: without the patched dw9719 NO camera enumerates; re-cement with `sudo cement-ir dkms install`"
        );
    }
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two srcversions are the diagnosis, so they must not be confusable.
    #[test]
    fn the_two_builds_are_distinguishable() {
        assert!(!PATCHED_SRCVERSION.starts_with(STOCK_SRCVERSION_PREFIX));
        assert_eq!(PATCHED_SRCVERSION.len(), 23, "srcversion is 23 hex chars here");
    }

    /// Every check name is unique — the shell half keys its table rows off
    /// them, and a duplicate would silently overwrite a row.
    #[test]
    fn check_names_are_unique() {
        let checks = verify();
        let mut names: Vec<&str> = checks.iter().map(|c| c.name).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "duplicate check name");
    }
}
