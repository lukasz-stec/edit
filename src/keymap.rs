//! Key bindings. Sublime Text defaults, with IntelliJ IDEA extras layered on top.

use crate::doc::Motion;
use eframe::egui::{Key, Modifiers};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cmd {
    // ---- application
    NewFile,
    OpenFile,
    OpenFolder,
    AddFolder,
    Save,
    SaveAs,
    SaveAll,
    CloseTab,
    NextTab,
    PrevTab,
    CommandPalette,
    GotoAnything,
    GotoLine,
    Find,
    FindInFiles,
    ReplaceInFiles,
    Replace,
    FindNext,
    FindPrev,
    ToggleSidebar,
    ToggleMinimap,
    SelectTheme,
    SetSyntax,
    PasteFromHistory,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    ToggleCamelHumps,
    ToggleIndentation,
    ToggleLineEndings,
    ToggleColumnMode,
    DiffWithSaved,
    DiffWithHead,
    DiffWithClipboard,
    DiffWithFile,
    DiffWithTab,
    DiffTreeSelection,
    ShowDiff,
    ShowHistory,
    PutLabel,
    CloseDiff,
    NextDiff,
    PrevDiff,
    Quit,
    // ---- editor
    Undo,
    Redo,
    SelectAll,
    SelectLine,
    ExpandSelection,
    ShrinkSelection,
    SelectNextOccurrence,
    UnselectOccurrence,
    SelectAllOccurrences,
    SplitIntoLines,
    AddCaretAbove,
    AddCaretBelow,
    CaretsAtLineEnds,
    Escape,
    Duplicate,
    DeleteLine,
    MoveLineUp,
    MoveLineDown,
    ToggleComment,
    Indent,
    Outdent,
    JoinLines,
    SortLines,
    ToggleCase,
    InsertLineBelow,
    InsertLineAbove,
    GotoBracket,
    Move(Motion, bool),
    Backspace(bool),
    DeleteFwd(bool),
    Newline,
    Tab,
}

impl Cmd {
    /// Commands that work regardless of which widget has focus.
    pub fn is_global(self) -> bool {
        use Cmd::*;
        matches!(
            self,
            NewFile | OpenFile | OpenFolder | AddFolder | Save | SaveAs | SaveAll | CloseTab | NextTab | PrevTab
                | CommandPalette | GotoAnything | GotoLine | Find | FindInFiles | ReplaceInFiles | Replace | FindNext | FindPrev
                | ToggleSidebar | ToggleMinimap | SelectTheme | SetSyntax | PasteFromHistory | ZoomIn
                | ZoomOut | ZoomReset | ToggleCamelHumps | ToggleIndentation | ToggleLineEndings | ToggleColumnMode | Quit
                | DiffWithSaved | DiffWithHead | DiffWithClipboard | DiffWithFile | DiffWithTab | DiffTreeSelection | ShowDiff | ShowHistory | PutLabel | CloseDiff
                | NextDiff | PrevDiff
        )
    }

    pub fn name(self) -> &'static str {
        use Cmd::*;
        match self {
            NewFile => "File: New File",
            OpenFile => "File: Open File…",
            OpenFolder => "File: Open Folder…",
            AddFolder => "File: Add Folder to Sidebar…",
            Save => "File: Save",
            SaveAs => "File: Save As…",
            SaveAll => "File: Save All",
            CloseTab => "File: Close Tab",
            NextTab => "View: Next Tab",
            PrevTab => "View: Previous Tab",
            CommandPalette => "Command Palette",
            GotoAnything => "Goto Anything (files)",
            GotoLine => "Goto Line…",
            Find => "Find…",
            FindInFiles => "Find in Files…",
            ReplaceInFiles => "Replace in Files…",
            Replace => "Replace…",
            FindNext => "Find Next",
            FindPrev => "Find Previous",
            ToggleSidebar => "View: Toggle Sidebar",
            ToggleMinimap => "View: Toggle Minimap",
            SelectTheme => "Preferences: Select Color Theme…",
            SetSyntax => "Set Syntax…",
            PasteFromHistory => "Edit: Paste from History…",
            ZoomIn => "View: Increase Font Size",
            ZoomOut => "View: Decrease Font Size",
            ZoomReset => "View: Reset Font Size",
            ToggleCamelHumps => "Preferences: Toggle CamelHumps Word Movement",
            ToggleIndentation => "Indentation: Toggle Tabs / Spaces",
            ToggleLineEndings => "Line Endings: Toggle LF / CRLF",
            ToggleColumnMode => "Selection: Column Selection Mode (IntelliJ)",
            DiffWithSaved => "Diff: Compare with Saved File",
            DiffWithHead => "Diff: Compare with Git HEAD",
            DiffWithClipboard => "Diff: Compare with Clipboard",
            DiffWithFile => "Diff: Compare with File…",
            DiffWithTab => "Diff: Compare with Open Tab…",
            DiffTreeSelection => "Diff: Compare Files Selected in Sidebar",
            ShowDiff => "Diff: Show Diff (sidebar pair, unsaved changes or Git HEAD)",
            ShowHistory => "Local History: Show History of Current File…",
            PutLabel => "Local History: Put Label…",
            CloseDiff => "Diff: Close Diff",
            NextDiff => "Diff: Next Difference",
            PrevDiff => "Diff: Previous Difference",
            Quit => "Quit",
            Undo => "Edit: Undo",
            Redo => "Edit: Redo",
            SelectAll => "Selection: Select All",
            SelectLine => "Selection: Expand to Line",
            ExpandSelection => "Selection: Extend Selection (IntelliJ)",
            ShrinkSelection => "Selection: Shrink Selection (IntelliJ)",
            SelectNextOccurrence => "Selection: Add Next Occurrence",
            UnselectOccurrence => "Selection: Unselect Last Occurrence",
            SelectAllOccurrences => "Selection: Select All Occurrences",
            SplitIntoLines => "Selection: Split into Lines",
            AddCaretAbove => "Selection: Add Caret Above",
            AddCaretBelow => "Selection: Add Caret Below",
            CaretsAtLineEnds => "Selection: Add Carets to Ends of Selected Lines",
            Escape => "Selection: Single Selection",
            Duplicate => "Edit: Duplicate Line / Selection",
            DeleteLine => "Edit: Delete Line",
            MoveLineUp => "Edit: Move Line Up",
            MoveLineDown => "Edit: Move Line Down",
            ToggleComment => "Edit: Toggle Line Comment",
            Indent => "Edit: Indent",
            Outdent => "Edit: Unindent",
            JoinLines => "Edit: Join Lines",
            SortLines => "Edit: Sort Lines",
            ToggleCase => "Edit: Toggle Case",
            InsertLineBelow => "Edit: Insert Line After",
            InsertLineAbove => "Edit: Insert Line Before",
            GotoBracket => "Goto Matching Bracket",
            Move(..) | Backspace(_) | DeleteFwd(_) | Newline | Tab => "",
        }
    }

    /// Commands listed in the command palette.
    pub fn palette() -> &'static [Cmd] {
        use Cmd::*;
        &[
            NewFile, OpenFile, OpenFolder, AddFolder, Save, SaveAs, SaveAll, CloseTab, GotoAnything, GotoLine,
            Find, FindInFiles, ReplaceInFiles, Replace, ToggleSidebar, ToggleMinimap, SelectTheme, SetSyntax, PasteFromHistory,
            ZoomIn, ZoomOut, ZoomReset, ToggleCamelHumps, ToggleIndentation, ToggleLineEndings,
            Undo, Redo, SelectAll, SelectLine, ExpandSelection, ShrinkSelection,
            SelectNextOccurrence, UnselectOccurrence, SelectAllOccurrences, SplitIntoLines,
            AddCaretAbove, AddCaretBelow, CaretsAtLineEnds, ToggleColumnMode, Duplicate, DeleteLine, MoveLineUp, MoveLineDown,
            ToggleComment, Indent, Outdent, JoinLines, SortLines, ToggleCase, InsertLineBelow,
            InsertLineAbove, GotoBracket, NextTab, PrevTab, Quit,
            DiffWithSaved, DiffWithHead, DiffWithClipboard, DiffWithFile, DiffWithTab, DiffTreeSelection, ShowDiff, ShowHistory, PutLabel, CloseDiff,
            NextDiff, PrevDiff,
        ]
    }
}

const C: u8 = 1;
const S: u8 = 2;
const A: u8 = 4;

struct B(Cmd, u8, Key);

#[rustfmt::skip]
const BINDINGS: &[B] = &[
    // files & tabs
    B(Cmd::NewFile, C, Key::N),
    B(Cmd::OpenFile, C, Key::O),
    B(Cmd::OpenFolder, C | S, Key::O),
    B(Cmd::Save, C, Key::S),
    B(Cmd::SaveAs, C | S, Key::S),
    B(Cmd::SaveAll, C | A, Key::S),
    B(Cmd::CloseTab, C, Key::F4),
    B(Cmd::NextTab, C, Key::Tab),
    B(Cmd::NextTab, C, Key::PageDown),
    B(Cmd::PrevTab, C | S, Key::Tab),
    B(Cmd::PrevTab, C, Key::PageUp),
    // navigation / panels
    B(Cmd::CommandPalette, C | S, Key::P),
    B(Cmd::CommandPalette, C | S, Key::A),      // IntelliJ "Find Action"
    B(Cmd::GotoAnything, C, Key::P),
    B(Cmd::GotoAnything, C | S, Key::N),        // IntelliJ "Go to File"
    B(Cmd::GotoLine, C, Key::G),
    B(Cmd::Find, C, Key::F),
    B(Cmd::FindInFiles, C | S, Key::F),          // IntelliJ / Sublime
    B(Cmd::Replace, C, Key::H),
    B(Cmd::Replace, C, Key::R),                 // IntelliJ
    B(Cmd::ReplaceInFiles, C | S, Key::R),       // IntelliJ "Replace in Path"
    B(Cmd::FindNext, 0, Key::F3),
    B(Cmd::FindPrev, S, Key::F3),
    B(Cmd::NextDiff, 0, Key::F7),               // IntelliJ
    B(Cmd::PrevDiff, S, Key::F7),               // IntelliJ
    B(Cmd::ToggleSidebar, A, Key::Num1),        // IntelliJ project tool window
    B(Cmd::PasteFromHistory, C | S, Key::V),    // IntelliJ clipboard history
    B(Cmd::ZoomIn, C, Key::Equals),
    B(Cmd::ZoomIn, C, Key::Plus),
    B(Cmd::ZoomOut, C, Key::Minus),
    B(Cmd::ZoomReset, C, Key::Num0),
    // edit
    B(Cmd::Undo, C, Key::Z),
    B(Cmd::Redo, C | S, Key::Z),
    B(Cmd::Redo, C, Key::Y),
    B(Cmd::SelectAll, C, Key::A),
    B(Cmd::SelectLine, C, Key::L),
    B(Cmd::ExpandSelection, C, Key::W),         // IntelliJ
    B(Cmd::ShrinkSelection, C | S, Key::W),     // IntelliJ
    B(Cmd::ShowDiff, C, Key::D),                // IntelliJ "Show Diff"
    B(Cmd::ShowHistory, A | S, Key::H),         // Local History
    B(Cmd::SelectNextOccurrence, A, Key::J),    // IntelliJ
    B(Cmd::UnselectOccurrence, A | S, Key::J),  // IntelliJ
    B(Cmd::SelectAllOccurrences, A, Key::F3),   // Sublime
    B(Cmd::SelectAllOccurrences, C | A | S, Key::J), // IntelliJ
    B(Cmd::SplitIntoLines, C | S, Key::L),
    B(Cmd::AddCaretAbove, C | A, Key::ArrowUp),
    B(Cmd::AddCaretBelow, C | A, Key::ArrowDown),
    B(Cmd::ToggleColumnMode, A | S, Key::Insert),  // IntelliJ
    B(Cmd::CaretsAtLineEnds, A | S, Key::G),       // IntelliJ
    B(Cmd::Escape, 0, Key::Escape),
    B(Cmd::Duplicate, C | S, Key::D),
    B(Cmd::DeleteLine, C | S, Key::K),
    B(Cmd::MoveLineUp, C | S, Key::ArrowUp),
    B(Cmd::MoveLineDown, C | S, Key::ArrowDown),
    B(Cmd::MoveLineUp, A | S, Key::ArrowUp),    // IntelliJ
    B(Cmd::MoveLineDown, A | S, Key::ArrowDown),// IntelliJ
    B(Cmd::ToggleComment, C, Key::Slash),
    B(Cmd::Indent, C, Key::CloseBracket),
    B(Cmd::Outdent, C, Key::OpenBracket),
    B(Cmd::JoinLines, C, Key::J),
    B(Cmd::JoinLines, C | S, Key::J),           // IntelliJ
    B(Cmd::SortLines, 0, Key::F9),
    B(Cmd::ToggleCase, C | S, Key::U),          // IntelliJ
    B(Cmd::InsertLineBelow, C, Key::Enter),
    B(Cmd::InsertLineBelow, S, Key::Enter),     // IntelliJ "Start New Line"
    B(Cmd::InsertLineAbove, C | S, Key::Enter),
    B(Cmd::GotoBracket, C, Key::M),
    B(Cmd::GotoBracket, C | S, Key::M),         // IntelliJ
    B(Cmd::Backspace(false), 0, Key::Backspace),
    B(Cmd::Backspace(false), S, Key::Backspace),
    B(Cmd::Backspace(true), C, Key::Backspace),
    B(Cmd::DeleteFwd(false), 0, Key::Delete),
    B(Cmd::DeleteFwd(true), C, Key::Delete),
    B(Cmd::Newline, 0, Key::Enter),
    B(Cmd::Tab, 0, Key::Tab),
    B(Cmd::Outdent, S, Key::Tab),
];

fn bits(m: Modifiers) -> u8 {
    (if m.ctrl || m.command { C } else { 0 }) | (if m.shift { S } else { 0 }) | (if m.alt { A } else { 0 })
}

pub fn lookup(key: Key, mods: Modifiers) -> Option<Cmd> {
    let ctrl = mods.ctrl || mods.command;
    let (shift, alt) = (mods.shift, mods.alt);
    // Caret movement: shift extends; ctrl = word / document.
    if !alt {
        let m = match key {
            Key::ArrowLeft => Some(if ctrl { Motion::WordLeft } else { Motion::Left }),
            Key::ArrowRight => Some(if ctrl { Motion::WordRight } else { Motion::Right }),
            Key::ArrowUp if !ctrl => Some(Motion::Up),
            Key::ArrowDown if !ctrl => Some(Motion::Down),
            Key::Home => Some(if ctrl { Motion::DocStart } else { Motion::Home }),
            Key::End => Some(if ctrl { Motion::DocEnd } else { Motion::End }),
            Key::PageUp if !ctrl => Some(Motion::PageUp(0)),
            Key::PageDown if !ctrl => Some(Motion::PageDown(0)),
            _ => None,
        };
        if let Some(m) = m {
            return Some(Cmd::Move(m, shift));
        }
    }
    let b = bits(mods);
    BINDINGS.iter().find(|x| x.2 == key && x.1 == b).map(|x| x.0)
}

/// Human-readable shortcut for a command (first binding).
pub fn shortcut(cmd: Cmd) -> String {
    let Some(b) = BINDINGS.iter().find(|x| x.0 == cmd) else {
        return String::new();
    };
    let mut s = String::new();
    if b.1 & C != 0 {
        s += "Ctrl+";
    }
    if b.1 & A != 0 {
        s += "Alt+";
    }
    if b.1 & S != 0 {
        s += "Shift+";
    }
    s += match b.2 {
        Key::Slash => "/",
        Key::OpenBracket => "[",
        Key::CloseBracket => "]",
        Key::Equals => "=",
        Key::Minus => "-",
        Key::ArrowUp => "Up",
        Key::ArrowDown => "Down",
        k => k.name(),
    };
    s
}
