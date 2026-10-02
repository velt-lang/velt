//! A work counter for tests: the lexer counts the tokens it lexes, the parser the tokens it
//! consumes (also while speculating) and the tokens a parenthesis scan passes, and both the
//! cache entries a JSX re-lex drops. Tests compare the totals for inputs of size n and 4n to
//! check that parsing stays linear without timing it. Outside tests `add` compiles to nothing.

#[cfg(test)]
thread_local! {
    static WORK: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

/// Count `n` units of work (a no-op outside tests).
#[inline(always)]
pub(crate) fn add(n: usize) {
    #[cfg(test)]
    WORK.with(|w| w.set(w.get() + n as u64));
    #[cfg(not(test))]
    let _ = n;
}

/// The work counted on this thread since the last call.
#[cfg(test)]
pub(crate) fn take() -> u64 {
    WORK.with(|w| w.replace(0))
}
