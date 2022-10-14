//! The public-key (validity) proof system.
//!
//! A public-key proof is defined with respect to an ElGamal public key. The proof certifies that a
//! given public key is a valid ElGamal public key (i.e. the prover knows a corresponding secret
//! key). To generate the proof, a prover must prove the secret key for the public key.
//!
//! The protocol guarantees computational soundness (by the hardness of discrete log) and perfect
//! zero-knowledge in the random oracle model.

use curve25519_dalek::traits::VartimeMultiscalarMul;

#[cfg(not(target_os = "solana"))]
use {
    crate::encryption::{
        elgamal::{ElGamalKeypair, ElGamalPubkey},
        pedersen::H,
    },
    rand::rngs::OsRng,
    zeroize::Zeroize,
};
use {
    crate::{sigma_proofs::errors::PubkeyProofError, transcript::TranscriptProtocol},
    arrayref::{array_ref, array_refs},
    curve25519_dalek::{
        ristretto::{CompressedRistretto, RistrettoPoint},
        scalar::Scalar,
        traits::IsIdentity,
    },
    merlin::Transcript,
};

/// Public-key proof.
///
/// Contains all the elliptic curve and scalar components that make up the sigma protocol.
#[allow(non_snake_case)]
#[derive(Clone)]
pub struct PubkeyProof {
    Y: CompressedRistretto,
    z: Scalar,
}

#[allow(non_snake_case)]
#[cfg(not(target_os = "solana"))]
impl PubkeyProof {
    /// Public-key proof constructor.
    ///
    /// The function does *not* hash the public key and ciphertext into the transcript. For
    /// security, the caller (the main protocol) should hash these public key components prior to
    /// invoking this constructor.
    ///
    /// This function is randomized. It uses `OsRng` internally to generate random scalars.
    ///
    /// This function panics if the provided keypair is not valid (i.e. secret key is not
    /// invertible).
    ///
    /// * `elgamal_keypair` = The ElGamal keypair that pertains to the ElGamal public key to be
    /// proved
    /// * `transcript` - The transcript that does the bookkeeping for the Fiat-Shamir heuristic
    pub fn new(elgamal_keypair: &ElGamalKeypair, transcript: &mut Transcript) -> Self {
        transcript.pubkey_proof_domain_sep();

        // extract the relevant scalar and Ristretto points from the input
        let s = elgamal_keypair.secret.get_scalar();

        assert!(s != &Scalar::zero());
        let s_inv = s.invert();

        // generate a random masking factor that also serves as a nonce
        let mut y = Scalar::random(&mut OsRng);
        let Y = (&y * &(*H)).compress();

        // record masking factors in transcript and get challenges
        transcript.append_point(b"Y", &Y);
        let c = transcript.challenge_scalar(b"c");

        // compute masked secret key
        let z = &(&c * s_inv) + &y;

        y.zeroize();

        Self { Y, z }
    }

    /// Public-key proof verifier.
    ///
    /// * `elgamal_pubkey` - The ElGamal public key to be proved
    /// * `transcript` - The transcript that does the bookkeeping for the Fiat-Shamir heuristic
    pub fn verify(
        self,
        elgamal_pubkey: &ElGamalPubkey,
        transcript: &mut Transcript,
    ) -> Result<(), PubkeyProofError> {
        transcript.pubkey_proof_domain_sep();

        // extract the relvant scalar and Ristretto points from the input
        let P = elgamal_pubkey.get_point();

        // include Y to transcript and extract challenge
        transcript.validate_and_append_point(b"Y", &self.Y)?;
        let c = transcript.challenge_scalar(b"c");

        // check that the required algebraic condition holds
        let Y = self.Y.decompress().ok_or(PubkeyProofError::Format)?;

        let check = RistrettoPoint::vartime_multiscalar_mul(
            vec![&self.z, &(-&c), &(-&Scalar::one())],
            vec![&(*H), P, &Y],
        );

        if check.is_identity() {
            Ok(())
        } else {
            Err(PubkeyProofError::AlgebraicRelation)
        }
    }

    pub fn to_bytes(&self) -> [u8; 64] {
        let mut buf = [0_u8; 64];
        buf[..32].copy_from_slice(self.Y.as_bytes());
        buf[32..64].copy_from_slice(self.z.as_bytes());
        buf
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, PubkeyProofError> {
        if bytes.len() != 64 {
            return Err(PubkeyProofError::Format);
        }

        let bytes = array_ref![bytes, 0, 64];
        let (Y, z) = array_refs![bytes, 32, 32];

        let Y = CompressedRistretto::from_slice(Y);
        let z = Scalar::from_canonical_bytes(*z).ok_or(PubkeyProofError::Format)?;

        Ok(PubkeyProof { Y, z })
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_pubkey_proof_correctness() {
        let keypair = ElGamalKeypair::new_rand();

        let mut prover_transcript = Transcript::new(b"test");
        let mut verifier_transcript = Transcript::new(b"test");

        let proof = PubkeyProof::new(&keypair, &mut prover_transcript);
        assert!(proof
            .verify(&keypair.public, &mut verifier_transcript)
            .is_ok());
    }
}
