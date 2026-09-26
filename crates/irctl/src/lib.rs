//! V4L2 + media-controller ioctls: graph, capture, loopback sink.
//!
//! Stub in slice 013 of `ir-face-root-cause` (foundation only). The typed
//! ioctl wrappers land with the feed loop in slice 004; the crate exists now
//! so the runtime verbs (`feed`, `doctor`, `up`) link **one** implementation
//! instead of each wrapping the ioctls again.
//!
//! Expect ~200 lines of hand-written wrappers when this fills in — the IPU3
//! raw-CIO2 + media-controller path is not what the generic `v4l` crates
//! target (see `decision-cement-ir-rust-runtime-core`, accepted cost 3).
