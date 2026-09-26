//! Register decoders, ported from `lib/tps68470.py` (same bit names, same
//! output strings where the Python formats them).

/// Decode GSTAT. Empty = clean.
pub fn gstat(v: u8) -> Vec<&'static str> {
    let mut bits = Vec::new();
    if v & 0x80 != 0 {
        bits.push("SHORT_FLAG");
    }
    if v & 0x40 != 0 {
        bits.push("PWR_FLAG");
    }
    if v & 0x20 != 0 {
        bits.push("ILEDF");
    }
    if v & 0x10 != 0 {
        bits.push("WLEDF");
    }
    if v & 0x08 != 0 {
        bits.push("OVP");
    }
    if v & 0x04 != 0 {
        bits.push("UVLO");
    }
    if v & 0x02 != 0 {
        bits.push("TSD_FLAG");
    }
    if v & 0x01 != 0 {
        bits.push("WAKE");
    }
    bits
}

/// Decode WLEDSTAT. Empty = clean.
pub fn status(v: u8) -> Vec<&'static str> {
    let mut bits = Vec::new();
    if v & 0x80 != 0 {
        bits.push("S_STROBE-high");
    }
    if v & 0x40 != 0 {
        bits.push("TSD-THERMAL-SHUTDOWN");
    }
    if v & 0x20 != 0 {
        bits.push("LEDF-LED-SHORT/FAULT");
    }
    if v & 0x10 != 0 {
        bits.push("TO-timeout");
    }
    if v & 0x04 != 0 {
        bits.push("LEDHOT");
    }
    if v & 0x02 != 0 {
        bits.push("LEDWARN");
    }
    bits
}

/// Split WLEDCTL into its fields. `start` (bit 5) is read-only status — LED
/// current actually flowing, not a write confirmation.
pub fn ctl(v: u8) -> (u8, bool, bool, bool, bool) {
    (
        v & 0x03,          // MODE[1:0]
        v & 0x04 != 0,     // EN
        v & 0x08 != 0,     // DISLED1
        v & 0x10 != 0,     // DISLED2
        v & 0x20 != 0,     // START (read-only)
    )
}

pub fn ctl_mode_name(mode: u8) -> &'static str {
    ["flash", "torch", "red-eye", "focus-assist"][mode as usize & 0x03]
}

/// Format ILEDCTL like `lib/tps68470.py` does. FAILA/FAILB are read-only
/// failure-mode bits (0=Open, 1=Shorted) and only mean anything while
/// GSTAT.ILEDF is set.
pub fn iled(v: u8) -> String {
    let ma = [2, 4, 8, 16][((v >> 4) & 0x03) as usize];
    format!(
        "ENA={} FAILA={}  ENB={} B_current={}mA  FAILB={}",
        (v >> 2) & 1,
        if v & 0x08 != 0 { "shorted" } else { "open" },
        (v >> 6) & 1,
        ma,
        if v & 0x80 != 0 { "shorted" } else { "open" },
    )
}

pub fn join(bits: &[&'static str]) -> String {
    if bits.is_empty() {
        "clean".to_string()
    } else {
        bits.join(", ")
    }
}
