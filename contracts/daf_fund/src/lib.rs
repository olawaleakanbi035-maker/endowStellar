#![no_std]
use soroban_sdk::{
    contract, contractimpl, contracttype, symbol_short, token, Address, Env, String, Symbol, Vec,
};

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct GrantRecommendation {
    pub recipient: Address,
    pub amount: i128,
    pub memo: String,
    pub executed: bool,
}

#[contracttype]
#[derive(Clone, Debug, PartialEq)]
pub struct PendingGrant {
    pub id: u32,
    pub token_address: Address,
    pub recipient: Address,
    pub amount: i128,
    pub memo: String,
    pub approvals: Vec<Address>,
    pub executed: bool,
}

const DONOR: Symbol = symbol_short!("DONOR");
const NAME: Symbol = symbol_short!("NAME");
const GRANTS: Symbol = symbol_short!("GRANTS");
const SIGNERS: Symbol = symbol_short!("SIGNERS");
const THRESHOLD: Symbol = symbol_short!("THRESHOLD");
const PENDING: Symbol = symbol_short!("PENDING");
const NEXT_ID: Symbol = symbol_short!("NEXT_ID");

#[contract]
pub struct DafFund;

#[contractimpl]
impl DafFund {
    /// Initialize an individual DAF Fund with a primary Donor Advisor and Fund Name
    pub fn init(env: Env, donor: Address, fund_name: String) {
        Self::init_with_threshold(env, donor, fund_name, 1);
    }

    /// Initialize a DAF Fund with a multi-sig advisory approval threshold
    pub fn init_with_threshold(env: Env, donor: Address, fund_name: String, threshold: u32) {
        if env.storage().instance().has(&DONOR) {
            panic!("Fund already initialized");
        }
        if threshold == 0 {
            panic!("Threshold must be at least 1");
        }
        env.storage().instance().set(&DONOR, &donor);
        env.storage().instance().set(&NAME, &fund_name);
        env.storage().instance().set(&THRESHOLD, &threshold);
        env.storage()
            .instance()
            .set(&SIGNERS, &Vec::<Address>::new(&env));
        env.storage()
            .instance()
            .set(&PENDING, &Vec::<PendingGrant>::new(&env));
        env.storage().instance().set(&NEXT_ID, &0u32);
    }

    /// Register a signatory (donor advisor authorized)
    pub fn add_signatory(env: Env, signatory: Address) {
        let donor: Address = env.storage().instance().get(&DONOR).expect("Not initialized");
        donor.require_auth();

        let mut signers: Vec<Address> = env
            .storage()
            .instance()
            .get(&SIGNERS)
            .unwrap_or(Vec::new(&env));
        if !signers.contains(&signatory) {
            signers.push_back(signatory);
        }
        env.storage().instance().set(&SIGNERS, &signers);
    }

    /// Remove a signatory (donor advisor authorized)
    pub fn remove_signatory(env: Env, signatory: Address) {
        let donor: Address = env.storage().instance().get(&DONOR).expect("Not initialized");
        donor.require_auth();

        let signers: Vec<Address> = env
            .storage()
            .instance()
            .get(&SIGNERS)
            .unwrap_or(Vec::new(&env));
        let mut updated: Vec<Address> = Vec::new(&env);
        for s in signers.iter() {
            if s != signatory {
                updated.push_back(s);
            }
        }
        env.storage().instance().set(&SIGNERS, &updated);
    }

    /// Set the approval threshold (donor advisor authorized)
    pub fn set_threshold(env: Env, threshold: u32) {
        let donor: Address = env.storage().instance().get(&DONOR).expect("Not initialized");
        donor.require_auth();
        if threshold == 0 {
            panic!("Threshold must be at least 1");
        }
        env.storage().instance().set(&THRESHOLD, &threshold);
    }

    /// Recommend a grant disbursement from the DAF to a verified charity / recipient
    pub fn recommend_grant(env: Env, token_address: Address, recipient: Address, amount: i128, memo: String) {
        let donor: Address = env.storage().instance().get(&DONOR).expect("Not initialized");
        donor.require_auth();

        let threshold: u32 = env.storage().instance().get(&THRESHOLD).unwrap_or(1);

        if threshold <= 1 {
            // Immediate disbursement
            let client = token::Client::new(&env, &token_address);
            let contract_address = env.current_contract_address();
            client.transfer(&contract_address, &recipient, &amount);

            let mut grants: Vec<GrantRecommendation> = env
                .storage()
                .instance()
                .get(&GRANTS)
                .unwrap_or(Vec::new(&env));

            grants.push_back(GrantRecommendation {
                recipient: recipient.clone(),
                amount,
                memo,
                executed: true,
            });

            env.storage().instance().set(&GRANTS, &grants);

            env.events().publish(
                (symbol_short!("grant"), symbol_short!("disbursed")),
                (recipient, amount),
            );
        } else {
            // Store as pending until threshold approvals are collected
            let next_id: u32 = env.storage().instance().get(&NEXT_ID).unwrap_or(0);
            let mut pending: Vec<PendingGrant> = env
                .storage()
                .instance()
                .get(&PENDING)
                .unwrap_or(Vec::new(&env));

            pending.push_back(PendingGrant {
                id: next_id,
                token_address,
                recipient,
                amount,
                memo,
                approvals: Vec::new(&env),
                executed: false,
            });

            env.storage().instance().set(&PENDING, &pending);
            env.storage().instance().set(&NEXT_ID, &(next_id + 1));
        }
    }

    /// Approve a pending grant. Disburses once the threshold is reached.
    pub fn approve_grant(env: Env, grant_id: u32) {
        let threshold: u32 = env.storage().instance().get(&THRESHOLD).unwrap_or(1);
        let signers: Vec<Address> = env
            .storage()
            .instance()
            .get(&SIGNERS)
            .unwrap_or(Vec::new(&env));

        let mut pending: Vec<PendingGrant> = env
            .storage()
            .instance()
            .get(&PENDING)
            .unwrap_or(Vec::new(&env));

        let mut index: Option<u32> = None;
        for (i, g) in pending.iter().enumerate() {
            if g.id == grant_id {
                index = Some(i as u32);
                break;
            }
        }
        let idx = index.expect("Pending grant not found");
        let mut grant = pending.get(idx).unwrap();

        if grant.executed {
            panic!("Grant already executed");
        }

        // The approver must be a registered signatory and authorize the call.
        // Find which signatory is calling by requiring auth on each candidate is not
        // possible; instead require the caller to be passed implicitly via auth.
        // We require that at least one registered signatory authorizes this invocation.
        let mut approver: Option<Address> = None;
        for s in signers.iter() {
            // require_auth will succeed only for the actual invoker among signatories
            // We use try_require_auth-like pattern via require_auth_for_args is not
            // available; so we rely on the first signatory that authorizes.
            // To keep it deterministic, we check each and break on success.
            // Soroban's require_auth panics if not authorized, so we cannot probe.
            // Instead, we accept the approver as the first signatory not yet approved
            // and require its auth.
            if !grant.approvals.contains(&s) {
                approver = Some(s);
                break;
            }
        }
        let approver = approver.expect("No eligible signatory to approve");
        approver.require_auth();

        grant.approvals.push_back(approver.clone());
        pending.set(idx, grant.clone());
        env.storage().instance().set(&PENDING, &pending);

        env.events().publish(
            (symbol_short!("grant"), symbol_short!("approved")),
            (grant_id, approver),
        );

        if grant.approvals.len() >= threshold {
            let client = token::Client::new(&env, &grant.token_address);
            let contract_address = env.current_contract_address();
            client.transfer(&contract_address, &grant.recipient, &grant.amount);

            grant.executed = true;
            pending.set(idx, grant.clone());
            env.storage().instance().set(&PENDING, &pending);

            let mut grants: Vec<GrantRecommendation> = env
                .storage()
                .instance()
                .get(&GRANTS)
                .unwrap_or(Vec::new(&env));
            grants.push_back(GrantRecommendation {
                recipient: grant.recipient.clone(),
                amount: grant.amount,
                memo: grant.memo.clone(),
                executed: true,
            });
            env.storage().instance().set(&GRANTS, &grants);

            env.events().publish(
                (symbol_short!("grant"), symbol_short!("disbursed")),
                (grant.recipient, grant.amount),
            );
        }
    }

    /// Get DAF donor advisor
    pub fn get_donor(env: Env) -> Address {
        env.storage().instance().get(&DONOR).expect("Not initialized")
    }

    /// Get DAF fund name
    pub fn get_name(env: Env) -> String {
        env.storage().instance().get(&NAME).expect("Not initialized")
    }

    /// Get all grants history
    pub fn get_grants(env: Env) -> Vec<GrantRecommendation> {
        env.storage().instance().get(&GRANTS).unwrap_or(Vec::new(&env))
    }

    /// Get registered signatories
    pub fn get_signatories(env: Env) -> Vec<Address> {
        env.storage()
            .instance()
            .get(&SIGNERS)
            .unwrap_or(Vec::new(&env))
    }

    /// Get the approval threshold
    pub fn get_threshold(env: Env) -> u32 {
        env.storage().instance().get(&THRESHOLD).unwrap_or(1)
    }

    /// Get all pending grants
    pub fn get_pending_grants(env: Env) -> Vec<PendingGrant> {
        env.storage()
            .instance()
            .get(&PENDING)
            .unwrap_or(Vec::new(&env))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use soroban_sdk::testutils::Address as _;
    use soroban_sdk::token::{Client as TokenClient, StellarAssetClient};

    fn setup_token(env: &Env, admin: &Address) -> Address {
        let sac = env.register_stellar_asset_contract_v2(admin.clone());
        sac.address()
    }

    #[test]
    fn test_init_with_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, DafFund);
        let client = DafFundClient::new(&env, &contract_id);

        let donor = Address::generate(&env);
        let name = String::from_str(&env, "Family Foundation");
        client.init_with_threshold(&donor, &name, &2);

        assert_eq!(client.get_threshold(), 2);
        assert_eq!(client.get_donor(), donor);
        assert_eq!(client.get_name(), name);
    }

    #[test]
    fn test_signatory_management() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, DafFund);
        let client = DafFundClient::new(&env, &contract_id);

        let donor = Address::generate(&env);
        let s1 = Address::generate(&env);
        let s2 = Address::generate(&env);
        client.init_with_threshold(&donor, &String::from_str(&env, "Fund"), &2);

        client.add_signatory(&s1);
        client.add_signatory(&s2);
        assert_eq!(client.get_signatories().len(), 2);

        client.remove_signatory(&s1);
        assert_eq!(client.get_signatories().len(), 1);

        client.set_threshold(&3);
        assert_eq!(client.get_threshold(), 3);
    }

    #[test]
    fn test_immediate_disbursement_when_threshold_one() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, DafFund);
        let client = DafFundClient::new(&env, &contract_id);

        let donor = Address::generate(&env);
        let admin = Address::generate(&env);
        let token = setup_token(&env, &admin);
        let token_admin = StellarAssetClient::new(&env, &token);
        token_admin.mint(&contract_id, &1000);

        client.init(&donor, &String::from_str(&env, "Fund"));
        assert_eq!(client.get_threshold(), 1);

        let recipient = Address::generate(&env);
        client.recommend_grant(&token, &recipient, &100, &String::from_str(&env, "grant"));

        let token_client = TokenClient::new(&env, &token);
        assert_eq!(token_client.balance(&recipient), 100);
        assert_eq!(client.get_grants().len(), 1);
        assert_eq!(client.get_pending_grants().len(), 0);
    }

    #[test]
    fn test_pending_and_blocked_until_threshold() {
        let env = Env::default();
        env.mock_all_auths();
        let contract_id = env.register_contract(None, DafFund);
        let client = DafFundClient::new(&env, &contract_id);

        let donor = Address::generate(&env);
        let admin = Address::generate(&env);
        let token = setup_token(&env, &admin);
        let token_admin = StellarAssetClient::new(&env, &token);
        token_admin.mint(&contract_id, &1000);

        let s1 = Address::generate(&env);
        let s2 = Address::generate(&env);
        let s3 = Address::generate(&env);

        client.init_with_threshold(&donor, &String::from_str(&env, "Fund"), &2);
        client.add_signatory(&s1);
        client.add_signatory(&s2);
        client.add_signatory(&s3);

        let recipient = Address::generate(&env);
        client.recommend_grant(&token, &recipient, &250, &String::from_str(&env, "grant"));

        let pending = client.get_pending_grants();
        assert_eq!(pending.len(), 1);
        assert!(!pending.get(0).unwrap().executed);

        let token_client = TokenClient::new(&env, &token);
        assert_eq!(token_client.balance(&recipient), 0);

        // First approval: still blocked
        client.approve_grant(&0);
        assert_eq!(token_client.balance(&recipient), 0);
        let pending = client.get_pending_grants();
        assert_eq!(pending.get(0).unwrap().approvals.len(), 1);
        assert!(!pending.get(0).unwrap().executed);

        // Second approval: threshold reached, disbursed
        client.approve_grant(&0);
        assert_eq!(token_client.balance(&recipient), 250);
        let pending = client.get_pending_grants();
        assert!(pending.get(0).unwrap().executed);
        assert_eq!(client.get_grants().len(), 1);
    }
}
