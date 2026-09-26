//! Live diagnostic: dump the v2 topology the kernel reports (entity id, name,
//! function; and every entity→interface→devnode pair, the way media-ctl
//! resolves them). Run as ukv — the logind ACL covers /dev/media0.
//! `cargo test -p irctl --test live_dump -- --ignored --nocapture`
//!
//! Kept because "what does the kernel actually say" is the first question
//! every time the graph surprises us (it caught the legacy-enum devnode
//! mismatch during slice 006 bring-up).

use irctl::media;

#[test]
#[ignore]
fn dump_topology() {
    let f = std::fs::File::open("/dev/media0").expect("open /dev/media0");
    media::check_pad_index_api(&f).expect("pad-index API check");
    let t = media::get_topology(&f).expect("get_topology");
    println!(
        "{} entities, {} interfaces, {} pads, {} links",
        t.entities.len(),
        t.interfaces.len(),
        t.pads.len(),
        t.links.len()
    );
    for e in &t.entities {
        let nodes: Vec<String> = t
            .links
            .iter()
            .filter(|l| {
                (l.source_id == e.id || l.sink_id == e.id)
                    && l.flags & media::LNK_FL_LINK_TYPE_MASK == media::LNK_FL_INTERFACE_LINK
            })
            .filter_map(|l| {
                let iface_id = if l.source_id == e.id { l.sink_id } else { l.source_id };
                t.interfaces.iter().find(|i| i.id == iface_id)
            })
            .filter_map(|i| {
                i.dev.map(|(maj, min)| {
                    let prefix = if i.intf_type == media::INTF_T_V4L_SUBDEV {
                        "v4l-subdev"
                    } else {
                        "video"
                    };
                    format!("{maj}:{prefix}{min}")
                })
            })
            .collect();
        println!(
            "  id={:3} fn=0x{:05x} dev={} name={}",
            e.id,
            e.function,
            nodes.join(","),
            e.name
        );
    }
}
