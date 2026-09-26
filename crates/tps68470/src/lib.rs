//! TPS68470 PMIC register access over raw `/dev/i2c-2`, plus decoders and
//! the torch guard.
//!
//! The kernel's `int3472` driver owns address `0x4D`, so writes need
//! `I2C_SLAVE_FORCE`. Its regmap is `REGCACHE_NONE`, so a direct write cannot
//! desync a driver cache. The WLED block accessed here is the high-current
//! flash/torch driver — a different output entirely from the ILEDA/ILEDB
//! *indicator* pins that `leds-tps68470` exposes (those top out at 16 mA;
//! WLED torch goes to 227.5 mA). On this machine the whole WLED block sits at
//! hardware reset, because nothing in Linux has ever programmed it.
//!
//! Register semantics and the wedge this block can cause:
//! garden `hardware/notes/005-front-camera-pmic-topology.md`.

pub mod bus;
pub mod decode;
pub mod regs;
pub mod torch;
