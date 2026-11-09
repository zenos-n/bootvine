use std::{collections::HashSet, fs, mem, ptr};

use rustix::{
    io::Errno,
    ioctl::{Getter, IntegerSetter, Opcode, Updater, ioctl},
    stdio::stdin,
    termios::ttyname,
};

use crate::{
    console_session::{
        ask_password,
        console_palette::{ConsoleLook, ConsolePalette},
    },
    screen_settings::font_glyphs::Glyphs,
};

const GIO_UNIMAP: Opcode = 0x4B66;
const VT_GETSTATE: Opcode = 0x5603;
const VT_ACTIVATE: Opcode = 0x5606;
const VIRTUAL_TERMINAL_PREFIX: &str = "/dev/tty";
const KERNEL_PRINTK: &str = "/proc/sys/kernel/printk";
const ALERTS_ONLY: &str = "1";
const HIGHEST_LOW_GLYPH: u16 = 255;

pub(super) struct QuietKernelConsole {
    previous_level: String,
}

fn console_level() -> Option<String> {
    let levels = fs::read_to_string(KERNEL_PRINTK).ok()?;
    levels.split_whitespace().next().map(ToOwned::to_owned)
}

impl QuietKernelConsole {
    pub(super) fn take() -> Option<Self> {
        let previous_level = console_level()?;
        fs::write(KERNEL_PRINTK, ALERTS_ONLY).ok()?;
        Some(Self { previous_level })
    }

    pub(super) fn keep_quiet(&mut self) -> bool {
        let Some(level) = console_level() else {
            return false;
        };
        if level == ALERTS_ONLY || fs::write(KERNEL_PRINTK, ALERTS_ONLY).is_err() {
            return false;
        }
        self.previous_level = level;
        true
    }
}

struct LoadedFont {
    characters: HashSet<char>,
    uses_high_glyphs: bool,
}

pub(super) fn console_look(own_vt: Option<u16>) -> ConsoleLook {
    let font = loaded_font();
    let glyphs = font.as_ref().map_or_else(Glyphs::default, |font| {
        Glyphs::fitting(|symbol| symbol.is_ascii() || font.characters.contains(&symbol))
    });
    let palette = match own_vt {
        None => ConsolePalette::Terminal,
        Some(_) if font.as_ref().is_some_and(|font| font.uses_high_glyphs) => {
            ConsolePalette::EightColorVt
        }
        Some(_) => ConsolePalette::SixteenColorVt,
    };
    ConsoleLook { glyphs, palette }
}

impl Drop for QuietKernelConsole {
    fn drop(&mut self) {
        let _ = fs::write(KERNEL_PRINTK, &self.previous_level);
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
struct UnicodePair {
    unicode: u16,
    font_position: u16,
}

#[repr(C)]
struct UnicodeMap {
    count: u16,
    pairs: *mut UnicodePair,
}

fn read_unicode_map(map: &mut UnicodeMap) -> Result<(), Errno> {
    unsafe { ioctl(stdin(), Updater::<GIO_UNIMAP, UnicodeMap>::new(map)) }
}

fn loaded_font() -> Option<LoadedFont> {
    let mut probe = UnicodeMap {
        count: 0,
        pairs: ptr::null_mut(),
    };
    let _ = read_unicode_map(&mut probe);
    if probe.count == 0 {
        return None;
    }
    let mut pairs = vec![UnicodePair::default(); usize::from(probe.count)];
    let mut map = UnicodeMap {
        count: probe.count,
        pairs: pairs.as_mut_ptr(),
    };
    read_unicode_map(&mut map).ok()?;
    let mapped = &pairs[..usize::from(map.count).min(pairs.len())];
    Some(LoadedFont {
        characters: mapped
            .iter()
            .filter_map(|pair| char::from_u32(u32::from(pair.unicode)))
            .collect(),
        uses_high_glyphs: mapped
            .iter()
            .any(|pair| pair.font_position > HIGHEST_LOW_GLYPH),
    })
}

#[repr(C)]
struct VtState {
    active: u16,
    _signal: u16,
    _state: u16,
}

pub(super) struct ForegroundVt {
    own: u16,
    previous: u16,
}

fn active_vt() -> Option<u16> {
    let state: VtState = unsafe { ioctl(stdin(), Getter::<VT_GETSTATE, VtState>::new()) }.ok()?;
    Some(state.active)
}

pub(super) fn vt_of_stdin() -> Option<u16> {
    let name = ttyname(stdin(), Vec::new()).ok()?;
    name.to_str()
        .ok()?
        .strip_prefix(VIRTUAL_TERMINAL_PREFIX)?
        .parse()
        .ok()
        .filter(|number| *number > 0)
}

impl ForegroundVt {
    pub(super) fn take(own: u16) -> Option<Self> {
        let previous = active_vt()?;
        if previous != own {
            if let Err(error) = activate(own) {
                eprintln!("bootvine: cannot activate tty{own}: {error}");
                return None;
            }
        }
        Some(Self { own, previous })
    }

    pub(super) fn insist(&self) -> bool {
        self.taken_away() && activate(self.own).is_ok()
    }

    pub(super) fn taken_away(&self) -> bool {
        active_vt().is_some_and(|active| active != self.own)
    }

    pub(super) fn restore_previous(self) {
        let _ = activate(self.previous);
        mem::forget(self);
    }

    pub(super) fn stay_switched(self) {
        mem::forget(self);
    }
}

impl Drop for ForegroundVt {
    fn drop(&mut self) {
        if ask_password::requested() {
            eprintln!("bootvine: leaving a password prompt to the console agent on this terminal");
            return;
        }
        let _ = activate(self.previous);
    }
}

fn activate(number: u16) -> Result<(), Errno> {
    let number = usize::from(number);
    unsafe { ioctl(stdin(), IntegerSetter::<VT_ACTIVATE>::new_usize(number)) }
}
