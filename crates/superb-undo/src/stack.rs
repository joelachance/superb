//! The [`UndoStack`].

use std::time::{Duration, Instant};

use superb_core::{Result, SuperbError};

use crate::{CoalesceKey, Command};

/// One undo step: one command, a coalesced run, or a group.
struct Entry<T: 'static> {
    steps: Vec<Box<dyn Command<T>>>,
    label: String,
    key: Option<CoalesceKey>,
    at: Instant,
    /// Sealed entries never accept further coalescing.
    sealed: bool,
}

impl<T: 'static> Entry<T> {
    fn revert_all(&mut self, target: &mut T) -> Result<()> {
        for step in self.steps.iter_mut().rev() {
            step.revert(target)?;
        }
        Ok(())
    }

    fn apply_all(&mut self, target: &mut T) -> Result<()> {
        for step in self.steps.iter_mut() {
            step.apply(target)?;
        }
        Ok(())
    }
}

struct OpenGroup<T: 'static> {
    label: String,
    steps: Vec<Box<dyn Command<T>>>,
}

/// Undo/redo history for a document of type `T`.
///
/// * [`push`](UndoStack::push) applies a command and records it. Consecutive
///   commands with the same [`CoalesceKey`] collapse into one step until
///   [`seal`](UndoStack::seal) is called, a command with another key arrives,
///   or the optional time window elapses.
/// * [`begin_group`](UndoStack::begin_group) / [`end_group`](UndoStack::end_group)
///   bundle any number of pushes into one step (groups nest; only the
///   outermost creates the step).
/// * [`mark_clean`](UndoStack::mark_clean) / [`is_clean`](UndoStack::is_clean)
///   track whether the document matches its last saved state.
/// * [`set_limit`](UndoStack::set_limit) bounds the number of undo steps.
///
/// A failed `apply` in `push` records nothing. A failed `revert`/`apply`
/// during undo/redo leaves the document in an unknown state, so the whole
/// history is cleared before the error is returned.
pub struct UndoStack<T: 'static> {
    undo: Vec<Entry<T>>,
    redo: Vec<Entry<T>>,
    limit: usize,
    window: Option<Duration>,
    open: Option<OpenGroup<T>>,
    depth: usize,
    /// `undo.len()` at the last `mark_clean`; `None` if that state can no
    /// longer be reached by undo/redo.
    clean: Option<usize>,
}

impl<T: 'static> Default for UndoStack<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T: 'static> UndoStack<T> {
    /// Default maximum number of undo steps.
    pub const DEFAULT_LIMIT: usize = 1_000;

    /// Creates an empty, clean history with [`DEFAULT_LIMIT`](Self::DEFAULT_LIMIT)
    /// and no time window (same-key edits coalesce until sealed).
    pub fn new() -> Self {
        Self {
            undo: Vec::new(),
            redo: Vec::new(),
            limit: Self::DEFAULT_LIMIT,
            window: None,
            open: None,
            depth: 0,
            clean: Some(0),
        }
    }

    /// Creates a history holding at most `limit` undo steps.
    pub fn with_limit(limit: usize) -> Self {
        let mut s = Self::new();
        s.set_limit(limit);
        s
    }

    /// Maximum number of undo steps kept.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// Changes the depth limit, dropping the oldest steps if necessary.
    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit.max(1);
        self.trim();
    }

    /// Time window for coalescing: edits with the same key merge only if
    /// pushed within `window` of the previous one. `None` (the default)
    /// merges regardless of time.
    pub fn set_coalesce_window(&mut self, window: Option<Duration>) {
        self.window = window;
    }

    /// The current coalescing window.
    pub fn coalesce_window(&self) -> Option<Duration> {
        self.window
    }

    // ------------------------------------------------------------------
    // Recording
    // ------------------------------------------------------------------

    /// Applies `command` to `target` and records it.
    pub fn push(&mut self, target: &mut T, command: impl Command<T>) -> Result<()> {
        self.push_boxed(target, Box::new(command))
    }

    /// Boxed form of [`push`](Self::push).
    pub fn push_boxed(&mut self, target: &mut T, command: Box<dyn Command<T>>) -> Result<()> {
        self.push_at(target, command, Instant::now())
    }

    /// [`push_boxed`](Self::push_boxed) with an explicit timestamp for the
    /// coalescing window (useful for deterministic tests and replays).
    pub fn push_at(
        &mut self,
        target: &mut T,
        mut command: Box<dyn Command<T>>,
        at: Instant,
    ) -> Result<()> {
        command.apply(target)?;
        let key = command.coalesce_key();

        if let Some(group) = self.open.as_mut() {
            match (key, group.steps.last_mut()) {
                (Some(k), Some(last)) if last.coalesce_key() == Some(k) => {
                    if let Err(refused) = last.merge(command) {
                        group.steps.push(refused);
                    }
                }
                _ => group.steps.push(command),
            }
            return Ok(());
        }

        self.clear_redo();
        if let (Some(k), Some(last)) = (key, self.undo.last_mut()) {
            let in_window = self
                .window
                .map_or(true, |w| at.saturating_duration_since(last.at) <= w);
            if !last.sealed && last.key == Some(k) && in_window {
                let tail = last.steps.last_mut().expect("entries are never empty");
                if let Err(refused) = tail.merge(command) {
                    last.steps.push(refused);
                }
                last.at = at;
                // The saved state (if it was "after this entry") is now
                // strictly inside the entry and unreachable.
                if self.clean == Some(self.undo.len()) {
                    self.clean = None;
                }
                return Ok(());
            }
        }

        let label = command.label().to_owned();
        self.undo.push(Entry {
            steps: vec![command],
            label,
            key,
            at,
            sealed: false,
        });
        self.trim();
        Ok(())
    }

    /// Ends the current coalescing run: the next push always starts a new
    /// step. Call this on "mouse up" after a drag.
    pub fn seal(&mut self) {
        if let Some(last) = self.undo.last_mut() {
            last.sealed = true;
        }
    }

    /// Opens a group: every push until the matching
    /// [`end_group`](Self::end_group) becomes part of one undo step labelled
    /// `label`. Groups nest; inner labels are ignored.
    pub fn begin_group(&mut self, label: impl Into<String>) {
        self.depth += 1;
        if self.depth == 1 {
            self.open = Some(OpenGroup {
                label: label.into(),
                steps: Vec::new(),
            });
        }
    }

    /// Closes the innermost group. Closing the outermost group records the
    /// collected commands as one step (nothing if the group was empty).
    /// Returns `true` if a step was recorded.
    pub fn end_group(&mut self) -> bool {
        if self.depth == 0 {
            return false;
        }
        self.depth -= 1;
        if self.depth > 0 {
            return false;
        }
        let group = self.open.take().expect("open group at depth 1");
        if group.steps.is_empty() {
            return false;
        }
        self.clear_redo();
        self.undo.push(Entry {
            steps: group.steps,
            label: group.label,
            key: None,
            at: Instant::now(),
            sealed: true,
        });
        self.trim();
        true
    }

    /// Reverts and discards everything pushed since the outermost
    /// [`begin_group`](Self::begin_group), closing all nested groups.
    pub fn cancel_group(&mut self, target: &mut T) -> Result<()> {
        self.depth = 0;
        match self.open.take() {
            Some(mut group) => {
                let result = group
                    .steps
                    .iter_mut()
                    .rev()
                    .try_for_each(|s| s.revert(target));
                if result.is_err() {
                    self.clear();
                }
                result
            }
            None => Ok(()),
        }
    }

    /// `true` while a group is open.
    pub fn in_group(&self) -> bool {
        self.depth > 0
    }

    /// Nesting depth of open groups.
    pub fn group_depth(&self) -> usize {
        self.depth
    }

    // ------------------------------------------------------------------
    // Walking
    // ------------------------------------------------------------------

    /// Reverts the most recent step. Returns `Ok(false)` if there is nothing
    /// to undo. Fails if a group is open.
    pub fn undo(&mut self, target: &mut T) -> Result<bool> {
        self.ensure_no_group("undo")?;
        let Some(mut entry) = self.undo.pop() else {
            return Ok(false);
        };
        if let Err(e) = entry.revert_all(target) {
            self.clear();
            return Err(e);
        }
        entry.sealed = true;
        self.redo.push(entry);
        Ok(true)
    }

    /// Re-applies the most recently undone step. Returns `Ok(false)` if
    /// there is nothing to redo. Fails if a group is open.
    pub fn redo(&mut self, target: &mut T) -> Result<bool> {
        self.ensure_no_group("redo")?;
        let Some(mut entry) = self.redo.pop() else {
            return Ok(false);
        };
        if let Err(e) = entry.apply_all(target) {
            self.clear();
            return Err(e);
        }
        self.undo.push(entry);
        Ok(true)
    }

    /// `true` if [`undo`](Self::undo) would do something.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty() && self.depth == 0
    }

    /// `true` if [`redo`](Self::redo) would do something.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty() && self.depth == 0
    }

    /// Label of the step [`undo`](Self::undo) would revert.
    pub fn undo_label(&self) -> Option<&str> {
        self.undo.last().map(|e| e.label.as_str())
    }

    /// Label of the step [`redo`](Self::redo) would re-apply.
    pub fn redo_label(&self) -> Option<&str> {
        self.redo.last().map(|e| e.label.as_str())
    }

    /// Number of undo steps.
    pub fn undo_len(&self) -> usize {
        self.undo.len()
    }

    /// Number of redo steps.
    pub fn redo_len(&self) -> usize {
        self.redo.len()
    }

    /// Labels of the undo steps, most recent first.
    pub fn undo_labels(&self) -> impl Iterator<Item = &str> + '_ {
        self.undo.iter().rev().map(|e| e.label.as_str())
    }

    /// Labels of the redo steps, next-to-redo first.
    pub fn redo_labels(&self) -> impl Iterator<Item = &str> + '_ {
        self.redo.iter().rev().map(|e| e.label.as_str())
    }

    /// Drops all history (and any open group without reverting it). The
    /// current state becomes the clean state.
    pub fn clear(&mut self) {
        self.undo.clear();
        self.redo.clear();
        self.open = None;
        self.depth = 0;
        self.clean = Some(0);
    }

    // ------------------------------------------------------------------
    // Clean state
    // ------------------------------------------------------------------

    /// Records that the document was just saved.
    pub fn mark_clean(&mut self) {
        self.clean = Some(self.undo.len());
    }

    /// `true` if the document matches the state at the last
    /// [`mark_clean`](Self::mark_clean) (or creation). Open groups count as
    /// dirty.
    pub fn is_clean(&self) -> bool {
        self.depth == 0 && self.clean == Some(self.undo.len())
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    fn ensure_no_group(&self, what: &'static str) -> Result<()> {
        if self.depth > 0 {
            return Err(SuperbError::Message(format!(
                "cannot {what} while an undo group is open"
            )));
        }
        Ok(())
    }

    fn clear_redo(&mut self) {
        if !self.redo.is_empty() {
            self.redo.clear();
            if self.clean.is_some_and(|c| c > self.undo.len()) {
                self.clean = None;
            }
        }
    }

    fn trim(&mut self) {
        while self.undo.len() > self.limit {
            self.undo.remove(0);
            self.clean = self.clean.and_then(|c| c.checked_sub(1));
        }
    }
}

impl<T: 'static> core::fmt::Debug for UndoStack<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UndoStack")
            .field("undo", &self.undo.len())
            .field("redo", &self.redo.len())
            .field("limit", &self.limit)
            .field("group_depth", &self.depth)
            .field("clean", &self.is_clean())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PropertyEdit;

    #[derive(Default, Debug, PartialEq)]
    struct Doc {
        n: i32,
        name: String,
    }

    /// Adds `delta`; never merges.
    struct Add(i32);
    impl Command<Doc> for Add {
        fn label(&self) -> &str {
            "Add"
        }
        fn apply(&mut self, d: &mut Doc) -> Result<()> {
            d.n += self.0;
            Ok(())
        }
        fn revert(&mut self, d: &mut Doc) -> Result<()> {
            d.n -= self.0;
            Ok(())
        }
    }

    /// Adds `delta` with a coalesce key but refuses to merge (composite path).
    struct KeyedAdd(i32);
    impl Command<Doc> for KeyedAdd {
        fn label(&self) -> &str {
            "Keyed add"
        }
        fn apply(&mut self, d: &mut Doc) -> Result<()> {
            d.n += self.0;
            Ok(())
        }
        fn revert(&mut self, d: &mut Doc) -> Result<()> {
            d.n -= self.0;
            Ok(())
        }
        fn coalesce_key(&self) -> Option<CoalesceKey> {
            Some(CoalesceKey::new(9, 9))
        }
    }

    struct Failing {
        on_apply: bool,
    }
    impl Command<Doc> for Failing {
        fn label(&self) -> &str {
            "Failing"
        }
        fn apply(&mut self, _: &mut Doc) -> Result<()> {
            if self.on_apply {
                Err(SuperbError::InvalidArgument { what: "apply" })
            } else {
                Ok(())
            }
        }
        fn revert(&mut self, _: &mut Doc) -> Result<()> {
            Err(SuperbError::InvalidArgument { what: "revert" })
        }
    }

    const N_KEY: CoalesceKey = CoalesceKey::new(1, 0);

    fn set_n(v: i32) -> PropertyEdit<Doc, i32> {
        PropertyEdit::new(
            "Set n",
            Some(N_KEY),
            v,
            |d: &Doc| d.n,
            |d: &mut Doc, v| {
                d.n = v;
                Ok(())
            },
        )
    }

    fn set_name(v: &str) -> PropertyEdit<Doc, String> {
        PropertyEdit::new(
            "Set name",
            Some(CoalesceKey::new(2, 0)),
            v.to_owned(),
            |d: &Doc| d.name.clone(),
            |d: &mut Doc, v| {
                d.name = v;
                Ok(())
            },
        )
    }

    #[test]
    fn push_undo_redo_round_trip() {
        let mut doc = Doc::default();
        let mut h = UndoStack::new();
        assert!(h.is_clean());
        assert!(!h.can_undo() && !h.can_redo());
        assert!(!h.undo(&mut doc).unwrap());
        assert!(!h.redo(&mut doc).unwrap());

        h.push(&mut doc, Add(5)).unwrap();
        h.push(&mut doc, set_name("lead")).unwrap();
        assert_eq!(
            doc,
            Doc {
                n: 5,
                name: "lead".into()
            }
        );
        assert_eq!(h.undo_len(), 2);
        assert_eq!(h.undo_label(), Some("Set name"));
        assert_eq!(h.undo_labels().collect::<Vec<_>>(), ["Set name", "Add"]);
        assert!(!h.is_clean());

        assert!(h.undo(&mut doc).unwrap());
        assert_eq!(doc.name, "");
        assert_eq!(h.redo_label(), Some("Set name"));
        assert!(h.undo(&mut doc).unwrap());
        assert_eq!(doc, Doc::default());
        assert!(h.is_clean(), "back at the initial state");
        assert_eq!(h.redo_labels().collect::<Vec<_>>(), ["Add", "Set name"]);

        assert!(h.redo(&mut doc).unwrap());
        assert!(h.redo(&mut doc).unwrap());
        assert!(!h.redo(&mut doc).unwrap());
        assert_eq!(
            doc,
            Doc {
                n: 5,
                name: "lead".into()
            }
        );
        assert_eq!(
            format!("{h:?}"),
            "UndoStack { undo: 2, redo: 0, limit: 1000, group_depth: 0, clean: false }"
        );
    }

    #[test]
    fn a_new_push_discards_redo() {
        let mut doc = Doc::default();
        let mut h = UndoStack::new();
        h.push(&mut doc, Add(1)).unwrap();
        h.push(&mut doc, Add(2)).unwrap();
        h.undo(&mut doc).unwrap();
        assert_eq!(h.redo_len(), 1);
        h.push(&mut doc, Add(10)).unwrap();
        assert_eq!(h.redo_len(), 0);
        assert_eq!(doc.n, 11);
        assert!(!h.can_redo());
    }

    #[test]
    fn same_key_edits_coalesce_until_sealed() {
        let mut doc = Doc::default();
        let mut h = UndoStack::new();
        h.push(&mut doc, set_n(1)).unwrap();
        h.push(&mut doc, set_n(2)).unwrap();
        h.push(&mut doc, set_n(3)).unwrap();
        assert_eq!(h.undo_len(), 1);
        assert_eq!(doc.n, 3);
        // A different key breaks the run.
        h.push(&mut doc, set_name("a")).unwrap();
        h.push(&mut doc, set_name("ab")).unwrap();
        assert_eq!(h.undo_len(), 2);
        // Back to the first key: a new run, because the last entry has another key.
        h.push(&mut doc, set_n(4)).unwrap();
        assert_eq!(h.undo_len(), 3);
        h.seal();
        h.push(&mut doc, set_n(5)).unwrap();
        assert_eq!(h.undo_len(), 4);
        // Keyless commands never coalesce.
        h.push(&mut doc, Add(1)).unwrap();
        h.push(&mut doc, Add(1)).unwrap();
        assert_eq!(h.undo_len(), 6);

        h.undo(&mut doc).unwrap();
        h.undo(&mut doc).unwrap();
        h.undo(&mut doc).unwrap(); // 5 -> 4
        assert_eq!(doc.n, 4);
        h.undo(&mut doc).unwrap(); // 4 -> 3
        assert_eq!(doc.n, 3);
        h.undo(&mut doc).unwrap(); // name run
        assert_eq!(doc.name, "");
        h.undo(&mut doc).unwrap(); // whole 1,2,3 run
        assert_eq!(doc.n, 0);
        assert!(h.is_clean());
    }

    #[test]
    fn coalescing_window_expires() {
        let mut doc = Doc::default();
        let mut h = UndoStack::new();
        h.set_coalesce_window(Some(Duration::from_millis(500)));
        assert_eq!(h.coalesce_window(), Some(Duration::from_millis(500)));
        let t0 = Instant::now();
        h.push_at(&mut doc, Box::new(set_n(1)), t0).unwrap();
        h.push_at(
            &mut doc,
            Box::new(set_n(2)),
            t0 + Duration::from_millis(400),
        )
        .unwrap();
        assert_eq!(h.undo_len(), 1);
        // Measured from the previous push, not the first.
        h.push_at(
            &mut doc,
            Box::new(set_n(3)),
            t0 + Duration::from_millis(800),
        )
        .unwrap();
        assert_eq!(h.undo_len(), 1);
        h.push_at(
            &mut doc,
            Box::new(set_n(4)),
            t0 + Duration::from_millis(1400),
        )
        .unwrap();
        assert_eq!(h.undo_len(), 2);
        // Timestamps in the past never panic.
        h.push_at(&mut doc, Box::new(set_n(5)), t0).unwrap();
        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn refused_merge_keeps_both_commands_in_one_step() {
        let mut doc = Doc::default();
        let mut h = UndoStack::new();
        h.push(&mut doc, KeyedAdd(1)).unwrap();
        h.push(&mut doc, KeyedAdd(10)).unwrap();
        h.push(&mut doc, KeyedAdd(100)).unwrap();
        assert_eq!(doc.n, 111);
        assert_eq!(h.undo_len(), 1);
        h.undo(&mut doc).unwrap();
        assert_eq!(doc.n, 0);
        h.redo(&mut doc).unwrap();
        assert_eq!(doc.n, 111);
        // After undo/redo the entry is sealed: no further coalescing.
        h.push(&mut doc, KeyedAdd(1)).unwrap();
        assert_eq!(h.undo_len(), 2);
    }

    #[test]
    fn groups_collapse_into_one_step() {
        let mut doc = Doc::default();
        let mut h = UndoStack::new();
        h.begin_group("Import");
        assert!(h.in_group());
        assert!(!h.can_undo());
        h.push(&mut doc, Add(1)).unwrap();
        h.begin_group("inner (ignored)");
        assert_eq!(h.group_depth(), 2);
        h.push(&mut doc, set_name("x")).unwrap();
        h.push(&mut doc, set_name("xy")).unwrap(); // merges inside the group
        assert!(!h.end_group());
        assert!(h.in_group());
        assert!(h.undo(&mut doc).is_err(), "undo inside a group is refused");
        assert!(h.redo(&mut doc).is_err());
        assert!(!h.is_clean());
        assert!(h.end_group());
        assert!(!h.in_group());
        assert_eq!(h.undo_len(), 1);
        assert_eq!(h.undo_label(), Some("Import"));
        assert_eq!(
            doc,
            Doc {
                n: 1,
                name: "xy".into()
            }
        );
        h.undo(&mut doc).unwrap();
        assert_eq!(doc, Doc::default());
        h.redo(&mut doc).unwrap();
        assert_eq!(
            doc,
            Doc {
                n: 1,
                name: "xy".into()
            }
        );

        // Empty groups record nothing; stray end_group is harmless.
        h.begin_group("nothing");
        assert!(!h.end_group());
        assert!(!h.end_group());
        assert_eq!(h.undo_len(), 1);
        // A group does not coalesce with a following same-key push.
        h.begin_group("g");
        h.push(&mut doc, set_n(7)).unwrap();
        h.end_group();
        h.push(&mut doc, set_n(8)).unwrap();
        assert_eq!(h.undo_len(), 3);
    }

    #[test]
    fn a_group_push_discards_redo() {
        let mut doc = Doc::default();
        let mut h = UndoStack::new();
        h.push(&mut doc, Add(1)).unwrap();
        h.undo(&mut doc).unwrap();
        h.begin_group("g");
        h.push(&mut doc, Add(2)).unwrap();
        h.end_group();
        assert_eq!(h.redo_len(), 0);
        assert_eq!(doc.n, 2);
    }

    #[test]
    fn cancel_group_reverts_everything_since_begin() {
        let mut doc = Doc::default();
        let mut h = UndoStack::new();
        h.push(&mut doc, Add(1)).unwrap();
        h.begin_group("outer");
        h.push(&mut doc, Add(10)).unwrap();
        h.begin_group("inner");
        h.push(&mut doc, set_name("z")).unwrap();
        h.cancel_group(&mut doc).unwrap();
        assert_eq!(
            doc,
            Doc {
                n: 1,
                name: String::new()
            }
        );
        assert!(!h.in_group());
        assert_eq!(h.undo_len(), 1);
        h.cancel_group(&mut doc).unwrap(); // nothing open
    }

    #[test]
    fn limit_drops_oldest_and_keeps_clean_index_honest() {
        let mut doc = Doc::default();
        let mut h = UndoStack::with_limit(3);
        assert_eq!(h.limit(), 3);
        h.push(&mut doc, Add(1)).unwrap();
        h.mark_clean();
        h.push(&mut doc, Add(2)).unwrap();
        h.push(&mut doc, Add(3)).unwrap();
        h.push(&mut doc, Add(4)).unwrap();
        assert_eq!(h.undo_len(), 3);
        // Clean state (after Add(1)) is still reachable: undo everything.
        for _ in 0..3 {
            h.undo(&mut doc).unwrap();
        }
        assert_eq!(doc.n, 1);
        assert!(h.is_clean());
        for _ in 0..3 {
            h.redo(&mut doc).unwrap();
        }
        h.push(&mut doc, Add(5)).unwrap();
        assert_eq!(h.undo_len(), 3);
        for _ in 0..3 {
            h.undo(&mut doc).unwrap();
        }
        assert!(!h.is_clean(), "clean state fell off the end of the history");
        assert_eq!(doc.n, 3);

        h.set_limit(0);
        assert_eq!(h.limit(), 1, "limit is at least one");
        h.push(&mut doc, Add(1)).unwrap();
        h.push(&mut doc, Add(1)).unwrap();
        assert_eq!(h.undo_len(), 1);
        UndoStack::<Doc>::default();
    }

    #[test]
    fn clean_tracking_across_undo_redo_and_divergence() {
        let mut doc = Doc::default();
        let mut h = UndoStack::new();
        h.push(&mut doc, Add(1)).unwrap();
        h.push(&mut doc, Add(2)).unwrap();
        h.mark_clean();
        assert!(h.is_clean());
        h.undo(&mut doc).unwrap();
        assert!(!h.is_clean());
        h.redo(&mut doc).unwrap();
        assert!(h.is_clean());
        h.undo(&mut doc).unwrap();
        h.push(&mut doc, Add(3)).unwrap(); // diverges: clean state is in the discarded redo
        assert!(!h.is_clean());
        h.undo(&mut doc).unwrap();
        assert!(
            !h.is_clean(),
            "same depth as the saved state but different content"
        );
        h.mark_clean();
        assert!(h.is_clean());

        // Coalescing onto the clean entry makes the state unreachable.
        h.push(&mut doc, set_n(1)).unwrap();
        h.mark_clean();
        h.push(&mut doc, set_n(2)).unwrap();
        assert!(!h.is_clean());
        h.undo(&mut doc).unwrap();
        assert!(!h.is_clean());

        h.clear();
        assert!(h.is_clean());
        assert_eq!(h.undo_len() + h.redo_len(), 0);
    }

    #[test]
    fn failures_are_contained() {
        let mut doc = Doc::default();
        let mut h = UndoStack::new();
        h.push(&mut doc, Add(1)).unwrap();
        assert!(h.push(&mut doc, Failing { on_apply: true }).is_err());
        assert_eq!(h.undo_len(), 1, "failed apply records nothing");
        assert_eq!(doc.n, 1);

        h.push(&mut doc, Failing { on_apply: false }).unwrap();
        assert_eq!(h.undo_len(), 2);
        assert!(h.undo(&mut doc).is_err());
        assert_eq!(h.undo_len(), 0, "history is cleared after a failed revert");
        assert!(h.is_clean());

        // Failure inside a cancelled group also clears.
        h.push(&mut doc, Add(1)).unwrap();
        h.begin_group("g");
        h.push(&mut doc, Failing { on_apply: false }).unwrap();
        assert!(h.cancel_group(&mut doc).is_err());
        assert_eq!(h.undo_len(), 0);
        assert!(!h.in_group());
    }

    #[test]
    fn stack_is_send_for_send_documents() {
        fn assert_send<S: Send>(_: &S) {}
        let h = UndoStack::<Doc>::new();
        assert_send(&h);
    }
}
