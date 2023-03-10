use super::SExp;
use crate::{diagnostics::InfoIdx, errors::Error};
use itertools::Itertools;

/// An obligation generated by during type checking.
pub struct Obligation {
    /// Path condition under which this obligation was generated
    path_cond: Vec<SExp>,
    /// The constraint that needs to be satisfied
    cons: SExp,
    /// Why this obligation was created
    reason: String,
    /// Any extra information that can be used to debug the obligation
    info: Vec<InfoIdx>,
}

impl Obligation {
    /// Construct a new obligation
    pub fn new(cons: SExp, reason: String) -> Self {
        Self {
            cons,
            reason,
            info: vec![],
            path_cond: Vec::new(),
        }
    }

    /// Add a path condition to this obligation
    pub fn with_path_cond(mut self, path_cond: Vec<SExp>) -> Self {
        self.path_cond = path_cond;
        self
    }

    /// Adds extra information to the obligation
    pub fn add_note(mut self, info: InfoIdx) -> Self {
        self.info.push(info);
        self
    }

    /// The constraint associated with this obligation
    pub fn constraint(&self) -> SExp {
        if self.path_cond.is_empty() {
            self.cons.clone()
        } else {
            let assumes = format!(
                "(or {})",
                self.path_cond
                    .iter()
                    .map(|x| x.to_string())
                    .collect_vec()
                    .join(" ")
            );
            SExp(format!("(=> {} {})", assumes, &self.cons))
        }
    }

    /// Turn this Obligation into an Error
    pub fn error(self) -> Error {
        self.into()
    }
}

impl From<Obligation> for Error {
    fn from(v: Obligation) -> Self {
        let mut e = Error::misc(v.reason);
        for i in v.info {
            e = e.add_note(i);
        }
        e
    }
}