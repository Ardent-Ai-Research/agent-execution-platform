// SPDX-License-Identifier: GPL-3.0
pragma solidity ^0.8.28;

import "@account-abstraction/contracts/interfaces/IEntryPoint.sol";
import "@account-abstraction/contracts/interfaces/ISenderCreator.sol";
import "@account-abstraction/contracts/accounts/SimpleAccount.sol";
import "@openzeppelin/contracts/utils/Create2.sol";
import "@openzeppelin/contracts/proxy/ERC1967/ERC1967Proxy.sol";

/**
 * @title SimpleAccountFactory
 * @notice Factory for deploying ERC-4337 SimpleAccount (v0.9) proxies.
 *
 * EntryPoint v0.9 requires that `createAccount()` is called through the
 * EntryPoint's SenderCreator contract. Direct calls to `createAccount()`
 * will revert because v0.9's factory enforces `msg.sender == senderCreator`.
 *
 * `getAddress()` is a pure view function that computes the counterfactual
 * address for any (owner, salt) pair without deploying.
 *
 * Deployment flow:
 *   1. Platform computes address via `getAddress(owner, salt)`
 *   2. UserOperation's `initCode` = factory address ++ `createAccount(owner, salt)`
 *   3. EntryPoint's SenderCreator calls `createAccount()` during first UserOp
 *   4. Factory deploys ERC1967Proxy pointing to SimpleAccount implementation
 *
 * @custom:security-contact security@platform.example
 */
contract SimpleAccountFactory {
    /// The canonical SimpleAccount implementation that all proxies delegate to.
    SimpleAccount public immutable accountImplementation;

    /// The EntryPoint's SenderCreator — only it may call `createAccount`.
    ISenderCreator public immutable senderCreator;

    /// Reverts when createAccount is called by anyone other than the SenderCreator.
    error NotSenderCreator(address msgSender, address entity, address senderCreator);

    /**
     * @param _entryPoint The EntryPoint v0.9 contract address.
     */
    constructor(IEntryPoint _entryPoint) {
        accountImplementation = new SimpleAccount(_entryPoint);
        senderCreator = _entryPoint.senderCreator();
    }

    /**
     * @notice Deploy a new SimpleAccount proxy for `owner` with `salt`.
     * @dev If the account is already deployed, returns the existing address.
     *      In v0.9, this is called via EntryPoint → SenderCreator → Factory.
     *      Direct calls from any other address will revert.
     * @param owner The EOA that owns the smart wallet.
     * @param salt  Unique salt for deterministic addressing (typically 0).
     * @return ret  The deployed (or existing) SimpleAccount proxy address.
     */
    function createAccount(
        address owner,
        uint256 salt
    ) public returns (SimpleAccount ret) {
        require(
            msg.sender == address(senderCreator),
            NotSenderCreator(msg.sender, address(this), address(senderCreator))
        );
        address addr = getAddress(owner, salt);
        uint256 codeSize = addr.code.length;
        if (codeSize > 0) {
            return SimpleAccount(payable(addr));
        }
        ret = SimpleAccount(
            payable(
                new ERC1967Proxy{salt: bytes32(salt)}(
                    address(accountImplementation),
                    abi.encodeCall(SimpleAccount.initialize, (owner))
                )
            )
        );
    }

    /**
     * @notice Compute the counterfactual address for a SimpleAccount proxy.
     * @dev This is a view function — no gas cost, no deployment.
     * @param owner The EOA that will own the smart wallet.
     * @param salt  The same salt used in `createAccount()`.
     * @return The deterministic address where the proxy would be deployed.
     */
    function getAddress(
        address owner,
        uint256 salt
    ) public view returns (address) {
        return
            Create2.computeAddress(
                bytes32(salt),
                keccak256(
                    abi.encodePacked(
                        type(ERC1967Proxy).creationCode,
                        abi.encode(
                            address(accountImplementation),
                            abi.encodeCall(SimpleAccount.initialize, (owner))
                        )
                    )
                )
            );
    }
}
