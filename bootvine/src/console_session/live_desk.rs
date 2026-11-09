use std::{future::ready, io, mem, ops::ControlFlow, path::PathBuf, time::Duration};

use crossterm::event::Event;
use futures::{Stream, StreamExt};

use crate::{
    boot_model::BootChange,
    boot_view::PasswordPrompt,
    console_session::{
        ask_password::{self, PasswordEntry, RequestWatch},
        console_palette::ConsoleLook,
        console_takeover::{ForegroundVt, QuietKernelConsole, console_look, vt_of_stdin},
        passphrase_input::asks_to_quit,
        plymouth::Plymouth,
        resume_console::{RESUME_CONSOLE_MARKER, resume_console_is_open},
        stream_sources::items_when_readable,
        watch_screen::{ConsoleDesk, Refresh, ScreenInput, WatchEnding},
    },
};

pub struct LiveDesk {
    plymouth: Option<Plymouth>,
    quiet_kernel: Option<QuietKernelConsole>,
    foreground: Option<ForegroundVt>,
    for_system: bool,
    own_vt: Option<u16>,
    look: ConsoleLook,
    entry: Option<PasswordEntry>,
    handover_deadline: Option<Duration>,
    resume_marker: PathBuf,
    console_was_lent: bool,
}

impl LiveDesk {
    #[must_use]
    pub fn take_console(for_system: bool) -> Self {
        Self::take_console_with_plymouth(for_system, false)
    }

    pub fn take_console_with_plymouth(for_system: bool, standby: bool) -> Self {
        let plymouth = if for_system && standby && super::plymouth::command("--ping") {
            match Plymouth::open() {
                Ok(control) => {
                    eprintln!("bootvine: Plymouth visible; collecting the tree in the background");
                    Some(control)
                }
                Err(error) => {
                    eprintln!("bootvine: Plymouth handoff unavailable: {error}");
                    None
                }
            }
        } else {
            None
        };
        let hidden = plymouth.is_some();
        let prompt_elsewhere = for_system && ask_password::requested();
        if prompt_elsewhere {
            eprintln!("bootvine: a password prompt is already showing, staying behind it");
        }
        let own_vt = vt_of_stdin();
        Self {
            plymouth,
            quiet_kernel: (for_system && !hidden)
                .then(QuietKernelConsole::take)
                .flatten(),
            foreground: own_vt
                .filter(|_| !prompt_elsewhere && !hidden)
                .and_then(ForegroundVt::take),
            for_system,
            own_vt,
            look: console_look(own_vt),
            entry: None,
            handover_deadline: None,
            resume_marker: PathBuf::from(RESUME_CONSOLE_MARKER),
            console_was_lent: false,
        }
    }

    fn come_forward_once_prompts_are_gone(&mut self) -> bool {
        if self.plymouth.is_some()
            || !self.for_system
            || self.foreground.is_some()
            || ask_password::requested()
        {
            return false;
        }
        self.foreground = self.own_vt.and_then(ForegroundVt::take);
        if self.foreground.is_some() {
            eprintln!("bootvine: password prompt answered, taking the console back");
        }
        self.foreground.is_some()
    }

    fn follow_requests(&mut self, requests_changed: bool) -> Refresh {
        let came_forward = requests_changed && self.come_forward_once_prompts_are_gone();
        self.entry = if self.for_system && self.foreground.is_some() {
            PasswordEntry::follow(self.entry.take(), requests_changed)
        } else {
            None
        };
        if came_forward {
            Refresh::Clear
        } else {
            Refresh::Steady
        }
    }
}

impl ConsoleDesk for LiveDesk {
    fn look(&self) -> ConsoleLook {
        self.look
    }

    fn prompt(&self, _now: Duration) -> Option<PasswordPrompt<'_>> {
        self.entry.as_ref().map(PasswordEntry::prompt)
    }

    fn press_key(&mut self, event: &Event, _now: Duration) -> ControlFlow<WatchEnding> {
        if self.plymouth.as_ref().is_some_and(|p| p.visible)
            && matches!(event, Event::Key(key) if key.code == crossterm::event::KeyCode::Esc)
        {
            if self.plymouth.as_mut().is_some_and(Plymouth::hide) {
                eprintln!("bootvine: returning to Plymouth");
                self.entry = None;
                if let Some(foreground) = self.foreground.take() {
                    foreground.restore_previous();
                }
                self.quiet_kernel = None;
            }
            return ControlFlow::Continue(());
        }
        let Some(entry) = &mut self.entry else {
            return if asks_to_quit(event, false) {
                ControlFlow::Break(WatchEnding::ViewerLeft)
            } else {
                ControlFlow::Continue(())
            };
        };
        if let Err(error) = entry.type_key(event) {
            eprintln!("bootvine: cannot answer the password prompt: {error}");
        }
        ControlFlow::Continue(())
    }

    fn begin_handover(&mut self, reached: &str, deadline: Duration) {
        if self.handover_deadline.is_some() {
            return;
        }
        eprintln!("bootvine: {reached} reached, handing the console over");
        self.handover_deadline = Some(deadline);
    }

    fn frame_passed(&mut self, now: Duration) -> ControlFlow<WatchEnding, Vec<BootChange>> {
        let Some(deadline) = self.handover_deadline else {
            return ControlFlow::Continue(Vec::new());
        };
        if self
            .foreground
            .as_ref()
            .is_some_and(ForegroundVt::taken_away)
        {
            eprintln!("bootvine: another program took the console, leaving it there");
            if let Some(foreground) = self.foreground.take() {
                foreground.stay_switched();
                if let Some(plymouth) = &mut self.plymouth {
                    plymouth.visible = false;
                }
            }
            ControlFlow::Break(WatchEnding::BootCompleted)
        } else if now >= deadline {
            eprintln!("bootvine: nothing took the console, switching back");
            ControlFlow::Break(WatchEnding::BootCompleted)
        } else {
            ControlFlow::Continue(Vec::new())
        }
    }

    fn password_requests_changed(&mut self) -> Refresh {
        self.follow_requests(true)
    }

    fn check_prompts(&mut self) {
        self.follow_requests(false);
    }

    fn lends_console(&mut self) -> bool {
        let lent = self.plymouth.as_ref().is_some_and(|p| !p.visible)
            || resume_console_is_open(&self.resume_marker);
        self.console_was_lent |= lent;
        lent
    }

    fn keep_console_up(&mut self) -> Refresh {
        if let Some(plymouth) = &mut self.plymouth {
            if !plymouth.visible && plymouth.requested() && plymouth.show() {
                self.quiet_kernel = QuietKernelConsole::take();
                self.foreground = self.own_vt.and_then(ForegroundVt::take);
                eprintln!(
                    "bootvine: showing verbose tree on {:?}, activated={}",
                    self.own_vt,
                    self.foreground.is_some()
                );
                if self.foreground.is_none() {
                    let _ = plymouth.hide();
                }
                self.follow_requests(true);
                return Refresh::Clear;
            }
        }
        if self.lends_console() {
            return Refresh::Steady;
        }
        let console_returned = mem::take(&mut self.console_was_lent);
        let kernel_got_loud = self
            .quiet_kernel
            .as_mut()
            .is_some_and(QuietKernelConsole::keep_quiet);
        let terminal_was_taken = self.handover_deadline.is_none()
            && self.foreground.as_ref().is_some_and(ForegroundVt::insist);
        let look = console_look(self.own_vt);
        let restyled = look != self.look;
        self.look = look;
        if console_returned || kernel_got_loud || terminal_was_taken || restyled {
            Refresh::Repaint
        } else {
            Refresh::Steady
        }
    }
}

pub fn password_request_inputs(watch: RequestWatch) -> io::Result<impl Stream<Item = ScreenInput>> {
    Ok(items_when_readable(watch, RequestWatch::changed)?
        .filter(|changed| ready(*changed))
        .map(|_| ScreenInput::PasswordRequests))
}

#[cfg(test)]
mod tests {
    use std::{env, fs, process};

    use super::*;

    fn desk_watching(resume_marker: PathBuf) -> LiveDesk {
        LiveDesk {
            plymouth: None,
            quiet_kernel: None,
            foreground: None,
            for_system: true,
            own_vt: None,
            look: console_look(None),
            entry: None,
            handover_deadline: None,
            resume_marker,
            console_was_lent: false,
        }
    }

    #[test]
    fn the_console_is_repainted_only_after_it_was_lent_for_resume() {
        let marker = env::temp_dir().join(format!("bootvine-resume-marker-{}", process::id()));
        let mut desk = desk_watching(marker.clone());
        assert!(!desk.lends_console());

        fs::write(&marker, "1").expect("create the marker");
        assert!(desk.lends_console());
        assert_eq!(desk.keep_console_up(), Refresh::Steady);

        fs::remove_file(&marker).expect("remove the marker");
        assert!(!desk.lends_console());
        assert_eq!(desk.keep_console_up(), Refresh::Repaint);
        assert_eq!(desk.keep_console_up(), Refresh::Steady);
    }
}
