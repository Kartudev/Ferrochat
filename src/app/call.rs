//! Calls in the app: runs the [`crate::call::Calls`] state machine and
//! carries out its effects (decline/answer/hang up through the client,
//! ringtone, notification, audio devices, call bubbles).

use iced::Task;

use super::message::{Done, Message, Ui};
use super::{App, blocking};
use crate::call::audio::{Devices, Ringer, Tone};
use crate::call::{Effect, LogEntry, Phase};
use crate::net::{
    self, CallAudio, CallOutcome, CallSignal, Content, Message as ChatMessage, Origin,
    ReceiptStatus,
};

/// Call buttons and timers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallUi {
    /// Call button in a chat header (`video`: the video button).
    Start {
        /// Chat.
        chat: String,
        /// Video requested.
        video: bool,
    },
    /// Answer the ringing call.
    Accept,
    /// Decline the ringing call.
    Decline,
    /// Hang up / cancel.
    HangUp,
    /// Mute / unmute the microphone.
    ToggleMute,
    /// One second passed while a call is up.
    Tick,
}

/// Results of call tasks.
#[derive(Debug, Clone)]
pub enum CallDone {
    /// `start_call` finished.
    Started {
        /// Chat.
        chat: String,
        /// Call id or error.
        result: Result<String, String>,
    },
    /// `accept_call` finished.
    Accepted {
        /// Call id.
        call_id: String,
        /// Error, if answering failed.
        error: Option<String>,
    },
    /// A decline or hang-up could not be sent.
    Failed(String),
}

/// What the video button says until video calls exist.
pub const VIDEO_UNSUPPORTED: &str =
    "Video calls are not supported yet. Start a voice call, or call from your phone.";

/// Audio state of the call (owned by the app, dropped when the call ends).
#[derive(Debug, Default)]
pub struct CallMedia {
    /// The open microphone / speaker.
    devices: Option<Devices>,
    /// The ringtone / ringback that is playing.
    ringer: Option<(Tone, Ringer)>,
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

impl App {
    /// No sound card in `--mock`, tests and screenshots.
    fn call_sound(&self) -> bool {
        !self.cfg.mock && self.cfg.screenshot.is_none()
    }

    pub(super) fn on_call_signal(&mut self, mut signal: CallSignal) -> Task<Message> {
        // Calls arrive LID-addressed; show them under the phone number.
        match &mut signal {
            CallSignal::Ringing(info) => {
                info.caller = self.lid_map.normalize(&info.caller);
            }
            CallSignal::Missed { caller, .. } => *caller = self.lid_map.normalize(caller),
            _ => {}
        }
        let fx = self.calls.on_signal(signal, now());
        self.apply_call_effects(fx)
    }

    pub(super) fn on_call_ui(&mut self, ui: CallUi) -> Task<Message> {
        let t = now();
        let fx = match ui {
            CallUi::Start { video: true, .. } => vec![Effect::Toast(VIDEO_UNSUPPORTED.into())],
            CallUi::Start { chat, video: false } => self.calls.start(&chat, t),
            CallUi::Accept => self.calls.accept(t),
            CallUi::Decline => self.calls.decline(t),
            CallUi::HangUp => self.calls.hang_up(t),
            CallUi::ToggleMute => {
                if let (Some((id, muted)), Some(c)) = (self.calls.toggle_mute(), &self.client) {
                    c.set_call_muted(&id, muted);
                }
                Vec::new()
            }
            CallUi::Tick => self.calls.tick(t),
        };
        self.apply_call_effects(fx)
    }

    pub(super) fn on_call_done(&mut self, done: CallDone) -> Task<Message> {
        let t = now();
        let fx = match done {
            CallDone::Started { chat, result } => self.calls.started(&chat, result, t),
            CallDone::Accepted {
                call_id,
                error: Some(e),
            } => self.calls.on_signal(
                CallSignal::MediaEnded {
                    call_id,
                    error: Some(e),
                },
                t,
            ),
            CallDone::Accepted { .. } => Vec::new(),
            CallDone::Failed(e) => vec![Effect::Toast(e)],
        };
        self.apply_call_effects(fx)
    }

    fn apply_call_effects(&mut self, fx: Vec<Effect>) -> Task<Message> {
        let mut tasks = Vec::new();
        for e in fx {
            tasks.push(self.apply_call_effect(e));
        }
        self.sync_ringer();
        Task::batch(tasks)
    }

    /// Ringtone while ringing here, ringback while the peer's phone rings.
    fn sync_ringer(&mut self) {
        let want = self.calls.current.as_ref().and_then(|c| match c.phase {
            Phase::Incoming => Some(Tone::Ring),
            Phase::Calling | Phase::Ringing => Some(Tone::Ringback),
            _ => None,
        });
        let have = self.call_media.ringer.as_ref().map(|(t, _)| *t);
        if want == have {
            return;
        }
        self.call_media.ringer = match want {
            Some(t) if self.call_sound() => Some((t, Ringer::start(t))),
            _ => None,
        };
    }

    fn open_call_audio(&mut self) -> CallAudio {
        if !self.call_sound() {
            return CallAudio::silent();
        }
        let (audio, devices) = Devices::open();
        self.call_media.devices = Some(devices);
        audio
    }

    fn apply_call_effect(&mut self, e: Effect) -> Task<Message> {
        match e {
            Effect::Ring => self.announce_call(),
            // The ringer follows the phase (`sync_ringer`).
            Effect::StopRing => Task::none(),
            Effect::CloseAudio => {
                self.call_media.devices = None;
                Task::none()
            }
            Effect::Toast(t) => {
                self.toast(t);
                Task::none()
            }
            Effect::Log(entry) => self.log_call(entry),
            Effect::Reject(id) => {
                let Some(c) = self.client.clone() else {
                    return Task::none();
                };
                Task::perform(async move { c.reject_call(&id).await }, |r| match r {
                    Ok(()) => Message::done(Done::Nothing),
                    Err(e) => Message::done(Done::Call(CallDone::Failed(format!(
                        "Couldn't decline the call: {}",
                        e.user_message()
                    )))),
                })
            }
            Effect::End(id) => {
                let Some(c) = self.client.clone() else {
                    return Task::none();
                };
                Task::perform(async move { c.end_call(&id).await }, |r| match r {
                    Ok(()) => Message::done(Done::Nothing),
                    Err(e) => Message::done(Done::Call(CallDone::Failed(format!(
                        "Couldn't hang up: {}",
                        e.user_message()
                    )))),
                })
            }
            Effect::Accept(id) => {
                let audio = self.open_call_audio();
                let Some(c) = self.client.clone() else {
                    return Task::none();
                };
                Task::perform(
                    async move {
                        let error = c
                            .accept_call(&id, audio)
                            .await
                            .err()
                            .map(|e| e.user_message());
                        CallDone::Accepted { call_id: id, error }
                    },
                    |d| Message::done(Done::Call(d)),
                )
            }
            Effect::Start(chat) => {
                let audio = self.open_call_audio();
                let Some(c) = self.client.clone() else {
                    return self.on_call_done(CallDone::Started {
                        chat,
                        result: Err("not connected".into()),
                    });
                };
                Task::perform(
                    async move {
                        let result = c
                            .start_call(&chat, audio)
                            .await
                            .map_err(|e| e.user_message());
                        CallDone::Started { chat, result }
                    },
                    |d| Message::done(Done::Call(d)),
                )
            }
        }
    }

    /// Notification + window, like the official client's incoming call.
    fn announce_call(&mut self) -> Task<Message> {
        let Some(c) = self.calls.current.clone() else {
            return Task::none();
        };
        let name = self.chats.resolve(&c.peer, None).plain();
        let kind = if c.video { "video" } else { "voice" };
        let summary = crate::notify::escape_markup(&if c.group {
            format!("{} @ {}", name, self.chats.plain_name(&c.chat))
        } else {
            name
        });
        let body = crate::notify::escape_markup(&format!("Incoming {kind} call"));
        let effect = self.window.show();
        let show = self.apply(effect);
        if self.cfg.screenshot.is_some() || self.cfg.mock {
            return show;
        }
        let chat = c.chat.clone();
        Task::batch([
            show,
            blocking(
                move || {
                    let on_click: crate::notify::OnClick = Box::new(move || {
                        crate::ext::send(crate::ext::External::Show { chat: Some(chat) })
                    });
                    crate::notify::show(&summary, &body, Some(on_click))
                },
                |_| Message::done(Done::Nothing),
            ),
        ])
    }

    /// Writes the call bubble through the normal message path. A missed
    /// incoming call counts as unread and notifies, like the official
    /// client; everything else is just history.
    fn log_call(&mut self, e: LogEntry) -> Task<Message> {
        let missed = e.incoming && e.log.outcome == CallOutcome::Missed;
        let m = ChatMessage {
            id: e.id,
            chat: e.chat,
            sender: e.sender,
            sender_name: None,
            from_me: !e.incoming,
            timestamp: e.ts,
            content: Content::Call(e.log),
            status: if e.incoming {
                ReceiptStatus::Read
            } else {
                ReceiptStatus::Sent
            },
            expires_at: None,
            reply: None,
            edited: false,
            forwarded: None,
            mentions: Vec::new(),
            reactions: Vec::new(),
        };
        let origin = if missed {
            Origin::Live
        } else {
            Origin::History
        };
        self.on_net(net::Event::Messages {
            messages: vec![m],
            origin,
        })
    }
}

/// `Ui` wrapper so views can build call messages.
pub fn ui(c: CallUi) -> Message {
    Message::Ui(Ui::Call(c))
}

/// Screenshot scenes: two call bubbles in `chat`, then a call from it that
/// rings (`active`: answered).
pub(super) fn scene(chat: &str, active: bool) -> Vec<Task<Message>> {
    use crate::net::{CallLog, Event, IncomingCallInfo};
    let t = now();
    let bubble = |id: &str, from_me: bool, ts: i64, outcome, seconds| ChatMessage {
        id: id.into(),
        chat: chat.into(),
        sender: if from_me { String::new() } else { chat.into() },
        sender_name: None,
        from_me,
        timestamp: ts,
        content: Content::Call(CallLog {
            video: false,
            group: false,
            outcome,
            seconds,
        }),
        status: ReceiptStatus::Read,
        expires_at: None,
        reply: None,
        edited: false,
        forwarded: None,
        mentions: Vec::new(),
        reactions: Vec::new(),
    };
    let mut v = vec![Task::done(Message::Net(Event::Messages {
        messages: vec![
            bubble("call-shot-1", false, t - 120, CallOutcome::Missed, 0),
            bubble("call-shot-2", true, t - 60, CallOutcome::Answered, 263),
        ],
        origin: Origin::History,
    }))];
    v.push(Task::done(Message::Net(Event::Call(CallSignal::Ringing(
        IncomingCallInfo {
            call_id: "SHOT-CALL".into(),
            caller: chat.into(),
            group: None,
            video: false,
            ts: t,
        },
    )))));
    if active {
        v.push(Task::done(ui(CallUi::Accept)));
    }
    v
}
