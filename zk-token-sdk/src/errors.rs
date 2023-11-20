//! Errors related to proving and verifying proofs.
use {
    crate::{range_proof::errors::RangeProofError, sigma_proofs::errors::*},
    thiserror::Error,
};

#[derive(Error, Clone, Debug, Eq, PartialEq)]
<<<<<<< HEAD
pub enum ProofError {
    #[error("invalid transfer amount range")]
    TransferAmount,
    #[error("proof generation failed")]
    Generation,
    #[error("proof verification failed")]
    VerificationError(ProofType, ProofVerificationError),
    #[error("failed to decrypt ciphertext")]
    Decryption,
    #[error("invalid ciphertext data")]
    CiphertextDeserialization,
    #[error("invalid pubkey data")]
    PubkeyDeserialization,
    #[error("ciphertext does not exist in instruction data")]
    MissingCiphertext,
=======
pub enum ProofGenerationError {
    #[error("not enough funds in account")]
    NotEnoughFunds,
    #[error("transfer fee calculation error")]
    FeeCalculation,
    #[error("illegal number of commitments")]
    IllegalCommitmentLength,
    #[error("illegal amount bit length")]
    IllegalAmountBitLength,
    #[error("invalid commitment")]
    InvalidCommitment,
    #[error("range proof generation failed")]
    RangeProof(#[from] RangeProofGenerationError),
    #[error("unexpected proof length")]
    ProofLength,
}

#[derive(Error, Clone, Debug, Eq, PartialEq)]
pub enum ProofVerificationError {
    #[error("range proof verification failed")]
    RangeProof(#[from] RangeProofVerificationError),
    #[error("sigma proof verification failed")]
    SigmaProof(SigmaProofType, SigmaProofVerificationError),
    #[error("ElGamal ciphertext or public key error")]
    ElGamal(#[from] ElGamalError),
    #[error("Invalid proof context")]
    ProofContext,
    #[error("illegal commitment length")]
    IllegalCommitmentLength,
>>>>>>> e251b8607c ([zk-token-sdk] Add a length check on range proof commitment length (#34165))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ProofType {
    EqualityProof,
    ValidityProof,
    ZeroBalanceProof,
    FeeSigmaProof,
    PubkeyValidityProof,
    RangeProof,
}

#[derive(Error, Clone, Debug, Eq, PartialEq)]
pub enum ProofVerificationError {
    #[error("required algebraic relation does not hold")]
    AlgebraicRelation,
    #[error("malformed proof")]
    Deserialization,
    #[error("multiscalar multiplication failed")]
    MultiscalarMul,
    #[error("transcript failed to produce a challenge")]
    Transcript(#[from] TranscriptError),
    #[error(
        "attempted to verify range proof with a non-power-of-two bit size or bit size is too big"
    )]
    InvalidBitSize,
    #[error("insufficient generators for the proof")]
    InvalidGeneratorsLength,
    #[error("number of blinding factors do not match the number of values")]
    WrongNumBlindingFactors,
}

#[derive(Error, Clone, Debug, Eq, PartialEq)]
pub enum TranscriptError {
    #[error("point is the identity")]
    ValidationError,
}

impl From<RangeProofError> for ProofError {
    fn from(err: RangeProofError) -> Self {
        Self::VerificationError(ProofType::RangeProof, err.0)
    }
}

impl From<EqualityProofError> for ProofError {
    fn from(err: EqualityProofError) -> Self {
        Self::VerificationError(ProofType::EqualityProof, err.0)
    }
}

impl From<FeeSigmaProofError> for ProofError {
    fn from(err: FeeSigmaProofError) -> Self {
        Self::VerificationError(ProofType::FeeSigmaProof, err.0)
    }
}

impl From<ZeroBalanceProofError> for ProofError {
    fn from(err: ZeroBalanceProofError) -> Self {
        Self::VerificationError(ProofType::ZeroBalanceProof, err.0)
    }
}
impl From<ValidityProofError> for ProofError {
    fn from(err: ValidityProofError) -> Self {
        Self::VerificationError(ProofType::ValidityProof, err.0)
    }
}

impl From<PubkeyValidityProofError> for ProofError {
    fn from(err: PubkeyValidityProofError) -> Self {
        Self::VerificationError(ProofType::PubkeyValidityProof, err.0)
    }
}
