//! Utilities for working with secret values. This module includes functionality for overwriting
//! memory with zeros.

use bls12_381::Scalar as Fr;
use ff::PrimeFieldRepr as FrRepr;

/// Overwrites a single field element with zeros.
pub(crate) fn clear_fr(fr: &mut Fr) {
    // TODO: Remove this after pairing support `Zeroize`
    let fr_repr = unsafe { &mut *(fr as *mut Fr as *mut dyn FrRepr) };
    fr_repr.0.zeroize();
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
