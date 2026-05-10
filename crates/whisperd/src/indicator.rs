//! Minimal GTK4 indicator window.
//!
//! Builds only with `--features gui` (which pulls the `gtk4` crate and
//! requires `libgtk-4-dev` on the build host). The `Indicator` type below
//! has a no-op variant for the default build so call-sites do not need
//! `#[cfg]` everywhere.
//!
//! Design notes:
//! - GTK4 insists on owning the main thread for its event loop. We run it
//!   on a dedicated OS thread and bridge daemon → indicator state via a
//!   `std::sync::mpsc::sync_channel` with depth 8 (drop-on-overflow; state
//!   transitions are rare).
//! - `set_focusable(false)` + `set_can_focus(false)` keep the window from
//!   stealing keyboard focus from the editor — `xdotool type` continues
//!   to land on whatever was focused when the user released F9.
//! - Window placement is left to the WM. For i3, add a `for_window` rule to
//!   float + position the indicator (see `doc/plan/05-build-install.md`).

use std::sync::mpsc::{SyncSender, TrySendError};
use whisper_stt_proto::DaemonState;

pub struct Indicator {
    tx: Option<SyncSender<DaemonState>>,
}

impl Indicator {
    pub fn spawn() -> Self {
        #[cfg(feature = "gui")]
        {
            let (tx, rx) = std::sync::mpsc::sync_channel::<DaemonState>(8);
            let res = std::thread::Builder::new()
                .name("indicator".into())
                .spawn(move || gtk_main::run(rx));
            match res {
                Ok(_) => Self { tx: Some(tx) },
                Err(e) => {
                    log::warn!("indicator thread spawn failed: {e}; running headless");
                    Self { tx: None }
                }
            }
        }
        #[cfg(not(feature = "gui"))]
        {
            Self { tx: None }
        }
    }

    pub fn set_state(&self, state: DaemonState) {
        if let Some(ref tx) = self.tx {
            match tx.try_send(state) {
                Ok(()) | Err(TrySendError::Full(_)) => {}
                Err(TrySendError::Disconnected(_)) => {
                    log::warn!("indicator thread is gone");
                }
            }
        }
    }
}

#[cfg(feature = "gui")]
mod gtk_main {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::mpsc::Receiver;
    use std::time::Duration;

    use gtk4::glib;
    use gtk4::prelude::*;
    use gtk4::{Application, ApplicationWindow, CssProvider, Label};
    use whisper_stt_proto::DaemonState;

    const APP_ID: &str = "com.shoneyj.whisper-stt.indicator";
    const CSS: &str = include_str!("../../../assets/indicator.css");

    pub fn run(rx: Receiver<DaemonState>) {
        let app = Application::builder().application_id(APP_ID).build();

        let rx = Rc::new(RefCell::new(rx));
        app.connect_activate(move |app| {
            load_css();

            let label = Label::new(Some(state_text(DaemonState::Idle)));
            label.add_css_class("indicator-label");

            let window = ApplicationWindow::builder()
                .application(app)
                .title("whisper-stt")
                .child(&label)
                .resizable(false)
                .decorated(false)
                .build();
            window.add_css_class("indicator-window");
            window.set_focusable(false);
            window.set_can_focus(false);

            // Poll the cross-thread receiver from the GTK main loop. 100 ms
            // is well below human-perception latency for state changes.
            let label_for_tick = label.clone();
            let rx_for_tick = rx.clone();
            glib::timeout_add_local(Duration::from_millis(100), move || {
                if let Ok(rx) = rx_for_tick.try_borrow_mut() {
                    while let Ok(state) = rx.try_recv() {
                        label_for_tick.set_text(state_text(state));
                        let class = state_class(state);
                        for c in [
                            "state-idle",
                            "state-listening",
                            "state-transcribing",
                            "state-error",
                        ] {
                            label_for_tick.remove_css_class(c);
                        }
                        label_for_tick.add_css_class(class);
                    }
                }
                glib::ControlFlow::Continue
            });

            window.present();
        });

        // We pass an empty argv so GTK does not try to interpret whisperd's
        // own flags (e.g. `--record 3`).
        app.run_with_args::<&str>(&[]);
    }

    fn state_text(state: DaemonState) -> &'static str {
        match state {
            DaemonState::Idle => "idle",
            DaemonState::Listening => "listening",
            DaemonState::Transcribing => "transcribing",
            DaemonState::Error => "error",
        }
    }

    fn state_class(state: DaemonState) -> &'static str {
        match state {
            DaemonState::Idle => "state-idle",
            DaemonState::Listening => "state-listening",
            DaemonState::Transcribing => "state-transcribing",
            DaemonState::Error => "state-error",
        }
    }

    fn load_css() {
        let provider = CssProvider::new();
        provider.load_from_data(CSS);
        if let Some(display) = gtk4::gdk::Display::default() {
            gtk4::style_context_add_provider_for_display(
                &display,
                &provider,
                gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
        }
    }
}
