//! Benchmark seam: the transcript and the frame renderer behind one
//! headless screen, so the Layer-2 render benchmark drives the code
//! the interface loop runs without a terminal. Hidden from the docs;
//! not part of the interface contract, and free to change.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::input::Input;
use crate::render::{self, Status, View};
use crate::transcript::{Kind, Transcript};

pub use crate::transcript::MAX_BLOCK_BYTES;

/// A transcript, an input line, a status bar, and a frame buffer of
/// one size. Frames render into the buffer and go nowhere.
pub struct Screen {
    transcript: Transcript,
    input: Input,
    status: Status,
    terminal: Terminal<TestBackend>,
}

impl Screen {
    pub fn new(width: u16, height: u16) -> Self {
        Self {
            transcript: Transcript::new(),
            input: Input::new(),
            status: Status {
                model: "bench-model".into(),
                mode: "auto".into(),
                session: Some("bench-session".into()),
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
            },
            terminal: Terminal::new(TestBackend::new(width, height)).expect("headless terminal"),
        }
    }

    /// Append streamed model text, the way one text delta arrives.
    pub fn push_text(&mut self, delta: &str) {
        self.transcript.push_delta(Kind::Text, delta);
    }

    /// Append one whole block, the way a notice arrives.
    pub fn push_line(&mut self, text: &str) {
        self.transcript.push_line(Kind::Notice, text);
    }

    /// Draw one frame: the same layout and the same buffer diff the
    /// live interface runs, minus the terminal write.
    pub fn draw(&mut self) {
        let Self {
            transcript,
            input,
            status,
            terminal,
        } = self;
        terminal
            .draw(|frame| {
                render::draw(
                    frame,
                    View {
                        transcript,
                        input,
                        status,
                        approval: None,
                    },
                );
            })
            .expect("headless draw");
    }

    /// Blocks the transcript holds.
    pub fn blocks(&self) -> usize {
        self.transcript.blocks().len()
    }
}
