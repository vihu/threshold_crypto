//! Utilities for working with secret values. This module includes functionality for overwriting
//! memory with zeros.

use bls12_381::Scalar as Fr;

/// Overwrites a single field element with zeros.
pub(crate) fn clear_fr(fr: &mut Fr) {
    *fr = Fr::zero();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::thread_rng;
    use ff::Field;

    #[test]
    fn test_clear() {
        let mut rng = thread_rng();

        let mut fr: Fr = Fr::random(&mut rng);
        assert_ne!(fr, Fr::zero());

        clear_fr(&mut fr);
        assert_eq!(fr, Fr::zero());
    }
}
