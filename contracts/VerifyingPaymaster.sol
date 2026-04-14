// SPDX-License-Identifier: GPL-3.0
pragma solidity ^0.8.28;

import "@account-abstraction/contracts/core/BasePaymaster.sol";
import "@account-abstraction/contracts/core/UserOperationLib.sol";
import "@account-abstraction/contracts/interfaces/PackedUserOperation.sol";
import "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import "@openzeppelin/contracts/utils/cryptography/MessageHashUtils.sol";

/**
 * @title VerifyingPaymaster
 * @notice A paymaster that sponsors gas for UserOperations that carry a valid
 *         ECDSA signature from a trusted off-chain signer (the platform).
 *
 * EntryPoint v0.9 compatible.
 *
 * ## How it works
 *
 * 1. The platform backend builds a UserOperation for an agent.
 * 2. The backend computes `getHash(userOp, validUntil, validAfter)` and
 *    signs it with the `verifyingSigner` key.
 * 3. The signature is packed into `paymasterData` (inside `paymasterAndData`).
 * 4. When the EntryPoint calls `validatePaymasterUserOp`, this contract
 *    recovers the signer from the hash + signature and checks it matches
 *    `verifyingSigner`.
 *
 * ## Deployment flow
 *
 * The platform auto-generates its paymaster signing key on first boot,
 * creating a chicken-and-egg problem: you need the signer address to deploy
 * the contract, but the address isn't known until the platform starts.
 *
 * Solution: deploy with `_verifyingSigner = address(0)`.  The contract is
 * safe in this state — every UserOp will fail validation since no private
 * key can produce a signature that recovers to `address(0)`.  After the
 * platform boots and logs its signer address, call `setVerifyingSigner()`
 * to activate sponsorship.
 *
 * ## v0.9 `paymasterAndData` layout
 *
 * The EntryPoint strips the first 52 bytes (paymaster address + gas limits)
 * before passing `paymasterData` to `validatePaymasterUserOp`:
 *
 * ```
 * paymasterAndData = paymaster(20) || pmVerifGas(16) || pmPostOpGas(16) || paymasterData
 *
 * paymasterData = abi.encode(validUntil, validAfter) (64 bytes)
 *              || signature (65 bytes)
 * ```
 *
 * ## Security
 *
 * - Inherits `BasePaymaster` which restricts `validatePaymasterUserOp` to
 *   calls from the EntryPoint only.
 * - `BasePaymaster` in v0.9 uses `Ownable2Step` for safe ownership transfer.
 * - The owner can change the `verifyingSigner` and withdraw funds.
 * - `validUntil` / `validAfter` provide time-bounded sponsorship.
 *
 * @custom:security-contact security@platform.example
 */
contract VerifyingPaymaster is BasePaymaster {
    using UserOperationLib for PackedUserOperation;

    /// The address whose ECDSA signatures authorize gas sponsorship.
    address public verifyingSigner;

    /// Emitted when the verifying signer is changed.
    event VerifyingSignerChanged(
        address indexed oldSigner,
        address indexed newSigner
    );

    /**
     * @param _entryPoint     The EntryPoint v0.9 contract.
     * @param _owner          The initial owner (can change signer, withdraw).
     * @param _verifyingSigner The initial trusted signer address.
     *                         Can be `address(0)` — sponsorship stays inactive
     *                         until `setVerifyingSigner()` is called.
     */
    constructor(
        IEntryPoint _entryPoint,
        address _owner,
        address _verifyingSigner
    ) BasePaymaster(_entryPoint, _owner) {
        verifyingSigner = _verifyingSigner;
        emit VerifyingSignerChanged(address(0), _verifyingSigner);
    }

    /**
     * @notice Change the trusted off-chain signer.
     * @dev Only callable by the contract owner.
     * @param _newSigner The new signer address.
     */
    function setVerifyingSigner(address _newSigner) external onlyOwner {
        require(
            _newSigner != address(0),
            "VerifyingPaymaster: signer is zero"
        );
        address old = verifyingSigner;
        verifyingSigner = _newSigner;
        emit VerifyingSignerChanged(old, _newSigner);
    }

    /**
     * @notice Compute the hash that the off-chain signer must sign to
     *         authorize gas sponsorship for a UserOperation.
     *
     * @dev The hash covers all UserOp fields except `signature` and
     *      `paymasterAndData` (which would be circular), plus the
     *      validity window and chain context.
     *
     * @param userOp     The packed UserOperation.
     * @param validUntil Sponsorship expires after this timestamp.
     * @param validAfter Sponsorship is not valid before this timestamp.
     * @return The keccak256 hash to sign.
     */
    function getHash(
        PackedUserOperation calldata userOp,
        uint48 validUntil,
        uint48 validAfter
    ) public view returns (bytes32) {
        return
            keccak256(
                abi.encode(
                    userOp.sender,
                    userOp.nonce,
                    keccak256(userOp.initCode),
                    keccak256(userOp.callData),
                    keccak256(abi.encode(userOp.accountGasLimits)),
                    userOp.preVerificationGas,
                    keccak256(abi.encode(userOp.gasFees)),
                    block.chainid,
                    address(this),
                    validUntil,
                    validAfter
                )
            );
    }

    /**
     * @notice Validate a UserOperation's paymaster data.
     * @dev Called by the EntryPoint during the validation phase.
     *      Decodes `(validUntil, validAfter)` and the ECDSA signature
     *      from `paymasterData`, recovers the signer, and checks it
     *      matches `verifyingSigner`.
     *
     * @param userOp          The packed UserOperation.
     * @param userOpHash      Unused (we compute our own hash).
     * @param maxCost         The maximum ETH this UserOp could cost.
     * @return context        Empty bytes (no post-op context needed).
     * @return validationData Packed (aggregator=0, validAfter, validUntil).
     */
    function _validatePaymasterUserOp(
        PackedUserOperation calldata userOp,
        bytes32 userOpHash,
        uint256 maxCost
    )
        internal
        virtual
        override
        returns (bytes memory context, uint256 validationData)
    {
        (userOpHash); // silence unused warning
        (maxCost);    // silence unused warning

        // paymasterData layout:
        //   abi.encode(validUntil, validAfter) = 64 bytes
        //   signature = 65 bytes
        // Total: 129 bytes minimum
        bytes calldata paymasterData = userOp.paymasterAndData[
            UserOperationLib.PAYMASTER_DATA_OFFSET:
        ];
        require(
            paymasterData.length >= 129,
            "VerifyingPaymaster: invalid paymasterData length"
        );

        // Decode time validity window
        (uint48 validUntil, uint48 validAfter) = abi.decode(
            paymasterData[:64],
            (uint48, uint48)
        );

        // Extract signature (last 65 bytes of paymasterData's first 129 bytes)
        bytes calldata signature = paymasterData[64:129];

        // Compute the hash and verify the signature
        bytes32 hash = MessageHashUtils.toEthSignedMessageHash(
            getHash(userOp, validUntil, validAfter)
        );
        address recovered = ECDSA.recover(hash, signature);

        if (recovered != verifyingSigner) {
            // Signature mismatch — return SIG_VALIDATION_FAILED
            // (address(1) signals signature failure to the EntryPoint)
            return (
                "",
                _packValidationData(true, validUntil, validAfter)
            );
        }

        // Valid signature — return packed validation data with time bounds
        return (
            "",
            _packValidationData(false, validUntil, validAfter)
        );
    }
}
