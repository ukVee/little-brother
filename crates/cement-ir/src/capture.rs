//! The capture cycle: bring the graph up, set the operating point, tear it
//! down in the order that does not wedge the sensor.
//!
//! Extracted from `irfaced.rs` in slice 007 so that the daemon and the soak
//! (`soak.rs`) share **one** copy of the ordering. That is not tidiness: the
//! teardown order *is* the wedge fix, and a soak that proved a second copy of
//! it would prove nothing about the auth path. Everything here was learned on
//! this device and is documented in `docs/irfaced-design.md` and the garden
//! (`hardware/notes/005`, `projects/cement-ir/notes/001`).

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use irctl::{fourcc, media, subdev, video};
use tps68470::torch::Torch;

use crate::faults;

/// Why a capture could not start, and whether it carries the wedge signature.
///
/// The distinction is the whole point of slice 007: "the sensor is wedged"
/// must be countable, and separable from "there is no `/dev/media0`" or "the
/// format is wrong". Callers report `msg`; the fault counter keys off
/// `wedged`.
#[derive(Debug)]
pub struct Error {
    pub msg: String,
    pub wedged: bool,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.msg)
    }
}

impl Error {
    /// A failure that is not the sensor wedging (a missing node, an
    /// unsupported kernel API, a bad format).
    pub fn plain(msg: impl Into<String>) -> Error {
        Error { msg: msg.into(), wedged: false }
    }

    /// A failure whose errno decides: EREMOTEIO/ENODEV/ENXIO/EINVAL from the
    /// sensor or the CIO2 pipeline is the wedge (`faults::looks_wedged`).
    pub fn from_io(msg: impl Into<String>, e: &io::Error) -> Error {
        Error { msg: msg.into(), wedged: faults::looks_wedged(e) }
    }
}

/// The resolved + configured IR capture path (see `media::find_ir_path`).
#[derive(Debug)]
pub struct Graph {
    pub sensor: String,
    pub csi2: String,
    pub video: PathBuf,
    pub subdev: Option<PathBuf>,
}

/// Resolve by entity name and configure the graph — the `lib/ir-up.sh`
/// discipline, in Rust. Never hardcode a node number; see `find_ir_path`.
pub fn bring_up(media_path: &Path) -> Result<Graph, Error> {
    let f = File::open(media_path)
        .map_err(|e| Error::plain(format!("cannot open {}: {e}", media_path.display())))?;
    media::check_pad_index_api(&f).map_err(|e| Error::plain(e.to_string()))?;
    let topo = media::get_topology(&f)
        .map_err(|e| Error::from_io(format!("get topology: {e}"), &e))?;
    // A missing sensor entity is the wedge's other face: an `ov7251` that
    // stopped answering I2C gets unbound, and the graph loses the entity.
    let path = media::find_ir_path(&topo).map_err(|m| Error { msg: m, wedged: true })?;

    // Device nodes come from sysfs (see media::node_name): a V4L2 node's name
    // number is NOT its char minor.
    let video_node = node_or_err(path.video, "CIO2 video")?;
    let subdev_node = match path.subdev {
        Some(d) => Some(node_or_err(d, "sensor subdev")?),
        None => None,
    };

    // Enable the sensor -> CSI-2 link (media-ctl -l ... [1]).
    media::setup_link(&f, path.sensor_pad, path.csi2_pad, media::LNK_FL_ENABLED).map_err(|e| {
        Error::from_io(
            format!("enable link {}:0 -> {}:0: {e}", path.sensor, path.csi2),
            &e,
        )
    })?;

    // Media-bus formats on both pads (media-ctl -V ... fmt:Y10_1X10/640x480).
    let sd = subdev_node.as_ref().unwrap();
    let sdf = File::open(sd).map_err(|e| Error::from_io(format!("open {sd}: {e}"), &e))?;
    // EREMOTEIO here IS the wedge: the sensor is not answering I2C at all, so
    // the driver cannot even write its reset register (garden hardware/005).
    subdev::set_format(&sdf, 0, fourcc::MEDIA_BUS_FMT_Y10_1X10, 640, 480)
        .map_err(|e| Error::from_io(format!("set fmt on {sd}: {e}"), &e))?;

    if let Some((maj, min)) = media::subdev_devnode_of(&topo, path.csi2_pad.entity) {
        if let Some(csd) = media::node_name(maj, min) {
            let csif =
                File::open(&csd).map_err(|e| Error::from_io(format!("open {csd}: {e}"), &e))?;
            subdev::set_format(&csif, 0, fourcc::MEDIA_BUS_FMT_Y10_1X10, 640, 480)
                .map_err(|e| Error::from_io(format!("set fmt on {csd}: {e}"), &e))?;
        }
    }

    // The CIO2 capture format is set by Stream::start when the feed opens the
    // node (it picks single-planar vs mplane from VIDIOC_QUERYCAP, like
    // v4l2-ctl). Nothing to do here — bring_up's job is the media graph.

    Ok(Graph {
        sensor: path.sensor,
        csi2: path.csi2,
        video: PathBuf::from(video_node),
        subdev: subdev_node.map(PathBuf::from),
    })
}

fn node_or_err(dev: (u32, u32), what: &str) -> Result<String, Error> {
    media::node_name(dev.0, dev.1).ok_or_else(|| {
        Error::plain(format!(
            "could not resolve the {what} node for {dev:?} (sysfs missing?)"
        ))
    })
}

/// Set gain + exposure immediately before capture — **every time**.
///
/// `analogue_gain` reverts to its minimum of 16, which renders a lit room and
/// a dark one identically (~mean 16 of 1023). Nothing runs auto-exposure on
/// this path, so if the caller does not set it, nothing does. The
/// 2026-08-21 trap; garden `hardware/notes/005`.
///
/// A failure here is a warning, not an abort: `ir-up.sh` has always warned and
/// continued, and the post-capture readback is what catches the consequence.
pub fn set_operating_point(tag: &str, graph: &Graph, gain: u16, exposure: u32) {
    let Some(sd) = &graph.subdev else {
        eprintln!("{tag}: WARNING: no subdev node; gain/exposure NOT set");
        return;
    };
    let Ok(sdf) = File::open(sd) else {
        eprintln!("{tag}: WARNING: could not open {}; gain/exposure NOT set", sd.display());
        return;
    };
    for (id, name, val) in [
        (subdev::CID_ANALOGUE_GAIN, "analogue_gain", gain as i32),
        (subdev::CID_EXPOSURE, "exposure", exposure as i32),
    ] {
        if let Err(e) = subdev::set_ctrl(&sdf, id, val) {
            eprintln!("{tag}: WARNING: set {name}={val} failed: {e} (frames may be near-black)");
        }
    }
}

/// What the sensor said about itself after the capture. `None` means the read
/// itself failed — on this path that is the strongest single sign of a wedge
/// (the sensor stops answering I2C at all).
#[derive(Debug, Default, Clone)]
pub struct Readback {
    pub gain: Option<i32>,
    pub exposure: Option<i32>,
    /// Why the readback could not be taken, if it could not.
    pub err: Option<String>,
}

impl Readback {
    /// The sensor answered both control reads — i.e. it is still on the bus.
    pub fn answered(&self) -> bool {
        self.gain.is_some() && self.exposure.is_some()
    }

    pub fn describe(&self) -> String {
        match &self.err {
            Some(e) => format!("gain/exposure readback failed: {e}"),
            None => format!(
                "gain={} exposure={}",
                self.gain.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
                self.exposure.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
            ),
        }
    }
}

/// Read the sensor's controls back.
///
/// Two faults render identically as a ~18-mean frame (torch dark vs gain
/// reverted to 16); the readback is what tells them apart. It doubles as the
/// "does the sensor still answer?" probe the soak needs after every cycle —
/// the same question `v4l2-ctl --get-ctrl analogue_gain,exposure` answers.
pub fn readback(graph: &Graph) -> Readback {
    let Some(sd) = &graph.subdev else {
        return Readback { err: Some("no subdev node".into()), ..Default::default() };
    };
    let sdf = match File::open(sd) {
        Ok(f) => f,
        Err(e) => {
            return Readback {
                err: Some(format!("open {}: {e}", sd.display())),
                ..Default::default()
            }
        }
    };
    let g = subdev::get_ctrl(&sdf, subdev::CID_ANALOGUE_GAIN);
    let x = subdev::get_ctrl(&sdf, subdev::CID_EXPOSURE);
    let err = match (&g, &x) {
        (Err(e), _) => Some(format!("analogue_gain: {e}")),
        (_, Err(e)) => Some(format!("exposure: {e}")),
        _ => None,
    };
    Readback { gain: g.ok(), exposure: x.ok(), err }
}

/// The result of a teardown: what the sensor said, and everything that went
/// wrong on the way out.
#[derive(Debug, Default)]
pub struct Teardown {
    pub readback: Readback,
    /// One entry per step that failed. Empty = clean teardown, which is the
    /// whole point of the ordering below.
    pub errors: Vec<String>,
}

/// Tear a capture cycle down in the learned order. **One copy for the daemon
/// and the soak** — see the module doc.
///
/// 1. **Torch out first** — the illuminator must never outlive the capture by
///    even one slow step of teardown.
/// 2. **Capture `STREAMOFF`** (the producer) — clean, in-process; nothing is
///    ever killed mid-`DQBUF`, which is what used to wedge the sensor
///    (`STREAMON` EINVAL, I2C EREMOTEIO afterwards).
/// 3. **Sink `STREAMOFF`** (the consumer).
/// 4. **Control readback**, always, even after a failure.
///
/// Never returns an error: every step reports its own and the cycle is over
/// either way. The caller decides what a non-empty `errors` means.
pub fn teardown(
    tag: &str,
    torch: Option<Torch>,
    cap: &mut video::Stream,
    sink: Option<&mut video::Stream>,
    graph: &Graph,
) -> Teardown {
    let mut errors = Vec::new();
    // 1. Torch out FIRST.
    if let Some(mut t) = torch {
        match t.off() {
            Err(e) => {
                let msg =
                    format!("torch release failed: {e} (the ~13 s hardware watchdog covers this)");
                eprintln!("{tag}: {msg}");
                errors.push(msg);
            }
            Ok(()) => eprintln!("{tag}: torch released"),
        }
    }
    // 2. Capture STREAMOFF (producer first).
    if let Err(e) = cap.stop() {
        let msg = format!("capture STREAMOFF: {e}");
        eprintln!("{tag}: {msg}");
        errors.push(msg);
    }
    // 3. Sink STREAMOFF (consumer).
    if let Some(s) = sink {
        if let Err(e) = s.stop() {
            let msg = format!("loopback STREAMOFF: {e}");
            eprintln!("{tag}: {msg}");
            errors.push(msg);
        }
    }
    // 4. Readback.
    Teardown { readback: readback(graph), errors }
}
