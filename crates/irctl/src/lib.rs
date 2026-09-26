//! V4L2 + media-controller ioctls: graph, capture, loopback sink.
//!
//! The typed wrappers the Rust decision (`decision-cement-ir-rust-runtime-core`,
//! accepted cost 3) said this path would need: the IPU3 raw-CIO2 +
//! media-controller API is not what the generic `v4l` crates target. Slice 006
//! (`irfaced-service`) is where they land — this crate fills in the stub 013
//! stood up.
//!
//! Every ABI constant and struct layout here was **ground-truthed against this
//! kernel's own UAPI headers** (linux `6.19.8.arch1-3-surface`) by compiling a
//! C probe against `/usr/include/linux/{media,videodev2,v4l2-subdev,
//! v4l2-mediabus}.h`; the numbers are pinned in `tests/abi_constants.rs`.
//! Notable traps that probe caught: `media_entity_desc` is 256 bytes (the
//! `raw[184]` union tail), `media_links_enum` carries **user pointers** (40
//! bytes), `v4l2_format`'s union is 200 bytes at offset 8, and
//! `V4L2_CID_ANALOGUE_GAIN` is `0x9e0903`.
//!
//! No `unsafe` escapes the boundary of this crate except the two that are the
//! point of it: the ioctl syscalls themselves, and the mmap of kernel video
//! buffers in `video::Stream` (which `munmap`s on `Drop`).

pub mod fourcc;
pub mod media;
pub mod subdev;
pub mod video;
