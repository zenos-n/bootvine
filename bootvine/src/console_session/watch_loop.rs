use std::{path::Path, time::Duration};

use futures::{StreamExt, stream};
use tokio::runtime::Builder;

use crate::{
    boot_model::{BootState, Phase, boot_origin::BootOrigin},
    console_session::{
        ask_password::RequestWatch,
        live_desk::{LiveDesk, password_request_inputs},
        stream_sources::{
            BACKGROUND_CHECK_INTERVAL, frame_inputs, key_inputs, periodic_inputs,
            termination_inputs,
        },
        watch_screen::{ScreenInput, WatchEnding, WatchScreen, watch_until_ending},
    },
    screen_settings::{color_scheme::Colors, config_file::Timing},
    systemd_link::{
        boot_feed::{RootRequest, bus_inputs},
        link_error::Error,
        switch_root::INITRD_RELEASE,
        systemd_bus::{Scope, Session, StatusRelease, restore_status_after_watch},
    },
};

pub struct Settings {
    pub plymouth: bool,
    pub scope: Scope,
    pub shutdown: bool,
    pub target: RootRequest,
    pub timing: Timing,
    pub colors: Colors,
}

const FULL_REPAINT_INTERVAL: Duration = Duration::from_secs(5);

fn boot_already_finished(settings: &Settings) -> bool {
    settings.scope == Scope::System
        && !settings.shutdown
        && Session::open(Scope::System)
            .and_then(|session| session.boot_in_progress())
            .is_ok_and(|booting| !booting)
}

fn starting_phase(settings: &Settings) -> Phase {
    match settings.scope {
        _ if settings.shutdown => Phase::Shutdown,
        Scope::User => Phase::User,
        Scope::System if Path::new(INITRD_RELEASE).exists() => Phase::Initrd,
        Scope::System => Phase::Host,
    }
}

pub fn run(settings: &Settings) -> Result<(), Error> {
    if boot_already_finished(settings) {
        eprintln!("bootvine: boot already finished, leaving the console alone");
        return Ok(());
    }
    Builder::new_current_thread()
        .enable_io()
        .enable_time()
        .build()?
        .block_on(watch_live_boot(settings, starting_phase(settings)))
}

async fn watch_live_boot(settings: &Settings, phase: Phase) -> Result<(), Error> {
    match settings.scope {
        Scope::System => restore_status_after_watch(watch_on_console(settings, phase)).await,
        Scope::User => watch_on_console(settings, phase).await.map(drop),
    }
}

async fn watch_on_console(settings: &Settings, phase: Phase) -> Result<StatusRelease, Error> {
    let for_system = settings.scope == Scope::System;
    let requests = RequestWatch::open();
    let prompt_check: fn() -> ScreenInput = if requests.is_some() {
        || ScreenInput::PromptCheck
    } else {
        || ScreenInput::PasswordRequests
    };
    let desk = LiveDesk::take_console_with_plymouth(for_system, settings.plymouth);
    let password_inputs = requests.map(password_request_inputs).transpose()?;
    let event_inputs = stream::select(
        stream::select(
            bus_inputs(phase, settings.scope, settings.target.clone()),
            key_inputs()?,
        ),
        stream::select(
            termination_inputs()?,
            stream::iter(password_inputs).flatten(),
        ),
    );
    let timer_inputs = stream::select(
        stream::select(
            frame_inputs(),
            periodic_inputs(BACKGROUND_CHECK_INTERVAL, prompt_check),
        ),
        stream::select(
            periodic_inputs(BACKGROUND_CHECK_INTERVAL, || ScreenInput::ConsoleUpkeep),
            periodic_inputs(FULL_REPAINT_INTERVAL, || ScreenInput::FullRepaint),
        ),
    );
    let inputs = stream::select(event_inputs, timer_inputs);
    let mut screen = WatchScreen::new(
        BootState::new(phase),
        desk,
        BootOrigin::read,
        settings.colors,
        settings.timing,
        Duration::ZERO,
    );
    let mut terminal = ratatui::try_init()?;
    let ending = watch_until_ending(&mut screen, &mut terminal, inputs).await;
    ratatui::restore();
    match ending {
        WatchEnding::Failed(error) => Err(error),
        WatchEnding::BootCompleted => Ok(StatusRelease::AfterBoot),
        WatchEnding::ViewerLeft => Ok(StatusRelease::Immediately),
    }
}
