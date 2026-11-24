//! Certificate-signing hostcall family.
//!
//! The identity guest is the only holder of `MintCertificate`; these handlers
//! are the host-held signing oracle. The private key never crosses the
//! boundary: each handler validates the tenant scope, asks the keyring to sign,
//! and returns certificates or an empty acknowledgement.

use selium_abi::{AbiError, AbiErrorCode, Capability, HostcallOutput, ProcessId};

use super::HostOperationState;
use crate::Runtime;

impl Runtime {
    pub(super) fn hostcall_sign_tenant_ca(
        &self,
        process_id: ProcessId,
        tenant: String,
    ) -> std::result::Result<HostOperationState, AbiError> {
        self.require_capability_in_tenant(process_id, Capability::MintCertificate, &tenant)?;
        let mut keyring = self.keyring.lock();
        let keyring = keyring.as_mut().ok_or_else(|| {
            AbiError::new(
                AbiErrorCode::Internal,
                "certificate signing keyring is not initialised",
            )
        })?;
        let cert_der = keyring
            .sign_tenant_ca(&tenant)
            .map_err(|error| AbiError::new(AbiErrorCode::Internal, error.to_string()))?;
        Ok(HostOperationState::Ready(HostcallOutput::Certificate(
            cert_der,
        )))
    }

    pub(super) fn hostcall_commit_tenant_ca(
        &self,
        process_id: ProcessId,
        tenant: String,
        ca_cert_der: Vec<u8>,
    ) -> std::result::Result<HostOperationState, AbiError> {
        self.require_capability_in_tenant(process_id, Capability::MintCertificate, &tenant)?;
        let mut keyring = self.keyring.lock();
        let keyring = keyring.as_mut().ok_or_else(|| {
            AbiError::new(
                AbiErrorCode::Internal,
                "certificate signing keyring is not initialised",
            )
        })?;
        keyring
            .commit_tenant_ca(&tenant, &ca_cert_der)
            .map_err(|error| AbiError::new(AbiErrorCode::Internal, error.to_string()))?;
        Ok(HostOperationState::Ready(HostcallOutput::Empty))
    }

    pub(super) fn hostcall_rollback_tenant_ca(
        &self,
        process_id: ProcessId,
        tenant: String,
        ca_cert_der: Vec<u8>,
    ) -> std::result::Result<HostOperationState, AbiError> {
        self.require_capability_in_tenant(process_id, Capability::MintCertificate, &tenant)?;
        let mut keyring = self.keyring.lock();
        let keyring = keyring.as_mut().ok_or_else(|| {
            AbiError::new(
                AbiErrorCode::Internal,
                "certificate signing keyring is not initialised",
            )
        })?;
        keyring
            .rollback_tenant_ca(&tenant, &ca_cert_der)
            .map_err(|error| AbiError::new(AbiErrorCode::Internal, error.to_string()))?;
        Ok(HostOperationState::Ready(HostcallOutput::Empty))
    }

    pub(super) fn hostcall_sign_user_cert(
        &self,
        process_id: ProcessId,
        tenant: String,
        spki_der: Vec<u8>,
    ) -> std::result::Result<HostOperationState, AbiError> {
        self.require_capability_in_tenant(process_id, Capability::MintCertificate, &tenant)?;
        let keyring = self.keyring.lock();
        let keyring = keyring.as_ref().ok_or_else(|| {
            AbiError::new(
                AbiErrorCode::Internal,
                "certificate signing keyring is not initialised",
            )
        })?;
        let cert_der = keyring
            .sign_user_cert(&tenant, &spki_der)
            .map_err(|error| AbiError::new(AbiErrorCode::Internal, error.to_string()))?;
        Ok(HostOperationState::Ready(HostcallOutput::Certificate(
            cert_der,
        )))
    }

    pub(super) fn hostcall_revoke_ca(
        &self,
        process_id: ProcessId,
        tenant: String,
    ) -> std::result::Result<HostOperationState, AbiError> {
        self.require_capability_in_tenant(process_id, Capability::MintCertificate, &tenant)?;
        let mut keyring = self.keyring.lock();
        let keyring = keyring.as_mut().ok_or_else(|| {
            AbiError::new(
                AbiErrorCode::Internal,
                "certificate signing keyring is not initialised",
            )
        })?;
        keyring
            .revoke_ca(&tenant)
            .map_err(|error| AbiError::new(AbiErrorCode::Internal, error.to_string()))?;
        Ok(HostOperationState::Ready(HostcallOutput::Empty))
    }
}
