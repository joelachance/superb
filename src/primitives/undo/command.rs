//! The [`Command`] trait, coalescing keys, downcasting and [`PropertyEdit`].

use std::any::{Any, TypeId};
use std::sync::Arc;

use crate::primitives::Result;

/// Identifies a run of edits that may collapse into one undo step.
///
/// Two consecutive commands with equal keys are candidates for merging (see
/// [`Command::merge`]). `kind` says what is being edited (one constant per
/// property type), `target` says on which object (typically a raw ID).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct CoalesceKey {
    /// What kind of edit this is.
    pub kind: u64,
    /// Which object is edited.
    pub target: u64,
}

impl CoalesceKey {
    /// Creates a key.
    pub const fn new(kind: u64, target: u64) -> Self {
        Self { kind, target }
    }
}

/// A reversible edit of a document of type `T`.
///
/// Contract:
/// * `apply` is called once when the command is pushed and again on every
///   redo; `revert` is called on every undo. The two must be inverses.
/// * A command should capture whatever it needs to revert *during* `apply`
///   (for example the previous value), not at construction time, so that a
///   redo after other edits still restores the right state.
/// * `revert`/redo failures leave the document in an unknown state; the
///   [`UndoStack`](crate::primitives::undo::UndoStack) clears its history when that happens.
pub trait Command<T>: Any + Send {
    /// Human-readable label ("Move clip"), shown as "Undo Move clip".
    fn label(&self) -> &str;

    /// Performs the edit.
    fn apply(&mut self, target: &mut T) -> Result<()>;

    /// Undoes the edit.
    fn revert(&mut self, target: &mut T) -> Result<()>;

    /// Key used to coalesce consecutive edits. `None` (the default) never
    /// coalesces.
    fn coalesce_key(&self) -> Option<CoalesceKey> {
        None
    }

    /// Absorbs `next` into `self` so that reverting `self` also reverts
    /// `next`. Both commands have already been applied, in order, and have
    /// the same coalesce key.
    ///
    /// Return `Err(next)` to refuse (the default); the stack then keeps
    /// `next` as a separate command inside the same undo step, which is
    /// always correct but uses more memory. Use
    /// `downcast` (defined on `dyn Command<T>`) to look inside `next`.
    fn merge(
        &mut self,
        next: Box<dyn Command<T>>,
    ) -> core::result::Result<(), Box<dyn Command<T>>> {
        Err(next)
    }
}

// `Command<T>: Any`, so a `dyn Command<T>` can be upcast to `dyn Any` and
// the standard library's checked downcasts do the rest. No unsafe needed.
impl<T: 'static> dyn Command<T> {
    /// `true` if the concrete type of this command is `C`.
    pub fn is<C: Command<T>>(&self) -> bool {
        Any::type_id(self) == TypeId::of::<C>()
    }

    /// Borrows the command as its concrete type.
    pub fn downcast_ref<C: Command<T>>(&self) -> Option<&C> {
        (self as &dyn Any).downcast_ref::<C>()
    }

    /// Mutably borrows the command as its concrete type.
    pub fn downcast_mut<C: Command<T>>(&mut self) -> Option<&mut C> {
        (self as &mut dyn Any).downcast_mut::<C>()
    }

    /// Converts the boxed command into its concrete type, or hands it back.
    pub fn downcast<C: Command<T>>(self: Box<Self>) -> core::result::Result<Box<C>, Box<Self>> {
        if !self.is::<C>() {
            return Err(self);
        }
        let any: Box<dyn Any> = self;
        match any.downcast::<C>() {
            Ok(command) => Ok(command),
            Err(_) => unreachable!("`is::<C>()` held, so the downcast cannot fail"),
        }
    }
}

impl<T: 'static> core::fmt::Debug for dyn Command<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Command")
            .field("label", &self.label())
            .field("key", &self.coalesce_key())
            .finish()
    }
}

type Getter<T, V> = Arc<dyn Fn(&T) -> V + Send + Sync>;
type Setter<T, V> = Arc<dyn Fn(&mut T, V) -> Result<()> + Send + Sync>;

/// A "set this value" command built from getter/setter closures.
///
/// `apply` remembers the previous value and stores the new one; `revert`
/// restores the previous value. Consecutive edits with the same
/// [`CoalesceKey`] merge by keeping the first edit's previous value and the
/// latest edit's new value, so a whole fader drag is one undo step.
pub struct PropertyEdit<T, V> {
    label: String,
    key: Option<CoalesceKey>,
    previous: Option<V>,
    value: V,
    get: Getter<T, V>,
    set: Setter<T, V>,
}

impl<T, V> PropertyEdit<T, V> {
    /// Creates an edit that will store `value` through `set`, remembering
    /// what `get` returned beforehand.
    pub fn new(
        label: impl Into<String>,
        key: Option<CoalesceKey>,
        value: V,
        get: impl Fn(&T) -> V + Send + Sync + 'static,
        set: impl Fn(&mut T, V) -> Result<()> + Send + Sync + 'static,
    ) -> Self {
        Self {
            label: label.into(),
            key,
            previous: None,
            value,
            get: Arc::new(get),
            set: Arc::new(set),
        }
    }

    /// The value this edit stores.
    pub fn value(&self) -> &V {
        &self.value
    }

    /// The value seen before the edit was applied (`None` until applied).
    pub fn previous(&self) -> Option<&V> {
        self.previous.as_ref()
    }
}

impl<T: 'static, V: Clone + Send + 'static> Command<T> for PropertyEdit<T, V> {
    fn label(&self) -> &str {
        &self.label
    }

    fn apply(&mut self, target: &mut T) -> Result<()> {
        let before = (self.get)(target);
        (self.set)(target, self.value.clone())?;
        self.previous = Some(before);
        Ok(())
    }

    fn revert(&mut self, target: &mut T) -> Result<()> {
        match self.previous.take() {
            Some(v) => (self.set)(target, v),
            None => Ok(()),
        }
    }

    fn coalesce_key(&self) -> Option<CoalesceKey> {
        self.key
    }

    fn merge(
        &mut self,
        next: Box<dyn Command<T>>,
    ) -> core::result::Result<(), Box<dyn Command<T>>> {
        let next = next.downcast::<Self>()?;
        self.value = next.value;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Doc {
        n: i32,
    }

    struct Add(i32);
    impl Command<Doc> for Add {
        fn label(&self) -> &str {
            "add"
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

    struct Mul(i32);
    impl Command<Doc> for Mul {
        fn label(&self) -> &str {
            "mul"
        }
        fn apply(&mut self, d: &mut Doc) -> Result<()> {
            d.n *= self.0;
            Ok(())
        }
        fn revert(&mut self, d: &mut Doc) -> Result<()> {
            d.n /= self.0;
            Ok(())
        }
    }

    #[test]
    fn downcasting_checks_the_concrete_type() {
        let mut boxed: Box<dyn Command<Doc>> = Box::new(Add(3));
        assert!(boxed.is::<Add>());
        assert!(!boxed.is::<Mul>());
        assert_eq!(boxed.downcast_ref::<Add>().map(|a| a.0), Some(3));
        assert!(boxed.downcast_ref::<Mul>().is_none());
        boxed.downcast_mut::<Add>().unwrap().0 = 4;
        assert!(boxed.downcast_mut::<Mul>().is_none());
        let boxed = boxed.downcast::<Mul>().err().unwrap();
        let add = boxed.downcast::<Add>().unwrap();
        assert_eq!(add.0, 4);
        let dbg = format!("{:?}", &*(Box::new(Mul(2)) as Box<dyn Command<Doc>>));
        assert!(dbg.contains("mul"));
    }

    #[test]
    fn default_merge_refuses() {
        let mut a: Box<dyn Command<Doc>> = Box::new(Add(1));
        assert!(a.coalesce_key().is_none());
        let refused = a.merge(Box::new(Add(2))).unwrap_err();
        assert_eq!(refused.downcast_ref::<Add>().unwrap().0, 2);
    }

    #[test]
    fn property_edit_round_trips_and_merges() {
        let key = CoalesceKey::new(7, 42);
        let mk = |v: i32| {
            PropertyEdit::new(
                "set",
                Some(key),
                v,
                |d: &Doc| d.n,
                |d: &mut Doc, v| {
                    d.n = v;
                    Ok(())
                },
            )
        };
        let mut doc = Doc { n: 10 };
        let mut e1 = mk(20);
        assert!(e1.previous().is_none());
        e1.apply(&mut doc).unwrap();
        assert_eq!(doc.n, 20);
        assert_eq!(e1.previous(), Some(&10));
        assert_eq!(*e1.value(), 20);
        assert_eq!(e1.coalesce_key(), Some(key));

        let mut e2 = mk(30);
        e2.apply(&mut doc).unwrap();
        e1.merge(Box::new(e2)).unwrap();
        assert_eq!(*e1.value(), 30);
        e1.revert(&mut doc).unwrap();
        assert_eq!(doc.n, 10);
        e1.apply(&mut doc).unwrap();
        assert_eq!(doc.n, 30);

        // A different command type is handed back untouched.
        let other = e1.merge(Box::new(Add(1))).unwrap_err();
        assert!(other.is::<Add>());
        // Reverting an unapplied edit is a no-op.
        let mut fresh = mk(99);
        fresh.revert(&mut doc).unwrap();
        assert_eq!(doc.n, 30);
    }

    #[test]
    fn property_edit_propagates_setter_errors() {
        let mut doc = Doc { n: 1 };
        let mut e = PropertyEdit::new(
            "bad",
            None,
            5,
            |d: &Doc| d.n,
            |_: &mut Doc, _| Err(crate::primitives::SuperbError::InvalidArgument { what: "n" }),
        );
        assert!(e.apply(&mut doc).is_err());
        assert!(e.previous().is_none());
        assert_eq!(doc.n, 1);
        assert_eq!(e.label(), "bad");
    }
}
