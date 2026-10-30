//! BLAKE3 in derive-key mode, with the versioned contexts of docs/02.

use crate::ir::Hash;

/// The derive-key contexts of docs/02. Distinct contexts keep hashes of different kinds apart.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Context {
    Op,
    Body,
    Contract,
    Node,
    Scc,
    Spec,
    Obligation,
}

impl Context {
    pub fn string(self) -> &'static str {
        match self {
            Context::Op => "VeriGraph 2026 op v2",
            Context::Body => "VeriGraph 2026 body v2",
            Context::Contract => "VeriGraph 2026 contract v2",
            Context::Node => "VeriGraph 2026 node v2",
            Context::Scc => "VeriGraph 2026 scc v2",
            Context::Spec => "VeriGraph 2026 spec v2",
            Context::Obligation => "VeriGraph 2026 obligation v2",
        }
    }
}

pub fn derive(ctx: Context, data: &[u8]) -> Hash {
    blake3::derive_key(ctx.string(), data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contexts_separate_kinds() {
        let a = derive(Context::Node, b"x");
        assert_ne!(a, derive(Context::Spec, b"x"));
        assert_eq!(a, blake3::derive_key("VeriGraph 2026 node v2", b"x"));
    }
}
