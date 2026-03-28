// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "@openzeppelin/contracts/utils/cryptography/ECDSA.sol";
import "@openzeppelin/contracts/utils/cryptography/EIP712.sol";

/// @title MinimalForwarder — EIP-2771 trusted forwarder for meta-transactions.
/// @notice Verifies EIP-712 signed requests and forwards calls to target contracts,
///         appending the original sender's address so that ERC2771Context targets
///         can recover the real `msg.sender`.
contract MinimalForwarder is EIP712 {
    using ECDSA for bytes32;

    struct ForwardRequest {
        address from;      // the agent (signer)
        address to;        // target contract
        uint256 value;     // ETH to forward
        uint256 gas;       // gas limit for the inner call
        uint256 nonce;     // anti-replay nonce
        uint48  deadline;  // expiry timestamp (0 = no expiry)
        bytes   data;      // calldata for the target
    }

    bytes32 private constant _TYPEHASH = keccak256(
        "ForwardRequest(address from,address to,uint256 value,uint256 gas,uint256 nonce,uint48 deadline,bytes data)"
    );

    /// @notice Per-sender nonce for replay protection.
    mapping(address => uint256) private _nonces;

    event ForwardExecuted(address indexed from, address indexed to, bool success, bytes returnData);

    error InvalidSigner(address expected, address recovered);
    error DeadlineExpired(uint48 deadline, uint256 currentTime);
    error InvalidNonce(uint256 expected, uint256 provided);
    error InsufficientValue(uint256 required, uint256 provided);

    constructor() EIP712("MinimalForwarder", "1") {}

    /// @notice Returns the current nonce for a given sender.
    function getNonce(address from) external view returns (uint256) {
        return _nonces[from];
    }

    /// @notice Verify that a ForwardRequest signature is valid WITHOUT executing.
    function verify(
        ForwardRequest calldata req,
        bytes calldata signature
    ) public view returns (bool) {
        (address recovered, , ) = _recoverSigner(req, signature);
        return _nonces[req.from] == req.nonce && recovered == req.from;
    }

    /// @notice Execute a meta-transaction on behalf of `req.from`.
    /// @dev The relayer calls this. It verifies the agent's signature, then
    ///      calls `req.to` with `req.data` + appended 20-byte sender address.
    function execute(
        ForwardRequest calldata req,
        bytes calldata signature
    ) external payable returns (bool, bytes memory) {
        // 1. Deadline check
        if (req.deadline != 0 && block.timestamp > req.deadline) {
            revert DeadlineExpired(req.deadline, block.timestamp);
        }

        // 2. Nonce check
        if (_nonces[req.from] != req.nonce) {
            revert InvalidNonce(_nonces[req.from], req.nonce);
        }

        // 3. Signature verification
        (address recovered, , ) = _recoverSigner(req, signature);
        if (recovered != req.from) {
            revert InvalidSigner(req.from, recovered);
        }

        // 4. Value check
        if (msg.value < req.value) {
            revert InsufficientValue(req.value, msg.value);
        }

        // 5. Increment nonce (before external call — CEI pattern)
        _nonces[req.from] = req.nonce + 1;

        // 6. Forward the call, appending the sender address (ERC-2771 convention)
        (bool success, bytes memory returnData) = req.to.call{gas: req.gas, value: req.value}(
            abi.encodePacked(req.data, req.from)
        );

        emit ForwardExecuted(req.from, req.to, success, returnData);

        return (success, returnData);
    }

    /// @dev Recover the signer from an EIP-712 typed data signature.
    function _recoverSigner(
        ForwardRequest calldata req,
        bytes calldata signature
    ) internal view returns (address, bytes32, bytes32) {
        bytes32 structHash = keccak256(abi.encode(
            _TYPEHASH,
            req.from,
            req.to,
            req.value,
            req.gas,
            req.nonce,
            req.deadline,
            keccak256(req.data)
        ));

        bytes32 digest = _hashTypedDataV4(structHash);
        address recovered = ECDSA.recover(digest, signature);
        return (recovered, structHash, digest);
    }
}
