//! Raw i2c-dev access to the TPS68470, mirroring `lib/tps68470.py`.
//!
//! The Python opens a fresh fd per get/put; a `Bus` here holds one fd for its
//! lifetime — same protocol (write the register address, read/write the data
//! byte on the force-addressed fd), fewer syscalls per capture.

use std::fs::OpenOptions;
use std::io;
use std::os::fd::{AsRawFd, RawFd};

pub const BUS: &str = "/dev/i2c-2";
/// Owned by the `int3472` driver — forcing is required, and safe: its regmap
/// is `REGCACHE_NONE`, so direct writes cannot desync a driver cache.
pub const ADDR: u8 = 0x4D;
pub const I2C_SLAVE_FORCE: libc::c_ulong = 0x0706;

pub struct Bus {
    fd: RawFd,
}

impl Bus {
    pub fn open() -> io::Result<Bus> {
        let f = OpenOptions::new().read(true).write(true).open(BUS)?;
        let fd = f.as_raw_fd();
        let rc = unsafe { libc::ioctl(fd, I2C_SLAVE_FORCE, ADDR as libc::c_ulong) };
        if rc != 0 {
            return Err(io::Error::last_os_error());
        }
        std::mem::forget(f); // the Bus owns the fd from here
        Ok(Bus { fd })
    }

    /// i2c-dev convention: write the register address, read one data byte.
    pub fn get(&self, reg: u8) -> io::Result<u8> {
        let reg = [reg];
        let n = unsafe { libc::write(self.fd, reg.as_ptr() as *const libc::c_void, 1) };
        if n != 1 {
            return Err(io::Error::last_os_error());
        }
        let mut b = [0u8; 1];
        let n = unsafe { libc::read(self.fd, b.as_mut_ptr() as *mut libc::c_void, 1) };
        if n != 1 {
            return Err(io::Error::last_os_error());
        }
        Ok(b[0])
    }

    pub fn put(&self, reg: u8, val: u8) -> io::Result<()> {
        let buf = [reg, val];
        let n = unsafe { libc::write(self.fd, buf.as_ptr() as *const libc::c_void, 2) };
        if n != 2 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        unsafe {
            libc::close(self.fd);
        }
    }
}
