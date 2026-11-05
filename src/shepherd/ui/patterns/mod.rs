//! Multi-agent pattern gallery, interactive editor, and topology visualization.

pub mod diagram;
pub mod editor;
pub mod gallery;
pub mod state;

#[cfg(test)]
mod tests;

#[allow(unused_imports)]
pub use diagram::generate_topology_diagram;
#[allow(unused_imports)]
pub use editor::{render_pattern_editor, EditorSection, PatternEditorHits, PatternEditorState};
pub use gallery::render_patterns_section;
#[allow(unused_imports)]
pub use state::{
    DuplicatePatternState, NewFromTemplateState, PatternRunFormState, PatternSubAction,
    ShepherdPatternsAction, ShepherdPatternsHits, ShepherdPatternsSectionState,
};

#[allow(unused_imports)]
pub use shepherd_core::patterns::{
    get_builtin_pattern, get_builtin_template, is_builtin_template, list_patterns, validate,
    Pattern, PatternAgent, PatternDetail, PatternKind, PatternSource, PatternSummary, Problem,
    Severity, Topic,
};
