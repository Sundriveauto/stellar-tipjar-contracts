#![no_std]

use soroban_sdk::{
    contract, contracterror, contractimpl, contracttype, panic_with_error, symbol_short, token,
    Address, Env, Map, String, Vec,
};

#[cfg(test)]
extern crate std;

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TipWithMessage {
    pub sender: Address,
    pub creator: Address,
    pub amount: i128,
    pub message: String,
    pub metadata: Map<String, String>,
    pub timestamp: u64,
}

#[contracttype]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Milestone {
    pub id: u64,
    pub creator: Address,
    pub goal_amount: i128,
    pub current_amount: i128,
    pub description: String,
    pub deadline: Option<u64>,
    pub completed: bool,
}

/// Storage layout for persistent contract data.
#[derive(Clone)]
#[contracttype]
pub enum DataKey {
    /// Token contract address used for all tips.
    Token,
    /// Creator's currently withdrawable balance held by this contract.
    CreatorBalance(Address),
    /// Historical total tips ever received by creator.
    CreatorTotal(Address),
    /// Emergency pause state (bool).
    Paused,
    /// Contract administrator (Address).
    Admin,
    /// Messages appended for a creator.
    CreatorMessages(Address),
    /// Current number of milestones for a creator (used for ID).
    MilestoneCounter(Address),
    /// Data for a specific milestone.
    Milestone(Address, u64),
    /// Active milestone IDs for a creator to track.
    ActiveMilestones(Address),
}

#[contracterror]
#[derive(Copy, Clone, Debug, Eq, PartialEq, PartialOrd, Ord)]
#[repr(u32)]
pub enum TipJarError {
    AlreadyInitialized = 1,
    TokenNotInitialized = 2,
    InvalidAmount = 3,
    NothingToWithdraw = 4,
    MessageTooLong = 5,
    MilestoneNotFound = 6,
    MilestoneAlreadyCompleted = 7,
    InvalidGoalAmount = 8,
}

#[contract]
pub struct TipJarContract;

#[contractimpl]
impl TipJarContract {
    /// One-time setup to choose the token contract and administrator for the TipJar.
    pub fn init(env: Env, token: Address, admin: Address) {
        if env.storage().instance().has(&DataKey::Token) {
            panic_with_error!(&env, TipJarError::AlreadyInitialized);
        }
        env.storage().instance().set(&DataKey::Token, &token);
        env.storage().instance().set(&DataKey::Admin, &admin);
        env.storage().instance().set(&DataKey::Paused, &false);
    }

    /// Moves `amount` tokens from `sender` into contract escrow for `creator`.
    ///
    /// The sender must authorize this call and have enough token balance.
    pub fn tip(env: Env, sender: Address, creator: Address, amount: i128) {
        Self::require_not_paused(&env);
        if amount <= 0 {
            panic_with_error!(&env, TipJarError::InvalidAmount);
        }

        sender.require_auth();

        let token_id = Self::read_token(&env);
        let token_client = token::Client::new(&env, &token_id);
        let contract_address = env.current_contract_address();

        // Transfer tokens into contract escrow first so creators can withdraw later.
        token_client.transfer(&sender, &contract_address, &amount);

        let creator_balance_key = DataKey::CreatorBalance(creator.clone());
        let creator_total_key = DataKey::CreatorTotal(creator.clone());

        let current_balance: i128 = env
            .storage()
            .persistent()
            .get(&creator_balance_key)
            .unwrap_or(0);
        let current_total: i128 = env
            .storage()
            .persistent()
            .get(&creator_total_key)
            .unwrap_or(0);

        let next_balance = current_balance + amount;
        let next_total = current_total + amount;

        env.storage()
            .persistent()
            .set(&creator_balance_key, &next_balance);
        env.storage()
            .persistent()
            .set(&creator_total_key, &next_total);

        // Event topics: ("tip", creator). Event data: (sender, amount).
        env.events()
            .publish((symbol_short!("tip"), creator.clone()), (sender, amount));

        Self::update_milestones(&env, creator, amount);
    }

    /// Allows supporters to attach a note and metadata to a tip.
    pub fn tip_with_message(
        env: Env,
        sender: Address,
        creator: Address,
        amount: i128,
        message: String,
        metadata: Map<String, String>,
    ) {
        Self::require_not_paused(&env);
        if amount <= 0 {
            panic_with_error!(&env, TipJarError::InvalidAmount);
        }
        if message.len() > 280 {
            panic_with_error!(&env, TipJarError::MessageTooLong);
        }

        sender.require_auth();

        let token_id = Self::read_token(&env);
        let token_client = token::Client::new(&env, &token_id);
        let contract_address = env.current_contract_address();

        // Transfer tokens into contract escrow first so creators can withdraw later.
        token_client.transfer(&sender, &contract_address, &amount);

        let creator_balance_key = DataKey::CreatorBalance(creator.clone());
        let creator_total_key = DataKey::CreatorTotal(creator.clone());
        let creator_msgs_key = DataKey::CreatorMessages(creator.clone());

        let current_balance: i128 = env
            .storage()
            .persistent()
            .get(&creator_balance_key)
            .unwrap_or(0);
        let current_total: i128 = env
            .storage()
            .persistent()
            .get(&creator_total_key)
            .unwrap_or(0);

        let next_balance = current_balance + amount;
        let next_total = current_total + amount;

        env.storage()
            .persistent()
            .set(&creator_balance_key, &next_balance);
        env.storage()
            .persistent()
            .set(&creator_total_key, &next_total);

        // Store message
        let timestamp = env.ledger().timestamp();
        let payload = TipWithMessage {
            sender: sender.clone(),
            creator: creator.clone(),
            amount,
            message: message.clone(),
            metadata: metadata.clone(),
            timestamp,
        };
        let mut messages: Vec<TipWithMessage> = env
            .storage()
            .persistent()
            .get(&creator_msgs_key)
            .unwrap_or_else(|| Vec::new(&env));
        messages.push_back(payload);
        env.storage().persistent().set(&creator_msgs_key, &messages);

        // Emit message payload
        env.events().publish(
            (symbol_short!("tip_msg"), creator.clone()),
            (sender, amount, message, metadata),
        );

        Self::update_milestones(&env, creator, amount);
    }

    /// Returns total historical tips for a creator.
    pub fn get_total_tips(env: Env, creator: Address) -> i128 {
        let key = DataKey::CreatorTotal(creator);
        env.storage().persistent().get(&key).unwrap_or(0)
    }

    /// Returns stored messages for a creator.
    pub fn get_messages(env: Env, creator: Address) -> Vec<TipWithMessage> {
        let key = DataKey::CreatorMessages(creator);
        env.storage()
            .persistent()
            .get(&key)
            .unwrap_or_else(|| Vec::new(&env))
    }

    /// Returns currently withdrawable escrowed tips for a creator.
    pub fn get_withdrawable_balance(env: Env, creator: Address) -> i128 {
        let key = DataKey::CreatorBalance(creator);
        env.storage().persistent().get(&key).unwrap_or(0)
    }

    /// Allows creator to withdraw their accumulated escrowed tips.
    pub fn withdraw(env: Env, creator: Address) {
        Self::require_not_paused(&env);
        creator.require_auth();

        let key = DataKey::CreatorBalance(creator.clone());
        let amount: i128 = env.storage().persistent().get(&key).unwrap_or(0);
        if amount <= 0 {
            panic_with_error!(&env, TipJarError::NothingToWithdraw);
        }

        let token_id = Self::read_token(&env);
        let token_client = token::Client::new(&env, &token_id);
        let contract_address = env.current_contract_address();

        token_client.transfer(&contract_address, &creator, &amount);
        env.storage().persistent().set(&key, &0i128);

        env.events()
            .publish((symbol_short!("withdraw"), creator), amount);
    }

    /// Creates a new milestone for a creator to track funding goals.
    pub fn create_milestone(
        env: Env,
        creator: Address,
        goal_amount: i128,
        description: String,
        deadline: Option<u64>,
    ) -> u64 {
        Self::require_not_paused(&env);
        creator.require_auth();

        if goal_amount <= 0 {
            panic_with_error!(&env, TipJarError::InvalidGoalAmount);
        }

        let id = Self::get_and_inc_milestone_counter(&env, creator.clone());
        let milestone = Milestone {
            id,
            creator: creator.clone(),
            goal_amount,
            current_amount: 0,
            description: description.clone(),
            deadline,
            completed: false,
        };

        env.storage()
            .persistent()
            .set(&DataKey::Milestone(creator.clone(), id), &milestone);

        let mut active_milestones = Self::get_active_milestones(&env, creator.clone());
        active_milestones.push_back(id);
        env.storage()
            .persistent()
            .set(&DataKey::ActiveMilestones(creator.clone()), &active_milestones);

        env.events().publish(
            (symbol_short!("ms_start"), creator, id),
            (goal_amount, description),
        );

        id
    }

    /// Returns the progress percentage for a specific milestone.
    pub fn get_milestone_progress(env: Env, creator: Address, milestone_id: u64) -> u32 {
        let milestone = Self::get_milestone_internal(&env, creator, milestone_id);
        if milestone.goal_amount == 0 {
            return 0;
        }

        let progress = (milestone.current_amount * 100) / milestone.goal_amount;
        if progress > 100 {
            100
        } else {
            progress as u32
        }
    }

    /// Returns all milestones for a specific creator.
    pub fn get_milestones(env: Env, creator: Address) -> Vec<Milestone> {
        let counter = env
            .storage()
            .persistent()
            .get(&DataKey::MilestoneCounter(creator.clone()))
            .unwrap_or(0u64);
        let mut milestones = Vec::new(&env);
        for id in 0..counter {
            if let Some(ms) = env
                .storage()
                .persistent()
                .get::<_, Milestone>(&DataKey::Milestone(creator.clone(), id))
            {
                milestones.push_back(ms);
            }
        }
        milestones
    }

    /// Internal helper to update all active milestones for a creator when they receive a tip.
    fn update_milestones(env: &Env, creator: Address, amount: i128) {
        let active_ids = Self::get_active_milestones(env, creator.clone());
        let mut new_active_ids = Vec::new(env);
        let now = env.ledger().timestamp();

        for id in active_ids.iter() {
            let mut ms = Self::get_milestone_internal(env, creator.clone(), id);

            // Skip if deadline passed
            if let Some(deadline) = ms.deadline {
                if now > deadline {
                    continue; // Milestone expired, but we keep it in active list until we decide otherwise?
                    // For now, let's just not update it.
                }
            }

            ms.current_amount += amount;

            if ms.current_amount >= ms.goal_amount {
                ms.completed = true;
                env.events().publish(
                    (symbol_short!("ms_done"), creator.clone(), id),
                    ms.goal_amount,
                );
            } else {
                new_active_ids.push_back(id);
            }

            env.storage()
                .persistent()
                .set(&DataKey::Milestone(creator.clone(), id), &ms);
        }

        env.storage()
            .persistent()
            .set(&DataKey::ActiveMilestones(creator.clone()), &new_active_ids);
    }

    fn get_milestone_internal(env: &Env, creator: Address, id: u64) -> Milestone {
        env.storage()
            .persistent()
            .get(&DataKey::Milestone(creator, id))
            .unwrap_or_else(|| panic_with_error!(env, TipJarError::MilestoneNotFound))
    }

    fn get_active_milestones(env: &Env, creator: Address) -> Vec<u64> {
        env.storage()
            .persistent()
            .get(&DataKey::ActiveMilestones(creator))
            .unwrap_or_else(|| Vec::new(env))
    }

    fn get_and_inc_milestone_counter(env: &Env, creator: Address) -> u64 {
        let key = DataKey::MilestoneCounter(creator.clone());
        let count = env.storage().persistent().get(&key).unwrap_or(0u64);
        env.storage().persistent().set(&key, &(count + 1));
        count
    }

    fn read_token(env: &Env) -> Address {
        env.storage()
            .instance()
            .get(&DataKey::Token)
            .unwrap_or_else(|| panic_with_error!(env, TipJarError::TokenNotInitialized))
    }

    /// Emergency pause to stop all state-changing activities (Admin only).
    pub fn pause(env: Env, admin: Address) {
        admin.require_auth();
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if admin != stored_admin {
            panic!("Unauthorized");
        }
        env.storage().instance().set(&DataKey::Paused, &true);
    }

    /// Resume contract activities after an emergency pause (Admin only).
    pub fn unpause(env: Env, admin: Address) {
        admin.require_auth();
        let stored_admin: Address = env.storage().instance().get(&DataKey::Admin).unwrap();
        if admin != stored_admin {
            panic!("Unauthorized");
        }
        env.storage().instance().set(&DataKey::Paused, &false);
    }

    /// Internal helper to check if the contract is paused.
    fn require_not_paused(env: &Env) {
        let is_paused: bool = env
            .storage()
            .instance()
            .get(&DataKey::Paused)
            .unwrap_or(false);
        if is_paused {
            panic!("Contract is paused");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use soroban_sdk::{testutils::Address as _, testutils::Ledger as _, token, Address, Env};

    fn setup() -> (Env, Address, Address, Address) {
        let env = Env::default();
        env.mock_all_auths();

        let token_admin = Address::generate(&env);
        let token_id = env
            .register_stellar_asset_contract_v2(token_admin.clone())
            .address();

        let admin = Address::generate(&env);
        let contract_id = env.register(TipJarContract, ());
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        tipjar_client.init(&token_id, &admin);

        (env, contract_id, token_id, admin)
    }

    #[test]
    fn test_tipping_functionality() {
        let (env, contract_id, token_id, _) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let token_client = token::Client::new(&env, &token_id);
        let token_admin_client = token::StellarAssetClient::new(&env, &token_id);
        let sender = Address::generate(&env);
        let creator = Address::generate(&env);

        token_admin_client.mint(&sender, &1_000);
        tipjar_client.tip(&sender, &creator, &250);

        assert_eq!(token_client.balance(&sender), 750);
        assert_eq!(token_client.balance(&contract_id), 250);
        assert_eq!(tipjar_client.get_total_tips(&creator), 250);
    }

    #[test]
    fn test_tipping_with_message_functionality() {
        let (env, contract_id, token_id, _) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let token_client = token::Client::new(&env, &token_id);
        let token_admin_client = token::StellarAssetClient::new(&env, &token_id);
        let sender = Address::generate(&env);
        let creator = Address::generate(&env);

        let message = soroban_sdk::String::from_str(&env, "Great job!");
        let metadata = soroban_sdk::Map::new(&env);

        token_admin_client.mint(&sender, &1_000);
        tipjar_client.tip_with_message(&sender, &creator, &250, &message, &metadata);

        assert_eq!(token_client.balance(&sender), 750);
        assert_eq!(token_client.balance(&contract_id), 250);
        assert_eq!(tipjar_client.get_total_tips(&creator), 250);

        let msgs = tipjar_client.get_messages(&creator);
        assert_eq!(msgs.len(), 1);
        let msg = msgs.get(0).unwrap();
        assert_eq!(msg.message, message);
    }

    #[test]
    #[should_panic(expected = "Error(Contract, #5)")]
    fn test_tipping_message_too_long() {
        let (env, contract_id, token_id, _) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let token_admin_client = token::StellarAssetClient::new(&env, &token_id);
        let sender = Address::generate(&env);
        let creator = Address::generate(&env);

        let long_str = "x".repeat(281);
        let message = soroban_sdk::String::from_str(&env, &long_str);
        let metadata = soroban_sdk::Map::new(&env);

        token_admin_client.mint(&sender, &1_000);
        tipjar_client.tip_with_message(&sender, &creator, &250, &message, &metadata);
    }

    #[test]
    fn test_balance_tracking_and_withdraw() {
        let (env, contract_id, token_id, _) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let token_client = token::Client::new(&env, &token_id);
        let token_admin_client = token::StellarAssetClient::new(&env, &token_id);
        let sender_a = Address::generate(&env);
        let sender_b = Address::generate(&env);
        let creator = Address::generate(&env);

        token_admin_client.mint(&sender_a, &1_000);
        token_admin_client.mint(&sender_b, &1_000);

        tipjar_client.tip(&sender_a, &creator, &100);
        tipjar_client.tip(&sender_b, &creator, &300);

        assert_eq!(tipjar_client.get_total_tips(&creator), 400);
        assert_eq!(tipjar_client.get_withdrawable_balance(&creator), 400);
        assert_eq!(token_client.balance(&contract_id), 400);

        tipjar_client.withdraw(&creator);

        assert_eq!(tipjar_client.get_withdrawable_balance(&creator), 0);
        assert_eq!(token_client.balance(&creator), 400);
        assert_eq!(token_client.balance(&contract_id), 0);
    }

    #[test]
    #[should_panic]
    fn test_invalid_tip_amount() {
        let (env, contract_id, token_id, _) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let token_admin_client = token::StellarAssetClient::new(&env, &token_id);
        let sender = Address::generate(&env);
        let creator = Address::generate(&env);

        token_admin_client.mint(&sender, &100);

        // Zero tips are rejected to prevent accidental or abusive calls.
        tipjar_client.tip(&sender, &creator, &0);
    }

    #[test]
    fn test_pause_unpause() {
        let (env, contract_id, _token_id, admin) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);

        tipjar_client.pause(&admin);

        let sender = Address::generate(&env);
        let creator = Address::generate(&env);

        // This should fail
        let result = tipjar_client.try_tip(&sender, &creator, &100);
        assert!(result.is_err());

        // Unpause
        tipjar_client.unpause(&admin);

        // This should now succeed (once we mint tokens)
        let token_admin_client = token::StellarAssetClient::new(&env, &_token_id);
        token_admin_client.mint(&sender, &100);
        tipjar_client.tip(&sender, &creator, &100);
        assert_eq!(tipjar_client.get_total_tips(&creator), 100);
    }

    #[test]
    #[should_panic(expected = "Unauthorized")]
    fn test_pause_admin_only() {
        let (env, contract_id, _, _) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let non_admin = Address::generate(&env);

        tipjar_client.pause(&non_admin);
    }

    #[test]
    fn test_withdraw_blocked_when_paused() {
        let (env, contract_id, token_id, admin) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let token_admin_client = token::StellarAssetClient::new(&env, &token_id);
        let sender = Address::generate(&env);
        let creator = Address::generate(&env);

        token_admin_client.mint(&sender, &100);
        tipjar_client.tip(&sender, &creator, &100);

        tipjar_client.pause(&admin);

        let result = tipjar_client.try_withdraw(&creator);
        assert!(result.is_err());
    }

    #[test]
    fn test_milestone_creation() {
        let (env, contract_id, _, _) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let creator = Address::generate(&env);

        let description = soroban_sdk::String::from_str(&env, "New Camera Gear");
        let goal_amount = 1000i128;
        let milestone_id = tipjar_client.create_milestone(&creator, &goal_amount, &description, &None);

        assert_eq!(milestone_id, 0);
        let milestones = tipjar_client.get_milestones(&creator);
        assert_eq!(milestones.len(), 1);
        let ms = milestones.get(0).unwrap();
        assert_eq!(ms.goal_amount, goal_amount);
        assert_eq!(ms.description, description);
        assert!(!ms.completed);
    }

    #[test]
    fn test_milestone_progress() {
        let (env, contract_id, token_id, _) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let token_admin_client = token::StellarAssetClient::new(&env, &token_id);
        let sender = Address::generate(&env);
        let creator = Address::generate(&env);

        let description = soroban_sdk::String::from_str(&env, "New Camera Gear");
        let milestone_id = tipjar_client.create_milestone(&creator, &1000i128, &description, &None);

        token_admin_client.mint(&sender, &1000);
        tipjar_client.tip(&sender, &creator, &250);

        assert_eq!(tipjar_client.get_milestone_progress(&creator, &milestone_id), 25);
        
        let ms = tipjar_client.get_milestones(&creator).get(0).unwrap();
        assert_eq!(ms.current_amount, 250);
        assert!(!ms.completed);
    }

    #[test]
    fn test_milestone_completion() {
        let (env, contract_id, token_id, _) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let token_admin_client = token::StellarAssetClient::new(&env, &token_id);
        let sender = Address::generate(&env);
        let creator = Address::generate(&env);

        let description = soroban_sdk::String::from_str(&env, "New Camera Gear");
        let milestone_id = tipjar_client.create_milestone(&creator, &1000i128, &description, &None);

        token_admin_client.mint(&sender, &1000);
        tipjar_client.tip(&sender, &creator, &1000);

        assert_eq!(tipjar_client.get_milestone_progress(&creator, &milestone_id), 100);
        
        let ms = tipjar_client.get_milestones(&creator).get(0).unwrap();
        assert_eq!(ms.current_amount, 1000);
        assert!(ms.completed);
    }

    #[test]
    fn test_multiple_concurrent_milestones() {
        let (env, contract_id, token_id, _) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let token_admin_client = token::StellarAssetClient::new(&env, &token_id);
        let sender = Address::generate(&env);
        let creator = Address::generate(&env);

        let desc1 = soroban_sdk::String::from_str(&env, "Goal 1");
        let desc2 = soroban_sdk::String::from_str(&env, "Goal 2");
        
        let ms1_id = tipjar_client.create_milestone(&creator, &1000i128, &desc1, &None);
        let ms2_id = tipjar_client.create_milestone(&creator, &500i128, &desc2, &None);

        token_admin_client.mint(&sender, &1000);
        tipjar_client.tip(&sender, &creator, &300);

        assert_eq!(tipjar_client.get_milestone_progress(&creator, &ms1_id), 30);
        assert_eq!(tipjar_client.get_milestone_progress(&creator, &ms2_id), 60);

        tipjar_client.tip(&sender, &creator, &200);
        
        // ms2 should be completed
        let milestones = tipjar_client.get_milestones(&creator);
        let ms1 = milestones.get(0).unwrap();
        let ms2 = milestones.get(1).unwrap();
        
        assert_eq!(ms1.current_amount, 500);
        assert!(!ms1.completed);
        assert_eq!(ms2.current_amount, 500);
        assert!(ms2.completed);

        // Further tips should only update ms1
        tipjar_client.tip(&sender, &creator, &100);
        let milestones_updated = tipjar_client.get_milestones(&creator);
        assert_eq!(milestones_updated.get(0).unwrap().current_amount, 600);
        assert_eq!(milestones_updated.get(1).unwrap().current_amount, 500); // ms2 stayed at 500
    }

    #[test]
    fn test_milestone_deadline_expired() {
        let (env, contract_id, token_id, _) = setup();
        let tipjar_client = TipJarContractClient::new(&env, &contract_id);
        let token_admin_client = token::StellarAssetClient::new(&env, &token_id);
        let sender = Address::generate(&env);
        let creator = Address::generate(&env);

        let deadline = 1000u64;
        env.ledger().set_timestamp(500);
        
        let description = soroban_sdk::String::from_str(&env, "Time limited goal");
        let ms_id = tipjar_client.create_milestone(&creator, &1000i128, &description, &Some(deadline));

        token_admin_client.mint(&sender, &1000);
        tipjar_client.tip(&sender, &creator, &100);
        assert_eq!(tipjar_client.get_milestone_progress(&creator, &ms_id), 10);

        // Advance time past deadline
        env.ledger().set_timestamp(1001);
        tipjar_client.tip(&sender, &creator, &200);
        
        // Progress should NOT have updated for this milestone
        assert_eq!(tipjar_client.get_milestone_progress(&creator, &ms_id), 10);
        
        let ms = tipjar_client.get_milestones(&creator).get(0).unwrap();
        assert_eq!(ms.current_amount, 100);
    }
}
