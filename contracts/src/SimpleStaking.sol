// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

import "@openzeppelin/contracts/metatx/ERC2771Context.sol";

/// @title SimpleStaking — ERC-2771-aware staking contract for integration testing.
/// @notice Users (or meta-tx forwarders on their behalf) can stake ETH,
///         check balances, and unstake. Uses `_msgSender()` so that when
///         called through a trusted forwarder, the real agent address is used.
contract SimpleStaking is ERC2771Context {
    mapping(address => uint256) public stakes;
    uint256 public totalStaked;

    event Staked(address indexed user, uint256 amount);
    event Unstaked(address indexed user, uint256 amount);

    /// @param trustedForwarder The address of the MinimalForwarder contract.
    constructor(address trustedForwarder) ERC2771Context(trustedForwarder) {}

    /// @notice Stake ETH sent with this call.
    function stake() external payable {
        require(msg.value > 0, "must stake > 0");
        address sender = _msgSender();
        stakes[sender] += msg.value;
        totalStaked += msg.value;
        emit Staked(sender, msg.value);
    }

    /// @notice Unstake a specific amount of ETH.
    function unstake(uint256 amount) external {
        require(amount > 0, "must unstake > 0");
        address sender = _msgSender();
        require(stakes[sender] >= amount, "insufficient stake");
        stakes[sender] -= amount;
        totalStaked -= amount;
        (bool ok, ) = sender.call{value: amount}("");
        require(ok, "ETH transfer failed");
        emit Unstaked(sender, amount);
    }

    /// @notice Get the staked balance for a user.
    function getStake(address user) external view returns (uint256) {
        return stakes[user];
    }
}
