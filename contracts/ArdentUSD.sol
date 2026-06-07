// SPDX-License-Identifier: GPL-3.0
pragma solidity ^0.8.28;

import {ERC20} from "@openzeppelin/contracts/token/ERC20/ERC20.sol";
import {ERC20Burnable} from "@openzeppelin/contracts/token/ERC20/extensions/ERC20Burnable.sol";
import {Ownable} from "@openzeppelin/contracts/access/Ownable.sol";
import {Ownable2Step} from "@openzeppelin/contracts/access/Ownable2Step.sol";

/**
 * @title ArdentUSD
 * @notice aUSD is Ardent's USD-denominated ERC-20 payment token for agent gas
 *         and platform payments across supported EVM chains.
 *
 * @dev This contract intentionally does not implement collateral management or
 *      price-stability mechanics. It is an owner-minted ERC-20 rail designed to
 *      be deployed deterministically across chains and accepted by the Ardent Research
 *      backend/paymaster payment flow.
 */
contract ArdentUSD is ERC20, ERC20Burnable, Ownable2Step {
    /// Reverts when minting to the zero address.
    error InvalidRecipient();

    /// Reverts when minting zero tokens.
    error InvalidAmount();

    /**
     * @param initialOwner The account allowed to mint and manage ownership.
     */
    constructor(address initialOwner) ERC20("Ardent USD", "aUSD") Ownable(initialOwner) {}

    /**
     * @notice Use 6 decimals, matching common USD stablecoin conventions.
     */
    function decimals() public pure override returns (uint8) {
        return 6;
    }

    /**
     * @notice Mint aUSD to an account.
     * @dev Restricted to the owner. The owner should be a multisig or secure
     *      operational account in production.
     */
    function mint(address to, uint256 amount) external onlyOwner {
        if (to == address(0)) {
            revert InvalidRecipient();
        }
        if (amount == 0) {
            revert InvalidAmount();
        }
        _mint(to, amount);
    }
}
