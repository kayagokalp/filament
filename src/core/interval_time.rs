use std::collections::HashMap;

use super::Id;

/// Represents a time variable which can either be:
///   1. An abstract variable like `G`.
///   2. A concrete time such as 1.
///   3. A binary operation of two other interval times.
#[derive(Clone, Hash, PartialEq, Eq)]
pub enum IntervalTime {
    Abstract(Id),
    Concrete(u64),
    Add {
        left: Box<IntervalTime>,
        right: Box<IntervalTime>,
    },
    Max {
        left: Box<IntervalTime>,
        right: Box<IntervalTime>,
    },
}
impl IntervalTime {
    /// Construct an [IntervalTime::Abstract].
    #[inline]
    pub fn abs(time_var: Id) -> Self {
        IntervalTime::Abstract(time_var)
    }

    pub fn binop_max(left: IntervalTime, right: IntervalTime) -> Self {
        match (left, right) {
            (IntervalTime::Concrete(n1), IntervalTime::Concrete(n2)) => {
                if n1 > n2 {
                    Self::concrete(n1)
                } else {
                    Self::concrete(n2)
                }
            }
            (l, r) => IntervalTime::Max {
                left: Box::new(l),
                right: Box::new(r),
            },
        }
    }

    /// Construct a binop add instance and try to peephole optimize the construction
    pub fn binop_add(left: IntervalTime, right: IntervalTime) -> Self {
        match (left, right) {
            (IntervalTime::Concrete(n1), IntervalTime::Concrete(n2)) => {
                IntervalTime::Concrete(n1 + n2)
            }
            (
                IntervalTime::Concrete(n1),
                IntervalTime::Add { left: l, right: r },
            )
            | (
                IntervalTime::Add { left: l, right: r },
                IntervalTime::Concrete(n1),
            ) => {
                if let IntervalTime::Concrete(n2) = &*l {
                    return IntervalTime::binop_add(
                        IntervalTime::Concrete(n1 + n2),
                        *r,
                    );
                }
                if let IntervalTime::Concrete(n2) = &*r {
                    return IntervalTime::binop_add(
                        IntervalTime::Concrete(n1 + n2),
                        *l,
                    );
                }
                let con = IntervalTime::Concrete(n1);
                let bin = IntervalTime::Add { left: l, right: r };
                IntervalTime::Add {
                    left: Box::new(con),
                    right: Box::new(bin),
                }
            }
            (l, r) => IntervalTime::Add {
                left: Box::new(l),
                right: Box::new(r),
            },
        }
    }

    #[inline]
    pub fn concrete(num: u64) -> Self {
        IntervalTime::Concrete(num)
    }
}

impl super::TimeRep for IntervalTime {
    /// Resolve the IntervalTime using the given bindings from abstract variables to exact
    /// bindings.
    fn resolve(&self, bindings: &HashMap<Id, IntervalTime>) -> Self {
        match self {
            IntervalTime::Concrete(_) => self.clone(),
            IntervalTime::Abstract(name) => bindings
                .get(name)
                .unwrap_or_else(|| panic!("No binding for {}", name))
                .clone(),
            IntervalTime::Max { left, right } => IntervalTime::binop_max(
                left.resolve(bindings),
                right.resolve(bindings),
            ),
            IntervalTime::Add { left, right } => IntervalTime::binop_add(
                left.resolve(bindings),
                right.resolve(bindings),
            ),
        }
    }
}

impl std::fmt::Debug for IntervalTime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IntervalTime::Abstract(id) => write!(f, "{}", id),
            IntervalTime::Concrete(n) => write!(f, "{}", n),
            IntervalTime::Add { left, right } => {
                left.fmt(f)?;
                write!(f, "+")?;
                right.fmt(f)
            }
            IntervalTime::Max { left, right } => {
                write!(f, "max(")?;
                left.fmt(f)?;
                write!(f, ",")?;
                right.fmt(f)?;
                write!(f, ")")
            }
        }
    }
}