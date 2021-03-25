use criterion::{criterion_group, criterion_main, Criterion};
use ff::Field;
use threshold_crypto::poly::{BivarPoly, BivarCommitment, Commitment, Poly};
use threshold_crypto::Fr;

const TEST_DEGREES: [usize; 4] = [5, 10, 20, 40];
const TEST_THRESHOLDS: [usize; 4] = [5, 10, 20, 40];
const RNG_SEED: [u8; 16] = *b"0123456789abcdef";

mod poly_benches {
    use super::*;
    use rand::SeedableRng;
    use rand_xorshift::XorShiftRng;

    /// Benchmarks multiplication of two polynomials.
    fn multiplication(c: &mut Criterion) {
        let mut rng = XorShiftRng::from_seed(RNG_SEED);
        c.bench_function_over_inputs(
            "Polynomial multiplication",
            move |b, &&deg| {
                let rand_factors = || {
                    let lhs = Poly::random(deg, &mut rng);
                    let rhs = Poly::random(deg, &mut rng);
                    (lhs, rhs)
                };
                b.iter_with_setup(rand_factors, |(lhs, rhs)| &lhs * &rhs)
            },
            &TEST_DEGREES,
        );
    }

    /// Benchmarks subtraction of two polynomials
    fn subtraction(c: &mut Criterion) {
        let mut rng = XorShiftRng::from_seed(RNG_SEED);
        c.bench_function_over_inputs(
            "Polynomial subtraction",
            move |b, &&deg| {
                let rand_factors = || {
                    let lhs = Poly::random(deg, &mut rng);
                    let rhs = Poly::random(deg, &mut rng);
                    (lhs, rhs)
                };
                b.iter_with_setup(rand_factors, |(lhs, rhs)| &lhs - &rhs)
            },
            &TEST_DEGREES,
        );
    }

    /// Benchmarks addition of two polynomials
    fn addition(c: &mut Criterion) {
        let mut rng = XorShiftRng::from_seed(RNG_SEED);
        c.bench_function_over_inputs(
            "Polynomial addition",
            move |b, &&deg| {
                let rand_factors = || {
                    let lhs = Poly::random(deg, &mut rng);
                    let rhs = Poly::random(deg, &mut rng);
                    (lhs, rhs)
                };
                b.iter_with_setup(rand_factors, |(lhs, rhs)| &lhs + &rhs)
            },
            &TEST_DEGREES,
        );
    }

    /// Benchmarks Lagrange interpolation for a polynomial.
    fn interpolate(c: &mut Criterion) {
        let mut rng = XorShiftRng::from_seed(RNG_SEED);
        c.bench_function_over_inputs(
            "Polynomial interpolation",
            move |b, &&deg| {
                b.iter_with_setup(
                    || {
                        (0..=deg)
                            .map(|i| (i, Fr::random(&mut rng)))
                            .collect::<Vec<_>>()
                    },
                    Poly::interpolate,
                )
            },
            &TEST_DEGREES,
        );
    }

    criterion_group! {
        name = poly_benches;
        config = Criterion::default();
        targets = multiplication, interpolate, addition, subtraction,
    }
}

mod commitment_benches {
    use super::*;
    use rand::SeedableRng;
    use rand_xorshift::XorShiftRng;

    /// Benchmarks [de]ser of a univariate commitment
    fn roundtrip(c: &mut Criterion) {
        let mut rng = XorShiftRng::from_seed(RNG_SEED);
        c.bench_function_over_inputs(
            "Commitment [de]Serializiation",
            move |b, &&deg| {
                let rand_factors = || {
                    let lhs = Poly::random(deg, &mut rng).commitment();
                    let rhs = Poly::random(deg, &mut rng).commitment();
                    let lhs_ser = bincode::serialize(&lhs).expect("lhs boom ser");
                    let rhs_ser = bincode::serialize(&rhs).expect("rhs boom ser");
                    (lhs_ser, rhs_ser)
                };
                b.iter_with_setup(rand_factors, |(lhs, rhs)| {
                    (
                        bincode::deserialize::<Commitment>(&lhs).expect("lhs boom deser"),
                        bincode::deserialize::<Commitment>(&rhs).expect("rhs boom deser"),
                    )
                })
            },
            &TEST_DEGREES,
        );
    }

    criterion_group! {
        name = commitment_benches;
        config = Criterion::default();
        targets = roundtrip,
    }
}

mod bicommitment_benches {
    use super::*;
    use rand::SeedableRng;
    use rand_xorshift::XorShiftRng;

    /// Benchmarks [de]ser of a univariate commitment
    fn roundtrip(c: &mut Criterion) {
        let mut rng = XorShiftRng::from_seed(RNG_SEED);
        c.bench_function_over_inputs(
            "Commitment [de]Serializiation",
            move |b, &&deg| {
                let rand_factors = || {
                    let lhs = BivarPoly::random(deg, &mut rng).commitment();
                    let rhs = BivarPoly::random(deg, &mut rng).commitment();
                    let lhs_ser = bincode::serialize(&lhs).expect("lhs boom ser");
                    let rhs_ser = bincode::serialize(&rhs).expect("rhs boom ser");
                    (lhs_ser, rhs_ser)
                };
                b.iter_with_setup(rand_factors, |(lhs, rhs)| {
                    (
                        bincode::deserialize::<BivarCommitment>(&lhs).expect("lhs boom deser"),
                        bincode::deserialize::<BivarCommitment>(&rhs).expect("rhs boom deser"),
                    )
                })
            },
            &TEST_DEGREES,
        );
    }

    criterion_group! {
        name = bicommitment_benches;
        config = Criterion::default();
        targets = roundtrip,
    }
}

mod public_key_set_benches {
    use super::*;
    use rand::SeedableRng;
    use rand_xorshift::XorShiftRng;
    use std::collections::BTreeMap;
    use threshold_crypto::SecretKeySet;

    /// Benchmarks combining signatures
    fn combine_signatures(c: &mut Criterion) {
        let mut rng = XorShiftRng::from_seed(RNG_SEED);
        let msg = "Test message";
        c.bench_function_over_inputs(
            "Combine Signatures",
            move |b, &&threshold| {
                let sk_set = SecretKeySet::random(threshold, &mut rng);
                let pk_set = sk_set.public_keys();
                let sigs: BTreeMap<_, _> = (0..=threshold)
                    .map(|i| {
                        let sig = sk_set.secret_key_share(i).sign(msg);
                        (i, sig)
                    })
                    .collect();
                b.iter(|| {
                    pk_set
                        .combine_signatures(&sigs)
                        .expect("could not combine signatures");
                })
            },
            &TEST_THRESHOLDS,
        );
    }

    criterion_group! {
        name = public_key_set_benches;
        config = Criterion::default();
        targets = combine_signatures,
    }
}

criterion_main!(
    poly_benches::poly_benches,
    public_key_set_benches::public_key_set_benches,
    commitment_benches::commitment_benches,
    bicommitment_benches::bicommitment_benches,
);
