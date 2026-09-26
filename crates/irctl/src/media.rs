//! Media-controller ioctls: the **v2 topology API** for enumeration +
//! resolution, and the legacy `SETUP_LINK` for the one link mutation.
//!
//! This is the port of `lib/ir-up.sh`'s resolution discipline: entities are
//! found **by name, never by number** — the media graph renumbers across
//! reboots, and streaming from the wrong CIO2 node does not error, it blocks
//! forever with no output. That trap is encoded in `find_ir_path`, which is
//! pure over `&Topology` so it is unit-tested without hardware.
//!
//! Why the v2 API: the legacy `MEDIA_IOC_ENUM_ENTITIES` + `dev` union turned
//! out to report wrong device-node minors on this kernel (slice 006 bring-up
//! found cio2 minors off by one and i2c subdev minors off by +15 against
//! `media-ctl`), and the kernel's own header deprecates the legacy enum for
//! new applications. `media-ctl` resolves nodes through `MEDIA_IOC_G_TOPOLOGY`
//! (entity → interface link → devnode), which is what `find_ir_path` now does.

use std::fs::File;
use std::io;
use std::os::unix::io::AsRawFd;

/// `MEDIA_IOC_DEVICE_INFO` — the driver string + media API version.
pub const MEDIA_IOC_DEVICE_INFO: libc::c_ulong = 0xc1007c00;
/// `MEDIA_IOC_G_TOPOLOGY` — the v2 topology snapshot.
pub const MEDIA_IOC_G_TOPOLOGY: libc::c_ulong = 0xc0487c04;
/// `MEDIA_IOC_SETUP_LINK` — enable/disable one pad-to-pad link (legacy; there
/// is no v2 replacement).
pub const MEDIA_IOC_SETUP_LINK: libc::c_ulong = 0xc0347c03;

pub const ENT_F_IO_V4L: u32 = 0x10001; // a /dev/videoN node (the CIO2 capture)
pub const ENT_F_CAM_SENSOR: u32 = 0x20001; // V4L2 subdev sensor (ov7251)
pub const ENT_F_VID_IF_BRIDGE: u32 = 0x5002; // CSI-2 receiver (ipu3-csi2 N)

pub const INTF_T_V4L_VIDEO: u32 = 0x200;
pub const INTF_T_V4L_SUBDEV: u32 = 0x203;

pub const LNK_FL_ENABLED: u32 = 0x1;
pub const LNK_FL_LINK_TYPE_MASK: u32 = 0xf << 28;
pub const LNK_FL_INTERFACE_LINK: u32 = 1 << 28;
pub const PAD_FL_SINK: u32 = 0x1;
pub const PAD_FL_SOURCE: u32 = 0x2;

/// `MEDIA_V2_PAD_HAS_INDEX` needs media API >= 4.19 — pad.index is what the
/// legacy `SETUP_LINK` wants. This kernel is 6.19; the check still runs.
const MEDIA_API_HAS_PAD_INDEX: u32 = (4 << 16) | (19 << 8);

const V2_ENTITY_NAME_LEN: usize = 64;

fn ioctl(fd: &File, request: libc::c_ulong, arg: usize) -> io::Result<()> {
    // SAFETY: the ioctl ABI contract; request numbers and struct layouts are
    // pinned against the running kernel's UAPI headers in abi_constants.rs.
    match unsafe { libc::ioctl(fd.as_raw_fd(), request, arg) } {
        -1 => Err(io::Error::last_os_error()),
        _ => Ok(()),
    }
}

/// `struct media_v2_entity` — 96 bytes (packed in the kernel header; the
/// natural repr(C) layout is identical: all fields land 4-aligned).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct V2Entity {
    pub id: u32,              // 0
    pub name: [u8; V2_ENTITY_NAME_LEN], // 4..68
    pub function: u32,        // 68
    pub flags: u32,           // 72
    pub reserved: [u32; 5],   // 76..96
}

/// `struct media_v2_interface` — 112 bytes; devnode major/minor at 48..56.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct V2Interface {
    pub id: u32,             // 0
    pub intf_type: u32,      // 4
    pub flags: u32,          // 8
    pub reserved: [u32; 9],  // 12..48
    pub raw: [u32; 16],      // 48..112 (union: devnode {major,minor} at 48)
}

/// `struct media_v2_pad` — 32 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct V2Pad {
    pub id: u32,            // 0
    pub entity_id: u32,     // 4
    pub flags: u32,         // 8
    pub index: u32,         // 12
    pub reserved: [u32; 4], // 16..32
}

/// `struct media_v2_link` — 40 bytes.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct V2Link {
    pub id: u32,            // 0
    pub source_id: u32,     // 4
    pub sink_id: u32,       // 8
    pub flags: u32,         // 12
    pub reserved: [u32; 6], // 16..40
}

/// `struct media_v2_topology` — 72 bytes; carries **user pointers** to the
/// caller's arrays (the kernel fills them in).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct V2Topology {
    pub topology_version: u64, // 0
    pub num_entities: u32,     // 8
    pub reserved1: u32,        // 12
    pub ptr_entities: u64,     // 16
    pub num_interfaces: u32,   // 24
    pub reserved2: u32,        // 28
    pub ptr_interfaces: u64,   // 32
    pub num_pads: u32,         // 40
    pub reserved3: u32,        // 44
    pub ptr_pads: u64,         // 48
    pub num_links: u32,        // 56
    pub reserved4: u32,        // 60
    pub ptr_links: u64,        // 64
}

/// `struct media_link_desc` (legacy) — 52 bytes; the SETUP_LINK payload.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct PadDesc {
    pub entity: u32,   // 0
    pub index: u16,    // 4
    pub _pad: u16,     // 6 (alignment)
    pub flags: u32,    // 8
    pub reserved: [u32; 2], // 12..20
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LinkDesc {
    pub source: PadDesc,    // 0..20
    pub sink: PadDesc,      // 20..40
    pub flags: u32,         // 40
    pub reserved: [u32; 2], // 44..52
}

/// A decoded entity: the fields the graph logic needs.
#[derive(Debug, Clone)]
pub struct Entity {
    pub id: u32,
    pub name: String,
    pub function: u32,
}

impl From<&V2Entity> for Entity {
    fn from(e: &V2Entity) -> Self {
        let name = String::from_utf8_lossy(
            &e.name[..e.name.iter().position(|&b| b == 0).unwrap_or(V2_ENTITY_NAME_LEN)],
        )
        .into_owned();
        Entity { id: e.id, name, function: e.function }
    }
}

/// A decoded interface (a device node exposed by an entity).
#[derive(Debug, Clone)]
pub struct Interface {
    pub id: u32,
    pub intf_type: u32,
    pub dev: Option<(u32, u32)>,
}

impl From<&V2Interface> for Interface {
    fn from(i: &V2Interface) -> Self {
        let dev = if i.raw[0] != 0 || i.raw[1] != 0 {
            Some((i.raw[0], i.raw[1]))
        } else {
            None
        };
        Interface { id: i.id, intf_type: i.intf_type, dev }
    }
}

/// The whole topology, decoded.
#[derive(Debug, Clone, Default)]
pub struct Topology {
    pub entities: Vec<Entity>,
    pub interfaces: Vec<Interface>,
    pub pads: Vec<V2Pad>,
    pub links: Vec<V2Link>,
}

/// Read the full topology through `MEDIA_IOC_G_TOPOLOGY` (two-phase: first
/// the counts, then the arrays — the same dance media-ctl does).
pub fn get_topology(fd: &File) -> io::Result<Topology> {
    // Phase 1: zero counts, NULL pointers — the kernel writes the counts.
    let mut t = V2Topology {
        topology_version: 0,
        num_entities: 0,
        reserved1: 0,
        ptr_entities: 0,
        num_interfaces: 0,
        reserved2: 0,
        ptr_interfaces: 0,
        num_pads: 0,
        reserved3: 0,
        ptr_pads: 0,
        num_links: 0,
        reserved4: 0,
        ptr_links: 0,
    };
    ioctl(fd, MEDIA_IOC_G_TOPOLOGY, &mut t as *mut V2Topology as usize)?;

    // Phase 2: caller-allocated arrays at the pointers.
    let mut ents = vec![zeroed_v2_entity(); t.num_entities as usize];
    let mut intfs = vec![zeroed_v2_interface(); t.num_interfaces as usize];
    let mut pads = vec![zeroed_v2_pad(); t.num_pads as usize];
    let mut links = vec![zeroed_v2_link(); t.num_links as usize];
    t.ptr_entities = ents.as_mut_ptr() as u64;
    t.ptr_interfaces = intfs.as_mut_ptr() as u64;
    t.ptr_pads = pads.as_mut_ptr() as u64;
    t.ptr_links = links.as_mut_ptr() as u64;
    ioctl(fd, MEDIA_IOC_G_TOPOLOGY, &mut t as *mut V2Topology as usize)?;

    Ok(Topology {
        entities: ents.iter().map(Entity::from).collect(),
        interfaces: intfs.iter().map(Interface::from).collect(),
        pads,
        links,
    })
}

fn zeroed_v2_entity() -> V2Entity {
    V2Entity { id: 0, name: [0; V2_ENTITY_NAME_LEN], function: 0, flags: 0, reserved: [0; 5] }
}
fn zeroed_v2_interface() -> V2Interface {
    V2Interface { id: 0, intf_type: 0, flags: 0, reserved: [0; 9], raw: [0; 16] }
}
fn zeroed_v2_pad() -> V2Pad {
    V2Pad { id: 0, entity_id: 0, flags: 0, index: 0, reserved: [0; 4] }
}
fn zeroed_v2_link() -> V2Link {
    V2Link { id: 0, source_id: 0, sink_id: 0, flags: 0, reserved: [0; 6] }
}

/// Verify the media API is new enough that v2 pads carry their index (which
/// the legacy SETUP_LINK payload needs). Pre-4.19 kernels are out of scope
/// for this device; the check makes the assumption loud instead of silent.
pub fn check_pad_index_api(fd: &File) -> io::Result<()> {
    #[repr(C)]
    struct DeviceInfo {
        driver: [u8; 16],
        model: [u8; 32],
        serial: [u8; 40],
        bus_info: [u8; 32],
        media_version: u32,
        hw_revision: u32,
        driver_version: u32,
        reserved: [u32; 31],
    }
    let mut info = DeviceInfo {
        driver: [0; 16],
        model: [0; 32],
        serial: [0; 40],
        bus_info: [0; 32],
        media_version: 0,
        hw_revision: 0,
        driver_version: 0,
        reserved: [0; 31],
    };
    ioctl(fd, MEDIA_IOC_DEVICE_INFO, &mut info as *mut DeviceInfo as usize)?;
    if info.media_version < MEDIA_API_HAS_PAD_INDEX {
        return Err(io::Error::other(format!(
            "media API version {}.{}.{} predates 4.19 (pads carry no index)",
            info.media_version >> 16,
            (info.media_version >> 8) & 0xff,
            info.media_version & 0xff,
        )));
    }
    Ok(())
}

/// Enable (or disable) one pad-to-pad link — the `media-ctl -l` equivalent.
/// Legacy payload; pad ids come from the v2 `index` fields.
pub fn setup_link(fd: &File, source: Pad, sink: Pad, flags: u32) -> io::Result<()> {
    let mut ld = LinkDesc {
        source: PadDesc {
            entity: source.entity,
            index: source.index,
            _pad: 0,
            flags: source.flags,
            reserved: [0; 2],
        },
        sink: PadDesc {
            entity: sink.entity,
            index: sink.index,
            _pad: 0,
            flags: sink.flags,
            reserved: [0; 2],
        },
        flags,
        reserved: [0; 2],
    };
    ioctl(fd, MEDIA_IOC_SETUP_LINK, &mut ld as *mut LinkDesc as usize)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pad {
    pub entity: u32,
    pub index: u16,
    pub flags: u32,
}

/// The resolved IR capture path: names, the devnodes to open (resolved to
/// `/dev/...` names via `node_name`), and the pad indices the link mutation
/// needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IrPath {
    /// `"ov7251 3-0060"` — the sensor's entity name.
    pub sensor: String,
    /// `"ipu3-csi2 2"` — the CSI-2 receiver the sensor feeds.
    pub csi2: String,
    /// The CIO2 capture node paired with that receiver, resolved by name,
    /// **never** by guessing `video$IDX` — and its node name via sysfs,
    /// never by assuming the minor is the name number.
    pub video: (u32, u32),
    /// The sensor's subdev node for gain/exposure.
    pub subdev: Option<(u32, u32)>,
    /// The pad the link mutation targets: sensor pad 0 -> csi2 sink pad.
    pub sensor_pad: Pad,
    pub csi2_pad: Pad,
}

/// The device node an entity exposes, as a `(major, minor)` devnode, given
/// the interface type it must be (video node vs subdev node). Entity →
/// interface link → devnode, the resolution media-ctl uses.
fn devnode_of(t: &Topology, entity_id: u32, intf_type: u32) -> Option<(u32, u32)> {
    let ilink = t.links.iter().find(|l| {
        (l.source_id == entity_id || l.sink_id == entity_id)
            && l.flags & LNK_FL_LINK_TYPE_MASK == LNK_FL_INTERFACE_LINK
    })?;
    let iface_id = if ilink.source_id == entity_id { ilink.sink_id } else { ilink.source_id };
    let iface = t.interfaces.iter().find(|i| i.id == iface_id)?;
    if iface.intf_type != intf_type {
        return None;
    }
    iface.dev
}

/// The `/dev/v4l-subdevN` devnode of any entity that exposes one (the CSI-2
/// receivers' pads also get their media-bus format set, via their own node).
pub fn subdev_devnode_of(t: &Topology, entity_id: u32) -> Option<(u32, u32)> {
    devnode_of(t, entity_id, INTF_T_V4L_SUBDEV)
}

/// Turn a `(major, minor)` devnode into a `/dev/<name>` path **via sysfs**.
///
/// The trap this exists to avoid: a V4L2 node's *name* number is not its char
/// device *minor* — `/dev/v4l-subdev7` has minor 22 on this kernel and
/// `/dev/video2` has minor 3, because the v4l2 core numbers video nodes,
/// subdevs and minors from different counters. `/sys/dev/char/M:m` resolves
/// to the device's own sysfs dir, whose basename IS the node name (what
/// media-ctl's "device node name" line is derived from). No arithmetic, no
/// /dev scanning, no udev.
pub fn node_name(major: u32, minor: u32) -> Option<String> {
    let target = std::fs::read_link(format!("/sys/dev/char/{major}:{minor}")).ok()?;
    let name = target.file_name()?.to_str()?.to_string();
    Some(format!("/dev/{name}"))
}

/// Resolve the IR path **by entity name** over a topology snapshot.
///
/// Pure — unit-tested against both a representative boot graph and the
/// renumbered-graph trap (`ipu3-csi2 2` paired with `/dev/video9` while
/// `ipu3-cio2 0` owns `/dev/video2`).
pub fn find_ir_path(t: &Topology) -> Result<IrPath, String> {
    let sensor = t
        .entities
        .iter()
        .find(|e| e.name.starts_with("ov7251 "))
        .ok_or_else(|| {
            "ov7251 not present in /dev/media0 — almost certainly dw9719 failed to bind, \
             so no links were created (check: lsmod | grep dw9719; cement-ir dkms verify)"
                .to_string()
        })?;

    // The sensor feeds exactly one CSI-2 receiver, from its source pad 0.
    let spad = t
        .pads
        .iter()
        .find(|p| p.entity_id == sensor.id && p.index == 0 && p.flags & PAD_FL_SOURCE != 0)
        .ok_or_else(|| format!("{} has no source pad 0", sensor.name))?;
    let link = t
        .links
        .iter()
        .find(|l| l.source_id == spad.id && l.flags & LNK_FL_LINK_TYPE_MASK == 0)
        .ok_or_else(|| format!("no link out of {} pad 0", sensor.name))?;
    let sink_pad = t
        .pads
        .iter()
        .find(|p| p.id == link.sink_id)
        .ok_or_else(|| format!("link sink pad {} not in the graph", link.sink_id))?;
    let csi2 = t
        .entities
        .iter()
        .find(|e| e.id == sink_pad.entity_id)
        .ok_or_else(|| format!("link sink entity {} not in the graph", sink_pad.entity_id))?;
    if !csi2.name.starts_with("ipu3-csi2 ") {
        return Err(format!(
            "{} pad 0 feeds {} — not an ipu3-csi2 receiver",
            sensor.name, csi2.name
        ));
    }

    // The CIO2 video node paired with that receiver: the entity whose name is
    // "ipu3-cio2 <IDX>" for the receiver's IDX. Deriving /dev/video$IDX
    // instead is the renumbered-graph trap (csi2 2 paired with /dev/video9 on
    // one boot, /dev/video2 on another).
    let idx = csi2.name.rsplit(' ').next().unwrap_or("");
    let cio2_name = format!("ipu3-cio2 {idx}");
    let cio2 = t
        .entities
        .iter()
        .find(|e| e.name == cio2_name)
        .ok_or_else(|| format!("could not resolve the CIO2 video node for '{}'", csi2.name))?;
    let video = devnode_of(t, cio2.id, INTF_T_V4L_VIDEO)
        .ok_or_else(|| format!("'{}' has no video device node", cio2.name))?;
    let subdev = devnode_of(t, sensor.id, INTF_T_V4L_SUBDEV);

    Ok(IrPath {
        sensor: sensor.name.clone(),
        csi2: csi2.name.clone(),
        video,
        subdev,
        sensor_pad: Pad { entity: sensor.id, index: spad.index as u16, flags: spad.flags },
        csi2_pad: Pad { entity: csi2.id, index: sink_pad.index as u16, flags: sink_pad.flags },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sensor_entity(id: u32, name: &str) -> Entity {
        Entity { id, name: name.to_string(), function: ENT_F_CAM_SENSOR }
    }

    fn csi2_entity(id: u32, name: &str) -> Entity {
        Entity { id, name: name.to_string(), function: ENT_F_VID_IF_BRIDGE }
    }

    fn cio2_entity(id: u32, name: &str) -> Entity {
        Entity { id, name: name.to_string(), function: ENT_F_IO_V4L }
    }

    fn pad(id: u32, entity_id: u32, flags: u32, index: u32) -> V2Pad {
        V2Pad { id, entity_id, flags, index, reserved: [0; 4] }
    }

    fn link(id: u32, source_id: u32, sink_id: u32, flags: u32) -> V2Link {
        V2Link { id, source_id, sink_id, flags, reserved: [0; 6] }
    }

    fn interface(id: u32, intf_type: u32, minor: u32) -> Interface {
        Interface { id, intf_type, dev: Some((81, minor)) }
    }

    /// The boot this was written on: ov7251 3-0060 -> ipu3-csi2 2 ->
    /// ipu3-cio2 2 -> devnode (81, 3) — which IS /dev/video2 on this boot
    /// (the v4l2 core numbers node names and char minors from different
    /// counters; see `node_name`) — with cio2 0 owning (81, 12) as the trap
    /// bait and the sensor's subdev at (81, 22) = /dev/v4l-subdev7.
    fn representative_topology() -> Topology {
        Topology {
            entities: vec![
                csi2_entity(1, "ipu3-csi2 0"),
                cio2_entity(4, "ipu3-cio2 0"),
                csi2_entity(19, "ipu3-csi2 2"),
                cio2_entity(22, "ipu3-cio2 2"),
                sensor_entity(67, "ov7251 3-0060"),
            ],
            interfaces: vec![
                interface(100, INTF_T_V4L_VIDEO, 12),   // cio2 0 -> video9-ish (trap bait)
                interface(101, INTF_T_V4L_VIDEO, 3),    // cio2 2 -> /dev/video2
                interface(102, INTF_T_V4L_SUBDEV, 22),  // ov7251 -> /dev/v4l-subdev7
            ],
            pads: vec![
                pad(10, 67, PAD_FL_SOURCE, 0),  // sensor pad 0
                pad(11, 19, PAD_FL_SINK, 0),    // csi2 2 pad 0
            ],
            links: vec![
                link(50, 10, 11, LNK_FL_ENABLED),      // sensor -> csi2 2
                link(51, 22, 101, LNK_FL_INTERFACE_LINK), // cio2 2 -> video2 iface
                link(52, 4, 100, LNK_FL_INTERFACE_LINK),  // cio2 0 -> its iface
                link(53, 67, 102, LNK_FL_INTERFACE_LINK), // sensor -> subdev iface
            ],
        }
    }

    #[test]
    fn resolves_by_name() {
        let p = find_ir_path(&representative_topology()).unwrap();
        assert_eq!(p.sensor, "ov7251 3-0060");
        assert_eq!(p.csi2, "ipu3-csi2 2");
        assert_eq!(p.video, (81, 3)); // cio2 2's node — NOT cio2 0's (81, 12)
        assert_eq!(p.subdev, Some((81, 22)));
        assert_eq!(p.sensor_pad.entity, 67);
        assert_eq!(p.csi2_pad.entity, 19);
    }

    #[test]
    fn resolves_the_renumbered_boot() {
        // A later boot: the same names, different minors — csi2 2 now pairs
        // with (81, 12) and cio2 0 got (81, 3). The trap, mirrored.
        let mut t = representative_topology();
        t.interfaces = vec![
            interface(100, INTF_T_V4L_VIDEO, 3),
            interface(101, INTF_T_V4L_VIDEO, 12),
            interface(102, INTF_T_V4L_SUBDEV, 22),
        ];
        let p = find_ir_path(&t).unwrap();
        assert_eq!(p.video, (81, 12));
    }

    #[test]
    fn node_names_come_from_sysfs_not_minors() {
        // Live ground truth on this kernel: minor 22 -> /dev/v4l-subdev7,
        // minor 3 -> /dev/video2. (Skipped if sysfs isn't mounted, e.g. in a
        // container.)
        if let Some(n) = node_name(81, 22) {
            assert_eq!(n, "/dev/v4l-subdev7");
        }
        if let Some(n) = node_name(81, 3) {
            assert_eq!(n, "/dev/video2");
        }
    }

    #[test]
    fn missing_sensor_names_the_cause() {
        let mut t = representative_topology();
        t.entities.retain(|e| !e.name.starts_with("ov7251"));
        let err = find_ir_path(&t).unwrap_err();
        assert!(err.contains("dw9719"), "{err}");
    }

    #[test]
    fn sensor_with_no_link_is_an_error_not_a_block() {
        let mut t = representative_topology();
        // Drop only the sensor -> csi2 data link (pads 10 -> 11); the
        // interface links stay, so the topology is otherwise healthy.
        t.links.retain(|l| !(l.source_id == 10 && l.sink_id == 11));
        let err = find_ir_path(&t).unwrap_err();
        assert!(err.contains("no link out of"), "{err}");
    }
}
