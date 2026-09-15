//! Screen reader support through AccessKit (AT-SPI on Linux, NSAccessibility on macOS).
//!
//! The tree is a window holding a terminal node with one text run per visible
//! row. It is built from the render snapshot, only while an assistive
//! technology is listening, and at most every [`UPDATE_INTERVAL`].

use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Node, NodeId, Rect, Role, TextPosition,
    TextSelection, TreeId, TreeInfo, TreeUpdate,
};
use tron_core::{Flags, Row, Snapshot};
use winit::event_loop::EventLoopProxy;
use winit::window::Window;

#[cfg(not(target_os = "macos"))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

#[cfg(not(target_os = "macos"))]
use linux::Adapter;
#[cfg(target_os = "macos")]
use macos::Adapter;

const UPDATE_INTERVAL: Duration = Duration::from_millis(100);
const WINDOW: NodeId = NodeId(0);
const TERMINAL: NodeId = NodeId(1);
const FIRST_ROW: u64 = 2;

/// Cell geometry needed for node bounds, in physical pixels.
#[derive(Copy, Clone)]
pub struct Layout {
    pub cell_width: f64,
    pub cell_height: f64,
    pub padding: [f64; 2],
}

#[derive(Default)]
struct AdapterFlags {
    active: AtomicBool,
    /// The adapter asked for a full tree, send one without waiting.
    initial: AtomicBool,
}

struct Activation {
    flags: Arc<AdapterFlags>,
    proxy: EventLoopProxy,
}

impl ActivationHandler for Activation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        self.flags.active.store(true, Ordering::Release);
        self.flags.initial.store(true, Ordering::Release);
        self.proxy.wake_up();
        None
    }
}

struct Deactivation(Arc<AdapterFlags>);

impl DeactivationHandler for Deactivation {
    fn deactivate_accessibility(&mut self) {
        self.0.active.store(false, Ordering::Release);
    }
}

/// The terminal only has one focusable node, so actions need no handling.
struct IgnoreActions;

impl ActionHandler for IgnoreActions {
    fn do_action(&mut self, _request: ActionRequest) {}
}

pub struct Accessibility {
    adapter: Adapter,
    flags: Arc<AdapterFlags>,
    last_sent: Option<Instant>,
    last_hash: u64,
}

impl Accessibility {
    /// Connects the window to the platform's accessibility API. Never blocks.
    pub fn new(proxy: EventLoopProxy, window: &dyn Window) -> Option<Self> {
        let flags = Arc::new(AdapterFlags::default());
        let adapter = Adapter::new(Activation { flags: flags.clone(), proxy }, Deactivation(flags.clone()), window)?;
        Some(Self { adapter, flags, last_sent: None, last_hash: 0 })
    }

    pub fn set_focused(&mut self, focused: bool) {
        self.adapter.set_focused(focused);
    }

    /// Sends the screen when it changed. Returns when to call again if an
    /// update is waiting for the rate limit.
    pub fn update(&mut self, snapshot: &Snapshot, layout: Layout) -> Option<Instant> {
        if !self.flags.active.load(Ordering::Acquire) {
            return None;
        }
        let initial = self.flags.initial.swap(false, Ordering::AcqRel);
        let rows: Vec<RowText> = snapshot.rows.iter().map(RowText::new).collect();
        let cursor = snapshot.cursor();
        let mut hasher = DefaultHasher::new();
        for row in &rows {
            row.text.hash(&mut hasher);
        }
        (cursor.row, cursor.col, cursor.visible).hash(&mut hasher);
        let hash = hasher.finish();
        if !initial && hash == self.last_hash {
            return None;
        }
        let now = Instant::now();
        if !initial && let Some(due) = self.last_sent.map(|at| at + UPDATE_INTERVAL).filter(|&due| due > now) {
            return Some(due);
        }
        self.last_hash = hash;
        self.last_sent = Some(now);
        self.adapter.update_if_active(|| tree(&rows, snapshot, layout));
        None
    }
}

/// Text of one row, one entry per visible character cell.
struct RowText {
    text: String,
    lengths: Vec<u8>,
    word_starts: Vec<u8>,
    /// Column of each character.
    columns: Vec<usize>,
}

impl RowText {
    fn new(row: &Row) -> Self {
        let mut out = Self { text: String::new(), lengths: Vec::new(), word_starts: Vec::new(), columns: Vec::new() };
        let mut previous_space = true;
        for (col, cell) in row.cells.iter().enumerate() {
            if cell.flags.contains(Flags::WIDE_SPACER) {
                continue;
            }
            let start = out.text.len();
            row.push_cell_text(col, &mut out.text);
            let space = cell.is_empty();
            if previous_space && !space && out.lengths.len() <= usize::from(u8::MAX) {
                out.word_starts.push(out.lengths.len() as u8);
            }
            previous_space = space;
            out.lengths.push((out.text.len() - start).min(usize::from(u8::MAX)) as u8);
            out.columns.push(col);
        }
        out
    }
}

fn tree(rows: &[RowText], snapshot: &Snapshot, layout: Layout) -> TreeUpdate {
    let cursor = snapshot.cursor();
    let [pad_x, pad_y] = layout.padding;
    let width = snapshot.cols as f64 * layout.cell_width;
    let mut nodes = Vec::with_capacity(rows.len() + 2);

    let mut window = Node::new(Role::Window);
    window.set_label("tron");
    window.set_children(vec![TERMINAL]);
    nodes.push((WINDOW, window));

    let mut terminal = Node::new(Role::Terminal);
    terminal.set_children((0..rows.len() as u64).map(|i| NodeId(FIRST_ROW + i)).collect::<Vec<_>>());
    terminal.set_bounds(Rect {
        x0: pad_x,
        y0: pad_y,
        x1: pad_x + width,
        y1: pad_y + rows.len() as f64 * layout.cell_height,
    });
    if cursor.visible && cursor.row < rows.len() {
        let row = &rows[cursor.row];
        let index = row.columns.iter().position(|&col| col >= cursor.col).unwrap_or(row.columns.len());
        let caret = TextPosition { node: NodeId(FIRST_ROW + cursor.row as u64), character_index: index };
        terminal.set_text_selection(TextSelection { anchor: caret, focus: caret });
    }
    nodes.push((TERMINAL, terminal));

    for (i, row) in rows.iter().enumerate() {
        let mut run = Node::new(Role::TextRun);
        run.set_value(row.text.clone());
        run.set_character_lengths(row.lengths.clone());
        run.set_word_starts(row.word_starts.clone());
        let top = pad_y + i as f64 * layout.cell_height;
        run.set_bounds(Rect { x0: pad_x, y0: top, x1: pad_x + width, y1: top + layout.cell_height });
        nodes.push((NodeId(FIRST_ROW + i as u64), run));
    }
    TreeUpdate { nodes, tree: Some(TreeInfo::new(WINDOW)), tree_id: TreeId::ROOT, focus: TERMINAL }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tron_core::{Parser, Terminal};

    #[test]
    fn rows_become_text_runs_with_a_caret() {
        let mut term = Terminal::new(10, 3, 0);
        Parser::new().advance(&mut term, "ab 中x\r\nhi".as_bytes());
        let mut snapshot = Snapshot::default();
        term.snapshot(&mut snapshot);
        let rows: Vec<RowText> = snapshot.rows.iter().map(RowText::new).collect();
        assert!(rows[0].text.starts_with("ab 中x"));
        assert_eq!(&rows[0].lengths[..5], &[1, 1, 1, 3, 1]);
        assert_eq!(&rows[0].word_starts[..2], &[0, 3]);
        let layout = Layout { cell_width: 8.0, cell_height: 16.0, padding: [0.0, 0.0] };
        let update = tree(&rows, &snapshot, layout);
        assert_eq!(update.nodes.len(), 5);
        let terminal = &update.nodes[1].1;
        let caret = terminal.text_selection().unwrap().focus;
        assert_eq!((caret.node, caret.character_index), (NodeId(FIRST_ROW + 1), 2));
    }
}
