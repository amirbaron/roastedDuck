//! The members every expression binder embeds, and macros implementing the accessor
//! boilerplate of [`ExpressionBinder`](super::ExpressionBinder),
//! [`ColumnQualifier`](super::ColumnQualifier) and [`Scope`](crate::binder::Scope).

use rustdb_types::LogicalType;

use super::ExpressionBinderState;
use crate::binder::{Binder, Scope};

/// An expression binder's query level, the levels enclosing it, and its state.
pub struct BinderBase<'a> {
    pub binder: &'a mut Binder,
    pub outer: Option<&'a mut dyn Scope>,
    pub state: ExpressionBinderState,
}

impl std::fmt::Debug for BinderBase<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BinderBase")
            .field("state", &self.state)
            .finish_non_exhaustive()
    }
}

impl<'a> BinderBase<'a> {
    pub fn new(binder: &'a mut Binder, outer: Option<&'a mut dyn Scope>) -> Self {
        Self {
            binder,
            outer,
            state: ExpressionBinderState::default(),
        }
    }

    /// The binder and the enclosing levels, reborrowed separately (to create a nested
    /// expression binder for the same level).
    pub fn split(&mut self) -> (&mut Binder, Option<&mut dyn Scope>) {
        let outer = match &mut self.outer {
            Some(outer) => Some(&mut **outer as &mut dyn Scope),
            None => None,
        };
        (&mut *self.binder, outer)
    }

    /// With results cast to `target`.
    #[must_use]
    pub fn with_target_type(mut self, target: LogicalType) -> Self {
        self.state.target_type = Some(target);
        self
    }
}

/// Implements [`ColumnQualifier`](super::ColumnQualifier) and [`Scope`] for a binder
/// struct with a `base: BinderBase` field; extra `ColumnQualifier` items can follow in
/// braces.
macro_rules! expression_binder_base {
    ($binder:ident $({ $($qualifier:tt)* })?) => {
        impl $crate::expression_binder::ColumnQualifier for $binder<'_> {
            fn binder_ref(&self) -> &$crate::binder::Binder {
                self.base.binder
            }

            fn lambda_bindings(&self) -> &[$crate::binder::Binding] {
                &self.base.state.lambda_bindings
            }

            $($($qualifier)*)?
        }

        impl $crate::binder::Scope for $binder<'_> {
            fn scope_binder(&self) -> &$crate::binder::Binder {
                self.base.binder
            }

            fn parts(&mut self) -> (&mut $crate::binder::Binder, Option<&mut dyn $crate::binder::Scope>) {
                let outer = match &mut self.base.outer {
                    Some(outer) => Some(&mut **outer as &mut dyn $crate::binder::Scope),
                    None => None,
                };
                (&mut *self.base.binder, outer)
            }

            fn expression_binder(&mut self) -> Option<&mut dyn $crate::expression_binder::ExpressionBinder> {
                Some(self)
            }
        }
    };
}

/// The accessor methods every [`ExpressionBinder`](super::ExpressionBinder) implementation
/// starts with.
macro_rules! expression_binder_accessors {
    () => {
        fn binder(&mut self) -> &mut $crate::binder::Binder {
            self.base.binder
        }

        fn outer(&mut self) -> Option<&mut dyn $crate::binder::Scope> {
            match &mut self.base.outer {
                Some(outer) => Some(&mut **outer),
                None => None,
            }
        }

        fn as_scope(&mut self) -> &mut dyn $crate::binder::Scope {
            self
        }

        fn state(&mut self) -> &mut $crate::expression_binder::ExpressionBinderState {
            &mut self.base.state
        }

        fn state_ref(&self) -> &$crate::expression_binder::ExpressionBinderState {
            &self.base.state
        }
    };
}

pub(crate) use {expression_binder_accessors, expression_binder_base};
