//! The operations of a fuzz input.

use std::slice;

use libfuzzer_sys::arbitrary::{Arbitrary, Result, Unstructured};

/// The operations of an input, as many as its bytes make: the last field of
/// every input, decoded until the bytes run out. A list of arbitrary's own
/// spends a continuation byte per element, so that a mutation of one ends
/// the list there; here a mutation changes the operations it lands in.
#[derive(Debug)]
pub struct Operations<T>(Vec<T>);

impl<'a, T: Arbitrary<'a>> Arbitrary<'a> for Operations<T> {
    fn arbitrary(u: &mut Unstructured<'a>) -> Result<Self> {
        u.arbitrary_iter()?.collect::<Result<_>>().map(Self)
    }

    /// Decodes operations while bytes are left, each taking at least one,
    /// and drops an operation the last bytes are too few for.
    fn arbitrary_take_rest(mut u: Unstructured<'a>) -> Result<Self> {
        let mut operations = Vec::new();
        while !u.is_empty() {
            let Ok(operation) = T::arbitrary(&mut u) else {
                break;
            };
            operations.push(operation);
        }
        Ok(Self(operations))
    }
}

impl<'o, T> IntoIterator for &'o Operations<T> {
    type Item = &'o T;
    type IntoIter = slice::Iter<'o, T>;

    fn into_iter(self) -> slice::Iter<'o, T> {
        self.0.iter()
    }
}
