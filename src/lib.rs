//! A pairing-based threshold cryptosystem for collaborative decryption and signatures.

// Clippy warns that it's dangerous to derive `PartialEq` and explicitly implement `Hash`, but the
// `pairing::bls12_381` types don't implement `Hash`, so we can't derive it.
#![allow(clippy::derive_hash_xor_eq)]
// When using the mocktography, the resulting field elements become wrapped `u32`s, suddenly
// triggering pass-by-reference warnings. They are conditionally disabled for this reason:
#![cfg_attr(
    feature = "use-insecure-test-only-mock-crypto",
    allow(clippy::trivially_copy_pass_by_ref)
)]
#![warn(missing_docs)]

mod cmp_pairing;
mod secret;

#[cfg(feature = "codec-support")]
#[macro_use]
mod codec_impl;

pub mod error;
pub mod poly;

use core::ops::{Add, AddAssign};
use std::borrow::Borrow;
use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::vec::Vec;

use ff::Field;
use group::{Curve, Group};
use hex_fmt::HexFmt;
use log::debug;
use rand::distributions::{Distribution, Standard};
use rand::{rngs::OsRng, Rng, RngCore};
use rand_chacha::ChaChaRng;
use serde::{de::Visitor, Deserialize, Deserializer, Serialize, Serializer};
use zeroize::Zeroize;

use crate::cmp_pairing::cmp_projective;
use crate::error::{Error, FromBytesResult, Result};
use crate::poly::{Commitment, Poly};
use crate::secret::clear_fr;
extern crate serde_big_array;
use serde_big_array::big_array;

big_array! {BigArray; }

use bls12_381::{pairing, G1Affine, G1Projective, G2Affine, G2Projective, Scalar};

mod util;
use util::sha3_256;

#[cfg(feature = "use-insecure-test-only-mock-crypto")]
mod mock;

/// The size of a key's representation in bytes.
#[cfg(not(feature = "use-insecure-test-only-mock-crypto"))]
pub const PK_SIZE: usize = 48;

/// The size of a signature's representation in bytes.
#[cfg(not(feature = "use-insecure-test-only-mock-crypto"))]
pub const SIG_SIZE: usize = 96;

/// A public key.
#[derive(Copy, Clone, PartialEq, Eq)]
pub struct PublicKey(G1Affine);

#[derive(Serialize, Deserialize)]
struct SerializedPublicKey {
    #[serde(with = "BigArray")]
    bytes: [u8; PK_SIZE],
}

impl Serialize for PublicKey {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let bytes = SerializedPublicKey {
            bytes: self.0.to_compressed(),
        };
        serializer.serialize_newtype_struct("PublicKey", &bytes)
    }
}

struct PublicKeyVisitor;

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_newtype_struct("PublicKey", PublicKeyVisitor)
    }
}

impl<'de> Visitor<'de> for PublicKeyVisitor {
    type Value = PublicKey;

    fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "fuck your pubkey serialization")
    }

    fn visit_newtype_struct<D>(self, deserializer: D) -> std::result::Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_u8(self)
    }

    // fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
    // where
    //     M: MapAccess<'de>,
    // {
    //     let mut first = None;
    //     let mut second = None;

    //     while let Some(k) = map.next_key::<&str>()? {
    //         if k == "first" {
    //             first = Some(map.next_value()?);
    //         } else if k == "second" {
    //             second = Some(map.next_value()?);
    //         } else {
    //             return Err(serde::de::Error::custom(&format!("Invalid key: {}", k)));
    //         }
    //     }

    //     if first.is_none() || second.is_none() {
    //         return Err(serde::de::Error::custom("Missing first or second"));
    //     }

    //     Ok(Custom(first.unwrap(), second.unwrap()))
    // }
}

impl Hash for PublicKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        G1Affine::from(self.0).to_compressed().as_ref().hash(state);
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let uncomp = G1Affine::from(self.0).to_compressed();
        write!(f, "PublicKey({:0.10})", HexFmt(uncomp))
    }
}

impl PublicKey {
    /// Returns `true` if the signature matches the element of `G2`.
    pub fn verify_g2(&self, sig: &Signature, hash: &G2Affine) -> bool {
        pairing(&G1Affine::from(self.0), hash)
            == pairing(&G1Affine::identity(), &G2Affine::from(sig.0))
    }

    /// Returns `true` if the signature matches the message.
    ///
    /// This is equivalent to `verify_g2(sig, hash_g2(msg))`.
    pub fn verify<M: AsRef<[u8]>>(&self, sig: &Signature, msg: M) -> bool {
        self.verify_g2(sig, &G2Affine::from(hash_g2(msg)))
    }

    /// Encrypts the message using the OS random number generator.
    ///
    /// Uses the `OsRng` by default. To pass in a custom random number generator, use
    /// `encrypt_with_rng()`.
    pub fn encrypt<M: AsRef<[u8]>>(&self, msg: M) -> Ciphertext {
        self.encrypt_with_rng(&mut OsRng, msg)
    }

    /// Encrypts the message.
    pub fn encrypt_with_rng<R: RngCore, M: AsRef<[u8]>>(&self, rng: &mut R, msg: M) -> Ciphertext {
        let r: Scalar = Scalar::random(rng);
        let u = G1Affine::identity() * r;
        let v: Vec<u8> = {
            let g = G1Projective::from(self.0) * r;
            xor_with_hash(g, msg.as_ref())
        };
        let w = G2Projective::from(hash_g1_g2(u, &v)) * r;
        Ciphertext(u, v, w)
    }

    /// Returns the key with the given representation, if valid.
    pub fn from_bytes(bytes: [u8; PK_SIZE]) -> FromBytesResult<Self> {
        let g1_affine = G1Affine::from_compressed(&bytes).unwrap();
        Ok(PublicKey(G1Projective::from(g1_affine)))
    }

    /// Returns a byte string representation of the public key.
    pub fn to_bytes(&self) -> [u8; PK_SIZE] {
        G1Affine::from(self.0).to_compressed()
    }

    /// Generates a non-redacted debug string.
    pub fn reveal(&self) -> String {
        format!("PublicKey({:?})", self.0)
    }
}

/// A public key share.
#[cfg_attr(feature = "codec-support", derive(codec::Encode, codec::Decode))]
#[derive(Deserialize, Serialize, Clone, Copy, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct PublicKeyShare(PublicKey);

impl fmt::Debug for PublicKeyShare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let uncomp = G1Affine::from((self.0).0).to_uncompressed();
        write!(f, "PublicKeyShare({:0.10})", HexFmt(uncomp))
    }
}

impl PublicKeyShare {
    /// Returns `true` if the signature matches the element of `G2`.
    pub fn verify_g2(&self, sig: &SignatureShare, hash: G2Affine) -> bool {
        self.0.verify_g2(&sig.0, &hash)
    }

    /// Returns `true` if the signature matches the message.
    ///
    /// This is equivalent to `verify_g2(sig, hash_g2(msg))`.
    pub fn verify<M: AsRef<[u8]>>(&self, sig: &SignatureShare, msg: M) -> bool {
        self.verify_g2(sig, G2Affine::from(hash_g2(msg)))
    }

    /// Returns `true` if the decryption share matches the ciphertext.
    pub fn verify_decryption_share(&self, share: &DecryptionShare, ct: &Ciphertext) -> bool {
        let Ciphertext(ref u, ref v, ref w) = *ct;
        let hash = hash_g1_g2(*u, v);
        pairing(&G1Affine::from(share.0), &G2Affine::from(hash))
            == pairing(&G1Affine::from((self.0).0), &G2Affine::from(*w))
    }

    /// Returns the key share with the given representation, if valid.
    pub fn from_bytes(bytes: [u8; PK_SIZE]) -> FromBytesResult<Self> {
        Ok(PublicKeyShare(PublicKey::from_bytes(bytes)?))
    }

    /// Returns a byte string representation of the public key share.
    pub fn to_bytes(&self) -> [u8; PK_SIZE] {
        self.0.to_bytes()
    }

    /// Combines two public key shares to one (basically adds the two commitments)
    pub fn combine(&self, other: PublicKeyShare) -> PublicKeyShare {
        let mut commit = self.0.clone().0;
        commit.add_assign(&other.0.clone().0);
        PublicKeyShare(PublicKey(commit))
    }
}

/// A signature.
// Note: Random signatures can be generated for testing.
#[derive(Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct Signature(G2Projective);

impl PartialOrd for Signature {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(&other))
    }
}

impl Ord for Signature {
    fn cmp(&self, other: &Self) -> Ordering {
        cmp_projective(&self.0, &other.0)
    }
}

impl Distribution<Signature> for Standard {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> Signature {
        Signature(G2Projective::random(rng))
    }
}

impl fmt::Debug for Signature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let uncomp = G2Affine::from(self.0).to_uncompressed();
        write!(f, "Signature({:0.10})", HexFmt(uncomp))
    }
}

impl Hash for Signature {
    fn hash<H: Hasher>(&self, state: &mut H) {
        G2Affine::from(self.0).to_compressed().as_ref().hash(state);
    }
}

impl Signature {
    /// Returns `true` if the signature contains an odd number of ones.
    pub fn parity(&self) -> bool {
        let uncomp = G2Affine::from(self.0).to_uncompressed();
        let xor_bytes: u8 = uncomp.as_ref().iter().fold(0, |result, byte| result ^ byte);
        let parity = 0 != xor_bytes.count_ones() % 2;
        debug!("Signature: {:0.10}, parity: {}", HexFmt(uncomp), parity);
        parity
    }

    /// Returns the signature with the given representation, if valid.
    pub fn from_bytes(bytes: [u8; SIG_SIZE]) -> FromBytesResult<Self> {
        let g2_affine = G2Affine::from_compressed(&bytes).unwrap();
        Ok(Signature(G2Projective::from(g2_affine)))
    }

    /// Returns a byte string representation of the signature.
    pub fn to_bytes(&self) -> [u8; SIG_SIZE] {
        G2Affine::from(self.0).to_compressed()
    }
}

/// A signature share.
// Note: Random signature shares can be generated for testing.
#[cfg_attr(feature = "codec-support", derive(codec::Encode, codec::Decode))]
#[derive(Deserialize, Serialize, Clone, PartialEq, Eq, Hash, Ord, PartialOrd)]
pub struct SignatureShare(pub Signature);

impl Distribution<SignatureShare> for Standard {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> SignatureShare {
        SignatureShare(rng.gen())
    }
}

impl fmt::Debug for SignatureShare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let uncomp = G2Affine::from((self.0).0).to_uncompressed();
        write!(f, "SignatureShare({:0.10})", HexFmt(uncomp))
    }
}

impl SignatureShare {
    /// Returns the signature share with the given representation, if valid.
    pub fn from_bytes(bytes: [u8; SIG_SIZE]) -> FromBytesResult<Self> {
        Ok(SignatureShare(Signature::from_bytes(bytes)?))
    }

    /// Returns a byte string representation of the signature share.
    pub fn to_bytes(&self) -> [u8; SIG_SIZE] {
        self.0.to_bytes()
    }
}

/// A secret key; wraps a single prime field element. The field element is
/// heap allocated to avoid any stack copying that result when passing
/// `SecretKey`s between stack frames.
///
/// # Serde integration
/// `SecretKey` implements `Deserialize` but not `Serialize` to avoid accidental
/// serialization in insecure contexts. To enable both use the `::serde_impl::SerdeSecret`
/// wrapper which implements both `Deserialize` and `Serialize`.
#[derive(PartialEq, Eq, Clone)]
pub struct SecretKey(Scalar);

impl Zeroize for SecretKey {
    fn zeroize(&mut self) {
        clear_fr(&mut self.0)
    }
}

impl Drop for SecretKey {
    fn drop(&mut self) {
        self.zeroize();
    }
}

/// Creates a `SecretKey` containing the zero prime field element.
impl Default for SecretKey {
    fn default() -> Self {
        let mut fr = Scalar::zero();
        SecretKey::from_mut(&mut fr)
    }
}

impl Distribution<SecretKey> for Standard {
    /// Creates a new random instance of `SecretKey`. If you do not need to specify your own RNG,
    /// you should use the [`SecretKey::random()`](struct.SecretKey.html#method.random) constructor,
    /// which uses [`rand::thread_rng()`](https://docs.rs/rand/0.7.2/rand/fn.thread_rng.html)
    /// internally as its RNG.
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> SecretKey {
        SecretKey(Scalar::random(rng))
    }
}

/// A debug statement where the secret prime field element is redacted.
impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SecretKey").field(&DebugDots).finish()
    }
}

impl SecretKey {
    /// Creates a new `SecretKey` from a mutable reference to a field element. This constructor
    /// takes a reference to avoid any unnecessary stack copying/moving of secrets (i.e. the field
    /// element). The field element is copied bytewise onto the heap, the resulting `Box` is
    /// stored in the returned `SecretKey`.
    ///
    /// *WARNING* this constructor will overwrite the referenced `Fr` element with zeros after it
    /// has been copied onto the heap.
    pub fn from_mut(fr: &mut Scalar) -> Self {
        let sk = SecretKey(*fr);
        clear_fr(fr);
        sk
    }

    /// Creates a new random instance of `SecretKey`. If you want to use/define your own random
    /// number generator, you should use the constructor:
    /// [`SecretKey::sample()`](struct.SecretKey.html#impl-Distribution<SecretKey>). If you do not
    /// need to specify your own RNG, you should use the
    /// [`SecretKey::random()`](struct.SecretKey.html#method.random) constructor, which uses
    /// [`rand::thread_rng()`](https://docs.rs/rand/0.7.2/rand/fn.thread_rng.html) internally as its
    /// RNG.
    pub fn random() -> Self {
        rand::random()
    }

    /// Returns the matching public key.
    pub fn public_key(&self) -> PublicKey {
        PublicKey(G1Affine::identity() * self.0)
    }

    /// Signs the given element of `G2`.
    pub fn sign_g2(&self, hash: G2Affine) -> Signature {
        Signature(hash * self.0)
    }

    /// Signs the given message.
    ///
    /// This is equivalent to `sign_g2(hash_g2(msg))`.
    pub fn sign<M: AsRef<[u8]>>(&self, msg: M) -> Signature {
        self.sign_g2(G2Affine::from(hash_g2(msg)))
    }

    /// Returns the decrypted text, or `None`, if the ciphertext isn't valid.
    pub fn decrypt(&self, ct: &Ciphertext) -> Option<Vec<u8>> {
        if !ct.verify() {
            return None;
        }
        let Ciphertext(ref u, ref v, _) = *ct;
        let g = G1Affine::from(u) * self.0;
        Some(xor_with_hash(g, v))
    }

    /// Generates a non-redacted debug string. This method differs from
    /// the `Debug` implementation in that it *does* leak the secret prime
    /// field element.
    pub fn reveal(&self) -> String {
        format!("SecretKey({:?})", self.0)
    }
}

/// A secret key share.
///
/// # Serde integration
/// `SecretKeyShare` implements `Deserialize` but not `Serialize` to avoid accidental
/// serialization in insecure contexts. To enable both use the `::serde_impl::SerdeSecret`
/// wrapper which implements both `Deserialize` and `Serialize`.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct SecretKeyShare(SecretKey);

/// Can be used to create a new random instance of `SecretKeyShare`. This is only useful for testing
/// purposes as such a key has not been derived from a `SecretKeySet`.
impl Distribution<SecretKeyShare> for Standard {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> SecretKeyShare {
        SecretKeyShare(rng.gen())
    }
}

impl fmt::Debug for SecretKeyShare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("SecretKeyShare").field(&DebugDots).finish()
    }
}

/// Allow combining two `SecretKeyShare` together
impl<'a, 'b> Add<&'b SecretKeyShare> for &'a SecretKeyShare {
    type Output = SecretKeyShare;
    fn add(self, other: &'b SecretKeyShare) -> SecretKeyShare {
        let priv_key_1 = self.clone().0;
        let priv_key_2 = &other.0;
        let mut priv_key = priv_key_1.0;
        priv_key.add_assign(&(priv_key_2.0));
        SecretKeyShare(SecretKey(priv_key))
    }
}

impl SecretKeyShare {
    /// Creates a new `SecretKeyShare` from a mutable reference to a field element. This
    /// constructor takes a reference to avoid any unnecessary stack copying/moving of secrets
    /// field elements. The field element will be copied bytewise onto the heap, the resulting
    /// `Box` is stored in the `SecretKey` which is then wrapped in a `SecretKeyShare`.
    ///
    /// *WARNING* this constructor will overwrite the pointed to `Fr` element with zeros once it
    /// has been copied into a new `SecretKeyShare`.
    pub fn from_mut(fr: &mut Scalar) -> Self {
        SecretKeyShare(SecretKey::from_mut(fr))
    }

    /// Returns the matching public key share.
    pub fn public_key_share(&self) -> PublicKeyShare {
        PublicKeyShare(self.0.public_key())
    }

    /// Signs the given element of `G2`.
    pub fn sign_g2(&self, hash: G2Affine) -> SignatureShare {
        SignatureShare(self.0.sign_g2(hash))
    }

    /// Signs the given message.
    pub fn sign<M: AsRef<[u8]>>(&self, msg: M) -> SignatureShare {
        SignatureShare(self.0.sign(msg))
    }

    /// Returns a decryption share, or `None`, if the ciphertext isn't valid.
    pub fn decrypt_share(&self, ct: &Ciphertext) -> Option<DecryptionShare> {
        if !ct.verify() {
            return None;
        }
        Some(self.decrypt_share_no_verify(ct))
    }

    /// Returns a decryption share, without validating the ciphertext.
    pub fn decrypt_share_no_verify(&self, ct: &Ciphertext) -> DecryptionShare {
        DecryptionShare(G1Affine::from(ct.0) * (self.0).0)
    }

    /// Generates a non-redacted debug string. This method differs from
    /// the `Debug` implementation in that it *does* leak the secret prime
    /// field element.
    pub fn reveal(&self) -> String {
        format!("SecretKeyShare({:?})", (self.0).0)
    }
}

/// An encrypted message.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
pub struct Ciphertext(G1Projective, Vec<u8>, G2Projective);

impl Hash for Ciphertext {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let Ciphertext(ref u, ref v, ref w) = *self;
        G1Affine::from(u).to_compressed().as_ref().hash(state);
        v.hash(state);
        G2Affine::from(w).to_compressed().as_ref().hash(state);
    }
}

impl PartialOrd for Ciphertext {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(&other))
    }
}

impl Ord for Ciphertext {
    fn cmp(&self, other: &Self) -> Ordering {
        let Ciphertext(ref u0, ref v0, ref w0) = self;
        let Ciphertext(ref u1, ref v1, ref w1) = other;
        cmp_projective(u0, u1)
            .then(v0.cmp(v1))
            .then(cmp_projective(w0, w1))
    }
}

impl Ciphertext {
    /// Returns `true` if this is a valid ciphertext. This check is necessary to prevent
    /// chosen-ciphertext attacks.
    pub fn verify(&self) -> bool {
        let Ciphertext(ref u, ref v, ref w) = *self;
        let hash = hash_g1_g2(*u, v);
        pairing(&G1Affine::identity(), &G2Affine::from(*w))
            == pairing(&G1Affine::from(*u), &G2Affine::from(hash))
    }
}

/// A decryption share. A threshold of decryption shares can be used to decrypt a message.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct DecryptionShare(G1Projective);

impl Distribution<DecryptionShare> for Standard {
    fn sample<R: Rng + ?Sized>(&self, rng: &mut R) -> DecryptionShare {
        DecryptionShare(G1Projective::random(rng))
    }
}

impl Hash for DecryptionShare {
    fn hash<H: Hasher>(&self, state: &mut H) {
        G1Affine::from(self.0).to_compressed().as_ref().hash(state);
    }
}

impl fmt::Debug for DecryptionShare {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_tuple("DecryptionShare").field(&DebugDots).finish()
    }
}

/// A public key and an associated set of public key shares.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq, Ord, PartialOrd)]
pub struct PublicKeySet {
    /// The coefficients of a polynomial whose value at `0` is the "master key", and value at
    /// `i + 1` is key share number `i`.
    commit: Commitment,
}

impl Hash for PublicKeySet {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.commit.hash(state);
    }
}

impl From<Commitment> for PublicKeySet {
    fn from(commit: Commitment) -> PublicKeySet {
        PublicKeySet { commit }
    }
}

impl PublicKeySet {
    /// Returns the threshold `t`: any set of `t + 1` signature shares can be combined into a full
    /// signature.
    pub fn threshold(&self) -> usize {
        self.commit.degree()
    }

    /// Returns the public key.
    pub fn public_key(&self) -> PublicKey {
        PublicKey(self.commit.coeff[0])
    }

    /// Returns the `i`-th public key share.
    pub fn public_key_share(&self, i: Scalar) -> PublicKeyShare {
        let value = self.commit.evaluate(i + Scalar::one());
        PublicKeyShare(PublicKey(value))
    }

    /// Combines the shares into a signature that can be verified with the main public key.
    ///
    /// The validity of the shares is not checked: If one of them is invalid, the resulting
    /// signature also is. Only returns an error if there is a duplicate index or too few shares.
    ///
    /// Validity of signature shares should be checked beforehand, or validity of the result
    /// afterwards:
    ///
    /// ```
    /// # extern crate rand;
    /// #
    /// # use std::collections::BTreeMap;
    /// # use threshold_crypto::SecretKeySet;
    /// #
    /// let sk_set = SecretKeySet::random(3, &mut rand::thread_rng());
    /// let sk_shares: Vec<_> = (0..6).map(|i| sk_set.secret_key_share(i)).collect();
    /// let pk_set = sk_set.public_keys();
    /// let msg = "Happy birthday! If this is signed, at least four people remembered!";
    ///
    /// // Create four signature shares for the message.
    /// let sig_shares: BTreeMap<_, _> = (0..4).map(|i| (i, sk_shares[i].sign(msg))).collect();
    ///
    /// // Validate the signature shares.
    /// for (i, sig_share) in &sig_shares {
    ///     assert!(pk_set.public_key_share(*i).verify(sig_share, msg));
    /// }
    ///
    /// // Combine them to produce the main signature.
    /// let sig = pk_set.combine_signatures(&sig_shares).expect("not enough shares");
    ///
    /// // Validate the main signature. If the shares were valid, this can't fail.
    /// assert!(pk_set.public_key().verify(&sig, msg));
    /// ```
    pub fn combine_signatures<'a, T, I>(&self, shares: I) -> Result<Signature>
    where
        I: IntoIterator<Item = (T, &'a SignatureShare)>,
        T: Into<Scalar>,
    {
        let samples = shares.into_iter().map(|(i, share)| (i, &(share.0).0));
        Ok(Signature(interpolate(self.commit.degree(), samples)?))
    }

    /// Combines the shares to decrypt the ciphertext.
    pub fn decrypt<'a, T, I>(&self, shares: I, ct: &Ciphertext) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = (T, &'a DecryptionShare)>,
        T: Into<Scalar>,
    {
        let samples = shares.into_iter().map(|(i, share)| (i, &share.0));
        let g = interpolate(self.commit.degree(), samples)?;
        Ok(xor_with_hash(g, &ct.1))
    }

    /// Combine two PublicKeySet into a single one (used from threshold generation)
    pub fn combine(&self, other: PublicKeySet) -> PublicKeySet {
        let mut commit = self.commit.clone();
        commit.add_assign(&other.commit);
        PublicKeySet { commit }
    }
}

/// A secret key and an associated set of secret key shares.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretKeySet {
    /// The coefficients of a polynomial whose value at `0` is the "master key", and value at
    /// `i + 1` is key share number `i`.
    poly: Poly,
}

impl From<Poly> for SecretKeySet {
    fn from(poly: Poly) -> SecretKeySet {
        SecretKeySet { poly }
    }
}

impl SecretKeySet {
    /// Creates a set of secret key shares, where any `threshold + 1` of them can collaboratively
    /// sign and decrypt. This constructor is identical to the `SecretKeySet::try_random()` in every
    /// way except that this constructor panics if the other returns an error.
    ///
    /// # Panic
    ///
    /// Panics if the `threshold` is too large for the coefficients to fit into a `Vec`.
    pub fn random<R: Rng>(threshold: usize, rng: &mut R) -> Self {
        SecretKeySet::try_random(threshold, rng)
            .unwrap_or_else(|e| panic!("Failed to create random `SecretKeySet`: {}", e))
    }

    /// Creates a set of secret key shares, where any `threshold + 1` of them can collaboratively
    /// sign and decrypt. This constructor is identical to the `SecretKeySet::random()` in every
    /// way except that this constructor returns an `Err` where the `random` would panic.
    pub fn try_random<R: Rng>(threshold: usize, rng: &mut R) -> Result<Self> {
        Poly::try_random(threshold, rng).map(SecretKeySet::from)
    }

    /// Returns the threshold `t`: any set of `t + 1` signature shares can be combined into a full
    /// signature.
    pub fn threshold(&self) -> usize {
        self.poly.degree()
    }

    /// Returns the `i`-th secret key share.
    pub fn secret_key_share(&self, i: Scalar) -> SecretKeyShare {
        let mut fr = self.poly.evaluate(i + Scalar::one());
        SecretKeyShare::from_mut(&mut fr)
    }

    /// Returns the corresponding public key set. That information can be shared publicly.
    pub fn public_keys(&self) -> PublicKeySet {
        PublicKeySet {
            commit: self.poly.commitment(),
        }
    }

    /// Returns the secret master key.
    #[cfg(test)]
    fn secret_key(&self) -> SecretKey {
        let mut fr = self.poly.evaluate(0);
        SecretKey::from_mut(&mut fr)
    }
}

/// Returns a hash of the given message in `G2`.
pub fn hash_g2<M: AsRef<[u8]>>(msg: M) -> G2Projective {
    let digest = sha3_256(msg.as_ref());
    G2Projective::random(&mut ChaChaRng::from_seed(digest))
}

/// Returns a hash of the group element and message, in the second group.
fn hash_g1_g2<M: AsRef<[u8]>>(g1: G1Projective, msg: M) -> G2Projective {
    // If the message is large, hash it, otherwise copy it.
    // TODO: Benchmark and optimize the threshold.
    let mut msg = if msg.as_ref().len() > 64 {
        sha3_256(msg.as_ref()).to_vec()
    } else {
        msg.as_ref().to_vec()
    };
    msg.extend(G1Affine::from(g1).to_compressed().as_ref());
    hash_g2(&msg)
}

/// Returns the bitwise xor of `bytes` with a sequence of pseudorandom bytes determined by `g1`.
fn xor_with_hash(g1: G1Projective, bytes: &[u8]) -> Vec<u8> {
    let digest = sha3_256(G1Affine::from(g1).to_compressed().as_ref());
    let rng = ChaChaRng::from_seed(digest);
    let xor = |(a, b): (u8, &u8)| a ^ b;
    rng.sample_iter(&Standard).zip(bytes).map(xor).collect()
}

/// Given a list of `t + 1` samples `(i - 1, f(i) * g)` for a polynomial `f` of degree `t`, and a
/// group generator `g`, returns `f(0) * g`.
fn interpolate<C, B, T, I>(t: usize, items: I) -> Result<C>
where
    C: Curve,
    I: IntoIterator<Item = (T, B)>,
    T: Into<Scalar>,
    B: Borrow<C>,
{
    let samples: Vec<_> = items
        .into_iter()
        .take(t + 1)
        .map(|(i, sample)| (i + Scalar::one(), sample))
        .collect();
    if samples.len() <= t {
        return Err(Error::NotEnoughShares);
    }

    if t == 0 {
        return Ok(*samples[0].1.borrow());
    }

    // Compute the products `x_prod[i]` of all but the `i`-th entry.
    let mut x_prod: Vec<C::Scalar> = Vec::with_capacity(t);
    let mut tmp = C::Scalar::one();
    x_prod.push(tmp);
    for (x, _) in samples.iter().take(t) {
        tmp.mul_assign(x);
        x_prod.push(tmp);
    }
    tmp = C::Scalar::one();
    for (i, (x, _)) in samples[1..].iter().enumerate().rev() {
        tmp.mul_assign(x);
        x_prod[i].mul_assign(&tmp);
    }

    let mut result = C::zero();
    for (mut l0, (x, sample)) in x_prod.into_iter().zip(&samples) {
        // Compute the value at 0 of the Lagrange polynomial that is `0` at the other data
        // points but `1` at `x`.
        let mut denom = C::Scalar::one();
        for (x0, _) in samples.iter().filter(|(x0, _)| x0 != x) {
            let mut diff = *x0;
            diff.sub_assign(x);
            denom.mul_assign(&diff);
        }
        l0.mul_assign(&denom.inverse().ok_or(Error::DuplicateEntry)?);
        result.add_assign(&sample.borrow().into_affine().mul(l0));
    }
    Ok(result)
}

/// Type that implements `Debug` printing three dots. This can be used to hide the contents of a
/// field in a `Debug` implementation.
struct DebugDots;

impl fmt::Debug for DebugDots {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "...")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::collections::BTreeMap;

    use rand::{self, distributions::Standard, random, Rng};

    #[test]
    fn test_interpolate() {
        let mut rng = rand::thread_rng();
        for deg in 0..5 {
            println!("deg = {}", deg);
            let comm = Poly::random(deg, &mut rng).commitment();
            let mut values = Vec::new();
            let mut x = 0;
            for _ in 0..=deg {
                x += rng.gen_range(1, 5);
                values.push((x - 1, comm.evaluate(x)));
            }
            let actual = interpolate(deg, values).expect("wrong number of values");
            assert_eq!(comm.evaluate(0), actual);
        }
    }

    #[test]
    fn test_simple_sig() {
        let sk0 = SecretKey::random();
        let sk1 = SecretKey::random();
        let pk0 = sk0.public_key();
        let msg0 = b"Real news";
        let msg1 = b"Fake news";
        assert!(pk0.verify(&sk0.sign(msg0), msg0));
        assert!(!pk0.verify(&sk1.sign(msg0), msg0)); // Wrong key.
        assert!(!pk0.verify(&sk0.sign(msg1), msg0)); // Wrong message.
    }

    #[test]
    fn test_threshold_sig() {
        let mut rng = rand::thread_rng();
        let sk_set = SecretKeySet::random(3, &mut rng);
        let pk_set = sk_set.public_keys();
        let pk_master = pk_set.public_key();

        // Make sure the keys are different, and the first coefficient is the main key.
        assert_ne!(pk_master, pk_set.public_key_share(0).0);
        assert_ne!(pk_master, pk_set.public_key_share(1).0);
        assert_ne!(pk_master, pk_set.public_key_share(2).0);

        // Make sure we don't hand out the main secret key to anyone.
        let sk_master = sk_set.secret_key();
        let sk_share_0 = sk_set.secret_key_share(0).0;
        let sk_share_1 = sk_set.secret_key_share(1).0;
        let sk_share_2 = sk_set.secret_key_share(2).0;
        assert_ne!(sk_master, sk_share_0);
        assert_ne!(sk_master, sk_share_1);
        assert_ne!(sk_master, sk_share_2);

        let msg = "Totally real news";

        // The threshold is 3, so 4 signature shares will suffice to recreate the share.
        let sigs: BTreeMap<_, _> = [5, 8, 7, 10]
            .iter()
            .map(|&i| {
                let sig = sk_set.secret_key_share(i).sign(msg);
                (i, sig)
            })
            .collect();

        // Each of the shares is a valid signature matching its public key share.
        for (i, sig) in &sigs {
            assert!(pk_set.public_key_share(*i).verify(sig, msg));
        }

        // Combined, they produce a signature matching the main public key.
        let sig = pk_set.combine_signatures(&sigs).expect("signatures match");
        assert!(pk_set.public_key().verify(&sig, msg));

        // A different set of signatories produces the same signature.
        let sigs2: BTreeMap<_, _> = [42, 43, 44, 45]
            .iter()
            .map(|&i| {
                let sig = sk_set.secret_key_share(i).sign(msg);
                (i, sig)
            })
            .collect();
        let sig2 = pk_set.combine_signatures(&sigs2).expect("signatures match");
        assert_eq!(sig, sig2);
    }

    #[test]
    fn test_simple_enc() {
        let sk_bob: SecretKey = random();
        let sk_eve: SecretKey = random();
        let pk_bob = sk_bob.public_key();
        println!("pk_bob = {}", pk_bob.reveal());
        let msg = b"Muffins in the canteen today! Don't tell Eve!";
        let ciphertext = pk_bob.encrypt(&msg[..]);
        assert!(ciphertext.verify());

        // Bob can decrypt the message.
        let decrypted = sk_bob.decrypt(&ciphertext).expect("invalid ciphertext");
        assert_eq!(msg[..], decrypted[..]);

        // Eve can't.
        let decrypted_eve = sk_eve.decrypt(&ciphertext).expect("invalid ciphertext");
        assert_ne!(msg[..], decrypted_eve[..]);

        // Eve tries to trick Bob into decrypting `msg` xor `v`, but it doesn't validate.
        let Ciphertext(u, v, w) = ciphertext;
        let fake_ciphertext = Ciphertext(u, vec![0; v.len()], w);
        assert!(!fake_ciphertext.verify());
        assert_eq!(None, sk_bob.decrypt(&fake_ciphertext));
    }

    #[test]
    fn test_random_extreme_thresholds() {
        let mut rng = rand::thread_rng();
        let sks = SecretKeySet::random(0, &mut rng);
        assert_eq!(0, sks.threshold());
        assert!(SecretKeySet::try_random(usize::max_value(), &mut rng).is_err());
    }

    #[test]
    fn test_threshold_enc() {
        let mut rng = rand::thread_rng();
        let sk_set = SecretKeySet::random(3, &mut rng);
        let pk_set = sk_set.public_keys();
        let msg = b"Totally real news";
        let ciphertext = pk_set.public_key().encrypt(&msg[..]);

        // The threshold is 3, so 4 signature shares will suffice to decrypt.
        let shares: BTreeMap<_, _> = [5, 8, 7, 10]
            .iter()
            .map(|&i| {
                let dec_share = sk_set
                    .secret_key_share(i)
                    .decrypt_share(&ciphertext)
                    .expect("ciphertext is invalid");
                (i, dec_share)
            })
            .collect();

        // Each of the shares is valid matching its public key share.
        for (i, share) in &shares {
            pk_set
                .public_key_share(*i)
                .verify_decryption_share(share, &ciphertext);
        }

        // Combined, they can decrypt the message.
        let decrypted = pk_set
            .decrypt(&shares, &ciphertext)
            .expect("decryption shares match");
        assert_eq!(msg[..], decrypted[..]);
    }

    /// Some basic sanity checks for the `hash_g2` function.
    #[test]
    fn test_hash_g2() {
        let rng = rand::thread_rng();
        let msg: Vec<u8> = rng.sample_iter(&Standard).take(1000).collect();
        let msg_end0: Vec<u8> = msg.iter().chain(b"end0").cloned().collect();
        let msg_end1: Vec<u8> = msg.iter().chain(b"end1").cloned().collect();

        assert_eq!(hash_g2(&msg), hash_g2(&msg));
        assert_ne!(hash_g2(&msg), hash_g2(&msg_end0));
        assert_ne!(hash_g2(&msg_end0), hash_g2(&msg_end1));
    }

    /// Some basic sanity checks for the `hash_g1_g2` function.
    #[test]
    fn test_hash_g1_g2() {
        let mut rng = rand::thread_rng();
        let msg: Vec<u8> = rng.sample_iter(&Standard).take(1000).collect();
        let msg_end0: Vec<u8> = msg.iter().chain(b"end0").cloned().collect();
        let msg_end1: Vec<u8> = msg.iter().chain(b"end1").cloned().collect();
        let g0 = G1Projective::random(&mut rng);
        let g1 = G1Projective::random(&mut rng);

        assert_eq!(hash_g1_g2(g0, &msg), hash_g1_g2(g0, &msg));
        assert_ne!(hash_g1_g2(g0, &msg), hash_g1_g2(g0, &msg_end0));
        assert_ne!(hash_g1_g2(g0, &msg_end0), hash_g1_g2(g0, &msg_end1));
        assert_ne!(hash_g1_g2(g0, &msg), hash_g1_g2(g1, &msg));
    }

    /// Some basic sanity checks for the `hash_bytes` function.
    #[test]
    fn test_xor_with_hash() {
        let mut rng = rand::thread_rng();
        let g0 = G1Projective::random(&mut rng);
        let g1 = G1Projective::random(&mut rng);
        let xwh = xor_with_hash;
        assert_eq!(xwh(g0, &[0; 5]), xwh(g0, &[0; 5]));
        assert_ne!(xwh(g0, &[0; 5]), xwh(g1, &[0; 5]));
        assert_eq!(5, xwh(g0, &[0; 5]).len());
        assert_eq!(6, xwh(g0, &[0; 6]).len());
        assert_eq!(20, xwh(g0, &[0; 20]).len());
    }

    #[test]
    fn test_from_to_bytes() {
        let sk: SecretKey = random();
        let sig = sk.sign("Please sign here: ______");
        let pk = sk.public_key();
        let pk2 = PublicKey::from_bytes(pk.to_bytes()).expect("invalid pk representation");
        assert_eq!(pk, pk2);
        let sig2 = Signature::from_bytes(sig.to_bytes()).expect("invalid sig representation");
        assert_eq!(sig, sig2);
    }

    #[test]
    fn test_serde() {
        let sk = SecretKey::random();
        let sig = sk.sign("Please sign here: ______");
        let pk = sk.public_key();
        let ser_pk = bincode::serialize(&pk).expect("serialize public key");
        let deser_pk = bincode::deserialize(&ser_pk).expect("deserialize public key");
        assert_eq!(ser_pk.len(), PK_SIZE);
        assert_eq!(pk, deser_pk);
        let ser_sig = bincode::serialize(&sig).expect("serialize signature");
        let deser_sig = bincode::deserialize(&ser_sig).expect("deserialize signature");
        assert_eq!(ser_sig.len(), SIG_SIZE);
        assert_eq!(sig, deser_sig);
    }

    #[cfg(feature = "codec-support")]
    #[test]
    fn test_codec() {
        use codec::{Decode, Encode};
        use rand::distributions::{Distribution, Standard};
        use rand::thread_rng;

        macro_rules! assert_codec {
            ($obj:expr, $type:ty) => {
                let encoded: Vec<u8> = $obj.encode();
                let decoded: $type = <$type>::decode(&mut &encoded[..]).unwrap();
                assert_eq!(decoded, $obj.clone());
            };
        }

        let sk = SecretKey::random();
        let pk = sk.public_key();
        assert_codec!(pk, PublicKey);

        let pk_share = PublicKeyShare(pk);
        assert_codec!(pk_share, PublicKeyShare);

        let sig = sk.sign(b"this is a test");
        assert_codec!(sig, Signature);

        let sig_share = SignatureShare(sig);
        assert_codec!(sig_share, SignatureShare);

        let cipher_text = pk.encrypt(b"cipher text");
        assert_codec!(cipher_text, Ciphertext);

        let dec_share: DecryptionShare = Standard.sample(&mut thread_rng());
        assert_codec!(dec_share, DecryptionShare);

        let sk_set = SecretKeySet::random(3, &mut thread_rng());
        let pk_set = sk_set.public_keys();
        assert_codec!(pk_set, PublicKeySet);
    }

    #[test]
    fn test_size() {
        assert_eq!(<G1Affine as PrimeCurveAffine>::Compressed::size(), PK_SIZE);
        assert_eq!(<G2Affine as PrimeCurveAffine>::Compressed::size(), SIG_SIZE);
    }

    #[test]
    fn test_zeroize() {
        let zero_sk = SecretKey::from_mut(&mut Scalar::zero());

        let mut sk = SecretKey::random();
        assert_ne!(zero_sk, sk);

        sk.zeroize();
        assert_eq!(zero_sk, sk);
    }

    #[test]
    fn test_rng_seed() {
        let sk1 = SecretKey::random();
        let sk2 = SecretKey::random();

        assert_ne!(sk1, sk2);
        let mut seed = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut seed);

        let mut rng = ChaChaRng::from_seed(seed);
        let sk3: SecretKey = rng.sample(Standard);

        let mut rng = ChaChaRng::from_seed(seed);
        let sk4: SecretKey = rng.sample(Standard);
        assert_eq!(sk3, sk4);
    }
}
