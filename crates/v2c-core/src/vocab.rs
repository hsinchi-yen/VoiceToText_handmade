//! Personal vocabulary (names, product terms). Implemented in T1.8.

/// One word or phrase per entry, in the user's preferred spelling.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct Vocab {
    pub words: Vec<String>,
}

impl std::fmt::Debug for Vocab {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Vocab")
            .field("len", &self.words.len())
            .finish()
    }
}
