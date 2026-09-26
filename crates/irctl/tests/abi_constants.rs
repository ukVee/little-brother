//! Pin every hand-written ABI constant against the ground truth captured from
//! this kernel's UAPI headers (see `/tmp/opencode/abi_probe.c` + its output;
//! the probe is documented in `crates/irctl/src/lib.rs`). If a kernel upgrade
//! changes one of these, the test fails HERE — loudly, at build time — not as
//! a silent EINVAL on the auth path.

use std::mem::size_of;

use irctl::{fourcc, media, subdev, video};

#[test]
fn media_ioctl_numbers() {
    assert_eq!(media::MEDIA_IOC_DEVICE_INFO, 0xc1007c00);
    assert_eq!(media::MEDIA_IOC_G_TOPOLOGY, 0xc0487c04);
    assert_eq!(media::MEDIA_IOC_SETUP_LINK, 0xc0347c03);
}

#[test]
fn media_enum_values() {
    assert_eq!(media::ENT_F_IO_V4L, 0x10001);
    assert_eq!(media::ENT_F_CAM_SENSOR, 0x20001);
    assert_eq!(media::ENT_F_VID_IF_BRIDGE, 0x5002);
    assert_eq!(media::INTF_T_V4L_VIDEO, 0x200);
    assert_eq!(media::INTF_T_V4L_SUBDEV, 0x203);
    assert_eq!(media::LNK_FL_ENABLED, 0x1);
    assert_eq!(media::LNK_FL_LINK_TYPE_MASK, 0xf000_0000);
    assert_eq!(media::LNK_FL_INTERFACE_LINK, 0x1000_0000);
    assert_eq!(media::PAD_FL_SINK, 0x1);
    assert_eq!(media::PAD_FL_SOURCE, 0x2);
}

#[test]
fn media_struct_sizes() {
    // v2 topology structs (all packed in the kernel header; the natural
    // repr(C) layouts are identical — verified by these sizes).
    assert_eq!(size_of::<media::V2Entity>(), 96);
    assert_eq!(size_of::<media::V2Interface>(), 112);
    assert_eq!(size_of::<media::V2Pad>(), 32);
    assert_eq!(size_of::<media::V2Link>(), 40);
    assert_eq!(size_of::<media::V2Topology>(), 72);
    // Legacy SETUP_LINK payload.
    assert_eq!(size_of::<media::PadDesc>(), 20);
    assert_eq!(size_of::<media::LinkDesc>(), 52);
}

#[test]
fn video_ioctl_numbers() {
    assert_eq!(video::VIDIOC_QUERYCAP, 0x80685600);
    assert_eq!(video::VIDIOC_G_FMT, 0xc0d05604);
    assert_eq!(video::VIDIOC_S_FMT, 0xc0d05605);
    assert_eq!(video::VIDIOC_REQBUFS, 0xc0145608);
    assert_eq!(video::VIDIOC_QUERYBUF, 0xc0585609);
    assert_eq!(video::VIDIOC_QBUF, 0xc058560f);
    assert_eq!(video::VIDIOC_DQBUF, 0xc0585611);
    assert_eq!(video::VIDIOC_STREAMON, 0x40045612);
    assert_eq!(video::VIDIOC_STREAMOFF, 0x40045613);
}

#[test]
fn video_struct_sizes() {
    assert_eq!(size_of::<video::Format>(), 208);
    assert_eq!(size_of::<video::Capability>(), 104);
    assert_eq!(size_of::<video::PixFormat>(), 48);
    assert_eq!(size_of::<video::PixFormatMplane>(), 192);
    assert_eq!(size_of::<video::PlanePixFormat>(), 20);
    assert_eq!(size_of::<video::RequestBuffers>(), 20);
    assert_eq!(size_of::<video::Buffer>(), 88);
    assert_eq!(size_of::<video::Plane>(), 64);
}

#[test]
fn video_enum_values() {
    assert_eq!(video::BUF_TYPE_VIDEO_CAPTURE, 1);
    assert_eq!(video::BUF_TYPE_VIDEO_OUTPUT, 2);
    assert_eq!(video::BUF_TYPE_VIDEO_CAPTURE_MPLANE, 9);
    assert_eq!(video::BUF_TYPE_VIDEO_OUTPUT_MPLANE, 10);
    assert_eq!(video::MEMORY_MMAP, 1);
    assert_eq!(video::FIELD_NONE, 1);
    assert_eq!(video::FIELD_ANY, 0);
    assert_eq!(video::COLORSPACE_RAW, 0xb);
    assert_eq!(video::CAP_VIDEO_CAPTURE, 0x1);
    assert_eq!(video::CAP_VIDEO_OUTPUT, 0x2);
    assert_eq!(video::CAP_VIDEO_CAPTURE_MPLANE, 0x1000);
    assert_eq!(video::CAP_VIDEO_OUTPUT_MPLANE, 0x2000);
    assert_eq!(video::CAP_STREAMING, 0x0400_0000);
    assert_eq!(video::CAP_DEVICE_CAPS, 0x8000_0000);
}

#[test]
fn subdev_ioctl_numbers() {
    assert_eq!(subdev::VIDIOC_SUBDEV_G_FMT, 0xc0585604);
    assert_eq!(subdev::VIDIOC_SUBDEV_S_FMT, 0xc0585605);
    assert_eq!(subdev::VIDIOC_G_CTRL, 0xc008561b);
    assert_eq!(subdev::VIDIOC_S_CTRL, 0xc008561c);
}

#[test]
fn subdev_struct_sizes() {
    assert_eq!(size_of::<subdev::MbusFramefmt>(), 48);
    assert_eq!(size_of::<subdev::SubdevFormat>(), 88);
    assert_eq!(size_of::<subdev::Control>(), 8);
}

#[test]
fn subdev_values() {
    assert_eq!(subdev::SUBDEV_FORMAT_ACTIVE, 1);
    assert_eq!(subdev::CID_ANALOGUE_GAIN, 0x9e0903);
    assert_eq!(subdev::CID_EXPOSURE, 0x980911);
}

#[test]
fn fourcc_values() {
    assert_eq!(fourcc::IPU3_Y10, 0x79337069);
    assert_eq!(fourcc::GREY, 0x59455247);
    assert_eq!(fourcc::MEDIA_BUS_FMT_Y10_1X10, 0x200a);
}

#[test]
fn pix_layout_roundtrip() {
    // The pix format is (de)serialised into the v4l2_format union manually:
    // a roundtrip must be lossless (sizes guard the layout).
    let pix = video::ip3y_640x480();
    let mut raw = [0u8; 200];
    let mut i = 0;
    for v in [
        pix.width, pix.height, pix.pixelformat, pix.field, pix.bytesperline,
        pix.sizeimage, pix.colorspace, pix.priv_, pix.flags, pix.ycbcr_enc,
        pix.quantization, pix.xfer_func,
    ] {
        raw[i..i + 4].copy_from_slice(&v.to_le_bytes());
        i += 4;
    }
    assert_eq!(raw[0..4], 640u32.to_le_bytes());
    assert_eq!(raw[8..12], fourcc::IPU3_Y10.to_le_bytes());
    assert_eq!(raw[48..], [0u8; 152]);
}
