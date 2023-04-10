//! Implementation of VID from https://eprint.iacr.org/2021/1500
//! Why call it `advz`? authors Alhaddad-Duan-Varia-Zhang

use super::{Vec, VID};
use ark_ff::PrimeField;
use ark_poly::DenseUVPolynomial;
use ark_serialize::CanonicalSerializeHashExt;
use ark_std::string::ToString;

use jf_primitives::{
    erasure_code::{
        reed_solomon_erasure::{ReedSolomonErasureCode, ReedSolomonErasureCodeShard},
        ErasureCode,
    },
    errors::PrimitivesError,
    pcs::PolynomialCommitmentScheme,
};
use jf_utils::bytes_to_field_elements;
use jf_utils::test_rng;
use sha2::{
    digest::generic_array::{typenum::U32, GenericArray},
    Sha256,
};

pub struct Advz<P>
where
    P: PolynomialCommitmentScheme,
{
    erasure_code: ReedSolomonErasureCode<P::Evaluation>,
    ck: P::ProverParam,
    vk: P::VerifierParam,
}

impl<P> Advz<P>
where
    P: PolynomialCommitmentScheme,
{
    /// TODO we desperately need better error handling
    pub fn new(
        num_storage_nodes: usize,
        reconstruction_size: usize,
    ) -> Result<Self, PrimitivesError> {
        let erasure_code =
            ReedSolomonErasureCode::new(reconstruction_size, num_storage_nodes).unwrap();
        let pp = P::gen_srs_for_testing(&mut test_rng(), reconstruction_size).unwrap();
        let (ck, vk) = P::trim(pp, reconstruction_size, None).unwrap();

        Ok(Self {
            erasure_code,
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

    // TODO only one payload for now
    encoded_payload: ReedSolomonErasureCodeShard<P::Evaluation>,

    proof: P::Proof,
}

impl<P> VID for Advz<P>
where
    P: PolynomialCommitmentScheme<Point = <P as PolynomialCommitmentScheme>::Evaluation>,
    P::Polynomial: DenseUVPolynomial<P::Evaluation>,
    P::Evaluation: PrimeField, // TODO get rid of Primefield?
{
    type Commitment = Commitment;
    type Share = Share<P>;

    fn commit(&self, payload: &[u8]) -> Result<Self::Commitment, PrimitivesError> {
        // TODO eliminate fully qualified syntax?
        let field_elements = bytes_to_field_elements(payload);

        // TODO for now just put it all in a single polynomial
        let polynomial = DenseUVPolynomial::from_coefficients_vec(field_elements);

        // TODO eliminate fully qualified syntax?
        let commitment = P::commit(&self.ck, &polynomial).unwrap();

        Ok(commitment.hash_uncompressed::<Sha256>())
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
        let id: P::Point = P::Point::from(share.encoded_payload.index as u64);

        // TODO value = random lin combo of payloads
        let value = share.encoded_payload.value;

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
    P::Evaluation: PrimeField, // TODO get rid of Primefield?
{
    /// Compute shares to send to the storage nodes
    /// TODO take ownership of payload?
    pub fn disperse_field_elements(
        &self,
        payload: &[P::Evaluation],
    ) -> Result<Vec<<Advz<P> as VID>::Share>, PrimitivesError> {
        // TODO random linear combo of polynomials; for now just put it all in a single polynomial
        // let polynomial = DensePolynomial::from_coefficients_vec(field_elements);
        let polynomial = DenseUVPolynomial::from_coefficients_slice(&payload);

        // TODO eliminate fully qualified syntax?
        let commitment = P::commit(&self.ck, &polynomial)
            .map_err(|_| PrimitivesError::ParameterError("why am i fighting this.".to_string()))?;

        let encoded_payload = self.erasure_code.encode(payload).unwrap();

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
                    encoded_payload: chunk,
                    proof: proof,
                }
            })
            .collect();

        Ok(output)
    }

    pub fn recover_field_elements(
        &self,
        shares: &[<Advz<P> as VID>::Share],
    ) -> Result<Vec<P::Evaluation>, jf_primitives::errors::PrimitivesError> {
        // TODO this check is done in the ErasureCode
        // if shares.len() < self.reconstruction_size {
        //     return Err(PrimitivesError::ParameterError(
        //         "not enough shares.".to_string(),
        //     ));
        // }

        // TODO check payload commitment

        for s in shares.iter() {
            self.verify_share(s)?;
        }

        // assemble shares for erasure code recovery
        // TODO wasteful clone to turn [Share] into [Shard]; instead `decode` should take `IntoIterator` over shards
        let shards: Vec<_> = shares.iter().map(|s| s.encoded_payload.clone()).collect();

        self.erasure_code.decode(&shards)
    }
}

#[cfg(test)]
mod tests {
    use ark_bls12_381::Bls12_381;
    use jf_primitives::pcs::prelude::UnivariateKzgPCS;

    use super::*;
    type PCS = UnivariateKzgPCS<Bls12_381>;

    #[test]
    fn basic_correctness() {
        let vid = Advz::<PCS>::new(3, 2).unwrap();

        let payload = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32, 33,
        ];

        let shares = vid.disperse(&payload).unwrap();
        assert_eq!(shares.len(), 3);

        for s in shares.iter() {
            vid.verify_share(s).unwrap();
        }
    }

    #[test]
    fn basic_correctness_field_elements() {
        let vid = Advz::<PCS>::new(3, 2).unwrap();

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
}
