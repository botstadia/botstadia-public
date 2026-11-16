// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.8.30;

import {IERC20} from "openzeppelin/token/ERC20/IERC20.sol";
import {ReentrancyGuard} from "openzeppelin/utils/ReentrancyGuard.sol";

import {Settlement} from "./Settlement.sol";

/// @title BotStadia standing-orders contract
/// @notice Deposits and rules for bets placed automatically
/// (MONEY.md#standing-orders): "back bot X with N USDC in every bettable
/// session it's posted in", at most a set number of times per UTC day.
///
/// It knows nothing about posts or pools, and only ever bets into its one
/// settlement contract, by its depositors' own rules. It has no admin and no
/// gate: withdrawal can never be paused. Fills are open to anyone, who is
/// paid the fixed fill fee per fill.
contract StandingOrders is ReentrancyGuard {
    /// Paid to the filler per fill, from the owner's deposit. Fixed at deploy,
    /// with no setter. 0.01 USDC.
    uint256 public constant FILL_FEE = 10_000;

    Settlement public immutable SETTLEMENT;
    IERC20 public immutable TOKEN;

    struct Order {
        uint256 amount; // per session; zero: no order
        uint32 dailyCap; // fills per UTC day
    }

    struct FillItem {
        address owner;
        bytes32 bot;
        bytes32 session;
        bytes opening; // forwarded unread to the settlement contract, or empty
    }

    /// Each owner's deposit.
    mapping(address => uint256) public balances;
    /// Each owner's order per bot, by bundle digest.
    mapping(address => mapping(bytes32 => Order)) public orders;
    /// Which order has filled which session.
    mapping(address => mapping(bytes32 => mapping(bytes32 => bool))) public filled;
    /// Each order's fills on a UTC day.
    mapping(address => mapping(bytes32 => mapping(uint256 => uint32))) public fillsOnDay;

    event Deposited(address indexed owner, uint256 amount);
    event Withdrawn(address indexed owner, uint256 amount);
    event OrderSet(address indexed owner, bytes32 indexed bot, uint256 amount, uint32 dailyCap);
    event Filled(
        address indexed owner, bytes32 indexed bot, bytes32 indexed session, address filler, uint256 amount
    );
    event Skipped(address indexed owner, bytes32 indexed bot, bytes32 indexed session, string reason);

    constructor(Settlement settlement) {
        SETTLEMENT = settlement;
        TOKEN = settlement.TOKEN();
        require(TOKEN.approve(address(settlement), type(uint256).max), "approve");
    }

    function deposit(uint256 amount) external nonReentrant {
        balances[msg.sender] += amount;
        require(TOKEN.transferFrom(msg.sender, address(this), amount), "transfer");
        emit Deposited(msg.sender, amount);
    }

    /// Set, change or (with a zero amount) clear an order. The amount is at
    /// least the settlement contract's minimum bet.
    function setOrder(bytes32 bot, uint256 amount, uint32 dailyCap) external {
        if (amount != 0) {
            require(amount >= SETTLEMENT.MIN_BET(), "below the minimum bet");
            require(dailyCap > 0, "daily cap");
        }
        orders[msg.sender][bot] = Order(amount, amount == 0 ? 0 : dailyCap);
        emit OrderSet(msg.sender, bot, amount, dailyCap);
    }

    /// Never pausable: nothing here can stop it.
    function withdraw(uint256 amount) external nonReentrant {
        balances[msg.sender] -= amount;
        require(TOKEN.transfer(msg.sender, amount), "transfer");
        emit Withdrawn(msg.sender, amount);
    }

    /// Fill a list of (owner, bot, session), open to anyone. An entry that
    /// can't be placed is skipped with an event; the batch never fails over
    /// one entry. The fees for the fills that succeed are paid to `recipient`.
    function fill(FillItem[] calldata items, address recipient) external nonReentrant {
        uint256 fees;
        uint256 day = block.timestamp / 1 days;
        for (uint256 i = 0; i < items.length; i++) {
            FillItem calldata item = items[i];
            Order memory order = orders[item.owner][item.bot];
            if (order.amount == 0) {
                emit Skipped(item.owner, item.bot, item.session, "no order");
                continue;
            }
            if (filled[item.owner][item.bot][item.session]) {
                emit Skipped(item.owner, item.bot, item.session, "already filled");
                continue;
            }
            if (fillsOnDay[item.owner][item.bot][day] >= order.dailyCap) {
                emit Skipped(item.owner, item.bot, item.session, "daily cap");
                continue;
            }
            uint256 cost = order.amount + FILL_FEE;
            if (balances[item.owner] < cost) {
                emit Skipped(item.owner, item.bot, item.session, "balance");
                continue;
            }
            if (item.opening.length != 0) {
                // A pass-through: ignored if the pool is already open.
                try SETTLEMENT.openPool(abi.decode(item.opening, (Settlement.Opening))) {} catch {}
            }
            try SETTLEMENT.betFor(item.session, item.bot, order.amount, item.owner) {
                filled[item.owner][item.bot][item.session] = true;
                fillsOnDay[item.owner][item.bot][day] += 1;
                balances[item.owner] -= cost;
                fees += FILL_FEE;
                emit Filled(item.owner, item.bot, item.session, msg.sender, order.amount);
            } catch {
                emit Skipped(item.owner, item.bot, item.session, "settlement refused");
            }
        }
        if (fees > 0) require(TOKEN.transfer(recipient, fees), "transfer");
    }
}
