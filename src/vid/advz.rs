//! Implementation of VID from https://eprint.iacr.org/2021/1500
//! Why call it `advz`? authors Alhaddad-Duan-Varia-Zhang

use super::VID;
use ark_poly::DenseUVPolynomial;
use ark_serialize::CanonicalSerialize;
use ark_std::{format, string::ToString, vec, vec::Vec};

use jf_primitives::{
    erasure_code::{
        reed_solomon_erasure::{ReedSolomonErasureCode, ReedSolomonErasureCodeShare},
        ErasureCode,
    },
    errors::PrimitivesError,
    pcs::PolynomialCommitmentScheme,
};
use jf_utils::bytes_to_field_elements;
use jf_utils::test_rng;
use sha2::{
    digest::generic_array::{typenum::U32, GenericArray},
    Digest, Sha256,
};

pub struct Advz<P>
where
    P: PolynomialCommitmentScheme,
{
    reconstruction_size: usize,
    num_storage_nodes: usize,
    // ck: <P::SRS as StructuredReferenceString>::ProverParam,
    // vk: <P::SRS as StructuredReferenceString>::VerifierParam,
    ck: P::ProverParam,
    vk: P::VerifierParam,
}

impl<P> Advz<P>
where
    P: PolynomialCommitmentScheme,
{
    /// TODO we desperately need better error handling
    pub fn new(
        reconstruction_size: usize,
        num_storage_nodes: usize,
    ) -> Result<Self, PrimitivesError> {
        if num_storage_nodes < reconstruction_size {
            return Err(PrimitivesError::ParameterError(format!(
                "reconstruction_size {} exceeds num_storage_nodes {}",
                reconstruction_size, num_storage_nodes
            )));
        }
        let pp = P::gen_srs_for_testing(&mut test_rng(), reconstruction_size).unwrap();
        let (ck, vk) = P::trim(pp, reconstruction_size, None).unwrap();
        Ok(Self {
            reconstruction_size,
            num_storage_nodes,
            ck,
            vk,
        })
    }
}

// TODO sucks that I need `GenericArray` here. You'd think the `sha2` crate would export a type alias for hash outputs.
pub type Commitment = GenericArray<u8, U32>;

pub struct Share<P>
where
    P: PolynomialCommitmentScheme,
{
    // TODO: split `polynomial_commitments` from ShareData to avoid duplicate data?
    // TODO only one commitment for now
    polynomial_commitments: P::Commitment,

    id: usize,
    encoded_data: Vec<P::Evaluation>,

    proof: P::Proof,
}

impl<P> VID for Advz<P>
where
    P: PolynomialCommitmentScheme<Point = <P as PolynomialCommitmentScheme>::Evaluation>,
    P::Polynomial: DenseUVPolynomial<P::Evaluation>,
{
    type Commitment = Commitment;
    type Share = Share<P>;

    fn commit(&self, payload: &[u8]) -> Result<Self::Commitment, PrimitivesError> {
        let mut hasher = Sha256::new();

        // TODO perf: DenseUVPolynomial::from_coefficients_slice copies the slice.
        // We could avoid unnecessary mem copies if bytes_to_field_elements returned Vec<Vec<F>>
        let elems = bytes_to_field_elements(payload);
        for coeffs in elems.chunks(self.reconstruction_size) {
            let poly = DenseUVPolynomial::from_coefficients_slice(coeffs);
            let commitment = P::commit(&self.ck, &poly).unwrap();
            commitment.serialize_uncompressed(&mut hasher).unwrap();
        }

        Ok(hasher.finalize())
    }

    fn disperse(&self, payload: &[u8]) -> Result<Vec<Self::Share>, PrimitivesError> {
        // TODO eliminate fully qualified syntax?
        let field_elements: Vec<P::Evaluation> = bytes_to_field_elements(payload);

        // TODO temporary: one polynomial only
        // assert_eq!(field_elements.len(), self.reconstruction_size);

        self.disperse_field_elements(&field_elements)
    }

    fn verify_share(
        &self,
        share: &Self::Share,
    ) -> Result<(), jf_primitives::errors::PrimitivesError> {
        let id: P::Point = P::Point::from(share.id as u64);

        // TODO value = random lin combo of payloads
        let value = share.encoded_data[0];

        let success = P::verify(
            &self.vk,
            &share.polynomial_commitments,
            &id,
            &value,
            &share.proof,
        )
        .unwrap();

        match success {
            true => Ok(()),
            false => Err(PrimitivesError::ParameterError(
                "why am i fighting this.".to_string(),
            )),
        }
    }

    fn recover_payload(
        &self,
        shares: &[Self::Share],
    ) -> Result<Vec<u8>, jf_primitives::errors::PrimitivesError> {
        let field_elements = self.recover_field_elements(shares)?;

        // TODO return field_elements_to_bytes
        assert!(field_elements.len() != 0); // compiler pacification

        todo!()
    }
}

impl<P> Advz<P>
where
    P: PolynomialCommitmentScheme<Point = <P as PolynomialCommitmentScheme>::Evaluation>,
    P::Polynomial: DenseUVPolynomial<P::Evaluation>,
{
    /// Compute shares to send to the storage nodes
    /// TODO take ownership of payload?
    pub fn disperse_field_elements(
        &self,
        payload: &[P::Evaluation],
    ) -> Result<Vec<<Advz<P> as VID>::Share>, PrimitivesError> {
        // TODO random linear combo of polynomials; for now just put it all in a single polynomial
        // let polynomial = DensePolynomial::from_coefficients_vec(field_elements);
        let polynomial = DenseUVPolynomial::from_coefficients_slice(payload);

        // TODO eliminate fully qualified syntax?
        let commitment = P::commit(&self.ck, &polynomial)
            .map_err(|_| PrimitivesError::ParameterError("why am i fighting this.".to_string()))?;

        let encoded_payload = ReedSolomonErasureCode::encode(
            payload,
            self.num_storage_nodes - self.reconstruction_size,
        )
        .unwrap();

        // TODO range should be roots of unity
        let output: Vec<<Advz<P> as VID>::Share> = encoded_payload
            .into_iter()
            .map(|chunk| {
                let id = P::Point::from(chunk.index as u64);
                // let id = chunk.index;

                // TODO don't unwrap: use `collect` to handle `Result`
                let (proof, _value) = P::open(&self.ck, &polynomial, &id).unwrap();

                Share {
                    polynomial_commitments: commitment.clone(),
                    encoded_data: vec![chunk.value],
                    proof: proof,
                    id: chunk.index,
                }
            })
            .collect();

        Ok(output)
    }

    pub fn recover_field_elements(
        &self,
        shares: &[<Advz<P> as VID>::Share],
    ) -> Result<Vec<P::Evaluation>, PrimitivesError> {
        if shares.len() < self.reconstruction_size {
            return Err(PrimitivesError::ParameterError("not enough shares.".into()));
        }

        // TODO check payload commitment

        for s in shares.iter() {
            self.verify_share(s)?;
        }

        ReedSolomonErasureCode::decode(
            shares.iter().map(|s| ReedSolomonErasureCodeShare {
                index: s.id,
                value: s.encoded_data[0],
            }),
            self.reconstruction_size,
        )
    }
}

#[cfg(test)]
mod tests {
    use ark_bls12_381::Bls12_381;
    use ark_std::{rand::RngCore, vec};
    use jf_primitives::pcs::prelude::UnivariateKzgPCS;

    use super::*;
    type PCS = UnivariateKzgPCS<Bls12_381>;

    #[test]
    fn basic_correctness() {
        let vid = Advz::<PCS>::new(2, 3).unwrap();

        // choose payload len to produce the correct number of shares
        // TODO `disperse` should do this automatically
        let payload = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13];

        let shares = vid.disperse(&payload).unwrap();
        assert_eq!(shares.len(), 3);

        for s in shares.iter() {
            vid.verify_share(s).unwrap();
        }
    }

    #[test]
    fn basic_correctness_field_elements() {
        let vid = Advz::<PCS>::new(2, 3).unwrap();

        let field_elements = [
            <UnivariateKzgPCS<Bls12_381> as PolynomialCommitmentScheme>::Evaluation::from(7u64),
            <UnivariateKzgPCS<Bls12_381> as PolynomialCommitmentScheme>::Evaluation::from(13u64),
        ];

        let shares = vid.disperse_field_elements(&field_elements).unwrap();
        assert_eq!(shares.len(), 3);

        for s in shares.iter() {
            vid.verify_share(s).unwrap();
        }

        // recover from a subset of shares
        let recovered_field_elements = vid.recover_field_elements(&shares[..2]).unwrap();
        assert_eq!(recovered_field_elements, field_elements);
    }

    #[test]
    fn commit_basic_correctness() {
        let mut rng = test_rng();
        let lengths = [2, 16, 32, 48, 63, 64, 65, 100, 200];
        let vid = Advz::<PCS>::new(2, 3).unwrap();

        for len in lengths {
            let mut random_bytes = vec![0u8; len];
            rng.fill_bytes(&mut random_bytes);

            vid.commit(&random_bytes).unwrap();
        }
    }
}
