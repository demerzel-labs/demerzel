//! Benchmark seam: the transcript and the frame renderer behind one
//! headless screen, so the Layer-2 render benchmark drives the code
//! the interface loop runs without a terminal. Hidden from the docs;
//! not part of the interface contract, and free to change.

use ratatui::Terminal;
use ratatui::backend::TestBackend;

use demerzel_core::ApprovalMode;

use crate::editor::Editor;
use crate::render::{self, Status, View};
use crate::replay::Replay;
use crate::theme::{Depth, Palette, Theme};
use crate::transcript::{Kind, Transcript};

pub use crate::transcript::MAX_BLOCK_BYTES;

/// Buffer capacities, not process memory. The layout total includes
/// rendered text, row offsets, and faces. The text total excludes
/// card and thought backing stores. Neither includes allocator
/// overhead, the frame buffers, or the block objects.
#[derive(Debug)]
pub struct Buffers {
    pub blocks: usize,
    pub laid_out_blocks: usize,
    pub rendered_blocks: usize,
    pub text_capacity_bytes: usize,
    pub layout_capacity_bytes: usize,
}

/// A transcript, an input editor, a status bar, and a frame buffer
/// of one size. Frames render into the buffer and go nowhere.
pub struct Screen {
    transcript: Transcript,
    editor: Editor,
    status: Status,
    palette: Palette,
    terminal: Terminal<TestBackend>,
}

impl Screen {
    /// A screen of one size; `markdown` picks whether streamed text
    /// renders as markdown or lays out as plain text.
    pub fn new(width: u16, height: u16, markdown: bool) -> Self {
        Self {
            transcript: Transcript::new(markdown),
            editor: Editor::new(Vec::new()),
            status: Status {
                model: "bench-model".into(),
                mode: ApprovalMode::Auto,
                effort: "high".into(),
                project: "bench".into(),
                branch: "main".into(),
                context_tokens: 34_200,
                context_window: Some(200_000),
                input_tokens: 0,
                output_tokens: 0,
                cached_tokens: 0,
                quit_hint: false,
            },
            palette: Palette::new(&Theme::default(), Depth::TrueColor),
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

    /// Show a resumed session's past turns, the way the first prompt
    /// of a resumed session does.
    pub fn replay(&mut self, replay: Replay) {
        self.transcript.replay(replay);
    }

    /// Resize the headless backend. The next draw resizes the frame
    /// buffers and reflows the visible transcript, as the live loop does.
    pub fn resize(&mut self, width: u16, height: u16) {
        self.terminal.backend_mut().resize(width, height);
    }

    /// Replace the editor's text, including its next layout in a draw.
    pub fn set_editor(&mut self, text: &str) {
        self.editor.set(text);
    }

    /// Toggle the newest thinking block through the interface's path.
    pub fn toggle_thinking(&mut self) {
        self.transcript.toggle_thinking();
    }

    /// Expand or collapse all tool cards through the interface's path.
    pub fn set_cards_expanded(&mut self, expanded: bool) {
        self.transcript.set_expanded(expanded);
    }

    /// Browse toward the first row; the next draw clamps the offset.
    pub fn scroll_up(&mut self, rows: usize) {
        self.transcript.scroll_up(rows);
    }

    /// Return to the live end of the transcript.
    pub fn follow(&mut self) {
        self.transcript.follow();
    }

    /// Read retained text and layout buffer capacities.
    pub fn buffers(&self) -> Buffers {
        let mut buffers = Buffers {
            blocks: self.blocks(),
            laid_out_blocks: 0,
            rendered_blocks: self.rendered_blocks(),
            text_capacity_bytes: 0,
            layout_capacity_bytes: 0,
        };
        for block in self.transcript.blocks() {
            let (laid, text, layout) = block.bench_buffers();
            buffers.laid_out_blocks += usize::from(laid);
            buffers.text_capacity_bytes += text;
            buffers.layout_capacity_bytes += layout;
        }
        buffers
    }

    /// Read the current frame's cell text for fixture checks.
    pub fn visible_text(&self) -> String {
        self.terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// Draw one frame: the same layout and the same buffer diff the
    /// live interface runs, minus the terminal write.
    pub fn draw(&mut self) {
        let Self {
            transcript,
            editor,
            status,
            palette,
            terminal,
        } = self;
        terminal
            .draw(|frame| {
                render::draw(
                    frame,
                    View {
                        transcript,
                        editor,
                        status,
                        palette,
                        approval: None,
                        progress: None,
                    },
                );
            })
            .expect("headless draw");
    }

    /// Select the rows between two viewport positions, `(column,
    /// row)` each, the way a drag does. Needs a frame drawn first:
    /// the positions map through the last frame's rows. Returns
    /// whether the drag moved the head: false when `to` lies off
    /// the transcript pane, so a bench can assert its span exists.
    pub fn select(&mut self, from: (u16, u16), to: (u16, u16)) -> bool {
        self.transcript.select_press(from.0, from.1);
        self.transcript.select_drag(to.0, to.1)
    }

    /// Blocks the transcript holds.
    pub fn blocks(&self) -> usize {
        self.transcript.blocks().len()
    }

    /// Blocks whose rows come from a markdown render, among the
    /// blocks laid out so far.
    pub fn rendered_blocks(&self) -> usize {
        self.transcript
            .blocks()
            .filter(|block| block.rendered())
            .count()
    }
}
