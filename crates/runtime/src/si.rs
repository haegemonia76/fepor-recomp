//! Serial Interface (0xCC006400): up to four GameCube controllers, ports 1 to 4.
//!
//! The SDK probes the port with a transfer (command 0x00/0xFF returns the device ID, 0x40 the pad
//! status), then enables automatic polling; the SI hardware then refreshes `SIC0INBUF` every few
//! scanlines and raises the read-status interrupt. Here polling happens once per retrace.
//!
//! Port 1 always has a controller (the keyboard, at least); ports 2 to 4 answer only while the front end
//! reports a pad there ([`set_port_connected`]). A port that stops answering while polled raises the
//! no-response flag like an unplugged controller, so the SDK notices a pad being removed or added.
//!
//! Input comes from [`PadState`]. Until a front end feeds it, `FEPOR_PAD=<button>@<frame>+<frames>[,...]`
//! scripts presses by retrace count (buttons: A B X Y START Z L R UP DOWN LEFT RIGHT), e.g.
//! `FEPOR_PAD=A@300+6`.

/// Controller ports of the console.
pub const PORTS: usize = 4;
/// Register distance between two channels' output/input buffers.
const CHANNEL_STRIDE: u32 = 0x0C;
const C0_IN_H: u32 = 0x04;
const SIPOLL: u32 = 0x30;
const SICOMCSR: u32 = 0x34;
const SISR: u32 = 0x38;
const SIBUF: u32 = 0x80;

const CSR_TSTART: u32 = 1;
const CSR_RDSTINTMSK: u32 = 1 << 27;
const CSR_RDSTINT: u32 = 1 << 28;
const CSR_COMERR: u32 = 1 << 29;
const CSR_TCINTMSK: u32 = 1 << 30;
const CSR_TCINT: u32 = 1 << 31;

const SR_WR: u32 = 1 << 31;
/// Per-channel status bits in SISR (channel 0 in the top byte).
const SR_RDST0: u32 = 1 << 29;
const SR_NOREP0: u32 = 1 << 27;

const POLL_EN0: u32 = 1 << 7;

/// Button bits as in the SDK's `PAD_BUTTON_*`.
pub mod button {
    pub const LEFT: u16 = 0x0001;
    pub const RIGHT: u16 = 0x0002;
    pub const DOWN: u16 = 0x0004;
    pub const UP: u16 = 0x0008;
    pub const Z: u16 = 0x0010;
    pub const R: u16 = 0x0020;
    pub const L: u16 = 0x0040;
    pub const A: u16 = 0x0100;
    pub const B: u16 = 0x0200;
    pub const X: u16 = 0x0400;
    pub const Y: u16 = 0x0800;
    pub const START: u16 = 0x1000;
}

/// What the controller reports.
#[derive(Clone, Copy, Debug)]
pub struct PadState {
    pub buttons: u16,
    pub stick: (u8, u8),
    pub c_stick: (u8, u8),
    pub triggers: (u8, u8),
}

impl Default for PadState {
    fn default() -> Self {
        PadState {
            buttons: 0,
            stick: (128, 128),
            c_stick: (128, 128),
            triggers: (0, 0),
        }
    }
}

impl PadState {
    /// The two status words the hardware reads from a polled controller.
    fn words(&self) -> (u32, u32) {
        let b = self.buttons;
        let hi = (((b >> 8) as u32) << 24)
            | ((0x80 | (b & 0x7F) as u32) << 16)
            | ((self.stick.0 as u32) << 8)
            | self.stick.1 as u32;
        let lo = ((self.c_stick.0 as u32) << 24)
            | ((self.c_stick.1 as u32) << 16)
            | ((self.triggers.0 as u32) << 8)
            | self.triggers.1 as u32;
        (hi, lo)
    }
}

/// The controller state set by the front end (window or gamepad thread) for each port, packed so it can be
/// shared with the guest thread without locking: buttons, stick x/y, c-stick x/y, triggers L/R.
static LIVE_PADS: [std::sync::atomic::AtomicU64; PORTS] =
    [const { std::sync::atomic::AtomicU64::new(0) }; PORTS];
/// Bit `n`: a controller answers on port `n`. Port 1 always does.
static CONNECTED: std::sync::atomic::AtomicU8 = std::sync::atomic::AtomicU8::new(1);

/// Publish the controller state the guest sees on `port` (0-based) from the next retrace on.
pub fn set_pad(port: usize, p: PadState) {
    let v = p.buttons as u64
        | (p.stick.0 as u64) << 16
        | (p.stick.1 as u64) << 24
        | (p.c_stick.0 as u64) << 32
        | (p.c_stick.1 as u64) << 40
        | (p.triggers.0 as u64) << 48
        | (p.triggers.1 as u64) << 56;
    LIVE_PADS[port].store(v, std::sync::atomic::Ordering::Relaxed);
}

/// Plug or unplug the controller of `port` (0-based; port 1 cannot be unplugged).
pub fn set_port_connected(port: usize, connected: bool) {
    let bit = 1u8 << port;
    if connected || port == 0 {
        CONNECTED.fetch_or(bit, std::sync::atomic::Ordering::Relaxed);
    } else {
        CONNECTED.fetch_and(!bit, std::sync::atomic::Ordering::Relaxed);
    }
}

fn port_connected(port: usize) -> bool {
    CONNECTED.load(std::sync::atomic::Ordering::Relaxed) & (1 << port) != 0
}

fn live_pad(port: usize) -> PadState {
    let v = LIVE_PADS[port].load(std::sync::atomic::Ordering::Relaxed);
    if v == 0 {
        return PadState::default();
    }
    PadState {
        buttons: v as u16,
        stick: ((v >> 16) as u8, (v >> 24) as u8),
        c_stick: ((v >> 32) as u8, (v >> 40) as u8),
        triggers: ((v >> 48) as u8, (v >> 56) as u8),
    }
}

struct Press {
    buttons: u16,
    from: u64,
    frames: u64,
}

pub struct Si {
    regs: [u32; 0x20],
    buf: [u8; 128],
    poll: u32,
    csr: u32,
    sr: u32,
    pub pads: [PadState; PORTS],
    script: Vec<Press>,
}

fn parse_script(spec: &str) -> Vec<Press> {
    let mut out = Vec::new();
    for item in spec.split(',').filter(|s| !s.is_empty()) {
        let (names, when) = item.split_once('@').unwrap_or((item, "0+6"));
        let (from, frames) = when.split_once('+').unwrap_or((when, "6"));
        let mut buttons = 0;
        for n in names.split('|') {
            buttons |= match n.trim().to_ascii_uppercase().as_str() {
                "A" => button::A,
                "B" => button::B,
                "X" => button::X,
                "Y" => button::Y,
                "START" => button::START,
                "Z" => button::Z,
                "L" => button::L,
                "R" => button::R,
                "UP" => button::UP,
                "DOWN" => button::DOWN,
                "LEFT" => button::LEFT,
                "RIGHT" => button::RIGHT,
                other => {
                    eprintln!("fepor: FEPOR_PAD: unknown button {other:?}");
                    0
                }
            };
        }
        out.push(Press {
            buttons,
            from: from.parse().unwrap_or(0),
            frames: frames.parse().unwrap_or(6),
        });
    }
    out
}

impl Si {
    pub fn new() -> Si {
        let script = std::env::var("FEPOR_PAD")
            .map(|s| parse_script(&s))
            .unwrap_or_default();
        Si {
            regs: [0; 0x20],
            buf: [0; 128],
            poll: 0,
            csr: 0,
            sr: 0,
            pads: [PadState::default(); PORTS],
            script,
        }
    }

    /// One retrace: apply scripted input and refresh every polled channel.
    pub fn retrace(&mut self, frame: u64) {
        for (port, pad) in self.pads.iter_mut().enumerate() {
            *pad = live_pad(port);
        }
        if !self.script.is_empty() {
            self.pads[0].buttons |= self
                .script
                .iter()
                .filter(|p| frame >= p.from && frame < p.from + p.frames)
                .fold(0, |a, p| a | p.buttons);
        }
        for port in 0..PORTS {
            if self.poll & (POLL_EN0 >> port) == 0 {
                continue;
            }
            if port_connected(port) {
                let (hi, lo) = self.pads[port].words();
                let at = (C0_IN_H + CHANNEL_STRIDE * port as u32) / 4;
                self.regs[at as usize] = hi;
                self.regs[at as usize + 1] = lo;
                self.sr |= SR_RDST0 >> (8 * port);
            } else {
                self.sr |= SR_NOREP0 >> (8 * port);
            }
        }
    }

    fn refresh_csr(&mut self) {
        if self.sr & (SR_RDST0 | SR_RDST0 >> 8 | SR_RDST0 >> 16 | SR_RDST0 >> 24) != 0 {
            self.csr |= CSR_RDSTINT;
        } else {
            self.csr &= !CSR_RDSTINT;
        }
    }

    pub fn interrupt_pending(&self) -> bool {
        (self.csr & CSR_TCINT != 0 && self.csr & CSR_TCINTMSK != 0)
            || (self.csr & CSR_RDSTINT != 0 && self.csr & CSR_RDSTINTMSK != 0)
    }

    pub fn read(&mut self, off: u32, size: u32) -> u32 {
        let v = match off & !3 {
            SIPOLL => self.poll,
            SICOMCSR => self.csr,
            SISR => self.sr,
            o if o >= SIBUF => {
                u32::from_be_bytes(self.buf[(o - SIBUF) as usize..][..4].try_into().unwrap())
            }
            o => self.regs[(o / 4) as usize & 0x1F],
        };
        if let Some(port) = (0..PORTS).find(|p| off & !3 == C0_IN_H + CHANNEL_STRIDE * *p as u32) {
            // reading the status word acknowledges the poll
            self.sr &= !(SR_RDST0 >> (8 * port));
            self.refresh_csr();
        }
        match size {
            4 => v,
            2 => (v >> (16 - 8 * (off & 2))) & 0xFFFF,
            _ => (v >> (24 - 8 * (off & 3))) & 0xFF,
        }
    }

    pub fn write(&mut self, off: u32, value: u32, size: u32) {
        if size != 4 {
            // the SDK only uses word accesses on SI
            return;
        }
        match off {
            SIPOLL => self.poll = value,
            SICOMCSR => {
                let keep = self.csr & (CSR_RDSTINT | CSR_COMERR | CSR_TCINT);
                let cleared = keep & !(value & (CSR_COMERR | CSR_TCINT));
                self.csr = (value & !(CSR_RDSTINT | CSR_COMERR | CSR_TCINT | CSR_TSTART)) | cleared;
                if value & CSR_TSTART != 0 {
                    self.transfer(value);
                }
                self.refresh_csr();
            }
            SISR => {
                // error bits clear when written with 1; WR is consumed immediately
                self.sr &= !(value & 0x0F0F_0F0F);
                self.sr &= !(value & SR_WR);
            }
            o if o >= SIBUF => {
                self.buf[(o - SIBUF) as usize..][..4].copy_from_slice(&value.to_be_bytes())
            }
            o => self.regs[(o / 4) as usize & 0x1F] = value,
        }
    }

    /// A one-shot transfer on the channel in `csr`.
    fn transfer(&mut self, csr: u32) {
        let channel = (csr >> 1) & 3;
        let in_len = match (csr >> 8) & 0x7F {
            0 => 128,
            n => n as usize,
        };
        if !port_connected(channel as usize) {
            self.sr |= SR_NOREP0 >> (8 * channel);
            self.csr |= CSR_COMERR;
        } else {
            let cmd = self.buf[0];
            let mut reply = [0u8; 8];
            let n = match cmd {
                0x00 | 0xFF => {
                    reply[..4].copy_from_slice(&0x0900_0000u32.to_be_bytes());
                    4
                }
                0x40 | 0x42 => {
                    let (hi, lo) = self.pads[channel as usize].words();
                    reply[..4].copy_from_slice(&hi.to_be_bytes());
                    reply[4..].copy_from_slice(&lo.to_be_bytes());
                    8
                }
                0x41 => {
                    // origin: neutral sticks
                    let (hi, lo) = PadState::default().words();
                    reply[..4].copy_from_slice(&(hi & 0x00FF_FFFF).to_be_bytes());
                    reply[4..].copy_from_slice(&lo.to_be_bytes());
                    8
                }
                _ => 0,
            };
            let n = n.min(in_len);
            self.buf[..n].copy_from_slice(&reply[..n]);
        }
        self.csr |= CSR_TCINT;
    }
}

impl Default for Si {
    fn default() -> Self {
        Si::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Port 2 answers only while the front end reports a pad there, and polling it shows its own state.
    #[test]
    fn extra_ports_answer_only_while_a_pad_is_plugged_in() {
        let mut si = Si::new();
        // the SDK's probe of channel 1 (command 0x00, 3 bytes out would be 1 byte in; length is irrelevant here)
        let probe = |si: &mut Si| {
            si.buf[0] = 0x00;
            si.sr = 0;
            si.transfer((1 << 1) | (4 << 8) | 1);
            si.sr & (SR_NOREP0 >> 8) != 0
        };
        set_port_connected(1, false);
        assert!(probe(&mut si), "no pad: no response");
        set_port_connected(1, true);
        assert!(!probe(&mut si));
        assert_eq!(&si.buf[..4], &0x0900_0000u32.to_be_bytes());

        // polled, the channel carries the pad published for the port, and nothing from port 1's
        set_pad(
            1,
            PadState {
                buttons: button::B,
                ..PadState::default()
            },
        );
        si.poll = POLL_EN0 >> 1;
        si.sr = 0;
        si.retrace(0);
        let at = ((C0_IN_H + CHANNEL_STRIDE) / 4) as usize;
        assert_eq!((si.regs[at] >> 16) & 0xFF, 0x80, "low button byte");
        assert_eq!(si.regs[at] >> 24, (button::B >> 8) as u32);
        assert_ne!(si.sr & (SR_RDST0 >> 8), 0);

        // unplugged while polled: the no-response flag tells the SDK
        set_port_connected(1, false);
        si.sr = 0;
        si.retrace(1);
        assert_ne!(si.sr & (SR_NOREP0 >> 8), 0);
        assert_eq!(si.sr & (SR_RDST0 >> 8), 0);
        set_pad(1, PadState::default());
    }
}
