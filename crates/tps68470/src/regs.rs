//! WLED-block register addresses. Semantics:
//! garden `hardware/notes/005-front-camera-pmic-topology.md`.

/// SHORT_FLAG|PWR_FLAG|ILEDF|WLEDF|OVP|UVLO|TSD_FLAG|WAKE — latched, w1c
pub const GSTAT: u8 = 0x01;
/// FAILB|ENB|CTRLB[1:0]|FAILA|ENA|-|-
pub const ILEDCTL: u8 = 0x28;
/// S_STROBE|TSD|LEDF|TO|-|LEDHOT|LEDWARN|ENTMON
pub const WLEDSTAT: u8 = 0x29;
pub const VWLEDILIM: u8 = 0x2A;
/// MAX_CUR[2:0] — torch ceiling, 32.5 mA steps, max 227.5 mA.
/// Cannot be written while WLED is enabled.
pub const WLEDMAXT: u8 = 0x2D;
pub const WLEDMAXF: u8 = 0x2F;
pub const WLEDTO: u8 = 0x30;
pub const VWLEDCTL: u8 = 0x31;
/// ILED[4:0] — LED1 current, 32.5 mA steps.
pub const WLEDC1: u8 = 0x34;
/// ILED[4:0] — LED2 current, 32.5 mA steps.
pub const WLEDC2: u8 = 0x35;
/// TRIG_POL|TRIG|START|DISLED2|DISLED1|EN|MODE[1:0]
/// (MODE 00=flash 01=torch 10=red-eye 11=focus-assist; bit 5 START is
/// read-only status — LED current actually flowing.)
pub const WLEDCTL: u8 = 0x36;

pub fn name(reg: u8) -> &'static str {
    match reg {
        GSTAT => "GSTAT",
        ILEDCTL => "ILEDCTL",
        WLEDSTAT => "WLEDSTAT",
        VWLEDILIM => "VWLEDILIM",
        WLEDMAXT => "WLEDMAXT",
        WLEDMAXF => "WLEDMAXF",
        WLEDTO => "WLEDTO",
        VWLEDCTL => "VWLEDCTL",
        WLEDC1 => "WLEDC1",
        WLEDC2 => "WLEDC2",
        WLEDCTL => "WLEDCTL",
        _ => "?",
    }
}
